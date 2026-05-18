# Syntra Throughput & Latency Benchmark

A standalone Python harness that characterizes how many `/decide` and
`/feedback` requests per second a Syntra deployment can sustain at various
concurrency levels, and what the p50/p95/p99/p999 latencies look like.

## What it measures

- **Throughput (rps)** — requests per second for each operation kind
  (decide, feedback), measured over the configured window after a warmup
  period that lets the JIT, connection pools, and Syntra's warmup phase
  stabilise.
- **Latency percentiles** — p50/p95/p99/p999 in milliseconds, computed
  without numpy using a sorted-list + bisect approach. Memory cost is
  bounded: at 1 000 ops/s × 30 s = 30 000 floats ≈ 240 KB per operation
  kind.
- **Error breakdown** — connection refused, timeouts, HTTP 4xx, HTTP 5xx,
  and watchdog bailouts (fired when a worker hits 100 consecutive failures
  to prevent silently racking up errors).

## Quick start

```bash
cd examples/bench

# Run against a local dev instance (syntra binary must be on PATH):
python bench.py \
    --syntra-url http://localhost:8787 \
    --admin-key dev-key \
    --tenant acme --job perf --capsule harness \
    --concurrency 16 --duration-seconds 30 --warmup-seconds 5

# Skip capsule authoring if the capsule already exists:
python bench.py ... --no-author
```

Or use the convenience script that starts a fresh Docker container,
runs the bench, and tears down:

```bash
bash example_run.sh --concurrency 16 --duration-seconds 30 --ratio 3:1
```

## CLI reference

| Flag | Default | Description |
|---|---|---|
| `--syntra-url` | `http://localhost:8787` | Base URL of the Syntra instance |
| `--admin-key` | `$SYNTRA_ADMIN_KEY` or `dev-key` | Bearer token |
| `--tenant` | `bench` | Tenant identifier |
| `--job` | `perf` | Job identifier |
| `--capsule` | `harness` | Capsule identifier |
| `--concurrency N` | `8` | Concurrent worker threads |
| `--duration-seconds S` | `30` | Measurement window (warmup excluded) |
| `--warmup-seconds W` | `5` | Warmup period (results discarded) |
| `--ratio D:F` | `1:1` | Decide-to-feedback ratio; `3:1` = 3 decides then 1 feedback per cycle |
| `--context-type` | `discrete` | `discrete` = contextKey strings; `features` = typed vector (enables LinUCB) |
| `--no-author` | off | Skip capsule authoring (capsule must already exist) |

## Output

JSON is written to **stdout** so it can be piped to a file or downstream
tooling. A human-readable ASCII table and one-line summary are written to
**stderr**.

### JSON shape

```json
{
  "config": {
    "concurrency": 16,
    "duration_s": 30,
    "warmup_s": 5,
    "ratio": "1:1",
    "context_type": "discrete",
    "syntra_url": "http://localhost:8787",
    "capsule_path": "/tenants/bench/jobs/perf/capsules/harness"
  },
  "ops": {
    "decide": {
      "count": 12345,
      "throughput_rps": 411.5,
      "p50_ms": 2.10,
      "p95_ms": 4.80,
      "p99_ms": 12.00,
      "p999_ms": 28.40,
      "errors": 0
    },
    "feedback": {
      "count": 12342,
      "throughput_rps": 411.4,
      "p50_ms": 1.90,
      "p95_ms": 4.20,
      "p99_ms": 10.80,
      "p999_ms": 25.10,
      "errors": 0
    }
  },
  "errors": {
    "connection_refused": 0,
    "timeout": 0,
    "http_5xx": 0,
    "http_4xx": 0,
    "watchdog_bailouts": 0
  }
}
```

### Stderr table + summary

```
+----------+-------+-------+--------+--------+--------+---------+--------+
| op       | count | rps   | p50_ms | p95_ms | p99_ms | p999_ms | errors |
+----------+-------+-------+--------+--------+--------+---------+--------+
| decide   | 12345 | 411.5 | 2.10   | 4.80   | 12.00  | 28.40   | 0      |
| feedback | 12342 | 411.4 | 1.90   | 4.20   | 10.80  | 25.10   | 0      |
+----------+-------+-------+--------+--------+--------+---------+--------+

[bench] 30s @ N=16: 823 ops/s, p99 12.0 ms, 0 errors  [conn_refused=0 timeout=0 5xx=0 4xx=0 watchdog_bailouts=0]
```

## How to interpret the results

- **Throughput** is the steady-state rate after warmup. Compare runs at
  increasing `--concurrency` to find the saturation point.
- **p50** reflects the typical request latency. **p99** and **p999** reveal
  tail behaviour — long GC pauses, lock contention, or Syntra write-ahead
  journaling spikes show up here.
- **Warmup exclusion** is important: Syntra's capsule lifecycle starts in
  Warmup and runs uniform-random selection. The first several seconds of
  traffic are deliberately discarded so results reflect steady-state Active
  mode performance.
- **ratio** lets you shift load toward decide (CPU-heavier, reads learned
  weights) or feedback (I/O-heavier, mutates memory and writes to the
  audit log). A 3:1 decide/feedback ratio more closely models production
  traffic where outcomes arrive less frequently than decisions.

## Architecture notes

- **ThreadPoolExecutor** is used instead of asyncio because Syntra's Python
  SDK and the stdlib `http.client` are synchronous, and the benchmark's
  bottleneck is Syntra's server-side CPU, not Python's thread overhead.
  Threads give accurate wall-clock latencies without asyncio's cooperative
  scheduler adding noise.
- **Thread-local `http.client.HTTPConnection`** with `Connection: keep-alive`
  avoids per-request TCP handshake overhead so measured latency reflects
  the server, not connection setup.
- **No external dependencies** — stdlib only (urllib, http.client, bisect,
  concurrent.futures). Install nothing.

## Running the tests

```bash
cd examples/bench
PYTHONPATH=. python3 -m pytest tests/ -v
```

Tests cover:

1. `LatencyStats` percentiles on a known uniform distribution.
2. `LatencyStats` empty-input safety (zeros, not exceptions).
3. `parse_ratio` valid and invalid inputs.
4. Watchdog bailout when the target is unreachable.
5. `ascii_table` output format validation.
