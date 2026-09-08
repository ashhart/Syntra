# /decide Performance Baseline

Stage 7 baseline: measured concurrent `/decide` traffic before any
"production-grade" throughput claim. Reproduce with
`./scripts/bench-decide.sh [duration] [concurrency] [churn|keepalive]`
(boots its own server against a throwaway store).

## Environment (numbers are machine-specific)

- Apple M5 Max, 18 cores, 128 GB, macOS 26.5.2 (arm64), release build.
- Client and server on localhost; the Python client can itself cap
  throughput (GIL, per-request object churn) — treat sub-13k rps rows
  as min(client, server).
- Workload: the bench capsule (6-node LLM-routing graph, one strategy
  node, fixed input), shadow mode (`learn=false`), warm page cache.
- Server default posture: 8 worker threads, token-bucket limiter at
  1000 req/s/token + 2000 burst.

## Results (2026-09-08, post fsync-elimination)

### Default configuration (churn client, limiter active)

| concurrency | req/s | errors | client p50/p95/p99 ms | server mean ms |
|---|---|---|---|---|
| 1  | 1198 | 14002 (429s) | 0.44 / 0.84 / 1.19  | 0.19 |
| 8  | 1196 | 60387 (429s) | 1.21 / 1.93 / 2.57  | 0.29 |
| 32 | 1197 | 58280 (429s) | 4.40 / 8.67 / 36.31 | 0.30 |

Throughput is the **rate limiter** (1000/s + draining the 2000-token
burst over 10 s = 1200/s flat). "errors" are HTTP 429 refusals, not
failures: accepted requests never error.

### Keep-alive, limiter raised (`SYNTRA_RATE_LIMIT_RPS=1000000`)

| concurrency | req/s  | errors | client p50/p95/p99 ms | server mean ms |
|---|---|---|---|---|
| 1  | 5,296  | 0 | 0.18 / 0.24 / 0.36   | 0.13 |
| 4  | 11,281 | 0 | 0.32 / 0.58 / 0.78   | 0.27 |
| 8  | 11,591 | 0 | 0.64 / 1.09 / 1.45   | 0.59 |
| 16 | 10,234 | 0 | 1.41 / 2.51 / 3.42   | 0.77 |

Peak observed: **~11.6k sustained shadow decisions/s**, p99 1.45 ms,
server-side mean 0.59 ms at saturation (8 workers). At c=16 throughput
falls while latency rises — the Python client becomes co-limiting;
11.6k is a floor on server capability, not a ceiling.

## What the baseline exposed (and fixed)

1. **Per-request fsync in shadow decide (was 250/s).** Every `/decide`
   called `save_memory_in_job` → atomic write + `fsync`. On APFS,
   fsync commits serialize even across processes: two server instances
   shared one ~265 rps ceiling. Fix: content-aware save — serialize
   once, compare bytes, write only when changed (`memory.json`
   serialization is key-sorted, so steady-state shadow decides produce
   zero writes; any actual mutation still persists exactly as before).
   250 → thousands/s; server mean 4.83 ms → 0.27 ms at c=4.
2. **Metrics integrity.** `GET /metrics` double-counted hierarchical
   decide/feedback (handler + route recorded), legacy routes recorded
   nothing, and `decide_latency` was never observed on legacy paths.
   Recording cut over to routes uniformly (honest status per response);
   `syntra_decide_latency_seconds_count` now matches client-observed
   request counts exactly in every row above.
3. **The old numbers.** Pre-fix baseline (same script, same machine):
   4.83 ms mean / ~170 rps at c=1, flat ~250 rps at every concurrency,
   keep-alive or not — fsync was the wall, HTTP was never the wall.

## Decision note: tiny_http vs axum/hyper

**Keep tiny_http.** The measured ceiling is not the HTTP layer:
- Server mean decide cost at 11.6k rps is 0.59 ms; the worker pool, not
  connection handling, is the first constraint.
- Churn clients (new TCP connection per request) top out around
  ~1.3k accepted/s even with the limiter raised — beyond that, accept
  backlog errors appear. That is the pessimistic client pattern;
  keep-alive clients (every real SDK default) do not hit it, and the
  limiter deliberately caps clients above it anyway.
- The real capacity levers, in order: hot-path fsync (fixed), the
  per-token limiter, worker count. None of these require an engine
  swap.

Revisit axum/hyper if: TLS termination moves in-process, streaming
(SSE/websocket) endpoints are added, multi-instance fan-out is needed,
or keep-alive demand pushes the worker-pool math past ~20k rps.

## Known limits (honest caveats)

- `learn=true` decides hold the capsule lock and fsync graph+memory —
  governed/learning traffic costs far more per request than shadow;
  this baseline is the shadow-mode hot path.
- Append-only decision/audit logs stay buffered (no per-append fsync);
  a crash can lose the log tail (see `docs/store-retention.md`).
- The limiter is per token/principal; unauthenticated deployments
  share one bucket (or none, when no admin key is set).
