#!/usr/bin/env python3
"""Read-only dashboard for the Syntra demo (Phase 2).

Backs the single-screen dashboard served from ``static/``. Polls the
running Syntra server for ``/memory`` (meta-bandit + buckets) and
``/decisions`` (decision-log NDJSON), and reads two on-disk files
directly off the store volume:

* ``warmup.json``  — capsule lifecycle (Warmup / Active / Frozen)
* ``learning.json`` — capsule learning config, used to detect whether
  this capsule runs the meta-bandit (default) or shared-state LinUCB.

The dashboard never calls ``/decide`` or ``/feedback`` — it is a pure
observer. Browser polls ``/api/state`` every 2 seconds.

Env vars (with defaults matching the demo container):

* ``LYCAN_ADMIN_KEY``  — bearer token for Syntra HTTP
* ``SYNTRA_URL``       — Syntra server URL (default ``http://127.0.0.1:8787``)
* ``LYCAN_STORE_ROOT`` — Syntra on-disk store root (default ``/syntra/data``)
* ``DEMO_TENANT`` / ``DEMO_JOB`` / ``DEMO_CAPSULE`` — capsule path segments
  used only as the default when ``/api/state`` is called without a
  ``capsule=`` query parameter.

Phase 2 changes:

* ``/api/capsules`` proxies ``GET /admin/capsules`` from Syntra. The
  browser uses this to populate the capsule switcher and to derive
  option labels (no more hardcoded OPTIONS_BY_CAPSULE table).
* ``/api/state`` accepts ``?capsule=tenant/job/capsule`` and resolves
  paths dynamically. Each decision entry now carries the upstream
  ``published`` map; the response also surfaces ``publishedLatest``
  (the most-recent map) and ``publishedSeries`` (the last 60 values
  per published key, oldest-first) so Region 5 can render headline
  numbers + sparklines without the browser parsing NDJSON itself.

The decision log has no on-disk timestamp, so ``recentDecisions``
still surfaces an ``observedAt`` (when the dashboard first saw the
id). The browser uses ``observedAt`` for the relative-time display.
"""
from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

from flask import Flask, jsonify, request, send_from_directory

ADMIN_KEY = os.environ.get("LYCAN_ADMIN_KEY", "dev")
SYNTRA_URL = os.environ.get("SYNTRA_URL", "http://127.0.0.1:8787")
STORE_ROOT = Path(os.environ.get("LYCAN_STORE_ROOT", "/syntra/data"))
DEFAULT_TENANT = os.environ.get("DEMO_TENANT", "demo")
DEFAULT_JOB = os.environ.get("DEMO_JOB", "retry")
DEFAULT_CAPSULE = os.environ.get("DEMO_CAPSULE", "router")

# Decision-id -> first-seen monotonic timestamp (epoch ms). Populated by
# /api/state; used to give the recent-decisions feed a stable "observed
# at" time, since the on-disk decision log has no timestamp field.
# Keyed by (capsule_path, decision_id) so a switch between capsules
# doesn't leak ids.
_first_seen: dict[tuple[str, str], int] = {}
_FIRST_SEEN_CAP = 5000  # bound memory usage on long-running dashboards

# How many recent decisions we expose in publishedSeries / recentDecisions.
PUBLISHED_SERIES_LEN = 60

app = Flask(__name__, static_folder="static", static_url_path="/static")


# --------------------------------------------------------------------------- #
# HTTP helpers
# --------------------------------------------------------------------------- #

def _request(path: str) -> urllib.request.Request:
    req = urllib.request.Request(SYNTRA_URL + path, method="GET")
    req.add_header("Authorization", f"Bearer {ADMIN_KEY}")
    return req


def _get_json(path: str) -> Any:
    with urllib.request.urlopen(_request(path), timeout=2.5) as resp:
        return json.loads(resp.read())


def _get_text(path: str) -> str:
    with urllib.request.urlopen(_request(path), timeout=2.5) as resp:
        return resp.read().decode()


# --------------------------------------------------------------------------- #
# Capsule path resolution                                                     #
# --------------------------------------------------------------------------- #

def _resolve_capsule_path(raw: str | None) -> tuple[str, str, str]:
    """Parse ``?capsule=tenant/job/capsule`` (or fall back to env defaults).

    Returns the three segments. Rejects path traversal characters; if the
    request supplied junk we silently fall back to the env defaults.
    """
    if not raw:
        return DEFAULT_TENANT, DEFAULT_JOB, DEFAULT_CAPSULE
    parts = raw.strip("/").split("/")
    if len(parts) != 3:
        return DEFAULT_TENANT, DEFAULT_JOB, DEFAULT_CAPSULE
    for p in parts:
        if not p or "." in p or any(c in p for c in (" ", "\\", "?", "#")):
            return DEFAULT_TENANT, DEFAULT_JOB, DEFAULT_CAPSULE
    return parts[0], parts[1], parts[2]


# --------------------------------------------------------------------------- #
# Local disk helpers
# --------------------------------------------------------------------------- #

def _capsule_disk_dir(tenant: str, job: str, capsule: str) -> Path:
    return STORE_ROOT / "tenants" / tenant / "jobs" / job / "capsules" / capsule


def _read_warmup(disk_dir: Path) -> dict[str, Any]:
    """Return ``{lifecycle, warmupProgress, algorithm}`` from warmup.json."""
    out: dict[str, Any] = {"lifecycle": "unknown", "warmupProgress": None, "algorithm": None}
    warmup_file = disk_dir / "warmup.json"
    if not warmup_file.exists():
        return out
    try:
        w = json.loads(warmup_file.read_text())
    except Exception as exc:  # noqa: BLE001
        out["lifecycle"] = f"warmup-parse-error: {exc}"
        return out
    lc = w.get("lifecycle", {})
    if "Warmup" in lc:
        out["lifecycle"] = "warmup"
        wm = lc["Warmup"]
        out["warmupProgress"] = {
            "collected": int(wm.get("samples_collected", 0)),
            "target": int(wm.get("target", 0)),
        }
    elif "Active" in lc:
        out["lifecycle"] = "active"
        out["algorithm"] = _algorithm_to_str(lc["Active"].get("algorithm"))
    elif "Frozen" in lc:
        out["lifecycle"] = "frozen"
        out["algorithm"] = _algorithm_to_str(lc["Frozen"].get("algorithm"))
    return out


def _algorithm_to_str(alg: Any) -> str | None:
    """The on-disk algorithm field is a tagged enum like ``{"UCB": {"c": 2.0}}``."""
    if alg is None:
        return None
    if isinstance(alg, str):
        return alg.lower()
    if isinstance(alg, dict) and alg:
        return next(iter(alg)).lower()
    return str(alg).lower()


def _detect_scoring_mode(disk_dir: Path) -> str:
    """Inspect sidecars on disk to determine the capsule's adaptive flavor.

    Detection order matches the server's /admin/capsules logic:
      1. hierarchical_spec.json present → hierarchical
      2. learning.json::sharedState.enabled → shared-state-linucb
      3. Otherwise → meta-bandit (default)
    """
    if (disk_dir / "hierarchical_spec.json").exists():
        return "hierarchical"
    learning_file = disk_dir / "learning.json"
    if not learning_file.exists():
        return "meta-bandit"
    try:
        cfg = json.loads(learning_file.read_text())
    except Exception:  # noqa: BLE001
        return "meta-bandit"
    shared = cfg.get("sharedState") or {}
    if isinstance(shared, dict) and shared.get("enabled") is True:
        return "shared-state-linucb"
    return "meta-bandit"


def _load_hierarchical_summary(disk_dir: Path) -> dict[str, Any] | None:
    """For hierarchical capsules: read hierarchical_state.json and emit a
    compact per-bucket summary the dashboard can render.

    Returns None when the capsule has no state file yet (freshly installed,
    no decides have run). Each bucket is summarised as:
        {key, depth, parentPath, branchingFactor, totalRounds,
         currentLeader, leaderMean, weights}
    Keyed by the HierStateKey string ("d0|", "d1|0", etc.). The dashboard
    renders one chart line per bucket plus the spec's leaf names for
    Region 3 labels.
    """
    state_file = disk_dir / "hierarchical_state.json"
    if not state_file.exists():
        return None
    try:
        state = json.loads(state_file.read_text())
    except Exception:  # noqa: BLE001
        return None
    buckets_raw = state.get("buckets") or {}
    if not isinstance(buckets_raw, dict):
        return None
    out_buckets: list[dict[str, Any]] = []
    for key, b in sorted(buckets_raw.items()):
        # HierStateKey format: "d{depth}|{comma-joined parent path}".
        depth = 0
        parent_path: list[int] = []
        try:
            head, tail = key.split("|", 1)
            depth = int(head.lstrip("d"))
            if tail:
                parent_path = [int(x) for x in tail.split(",")]
        except Exception:  # noqa: BLE001
            pass
        weights = b.get("weights") or []
        mb = b.get("metaBandit") or {}
        cands = mb.get("candidates") or []
        # Mean = cumulative_reward / trials, with the same forgetting-decayed
        # numbers the server uses. Leader = candidate with highest mean.
        leader = None
        leader_mean = 0.0
        for c in cands:
            trials = float(c.get("trials") or 0.0)
            cum = float(c.get("cumulative_reward") or 0.0)
            if trials > 1e-9:
                m = cum / trials
                if leader is None or m > leader_mean:
                    leader = c.get("id")
                    leader_mean = m
        out_buckets.append({
            "key": key,
            "depth": depth,
            "parentPath": parent_path,
            "branchingFactor": len(weights),
            "totalRounds": int(mb.get("total_rounds") or 0),
            "currentLeader": leader,
            "leaderMean": leader_mean,
            "weights": [float(w) for w in weights],
        })
    return {"buckets": out_buckets}


# --------------------------------------------------------------------------- #
# Decision log parsing
# --------------------------------------------------------------------------- #

def _published_for(ev: dict[str, Any]) -> dict[str, Any]:
    """Extract the ``published`` map from a decision event, defaulting to {}."""
    decisions = ev.get("decisions") or []
    if not decisions:
        return {}
    first = decisions[0]
    if not isinstance(first, dict):
        return {}
    pub = first.get("published")
    if not isinstance(pub, dict):
        return {}
    # Shallow copy so downstream mutation can't bleed back into the parse cache.
    return dict(pub)


def _parse_decisions(
    raw: str,
    now_ms: int,
    capsule_path: str,
) -> dict[str, Any]:
    """Walk the decision-log NDJSON and pull out counts + recent events.

    The caller supplies ``capsule_path`` ("t/j/c") so the per-id
    observed-at cache can be partitioned by capsule.
    """
    lines = [ln for ln in raw.splitlines() if ln.strip()]
    recent = lines[-1000:]

    counts: dict[int, int] = {}
    refused_total = 0
    parsed: list[dict[str, Any]] = []

    for ln in recent:
        try:
            ev = json.loads(ln)
        except json.JSONDecodeError:
            continue
        parsed.append(ev)
        if ev.get("refused"):
            refused_total += 1
            continue
        d0 = (ev.get("decisions") or [{}])[0]
        idx = d0.get("chosen_option")
        if not isinstance(idx, int):
            continue
        counts[idx] = counts.get(idx, 0) + 1

    counts_sorted = sorted(
        ({"optionIndex": k, "count": v} for k, v in counts.items()),
        key=lambda kv: -kv["count"],
    )

    # Recent feed: newest first. Use observedAt because the decision
    # log has no timestamp of its own.
    feed: list[dict[str, Any]] = []
    for ev in reversed(parsed[-PUBLISHED_SERIES_LEN:]):
        decision_id = ev.get("id") or ""
        key = (capsule_path, decision_id)
        observed = _first_seen.get(key)
        if observed is None:
            observed = now_ms
            if len(_first_seen) >= _FIRST_SEEN_CAP:
                # cheapest possible eviction: drop one arbitrary entry
                _first_seen.pop(next(iter(_first_seen)))
            _first_seen[key] = observed
        d0 = (ev.get("decisions") or [{}])[0]
        idx = d0.get("chosen_option")
        feed.append({
            "id": decision_id,
            "observedAt": observed,
            "optionIndex": idx if isinstance(idx, int) else None,
            "refused": bool(ev.get("refused")),
            "refusalReason": ev.get("refusalReason"),
            "algorithm": ev.get("algorithm"),
            "contextKey": ev.get("contextKey"),
            "published": _published_for(ev),
        })

    last_decision = feed[0] if feed else None
    last_update_ts = max(
        (e["observedAt"] for e in feed if isinstance(e.get("observedAt"), int)),
        default=None,
    )

    # publishedSeries: oldest-first per key, length-capped at PUBLISHED_SERIES_LEN.
    # publishedLatest: the published map from the newest non-refused event with one.
    series: dict[str, list[Any]] = {}
    latest: dict[str, Any] = {}
    tail = parsed[-PUBLISHED_SERIES_LEN:]
    for ev in tail:
        if ev.get("refused"):
            continue
        pub = _published_for(ev)
        for k, v in pub.items():
            series.setdefault(k, []).append(v)
    # Walk in reverse to find the most-recent non-empty published map.
    for ev in reversed(tail):
        if ev.get("refused"):
            continue
        pub = _published_for(ev)
        if pub:
            latest = pub
            break

    return {
        "decisionCounts": counts_sorted,
        "totalDecisions": len(recent),
        "refusedCount": refused_total,
        "recentDecisions": feed,
        "lastDecision": last_decision,
        "lastUpdateAt": last_update_ts,
        "publishedSeries": series,
        "publishedLatest": latest,
    }


# --------------------------------------------------------------------------- #
# Routes
# --------------------------------------------------------------------------- #

@app.route("/")
def index():
    return send_from_directory("static", "index.html")


@app.route("/api/capsules")
def capsules():
    """Proxy ``GET /admin/capsules`` from Syntra.

    The browser never holds the admin bearer; the Python layer is the
    auth gateway.
    """
    try:
        d = _get_json("/admin/capsules")
        return jsonify(d)
    except urllib.error.URLError as exc:
        return jsonify({"capsules": [], "error": str(exc)}), 502
    except Exception as exc:  # noqa: BLE001
        return jsonify({"capsules": [], "error": str(exc)}), 502


@app.route("/api/state")
def state():
    now_ms = int(time.time() * 1000)
    tenant, job, capsule = _resolve_capsule_path(request.args.get("capsule"))
    capsule_api_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
    disk_dir = _capsule_disk_dir(tenant, job, capsule)
    capsule_path = f"{tenant}/{job}/{capsule}"

    scoring_mode = _detect_scoring_mode(disk_dir)
    out: dict[str, Any] = {
        "capsulePath": capsule_path,
        "scoringMode": scoring_mode,
        "lifecycle": "unknown",
        "warmupProgress": None,
        "algorithm": None,
        "candidates": [],
        "sharedState": None,
        # For hierarchical capsules only: per-HierState bucket summary.
        # Null for meta-bandit / shared-state capsules. The dashboard JS
        # uses this to render one chart line per bucket.
        "hierarchical": (
            _load_hierarchical_summary(disk_dir)
            if scoring_mode == "hierarchical" else None
        ),
        "decisionCounts": [],
        "recentDecisions": [],
        "lastDecision": None,
        "lastUpdateAt": None,
        "totalDecisions": 0,
        "refusedCount": 0,
        "totalMetaRounds": 0,
        "publishedSeries": {},
        "publishedLatest": {},
        "serverNow": now_ms,
        "errors": [],
    }

    # Lifecycle / warmup straight from disk.
    out.update(_read_warmup(disk_dir))

    # /memory for meta-bandit candidates and (optionally) shared-state stats.
    try:
        mem = _get_json(f"{capsule_api_path}/memory")
    except urllib.error.URLError as exc:
        out["errors"].append(f"memory: {exc}")
        mem = {}
    except Exception as exc:  # noqa: BLE001
        out["errors"].append(f"memory: {exc}")
        mem = {}

    for sm in (mem.get("strategies") or {}).values():
        mb = sm.get("metaBandit") or {}
        out["totalMetaRounds"] = max(out["totalMetaRounds"], int(mb.get("totalRounds", 0)))
        for c in mb.get("candidates", []):
            trials = float(c.get("trials", 0.0))
            cum = float(c.get("cumulativeReward", 0.0))
            mean = (cum / trials) if trials > 1e-9 else 0.0
            out["candidates"].append({
                "id": c.get("id", "?"),
                "trials": trials,
                "meanReward": mean,
                "cumulativeReward": cum,
            })

        # When the capsule runs shared-state LinUCB the per-strategy
        # memory carries a single online posterior. We surface its trial
        # count so the dashboard can render a single line in Region 2.
        ss = sm.get("sharedState")
        if ss:
            n = ss.get("n") or ss.get("trials") or 0
            mean = ss.get("meanReward") or ss.get("mean") or 0.0
            out["sharedState"] = {
                "trials": float(n),
                "meanReward": float(mean),
            }

    # /decisions — NDJSON, last 1000 lines.
    try:
        raw = _get_text(f"{capsule_api_path}/decisions")
    except urllib.error.URLError as exc:
        out["errors"].append(f"decisions: {exc}")
        raw = ""
    except Exception as exc:  # noqa: BLE001
        out["errors"].append(f"decisions: {exc}")
        raw = ""

    if raw:
        out.update(_parse_decisions(raw, now_ms, capsule_path))

    return jsonify(out)


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=int(os.environ.get("DASHBOARD_PORT", 8080)))
