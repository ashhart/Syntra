"""Prometheus metrics scrape and dashboard walkthrough.

Demonstrates GET /metrics, hand-rolled COUNTER/GAUGE/HISTOGRAM parsing
(no prometheus_client dep), and a dashboard: top capsule by decide volume,
p99 latency, refusal rate, meta-bandit candidate trial distribution.

Prerequisites: Syntra at $SYNTRA_URL; /metrics is public, no auth needed.
Run another walkthrough first to populate interesting counters.

Usage:
    python3 07_metrics_scrape.py [--syntra-url URL] [--dump-raw]

Apache-2.0.
"""
from __future__ import annotations
import argparse, json, os
import urllib.request, urllib.error


# ── Tiny Prometheus text-format parser ────────────────────────────────────────

def parse_labels(s):
    result = {}; i = 0; s = s.strip()
    while i < len(s):
        eq = s.find("=", i)
        if eq < 0: break
        key = s[i:eq].strip()
        if eq+1 >= len(s) or s[eq+1] != '"': break
        end = s.find('"', eq+2)
        while 0 < end and s[end-1] == '\\':
            end = s.find('"', end+1)
        if end < 0: break
        result[key] = s[eq+2:end].replace('\\"', '"').replace('\\\\', '\\')
        i = end+1
        if i < len(s) and s[i] == ',': i += 1
    return result


def parse_metrics(text):
    """Parse Prometheus text exposition into {canonical_name: {type, samples}}."""
    result = {}; types = {}
    for line in text.splitlines():
        line = line.strip()
        if not line: continue
        if line.startswith("# TYPE "):
            parts = line.split()
            if len(parts) >= 4: types[parts[2]] = parts[3]
            continue
        if line.startswith("#"): continue
        bo = line.find("{"); bc = line.find("}")
        if bo >= 0 and bc > bo:
            base = line[:bo]; lstr = line[bo+1:bc]; rest = line[bc+1:].split()
        else:
            parts = line.split(); base = parts[0] if parts else ""; lstr = ""; rest = parts[1:]
        if not rest: continue
        try:
            value = float(rest[0])
        except ValueError:
            continue
        labels = parse_labels(lstr)
        suffix = ""
        canonical = base
        for sfx in ("_bucket", "_count", "_sum", "_total"):
            if base.endswith(sfx):
                canonical = base[:-len(sfx)]; suffix = sfx; break
        if canonical not in result:
            result[canonical] = {
                "type": types.get(canonical, types.get(base, "untyped")), "samples": []
            }
        result[canonical]["samples"].append(
            {"labels": labels, "value": value, "suffix": suffix, "raw_name": base}
        )
    return result


# ── Dashboard helpers ──────────────────────────────────────────────────────────

def _find(metrics, *names):
    for n in names:
        if n in metrics: return metrics[n]
    return None


def top_capsule_by_decides(metrics):
    m = _find(metrics, "syntra_request_total", "lycan_request_total",
              "syntra_request", "lycan_request")
    if not m:
        cands = [k for k in metrics if "request" in k]
        if not cands: return None
        m = metrics[cands[0]]
    best = None; best_v = -1.0
    for s in m["samples"]:
        lb = s["labels"]
        if lb.get("kind") not in (None, "decide"): continue
        if lb.get("status", "ok") != "ok": continue
        if s["value"] > best_v:
            best_v = s["value"]
            best = {"tenant": lb.get("tenant","?"), "job": lb.get("job","?"),
                    "capsule": lb.get("capsule","?"), "decides": int(s["value"])}
    return best


def estimate_p99_latency(metrics):
    m = _find(metrics, "syntra_decide_latency_seconds", "lycan_decide_latency_seconds")
    if not m:
        cands = [k for k in metrics if "latency" in k]
        if not cands: return None
        m = metrics[cands[0]]
    buckets = []; total = 0.0
    for s in m["samples"]:
        if s["suffix"] == "_bucket":
            le_s = s["labels"].get("le", "+Inf")
            try: le = float(le_s) if le_s != "+Inf" else float("inf")
            except ValueError: le = float("inf")
            buckets.append((le, s["value"]))
        elif s["suffix"] == "_count":
            total = s["value"]
    if not buckets or total == 0: return None
    buckets.sort(key=lambda x: x[0]); target = 0.99 * total
    for i, (le, cum) in enumerate(buckets):
        if cum >= target:
            if i == 0: return le
            pl, pc = buckets[i-1]
            frac = (target - pc) / max(cum - pc, 1e-9)
            upper = le if le != float("inf") else pl * 10
            return pl + frac * (upper - pl)
    return None


def refusal_rate(metrics):
    ref_m = _find(metrics, "syntra_refusals_total", "lycan_refusals_total",
                  "syntra_refusals", "lycan_refusals")
    req_m = _find(metrics, "syntra_request_total", "lycan_request_total",
                  "syntra_request", "lycan_request")
    refusals = sum(s["value"] for s in (ref_m or {}).get("samples", [])
                   if s["suffix"] in ("_total", ""))
    decides = sum(s["value"] for s in (req_m or {}).get("samples", [])
                  if s["labels"].get("kind") == "decide" and s["labels"].get("status") == "ok")
    return refusals / decides if decides > 0 else None


def candidate_trial_distribution(metrics):
    m = _find(metrics, "syntra_meta_bandit_trials", "lycan_meta_bandit_trials")
    if not m: return None
    dist = {}
    for s in m["samples"]:
        c = s["labels"].get("candidate", "?")
        dist[c] = dist.get(c, 0) + int(s["value"])
    return dist or None


def sec(t): print(f"\n{'='*55}\n  {t}\n{'='*55}")


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"))
    p.add_argument("--dump-raw", action="store_true")
    args = p.parse_args()
    url = args.syntra_url.rstrip("/")

    sec("1. Fetching /metrics")
    req = urllib.request.Request(f"{url}/metrics")
    try:
        with urllib.request.urlopen(req) as r:
            raw = r.read().decode()
    except urllib.error.HTTPError as e:
        raise SystemExit(f"GET /metrics failed {e.code}: {e.reason}")
    print(json.dumps({"lines": raw.count("\n"), "bytes": len(raw)}))
    if args.dump_raw:
        sec("Raw exposition"); print(raw)

    sec("2. Parsed metric families")
    metrics = parse_metrics(raw)
    for name, m in sorted(metrics.items()):
        print(json.dumps({"metric": name, "type": m["type"],
                           "sample_count": len(m["samples"])}))

    sec("3. Dashboard")
    top = top_capsule_by_decides(metrics)
    print(json.dumps({"gauge": "top_capsule_by_decide_volume", "value": top}))

    p99 = estimate_p99_latency(metrics)
    print(json.dumps({"gauge": "decide_p99_latency_seconds",
                       "value": round(p99, 6) if p99 else None,
                       "value_ms": round(p99*1000, 3) if p99 else None}))

    rate = refusal_rate(metrics)
    print(json.dumps({"gauge": "global_refusal_rate",
                       "value": round(rate, 6) if rate is not None else None}))

    dist = candidate_trial_distribution(metrics)
    print(json.dumps({"gauge": "meta_bandit_candidate_trial_distribution", "value": dist}))

    print("\nDone.")


if __name__ == "__main__":
    main()
