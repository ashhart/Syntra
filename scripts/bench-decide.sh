#!/usr/bin/env bash
# /decide load benchmark.
#
# Measures the decision hot path under concurrent load:
#   - client-observed latency distribution (p50/p95/p99/max/mean)
#   - server-side latency histogram from /metrics (syntra_decide_latency_seconds)
#   - throughput (requests/sec, client-observed and server-counted)
#
# Usage: ./scripts/bench-decide.sh [duration_seconds] [concurrency] [mode]
# mode: churn (default) = new TCP connection per request, the pessimistic
#       real-world client; keepalive = persistent connections, exposes the
#       server's true ceiling without per-connection churn.
# Both defaults: 10 seconds, 8 workers. Self-contained: boots its own server
# against a throwaway store on a random port. Rate limiter stays at its
# default (1000 rps/token) unless SYNTRA_RATE_LIMIT_RPS is exported.
DURATION="${1:-10}"
CONCURRENCY="${2:-8}"
MODE="${3:-churn}"
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYNTRA="$ROOT/target/release/syntra"
LYCAN="$ROOT/target/release/lycan"

[[ -x "$SYNTRA" ]] || (cd "$ROOT" && cargo build --release --quiet --bin syntra)
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet --bin lycan)

STORE="$(mktemp -d "${TMPDIR:-/tmp}/syntra-bench.XXXXXX")/store"
KEY="bench-key"
PORT=$((20000 + RANDOM % 20000))
ADDR="127.0.0.1:$PORT"
BASE="http://$ADDR"

SERVER_PID=""
cleanup() { [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true; }
trap cleanup EXIT

"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  curl -sf "$BASE/health" >/dev/null 2>&1 && break
  sleep 0.1
done

# Minimal capsule with a strategy node — the default learning hot path.
SRC="$STORE/bench.lycs"
cat > "$SRC" <<'EOF'
($ inp (!cap "runtime.inputGet" "latencies"))
($ lat (? (!= inp null) inp (A 12 15 11 45 13)))
($ mean (!cap "stats.mean" lat))
($ decision (strategy "fast" "balanced" "safe"))
(!p mean)
EOF
"$LYCAN" compile "$SRC" >/dev/null
curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$STORE/bench.lyc" \
  "$BASE/tenants/bench/capsules/router/install" >/dev/null

snapshot_metrics() { curl -s "$BASE/metrics" | grep -E '^syntra_decide_latency_seconds_(count|sum)|^syntra_requests_total\{kind="decide"' || true; }
BEFORE=$(snapshot_metrics)

DURATION="$DURATION" CONCURRENCY="$CONCURRENCY" MODE="$MODE" URL="$BASE/tenants/bench/capsules/router/decide" KEY="$KEY" \
python3 - <<'PY'
import http.client, json, os, statistics, threading, time, urllib.parse, urllib.request

duration = float(os.environ["DURATION"])
concurrency = int(os.environ["CONCURRENCY"])
mode = os.environ.get("MODE", "churn")
url = os.environ["URL"]
key = os.environ["KEY"]
body = json.dumps({"latencies": [10, 20, 30, 40, 50]}).encode()
deadline = time.monotonic() + duration
latencies = []
errors = [0]
lock = threading.Lock()

def worker_churn():
    local = []
    while time.monotonic() < deadline:
        t0 = time.perf_counter()
        try:
            req = urllib.request.Request(url, data=body, method="POST",
                                         headers={"Authorization": f"Bearer {key}"})
            with urllib.request.urlopen(req, timeout=30) as resp:
                resp.read()
            local.append((time.perf_counter() - t0) * 1000.0)
        except Exception:
            with lock:
                errors[0] += 1
    with lock:
        latencies.extend(local)

def worker_keepalive():
    p = urllib.parse.urlsplit(url)
    local = []
    hdrs = {"Authorization": f"Bearer {key}", "Content-Type": "application/json",
            "Connection": "keep-alive"}
    try:
        conn = http.client.HTTPConnection(p.hostname, p.port, timeout=30)
    except Exception:
        with lock:
            errors[0] += 1
        return
    while time.monotonic() < deadline:
        t0 = time.perf_counter()
        try:
            conn.request("POST", p.path, body=body, headers=hdrs)
            response = conn.getresponse()
            response.read()
            if response.status != 200:
                with lock:
                    errors[0] += 1
                continue
            local.append((time.perf_counter() - t0) * 1000.0)
        except Exception:
            with lock:
                errors[0] += 1
            try:
                conn.close()
            except Exception:
                pass
            try:
                conn = http.client.HTTPConnection(p.hostname, p.port, timeout=30)
            except Exception:
                break
    try:
        conn.close()
    except Exception:
        pass
    with lock:
        latencies.extend(local)

worker = worker_keepalive if mode == "keepalive" else worker_churn
threads = [threading.Thread(target=worker) for _ in range(concurrency)]
t0 = time.monotonic()
for t in threads:
    t.start()
for t in threads:
    t.join()
elapsed = time.monotonic() - t0

latencies.sort()
n = len(latencies)
def pct(p):
    return latencies[min(n - 1, int(n * p / 100))] if n else 0.0
print(f"duration={elapsed:.2f}s concurrency={concurrency} mode={mode}")
print(f"client: {n} requests, {errors[0]} errors, {n / elapsed:.0f} req/s")
if n:
    print(f"client latency ms: p50={pct(50):.2f} p95={pct(95):.2f} p99={pct(99):.2f} "
          f"max={latencies[-1]:.2f} mean={statistics.fmean(latencies):.2f}")
if errors[0] or not n:
    raise SystemExit("benchmark failed: HTTP errors or no successful requests")
PY

AFTER=$(snapshot_metrics)
echo "server /metrics delta:"
python3 - "$BEFORE" "$AFTER" <<'PY'
import re, sys

def parse(text):
    out = {}
    for line in text.splitlines():
        m = re.match(r'^(syntra_\w+)(?:\{([^}]*)\})?\s+(\S+)$', line)
        if m:
            out[(m.group(1), m.group(2) or "")] = float(m.group(3))
    return out

before, after = parse(sys.argv[1]), parse(sys.argv[2])
def delta(name):
    keys = [k for k in after if k[0] == name]
    total_b = sum(before.get(k, 0.0) for k in keys)
    total_a = sum(after.get(k, 0.0) for k in keys)
    return total_b, total_a

cb, ca = delta("syntra_decide_latency_seconds_count")
print(f"  server decide count: {ca - cb:.0f}")
sb, sa = delta("syntra_decide_latency_seconds_sum")
n = ca - cb
if n > 0:
    print(f"  server mean latency: {(sa - sb) / n * 1000:.2f} ms")
PY
