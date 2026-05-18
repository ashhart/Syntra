# Capsule

A **capsule** is the unit of installation in Syntra. It is the thing
you install once at a path, configure once with a learning spec, and
then drive with traffic from your service forever after.

Concretely, a capsule is the bundle of files that live at one path
inside Syntra's store:

```
syntra-store/
  tenants/{tenant}/jobs/{job}/capsules/{capsule}/
    current.lyc       — the graph binary
    policy.json       — runtime permissions
    memory.json       — learned weights, meta-bandit, calibrators, OOD detectors
    learning.json     — algorithm config (contextSpec, refusal, …)
    warmup.json       — lifecycle state
    audit.jsonl       — mutation log
    decision.jsonl    — decision log (carries refused flag and confidence)
    feedback.jsonl    — feedback log
    snapshots/        — pre-mutation backups
```

The capsule is addressed by a three-segment path:

```
/tenants/{tenant}/jobs/{job}/capsules/{capsule}
```

Where:

- `tenant` — an organization or environment. Use it to keep production
  separate from staging, or one customer's state separate from
  another's.
- `job` — an independent learning context. Two jobs running the same
  capsule binary have separate memory, decision logs, and learned
  weights. This is what you reach for when you want one capsule to
  learn per-region, per-customer-segment, or per-product-line without
  having to author N copies.
- `capsule` — the installed graph plus its learned state.

## What is *inside* a capsule

A capsule is a **Lycan program**. Lycan is the graph-execution runtime
underneath Syntra; the language has a small native registry of 26
sandboxed capability kernels (see [Kernel](kernel.md)) plus the
`AdaptiveChoice` / strategy node that the bandit layer drives.

The shape of the program inside every capsule is the same:

```
HTTP request body
   |
   v
runtime.inputGet — walk the request
   |
   v
optional: file.readText / sql.sqliteQuery / http.get   <-- read state
   |
   v
optional: stats.mean / stdDev / percentile             <-- derive features
   |
   v
optional: series.ewmaForecast                          <-- project ahead
   |
   v
optional: ops.autoScaleRecommend                       <-- forecast → target
   |
   v
strategy node (Lycan adaptive choice)                  <-- learned weights
   |
   v
HTTP response: chosen option + decisionId + (optional) refusal block
```

Not every step is present in every capsule. A retry-tuning capsule may
skip everything between `runtime.inputGet` and the strategy node — the
caller supplies the features and the capsule just picks. A
predictive-autoscaling capsule walks through every step because it has
to derive the forecast and the percentile inside the graph.

## Authoring a capsule

The simplest authoring path is YAML, compiled by `syntra author` to a
deployable `.lyc`:

```yaml
name: llm-router
options:
  - cheap_fast
  - balanced
  - expensive_accurate
reward:
  type: continuous
  range: [-1.0, 1.0]
```

```bash
syntra author my-capsule.yaml --out-dir ./my-capsule/
# emits my-capsule/program.lyc + sidecar JSON
```

That gives you a thin strategy-node-only capsule — the caller computes
every feature, the capsule just picks. For an operational-intelligence
capsule that computes its *own* features in the same graph (EWMA
forecast, autoscale-recommend, percentile), author `.lycs` (Lycan
source) directly and compile with `lycan compile`. See the
[predictive-autoscaling example](../examples/predictive-autoscaling.md)
for the canonical worked instance.

## The lifecycle

Every capsule moves through three states:

1. **Warmup** — Syntra runs uniform-random selection for the first ~30
   feedback rounds. It watches the reward shape that comes back from
   `/feedback`, characterizes the problem (binary / continuous /
   sparse), and picks an initial algorithm automatically.
2. **Active** — a rate-adaptive [meta-bandit](meta-bandit.md) runs
   seven candidates in parallel. The meta-bandit converges on whichever
   performs best on this capsule's data.
3. **Frozen** — operator-triggered. The bandit stops learning but
   continues serving decisions from the current weights. Useful when
   you want to roll into a regulatory review window without the weights
   moving under your feet.

[Drift detection](drift.md) can re-warm a capsule out of Active back to
Warmup when the reward distribution shifts globally, or reset just a
single context bucket on a narrower shift.

## What a capsule is not

- **Not a microservice.** It does not own a network port, a thread
  pool, or a database. It is data + a program that the Syntra server
  runs on demand.
- **Not a model.** It does not train weights with gradient descent or
  hold parameters in GPU memory. The "learning" is the bandit updating
  a per-option weight vector from delayed feedback.
- **Not a forecaster.** It can *call* `series.ewmaForecast` — one kernel,
  one parameter — but it is not a substitute for a proper time-series
  model.
- **Not tied to one HTTP client.** Anything that can POST JSON to
  `/decide` and `/feedback` is a valid integration. Python, Go,
  TypeScript, Java, Rust, and a `curl`-driven shell loop are all viable.

## Inspecting a capsule

Once installed, a capsule's state is observable through five HTTP
endpoints:

| Endpoint | Returns |
|----------|---------|
| `/report` | Live strategy weights, per-option counts, average latency, graph hash. Cheap; use it on dashboards. |
| `/memory` | Full `memory.json`: per-context buckets, meta-bandit state, candidate-context buckets, ADWIN detectors, OOD detectors. Bigger payload. |
| `/contexts` | One row per `(nodeId, contextKey)` — weights, total tries, last-update timestamp. |
| `/decisions` | Full decision log (JSONL). One line per `/decide`. |
| `/audits` | Full audit log (JSONL). Installs, policy changes, deletes, refusals, drift events. |

The browser-rendered version of all of the above is the
[admin console](../reference/api.md#admin-console).

## Where to go next

- [Kernel](kernel.md) — the 26 building blocks a capsule's program can
  call.
- [Strategy node / choice node](strategy-node.md) — the bandit-driven
  decision point inside the graph.
- [Predictive-autoscaling demo](../examples/predictive-autoscaling.md)
  — the cleanest end-to-end worked capsule.
