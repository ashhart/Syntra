# Concepts: operational intelligence in a capsule

This document is a counterpart to [`../concepts.md`](../concepts.md), which
walks through contextual bandits in honest terms. Read that first if you
want the theoretical grounding for *why* Syntra's choice layer works the
way it does. Read this one for the practical question: **what does the
program inside a Syntra capsule actually compute, and why does that
matter?**

Short answer: a Syntra capsule is a Lycan program. Lycan programs can
compute. The bandit-driven strategy node is one capability among 26 in
Lycan's native registry. Earlier Syntra framing collapsed all of that
into "discrete options + reward" and that collapse hid the most useful
part of the appliance.

If you only ever needed the discrete-options story, the
[`retry-tuning`](../../examples/retry-tuning/) and
[`llm-routing`](../../examples/llm-routing/) packs are still the right
starting point. This document is for the cases where the decision the
appliance returns should be informed by *computed* features — features
the capsule derives at decide-time from a recent window the caller
posts, or from a sandboxed local sqlite database, or from an allowed
upstream the capsule's policy permits it to call.

## The pattern

Every operational-intelligence capsule has the same shape. Three of the
worked examples now under `Syntra/examples/` follow it; you can read
them in any order:

- [`predictive-autoscaling/`](../../examples/predictive-autoscaling/)
- [`anomaly-routing/`](../../examples/anomaly-routing/)
- [`seasonal-fraud-threshold/`](../../examples/seasonal-fraud-threshold/)

The shape:

```
                +-----------------------------+
                |  HTTP POST /decide          |
                |  body = { features, ... }   |
                +--------------+--------------+
                               |
                               v
                +-----------------------------+
                |  runtime.inputGet           |
                |  walk the JSON body         |
                +--------------+--------------+
                               |
                               v
                +-----------------------------+
                |  read external state        |
                |  (optional)                 |
                |   file.readText             |
                |   sql.sqliteQuery           |
                |   http.get                  |
                +--------------+--------------+
                               |
                               v
                +-----------------------------+
                |  compute features           |
                |   stats.mean / stdDev       |
                |   stats.percentile          |
                |   series.ewmaForecast       |
                |   ops.autoScaleRecommend    |
                +--------------+--------------+
                               |
                               v
                +-----------------------------+
                |  strategy node              |
                |  (Lycan adaptive choice)    |
                |                             |
                |  bandit picks one — flavor  |
                |  depends on capsule config  |
                |  (see "Adaptive flavors")   |
                +--------------+--------------+
                               |
                               v
                +-----------------------------+
                |  HTTP response              |
                |  { option, decisionId, ... }|
                +-----------------------------+
```

Not every step is present in every capsule. A retry-tuning capsule may
skip everything between `runtime.inputGet` and the strategy node — the
caller supplies the features and the capsule just picks. A predictive-
autoscaling capsule walks through every step because it has to derive
the forecast and the percentile inside the graph.

## Why this matters

Three reasons.

**1. The choice is informed by what the capsule sees, not only by what
the caller hand-builds.** If the appliance returns an option label and
the caller is responsible for computing every feature that informed it,
the appliance's job shrinks to "weighted lookup". Putting the
computation inside the graph means the kernels live with the bandit:
when you read `decision.jsonl` to audit a refusal, the values that
informed the decision are visible from the same program.

**2. The kernels are sandboxed once, at the runtime layer.** Lycan
enforces the `file_root`, `allowed_hosts`, and SQL-readonly policy at
the capability boundary. A capsule that calls `http.get` cannot reach
outside its allowed host list. A capsule that calls `sql.sqliteQuery`
cannot mutate the database. The capsule author does not have to
re-implement that sandbox in the integrating application.

**3. The program is inspectable.** `lycan inspect` returns the graph as
JSON; `lycan explain` returns a textual view. Every native call has a
metadata block in `Lycan/src/capabilities.rs` describing its inputs,
outputs, purity (`Pure` / `ReadOnlyEffect` / `Effectful`), effects, cost,
and failure modes. An auditor can read the program, see exactly what it
does, and check that the policy attached to the capsule actually permits
each call.

## The kernels worth knowing

These are the kernels the three demos exercise. Full registry is in
`Lycan/src/capabilities.rs`; the
[`Lycan/README.md`](Lycan/README.md)
has the package table.

| Kernel | Use it when |
|--------|-------------|
| `runtime.inputGet` | Pull a named field from the JSON body POSTed to `/decide`. Returns null if missing, which the program can branch on. |
| `stats.mean` | Average of a numeric array — typical use is recent-window aggregation before z-scoring or threshold tests. |
| `stats.stdDev` | Population stddev. Combines with `stats.mean` to build anomaly scores (`anomaly-routing` demo). |
| `stats.percentile` | Interpolated percentile. Useful for tail-aware decisions: "size for the p95 of recent load" rather than for the mean. |
| `series.ewmaForecast` | One-step exponential weighted moving average. Carries a single `alpha` parameter — high alpha is reactive, low alpha is smooth. |
| `ops.autoScaleRecommend` | Turn a predicted load into a target instance count, clamped to `[min, max]`, given per-instance target capacity. |
| `http.get` / `http.post` | Sandboxed HTTP. Requires `allowed_hosts`; refuses private networks; 1 MiB / 10 s caps. |
| `sql.sqliteQuery` | Read-only SELECT/WITH/PRAGMA against a sandboxed sqlite file. Used for sidecar / local state lookups. |
| `file.readText` / `file.writeText` | Sandboxed local I/O rooted at the capsule's working directory. 1 MiB cap. |
| `json.get` / `has` / `len` | Walk a JSON string with a dotted path. Pairs naturally with `http.get` or `file.readText`. |

A capsule is not obligated to use any of these. A capsule may use one or
all of them. The bandit layer behaves identically either way — Syntra
records which option was chosen, records the reward you POST to
`/feedback`, and updates the strategy weights.

## Adaptive flavors

The kernels above describe the *feature* side of the program — what the
capsule computes from the data it sees before the strategy node fires.
The *bandit* side has its own structural choice: three flavors of
adaptive layer, all reachable through the same `/decide` and
`/feedback` API. The runtime auto-detects which flavor a capsule uses
from its installed sidecars and `learning.json`. Operators pick by
authoring the right config; the API surface doesn't change.

**1. Meta-bandit over per-option LinUCB (default).** Seven candidate
algorithms run in parallel — Thompson, UCB1, EpsilonGreedy, Weighted,
Greedy, LinUCB, LinTS — and a rate-adaptive meta-bandit converges on
whichever performs best on this capsule's traffic. Every capsule that
doesn't explicitly opt into another flavor gets this. Best fit when the
N options are independent (no shared semantic structure) and you don't
have prior knowledge about which algorithm should win.

**2. Shared-state LinUCB** — for capsules whose options carry semantic
similarity. Enable by setting `sharedState.enabled = true` in
`learning.json` and supplying `optionFeatures: { name -> [f64; d] }`.
The runtime then maintains a single θ over `[x_context, x_option]`
rather than one θ per option. New options added later inherit a non-
zero prior from their action-feature vector alone — useful when you
want to add or rotate options without resetting the model. See
[`../capsule-features/shared-state-linucb.md`](../capsule-features/shared-state-linucb.md).

**3. Hierarchical bandits** — for capsules whose action space factors
into a tree (e.g. region × server-type, segment × creative). Enable by
declaring `hierarchical_options:` in `capsule.yaml`. The runtime walks
the tree at decide time, picking one option per level using a
meta-bandit per `HierState`, and `/feedback` propagates the observed
reward to every level along the path. Each level's meta-bandit
explores independently; credit at decide time is exact (the per-level
candidate id is recorded and threaded back through feedback) so the
meta-bandit's selection logic stays honest. See
[`../capsule-features/hierarchical-bandits.md`](../capsule-features/hierarchical-bandits.md).

The flavors are orthogonal to the kernel-feature story. A capsule can
use EWMA forecasting + autoscale-recommend + shared-state LinUCB; a
hierarchical capsule can compute its own context features inside the
program (subject to the v1 limitation that the graph isn't executed
for hierarchical decides — see roadmap.md "Future polish"). The
choice of flavor is about the *shape of the option space*; the choice
of kernels is about the *shape of the features that inform the
choice*.

## Two-layer drift detection

Every Syntra capsule runs ADWIN change detection at two levels, and the
relationship between them is load-bearing for how an operator reads a
drift alarm.

**Per-context ADWIN** lives at `(node_id, context_key)` granularity
inside `StrategyMemory.context_detectors`. It sees only the rewards
landing in one context bucket. When it fires, the runtime resets that
bucket's candidate state and emits a `context_change_detected` audit
event — but the capsule's lifecycle is untouched. This layer is meant
to catch *narrow* shifts: one merchant bucket, one region, one segment
gone bad while the rest of the workload is stable.

**Capsule-level ADWIN** lives on `WarmupState.detector`. It sees every
reward POSTed to `/feedback` for the capsule. When it fires, the
runtime moves the whole capsule back into warmup. This layer is meant
to catch *broad* shifts: an aggregate regime change that warrants
re-characterising the reward distribution and re-picking the
algorithm.

Both layers use the same ADWIN math — the difference is the `delta`
parameter. Smaller delta = wider Hoeffding bound = slower to fire.
The defaults are tuned so per-context fires first:

| Layer | `SafetyConfig` field | Default |
|-------|----------------------|---------|
| Per-context | `context_adwin_delta` | `0.002` |
| Capsule-level | `capsule_adwin_delta` | `0.0005` |

Configurable via `learning.json`:

```json
{
  "safety": {
    "capsuleAdwinDelta": 0.0005,
    "contextAdwinDelta": 0.002
  }
}
```

(A legacy single-delta key `adwinDelta` is still accepted as a fallback
for both layers so older configs continue to load.)

These defaults were chosen from synthetic characterization — see
`Lycan/tests/change_detection_characterization.rs`, which sweeps a
5x5 grid of `(capsule_delta, context_delta)` over a controlled N(0.2,
0.1) -> N(0.8, 0.1) drift step and a stationary N(0.5, 0.1) control,
and writes the matrix to `/tmp/adwin_characterization.md`. We don't
have production reward streams to tune against, so this is "best
available", not definitive. If on a stable workload you observe
capsule-level firing before per-context, your delta values likely
need adjustment. See `Syntra/docs/known-issues.md` for the standing
caveat.

## What this is not

This is a concept doc, not a guarantee. State things plainly:

- The capsule's kernels are exposed at the Lycan source layer. To
  exercise them, you author `.lycs` and compile to `.lyc`. The simpler
  `syntra author` YAML path emits a thin strategy-node-only capsule and
  does not use the operational kernels.
- The kernels are sandboxed but not magic. `http.get` cannot reach an
  upstream you have not allow-listed; that is a feature, not a
  workaround. If the capsule needs data from somewhere the policy
  doesn't permit, the capsule cannot get it. Adjust the policy or the
  capsule, not the runtime.
- The bandit's job is to learn from delayed feedback. None of the
  kernels above replace that. They make the decision *informed*; they
  do not make it *correct* on the first try. If you POST `/decide`
  forever and never POST `/feedback`, the strategy weights will not
  move.
- Forecasting in particular: `series.ewmaForecast` is one kernel with
  one parameter. It is not a substitute for a proper time-series
  forecaster when you actually have seasonality, multi-step horizons,
  exogenous regressors, or uncertainty quantification. Use it for what
  it is — a cheap smoothed projection one step ahead.

## Surfacing kernel outputs: `runtime.publish`

The kernels described above compute values that inform the strategy
node — but by default those values live and die inside the program.
The `/decide` response carries the chosen option and the bandit's
weights; it does not, by default, carry the forecast number or the
percentile that the program was reacting to. That's a problem for an
operator running a dashboard against `/decisions`: the *decision* is
auditable, but the *intermediate state that informed the decision* is
not.

`runtime.publish` closes that gap. A capsule's program names a
computed value with `(!cap "runtime.publish" "<name>" <value>)`, and
the runtime appends the named value to the current decision's
`published` map. That map is flushed onto the `/decide` response,
into `decisions.jsonl`, and out through `/decisions` to any
consumer — including the Syntra dashboard's Region 5, which renders
each `(name, value)` as a card next to the chosen option. The three
worked examples above all use this capability; see
[`../capsule-features/runtime-publish.md`](../capsule-features/runtime-publish.md)
for the full signature, supported value types, and the publish-vs-`!p`
trade-off.

## Where to go next

- [`../../POSITIONING.md`](../../POSITIONING.md) — the canonical statement
  of what Syntra is and is not in operational terms.
- [`../../examples/predictive-autoscaling/`](../../examples/predictive-autoscaling/) —
  the cleanest end-to-end example of the meta-bandit flavor with the
  kernel→strategy-node pattern.
- [`../../examples/shared-state-action-embeddings/`](../../examples/shared-state-action-embeddings/) —
  worked example of shared-state LinUCB with 6 options and demonstrated
  generalisation to unseen action-features.
- [`../../examples/hierarchical-region-routing/`](../../examples/hierarchical-region-routing/) —
  worked example of hierarchical bandits with a 2×3 = 6-leaf tree.
- [`../capsule-features/shared-state-linucb.md`](../capsule-features/shared-state-linucb.md)
  and
  [`../capsule-features/hierarchical-bandits.md`](../capsule-features/hierarchical-bandits.md)
  — concept docs for flavors 2 and 3.
- [`../../sidecar/`](../../sidecar/) — the metrics-ingestion sidecar that
  pulls feature values from Prometheus / Datadog / SQL / file sources so
  capsules don't have to.
- [`../concepts.md`](../concepts.md) — the contextual-bandit concept doc.
  This file leans on its definitions of context, option, reward, and
  exploration / exploitation.
