# syntra-ingest

A small sidecar that keeps a fresh snapshot of feature values pulled from
external systems (Prometheus, Datadog, SQL, a file on disk) and serves them
as JSON over HTTP. The intended consumer is whatever process is calling a
Syntra capsule's `/decide` endpoint: it `GET`s `/features/current`, reshapes
the snapshot into the capsule's expected feature vector, and posts it to
Syntra.

## Status

Working source + examples. **Tests and a Docker image are pending.**

## Do you actually need this?

Probably not. If your application already has the feature values in memory,
post them straight to Syntra `/decide`. You don't need a sidecar in front.

This exists for the awkward middle case: you want Syntra to make decisions
based on numbers that live somewhere else (a Prometheus scrape, a Datadog
metric, a counter in a SQLite file your batch job updates) and you don't
want to embed four client libraries inside your hot path.

The sidecar pulls those numbers on a timer and hands you the latest values
in one round trip. That's all it does.

## Mental model

**Best-effort cache. Not a metric store.**

- Latest value only, in memory. No history, no time series.
- One process. Single-instance. Restart = empty cache until pollers refill.
- No persistence. No durability. No replication.
- No auth. Bind to localhost. Put a TLS proxy in front if you need to expose it.
- Source failures are logged but do not stop the sidecar. The affected
  feature simply does not appear in the next snapshot. The caller decides
  what to do about that.
- This is **not** a Prometheus replacement. If you need historical data, use
  Prometheus. If you need durability, use a real database.

## Install

Local dev:

```bash
cd Syntra/sidecar
pip install -e .
syntra-ingest --config examples/mixed.yaml
```

Or run the module directly without installing:

```bash
cd Syntra/sidecar
pip install flask pyyaml requests
python -m syntra_ingest --config examples/mixed.yaml
```

By default the server binds to `127.0.0.1:9090`. Override with `--host` /
`--port`. The CLI also takes `--log-level`.

## Config schema

Top-level:

```yaml
staleness_seconds: 120     # /healthz returns 503 if no feature has updated
                           # within this window. Default 120.
sources: [...]             # list of source definitions, see below
```

Every source declares:

```yaml
type: prometheus | datadog | sql | file_watch
name: <string>             # used as the JSON key in the snapshot
interval_seconds: 30       # poll cadence (default 30)
timeout_seconds: 10        # per-poll timeout (default 10)
```

Source names must be unique. Names go straight into the snapshot dict, so
pick something your capsule will recognise.

### Source: `prometheus`

```yaml
- type: prometheus
  name: api_p95_latency_ms
  url: http://localhost:9091/api/v1/query
  query: 'histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket[5m])) by (le)) * 1000'
  interval_seconds: 15
```

Hits `GET {url}?query=<query>` and parses `data.result[0].value[1]` as a
float. This is for **scalar** queries (`/api/v1/query`). Range queries are
not supported — if you want a windowed value, aggregate it in PromQL.

### Source: `datadog`

```yaml
- type: datadog
  name: order_queue_depth
  query: 'max:app.orders.queue_depth{env:prod}'
  from_seconds_ago: 120
  aggregation: last        # last | mean | max | min  (default: last)
  interval_seconds: 30
  # url: https://api.datadoghq.eu/api/v1/query   # optional, defaults to .com
```

Credentials come from the environment:

```
DD_API_KEY=...
DD_APP_KEY=...
```

We don't read keys from YAML, on purpose. Keep secrets out of config files.

The poller queries the last `from_seconds_ago` seconds, takes
`series[0].pointlist`, strips nulls, and applies the aggregation.

### Source: `sql`

```yaml
- type: sql
  name: orders_last_hour
  database_path: /var/lib/myapp/state.sqlite
  sql: |
    SELECT COUNT(*)
    FROM orders
    WHERE created_at >= strftime('%s', 'now', '-1 hour')
  interval_seconds: 60
```

SQLite only. Opened read-only. The SQL must start with `SELECT`, `WITH`, or
`PRAGMA` — anything else is rejected at runtime. This isn't a security
boundary against an attacker who can edit your YAML; it's a guardrail
against typing `DELETE` by accident. Row 0, column 0 of the result is
coerced to a float.

### Source: `file_watch`

```yaml
- type: file_watch
  name: load_factor
  path: /etc/syntra/load.json
  format: json_path        # raw_float | json_path
  json_path: current.factor
  interval_seconds: 10
```

Re-reads the file each tick. Two formats:

- `raw_float`: file body is a number, parsed with `float()`.
- `json_path`: file body is JSON; `json_path` is a dot-separated key path
  (`a.b.c` → `data["a"]["b"]["c"]`).

No inotify. We just poll. If you need sub-second freshness, lower
`interval_seconds`.

## HTTP API

### `GET /features/current`

Returns the cached snapshot as a flat JSON object plus a `_meta` block:

```json
{
  "api_p95_latency_ms": 184.2,
  "load_factor": 0.72,
  "orders_last_hour": 1417.0,
  "_meta": {
    "api_p95_latency_ms": {"source": "prometheus", "stale_seconds": 4.1},
    "load_factor":        {"source": "file_watch", "stale_seconds": 1.8},
    "orders_last_hour":   {"source": "sql",         "stale_seconds": 22.5}
  }
}
```

`stale_seconds` is the wall-clock time since that feature was last
successfully refreshed. If the source has been failing for a while, that
number grows — the caller can decide whether to trust it.

Features that have never produced a value do not appear in the snapshot.
The caller must handle missing keys.

### `GET /healthz`

- `200 OK` with `{"status": "ok", ...}` if at least one feature was updated
  within `staleness_seconds`.
- `503` with `{"status": "stale" | "cold", ...}` otherwise.

This is a liveness signal for the cache, not a check on individual sources.
A single working source keeps the sidecar "healthy". For per-source health,
read `_meta.stale_seconds` from `/features/current`.

## Integration pattern with Syntra

The flow is intentionally one-shot per request:

```
your app  ──GET────►  syntra-ingest /features/current
your app  ──reshape──►  feature vector that your capsule expects
your app  ──POST───►  syntra /decide  {"features": {...}}
```

Worked example with `curl` and `jq`. Assume the sidecar is running on
:9090 and Syntra on :8080, and your capsule expects features named
`latency_ms`, `load`, and `orders_hr`:

```bash
# 1. Pull the snapshot.
SNAP=$(curl -s http://127.0.0.1:9090/features/current)

# 2. Reshape into the capsule's expected vector.
PAYLOAD=$(jq -n --argjson s "$SNAP" '{
  features: {
    latency_ms: $s.api_p95_latency_ms,
    load:       $s.load_factor,
    orders_hr:  $s.orders_last_hour
  }
}')

# 3. Ask Syntra to decide.
curl -s -X POST http://127.0.0.1:8080/decide \
  -H 'content-type: application/json' \
  -d "$PAYLOAD"
```

Do the reshape step in your app; don't let Syntra learn the names your
sidecar happens to use. The capsule's contract is the source of truth.

## Worked example end-to-end

Using `examples/mixed.yaml`:

```yaml
staleness_seconds: 120
sources:
  - type: prometheus
    name: api_p95_latency_ms
    url: http://localhost:9091/api/v1/query
    query: 'histogram_quantile(0.95, sum(rate(http_request_duration_seconds_bucket[5m])) by (le)) * 1000'
    interval_seconds: 15
  - type: file_watch
    name: load_factor
    path: /etc/syntra/load.json
    format: json_path
    json_path: current.factor
    interval_seconds: 10
  - type: sql
    name: orders_last_hour
    database_path: /var/lib/myapp/state.sqlite
    sql: SELECT COUNT(*) FROM orders WHERE created_at >= strftime('%s', 'now', '-1 hour')
    interval_seconds: 60
```

After about a minute of uptime, with all three sources healthy:

```bash
$ curl -s http://127.0.0.1:9090/features/current | jq
{
  "api_p95_latency_ms": 184.2,
  "load_factor": 0.72,
  "orders_last_hour": 1417,
  "_meta": {
    "api_p95_latency_ms": {"source": "prometheus", "stale_seconds": 4.1},
    "load_factor":        {"source": "file_watch", "stale_seconds": 1.8},
    "orders_last_hour":   {"source": "sql",         "stale_seconds": 22.5}
  }
}
```

Three features, three different `stale_seconds` reflecting the three
different `interval_seconds` and how far into each cycle we caught them.
Your app reshapes and posts to `/decide` as shown above.

### Failure modes for this example

- **Prometheus is down.** `poll_prometheus` logs a warning and returns
  `None`. The poller skips this tick. If Prometheus was previously up,
  `api_p95_latency_ms` keeps its last value but `stale_seconds` grows. If
  Prometheus was never up, the key is simply missing from the snapshot —
  your reshape step will produce `null` for `latency_ms` and your capsule
  has to cope (default value, refuse to decide, etc.).
- **`/etc/syntra/load.json` is deleted.** Same shape: warning logged, key
  goes missing on first poll (or stays stale if it was there before).
- **SQL file is corrupted.** Same shape.
- **The sidecar process itself dies.** Your app's `GET /features/current`
  fails with connection refused. Your app must handle that — fall back to
  defaults, fail the request, whatever your policy is. Treat the sidecar as
  a cache, not a dependency you can't live without.

## What the sidecar is NOT

- **Not a metric store.** Latest value only. No history. No queries over
  time.
- **Not highly available.** Single process. Restart = cold cache. If you
  need HA, post features directly from your app and skip this entirely.
- **Not authenticated.** No TLS, no tokens, no rate limiting. Bind to
  localhost. If you need to expose it across a network, put it behind a
  TLS-terminating proxy with auth (nginx, Caddy, your service mesh of
  choice).
- **Not a Prometheus replacement.** It's a thin pull-and-cache shim.
- **Not write-back.** It reads from external systems, never writes.
- **Best-effort.** A failing source does not crash the sidecar. The feature
  just stops appearing (or starts ageing). The caller decides how to react.

## Troubleshooting

**`/features/current` returns `{"_meta": {}}` and nothing else.**
No source has succeeded yet. Check logs (`--log-level DEBUG`). Common
causes: wrong `url` for Prometheus, missing `DD_API_KEY`/`DD_APP_KEY`,
SQLite file doesn't exist, `json_path` doesn't match the file's structure.

**`/healthz` returns 503.**
Either the cache is empty (`status: "cold"`) or every feature is older than
`staleness_seconds` (`status: "stale"`). The response body tells you which.
Bump `staleness_seconds` only if you genuinely tolerate that staleness — it
shouldn't be a way to silence a real problem.

**A specific feature is missing from the snapshot.**
That source has never produced a value. Logs will show why. The sidecar
deliberately does not surface this as an error on `/features/current`
because some features are optional from the consumer's point of view.

**The sidecar is up but my app gets connection refused.**
Default bind is `127.0.0.1`. If your app runs in a different network
namespace (container, etc.) you need `--host 0.0.0.0` and a way to route
to it. Recommendation: keep it localhost-only and put the consumer in the
same pod/host.

**Values look wrong / out of date.**
Check `_meta.<name>.stale_seconds`. If it's growing, the source is failing
silently — search logs for the source's `name`. Every failure is logged at
WARNING with the source name in `[brackets]`.

## Layout

```
sidecar/
  README.md                   # this file
  pyproject.toml
  syntra_ingest/
    __init__.py               # version
    __main__.py               # CLI entrypoint
    config.py                 # YAML → Config dataclass
    cache.py                  # FeatureCache (thread-safe in-memory dict)
    sources.py                # poll_prometheus / poll_datadog / poll_sql / poll_file_watch
    poller.py                 # one daemon thread per source
    server.py                 # Flask app: /features/current, /healthz
  examples/
    prometheus.yaml
    datadog.yaml
    sql.yaml
    mixed.yaml
```
