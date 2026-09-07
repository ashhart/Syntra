# Kernel

A **kernel** is a single sandboxed native operation a capsule's program
can call. Lycan ships 26 of them. The ones that matter for Syntra
workloads are the operational-intelligence subset — stats, forecast,
autoscale-recommend, sandboxed HTTP / SQL / file I/O.

Every kernel call is policy-enforced at the runtime layer, not by the
capsule author. A capsule that calls `http.get` cannot reach outside
its declared `allowed_hosts`. A capsule that calls `sql.sqliteQuery`
cannot mutate the database. The sandbox is enforced once, in Rust, for
every capsule in every tenant.

## The kernels that matter

| Package | Kernels | Why a capsule cares |
|---------|---------|---------------------|
| `math` | `stats.mean`, `stats.stdDev`, `stats.min`, `stats.max`, `stats.percentile` | Derive features from rolling windows the caller passes in. |
| `math` | `series.ewmaForecast` | One-step exponential weighted moving average. One `alpha` parameter. |
| `ops`  | `ops.autoScaleRecommend` | Turn a predicted load into a target instance count, clamped to `[min, max]`. |
| `net`  | `http.get`, `http.post` | Sandboxed HTTP. Requires explicit `allowed_hosts`; refuses private networks; 1 MiB / 10 s caps. |
| `data` | `sql.sqliteQuery` | Read-only `SELECT` / `WITH` / `PRAGMA` against a sandboxed sqlite file. |
| `data` | `json.get`, `json.has`, `json.len` | Walk a JSON string with a dotted path. Pairs with `http.get` / `file.readText`. |
| `io`   | `file.readText`, `file.writeText`, `file.exists` | Sandboxed local I/O rooted at the capsule's working directory. 1 MiB cap. |
| `runtime` | `runtime.input`, `runtime.inputGet` | Walk the JSON body POSTed to `/decide`. Returns null on missing field. |
| `runtime` | `runtime.publish` | Surface a computed value onto the decision response, the `decisions.jsonl` log, and `/decisions`. |

There are also `nav.*` and `astro.lambertSolve` kernels — those are the
Lambert-solver / ephemeris kernels Lycan ships for its Mars-transfer
demos. Syntra workloads don't touch them.

## Sandboxing

Every kernel call is bound by the capsule's runtime policy:

- **File I/O** is rooted at the capsule's working directory. Absolute
  paths and `..` are refused.
- **`http.get` / `http.post`** require an explicit `allowed_hosts`
  allow-list and refuse private / loopback networks by default.
- **`sql.sqliteQuery`** opens read-only and rejects non-`SELECT/WITH/
  PRAGMA` statements.
- **Responses** are capped at 1 MiB. **Requests** time out at 10 s.

These limits are enforced by the runtime, not by capsule authoring.
The capsule author cannot opt out, and the integrating application
does not have to re-implement the sandbox.

## Why kernels live inside the capsule

The interesting framing question is: why does a capsule compute its own
features at all? The caller could pre-derive them and pass them in.

Three reasons:

**1. The choice is informed by what the capsule sees, not only by what
the caller hand-builds.** If the appliance returns an option label and
the caller is responsible for computing every feature that informed it,
the appliance's job shrinks to "weighted lookup". Putting the
computation inside the graph means the kernels live with the bandit —
when you read `decision.jsonl` to audit a refusal, the values that
informed the decision are visible from the same program.

**2. The kernels are sandboxed once, at the runtime layer.** A capsule
that calls `http.get` cannot reach outside its allowed host list. A
capsule that calls `sql.sqliteQuery` cannot mutate the database. The
capsule author does not have to re-implement that sandbox in the
integrating application.

**3. The program is inspectable.** `lycan inspect` returns the graph as
JSON. `lycan explain` returns a textual view. Every native call has a
metadata block describing its inputs, outputs, purity, effects, cost,
and failure modes. An auditor can read the program, see exactly what
it does, and check that the policy attached to the capsule actually
permits each call.

## The pattern

The three flagship operational-intelligence demos all share the same
shape:

```
request body
    |
    runtime.inputGet recent_window / current_value / ...
    |
    stats.mean / stats.stdDev / stats.percentile
    |
    series.ewmaForecast (optional)
    |
    ops.autoScaleRecommend (optional)
    |
    strategy node picks one of N labelled policies
    |
    HTTP response: chosen option + decisionId
```

- [Predictive autoscaling](../examples/predictive-autoscaling.md) uses
  `runtime.inputGet` → `stats.mean` / `stats.percentile` /
  `series.ewmaForecast` → `ops.autoScaleRecommend` → strategy node.
- [Anomaly routing](../examples/anomaly-routing.md) uses
  `runtime.inputGet` → `stats.mean` / `stats.stdDev` (to derive a
  z-score) → strategy node.
- [Seasonal fraud threshold](../examples/seasonal-fraud-threshold.md)
  uses `runtime.inputGet` → `stats.mean` / `stats.percentile` /
  `series.ewmaForecast` → strategy node, with reward delayed by days.

A capsule is not obligated to use any of these. The bandit layer
behaves identically either way — Syntra records which option was
chosen, records the reward you POST to `/feedback`, and updates the
strategy weights.

## What kernels are not

- **Not a forecasting platform.** `series.ewmaForecast` is one kernel
  with one parameter. It is not a substitute for ARIMA, Prophet, deep
  forecasters, or anything that handles seasonality, multi-step
  horizons, exogenous regressors, or uncertainty quantification.
- **Not a metric store.** `sql.sqliteQuery` is read-only against a
  sandboxed sqlite file. Use a real database for real data. Use the
  sidecar (`syntra-ingest`) if you want to pull from Prometheus /
  Datadog / SQL on a schedule.
- **Not unrestricted.** `http.get` cannot reach an upstream you have
  not allow-listed. That is a feature, not a workaround. If the
  capsule needs data from somewhere the policy doesn't permit, adjust
  the policy or the capsule — not the runtime.

## Where to go next

- [Strategy node / choice node](strategy-node.md) — what the kernel
  outputs feed into.
- [Capsule](capsule.md) — how the kernels and the strategy node compose
  into a single installable thing.
- [Predictive autoscaling demo](../examples/predictive-autoscaling.md)
  — the cleanest worked example of the kernel → strategy-node pattern.
