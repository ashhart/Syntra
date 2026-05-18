# Copyright 2026 Syntra contributors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
"""syntra_export — library for exporting a capsule's learned state.

This module provides :func:`fetch_capsule_export` which calls the read-only
Syntra HTTP API and assembles the portable snapshot JSON that operators can
archive, move between instances, or feed into ``syntra-ope evaluate --mode
static`` as a ``--policy-json`` file.
"""
from __future__ import annotations

import hashlib
import json
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional

__all__ = [
    "SyntraExportError",
    "fetch_json",
    "derive_policy_by_context",
    "fetch_capsule_export",
]

# ── Exceptions ──────────────────────────────────────────────────────────────


class SyntraExportError(RuntimeError):
    """Raised when a Syntra HTTP response indicates an error."""

    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"HTTP {status}: {body}")
        self.status = status
        self.body = body


# ── HTTP helpers ─────────────────────────────────────────────────────────────


def fetch_json(url: str, admin_key: str) -> Any:
    """GET *url* with a Bearer token and return the parsed JSON body.

    Raises :class:`SyntraExportError` for any non-2xx response.
    """
    req = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {admin_key}"},
    )
    try:
        with urllib.request.urlopen(req) as resp:
            raw = resp.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise SyntraExportError(exc.code, body) from exc
    return json.loads(raw)


def fetch_text(url: str, admin_key: str) -> str:
    """GET *url* and return the raw response body as text.

    Used for the ``/decisions`` and ``/audits`` endpoints which return
    newline-delimited JSON logs rather than a single JSON object.

    Raises :class:`SyntraExportError` for any non-2xx response.
    """
    req = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {admin_key}"},
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise SyntraExportError(exc.code, body) from exc


# ── Policy derivation ─────────────────────────────────────────────────────────


def derive_policy_by_context(memory: Dict[str, Any]) -> Dict[str, Any]:
    """Build a ``policyByContext`` lookup table from a ``GET /memory`` payload.

    The memory JSON returned by Syntra has this shape (version 7)::

        {
          "version": 7,
          "strategies": {
            "<node_id>": {
              "nodeId": ...,
              "nOptions": N,
              "contexts": {
                "<context_key>": { "weights": [...], ... }
              },
              "candidateContexts": {
                "<candidate_id>|<context_key>": { "weights": [...], ... }
              },
              "metaBandit": { "leader": "<CandidateId>", ... } | null
            }
          }
        }

    For each strategy the function:

    1. Reads the meta-bandit leader string from ``metaBandit.leader`` (if
       present — the field is omitted / null during warmup).
    2. If a leader is known, scans ``candidateContexts`` for keys prefixed
       ``"<leader>|"`` and uses those buckets.
    3. Falls back to the legacy ``contexts`` bucket when no leader is set or
       no candidate-context entries exist for that leader.
    4. Emits one entry per ``(strategy, context_key)`` pair containing
       ``bestOption`` (the option index with the highest weight) and the full
       normalised ``weights`` vector.

    The returned dict maps ``<context_key>`` to its entry.  When multiple
    strategies share a context key the last writer wins (rare in practice
    because most capsules have a single strategy node).

    This output is intentionally compatible with the
    ``context_key -> bestOption`` shape consumed by
    ``syntra-ope evaluate --mode static --policy-json``.
    """
    policy: Dict[str, Any] = {}
    strategies = memory.get("strategies", {})

    for _node_id, sm in strategies.items():
        n_options: int = sm.get("nOptions", 0)
        if n_options == 0:
            continue

        # Determine the meta-bandit leader for this strategy (may be None).
        meta_bandit = sm.get("metaBandit")
        leader: Optional[str] = None
        if isinstance(meta_bandit, dict):
            leader = meta_bandit.get("leader") or None

        # Collect (context_key, weights) pairs from the appropriate bucket.
        buckets: List[tuple[str, List[float]]] = []

        if leader is not None:
            prefix = f"{leader}|"
            candidate_contexts: Dict[str, Any] = sm.get("candidateContexts", {})
            for combined_key, bucket in candidate_contexts.items():
                if combined_key.startswith(prefix):
                    ctx_key = combined_key[len(prefix):]
                    weights = bucket.get("weights", [])
                    if weights:
                        buckets.append((ctx_key, weights))

        # Fall back to the legacy contexts bucket when the candidate bucket is
        # empty (capsule still in warmup or no leader elected yet).
        if not buckets:
            contexts: Dict[str, Any] = sm.get("contexts", {})
            for ctx_key, bucket in contexts.items():
                weights = bucket.get("weights", [])
                if weights:
                    buckets.append((ctx_key, weights))

        for ctx_key, weights in buckets:
            # argmax of the weight vector
            best_idx = max(range(len(weights)), key=lambda i: weights[i])
            policy[ctx_key] = {
                "bestOption": best_idx,
                "weights": weights,
            }

    return policy


# ── Main export function ──────────────────────────────────────────────────────


def fetch_capsule_export(
    syntra_url: str,
    admin_key: str,
    tenant: str,
    job: str,
    capsule: str,
    include_decisions: bool = False,
    include_audits: bool = False,
    include_snapshots: bool = False,
) -> Dict[str, Any]:
    """Fetch a full capsule state snapshot from a running Syntra instance.

    Calls the following read-only endpoints:

    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/inspect``  — hash + version
    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/learning`` — LearningConfig
    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/report``   — strategy weights
    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/memory``   — full memory sidecar

    And optionally:

    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/decisions``
    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/audits``
    * ``GET /tenants/{t}/jobs/{j}/capsules/{c}/snapshots`` (metadata only)

    Returns a dict matching the version-1 export schema.
    """
    base = (
        f"{syntra_url.rstrip('/')}"
        f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
    )

    inspect = fetch_json(f"{base}/inspect", admin_key)
    learning_cfg = fetch_json(f"{base}/learning", admin_key)
    report = fetch_json(f"{base}/report", admin_key)
    memory = fetch_json(f"{base}/memory", admin_key)

    capsule_hash: str = inspect.get("hash", "")

    # Syntra version is embedded in the inspect payload when available.
    syntra_version: str = inspect.get("syntraVersion", inspect.get("version", "unknown"))

    # Warmup state: derive from report or inspect overlay metadata.
    # The report payload has a `weightsSource` field on each strategy when
    # in Active state; inspect has a top-level `warmupState` field on newer
    # server builds. Prefer the explicit field; fall back to heuristic.
    warmup_state: str = inspect.get("warmupState", "")
    if not warmup_state:
        strategies = report.get("strategies", [])
        if strategies and strategies[0].get("weightsSource") == "meta_bandit_leader":
            warmup_state = "active"
        else:
            warmup_state = "warmup"

    # Meta-bandit leader: prefer report overlay metadata, then memory.
    meta_bandit_leader: str = ""
    strategies = report.get("strategies", [])
    if strategies:
        meta_bandit_leader = strategies[0].get("leaderCandidate", "")
    if not meta_bandit_leader:
        # Try to extract from memory directly (any strategy node).
        for sm in memory.get("strategies", {}).values():
            mb = sm.get("metaBandit")
            if isinstance(mb, dict):
                candidate = mb.get("leader", "")
                if candidate:
                    meta_bandit_leader = candidate
                    break

    policy_by_context = derive_policy_by_context(memory)

    export: Dict[str, Any] = {
        "v": 1,
        "exportedAt": int(time.time()),
        "syntraVersion": syntra_version,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "capsuleHash": capsule_hash,
        "learningConfig": learning_cfg,
        "report": report,
        "memory": memory,
        "warmupState": warmup_state,
        "metaBanditLeader": meta_bandit_leader,
        "policyByContext": policy_by_context,
    }

    if include_decisions:
        raw = fetch_text(f"{base}/decisions", admin_key)
        export["decisions"] = _parse_ndjson(raw)

    if include_audits:
        raw = fetch_text(f"{base}/audits", admin_key)
        export["audits"] = _parse_ndjson(raw)

    if include_snapshots:
        # /snapshots returns {"snapshots": [...metadata...]} — no bodies.
        snaps_payload = fetch_json(f"{base}/snapshots", admin_key)
        export["snapshots"] = snaps_payload.get("snapshots", [])

    return export


# ── Utilities ─────────────────────────────────────────────────────────────────


def _parse_ndjson(text: str) -> List[Any]:
    """Parse newline-delimited JSON into a list, skipping blank lines."""
    result = []
    for line in text.splitlines():
        line = line.strip()
        if line:
            try:
                result.append(json.loads(line))
            except json.JSONDecodeError:
                # Non-JSON lines (comments, partial writes) are preserved as
                # plain strings so the operator can still see them.
                result.append(line)
    return result


def compute_policy_hash(policy_by_context: Dict[str, Any]) -> str:
    """SHA-256 of the canonical JSON encoding of ``policyByContext``."""
    canonical = json.dumps(policy_by_context, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode()).hexdigest()
