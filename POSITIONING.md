# Syntra — Positioning

This document is the honest, ground-up answer to "what is Syntra, and what
isn't it." It is written from what Lycan's capability registry and the
existing examples actually do — not from the marketing copy that earlier
README revisions inherited.

If anything in `Syntra/README.md` contradicts what you read below, this
document is correct and the README is being updated to match.

## TL;DR

**Syntra is a single self-hosted HTTP appliance that runs Lycan programs as
named capsules. Each capsule can compute from the data it sees — forecast,
aggregate, query, fetch — and pick among labeled options. Syntra learns
which option works best for each context from delayed feedback you POST
back to it.**

The interesting part isn't the bandit. The interesting part is that the
program *between* the input and the choice runs inside the same graph, with
26 sandboxed kernels available to it. That is what earlier framing hid.

## What Syntra actually computes

Syntra capsules are Lycan programs. Lycan ships 26 Rust-native capability
kernels. The ones that matter for the operational use cases Syntra targets:

| Package | Capabilities | Why a capsule cares |
|---------|--------------|---------------------|
| math    | `stats.mean`, `stats.stdDev`, `stats.min`, `stats.max`, `stats.percentile` | derive features from rolling windows the caller passes in |
| math    | `series.ewmaForecast` | project one step ahead on a recent series |
| ops     | `ops.autoScaleRecommend` | turn predicted load into a target instance count |
| net     | `http.get`, `http.post` | fetch live metrics, post to webhooks (host-allowlist enforced) |
| data    | `sql.sqliteQuery` | read-only SQL against a sandboxed sqlite database |
| data    | `json.get`, `json.has`, `json.len` | parse JSON payloads pulled by `http.get` or `file.readText` |
| io      | `file.readText`, `file.writeText`, `file.exists` | read sidecar-written feature files; write small artefacts |
| runtime | `runtime.input`, `runtime.inputGet` | walk the request body Syntra received |

(`nav.*` and `astro.lambertSolve` are in the registry too. They are the
Lambert-solver / ephemeris kernels used by Lycan's Mars-transfer demos.
Syntra-typical workloads don't touch them.)

Every capability call is sandboxed by the capsule's runtime policy:
- file I/O is rooted at the capsule's working directory; absolute paths
  and `..` are refused
- `http.get` / `http.post` require an explicit `allowed_hosts` allow-list
  and refuse private/loopback by default
- `sql.sqliteQuery` opens read-only and rejects non-`SELECT/WITH/PRAGMA`
- responses are capped at 1 MiB, requests time out at 10 s

These limits are enforced by the runtime, not by capsule authoring.

## What Syntra adds on top of Lycan

Lycan is the language and runtime. Syntra is the deployable appliance and
the learning layer that wraps the strategy node:

- **HTTP API.** `/decide`, `/feedback`, `/feedback/batch`, `/install`,
  `/learning`, `/report`, `/memory`, `/contexts`, `/admin/*`. Stable
  across the repositioning — this document does not change a single byte
  of the request/response contract.
- **Meta-bandit.** Seven candidate algorithms run in parallel (Thompson,
  UCB1, EpsilonGreedy, Weighted, Greedy, LinUCB, LinTS). The meta-bandit
  converges on whichever performs best on this capsule's actual traffic.
  You do not pick.
- **Lifecycle.** Warmup → Active → Frozen, per capsule. Warmup picks the
  active algorithm from the reward shape after ~30 rounds.
- **Drift detection.** Capsule-level ADWIN re-warms on global regime
  shifts; per-context ADWIN resets just the drifted bucket on narrower
  shifts.
- **Refusal (opt-in).** Split-conformal intervals + per-context OOD
  scores. When the interval is too wide or the input is OOD, `/decide`
  returns `{"refused": true, "confidence": {…}}`. Disabled by default.
- **Operational hardening.** Scoped auth tokens (`Admin`, `TenantAdmin`,
  `Read`), token-bucket rate limit (1000 req/sec/principal default),
  Prometheus `/metrics`, `/ready` store-writability probe, JSON
  structured logging via `tracing`, backup/restore via JSON bundles.
- **Multi-decision capsules.** A capsule can declare a `decisions[]`
  list. `do_decide` runs the meta-bandit independently per
  `AdaptiveChoice` node and embeds each node's selected `candidateId` in
  the decision-log entry. `/feedback` accepts a `decisionIndex`.
- **Shared-state LinUCB.** A capsule can opt into a shared θ over
  `[x_context, x_option]` by attaching `option_features` and
  `sharedState.enabled = true` in `learning.json`. The bandit then
  generalises across options — a new option added later inherits a
  non-zero posterior mean from its action-feature vector alone, with no
  separate cold-start. Validated end to end against the
  `shared-state-action-embeddings` example capsule: training on four
  corner options, the model produces non-trivial scores on the two
  un-trained interior options drawn purely from their action features.
  See `Syntra/docs/capsule-features/shared-state-linucb.md`.

The bandit core is the Phase A–F + G+H + I work. It is real, it is
tested (over 200 unit tests in `Lycan`, 40+ in `Syntra`), and it runs
unchanged.

- **Hierarchical bandits.** A capsule can opt into a nested-tree
  option set via `PUT /hierarchical_spec` after `/install`. The
  runtime walks the tree at decide time using one meta-bandit per
  `HierState` (root + per-branch), picks an option per level, and
  resolves to a leaf action. `/feedback` propagates the observed
  reward to every level along the recorded path. Validated end to
  end against a 2×3 tree: rewarding only one leaf for 100 rounds
  drove the root bucket to weights `[0.94, 0.06]` on the rewarded
  parent and the sub-bucket to `[0.05, 0.91, 0.04]` on the rewarded
  child. Useful when the action space factors naturally (e.g., 5
  regions × 4 server types — 20 leaves but only 5 region-level
  decisions matter most of the time). See
  `Syntra/docs/capsule-features/hierarchical-bandits.md` and
  `Syntra/examples/hierarchical-region-routing/`.

All three adaptive flavors share the same `/decide` and `/feedback`
contract; the runtime auto-detects which flavor a capsule uses from
its installed sidecars and `learning.json`.

## What a Syntra capsule can do, end to end

The pattern is the same in every demo under `Syntra/examples/` once you
look past the surface:

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
optional: ops.autoScaleRecommend                       <-- turn forecast into action target
   |
   v
strategy node (Lycan adaptive choice)                  <-- learned weights
   |
   v
HTTP response: chosen option + decisionId + (optional) refusal block
```

Every step is observable. `lycan inspect` shows the graph, `/memory`
shows the learned weights, `decision.jsonl` shows what was returned,
`feedback.jsonl` shows what reward arrived, `audit.jsonl` shows installs
and policy changes.

What Syntra is selling is not a clever bandit. It's that the whole chain
above lives inside one inspectable graph, in one Docker container, with
delayed feedback as the only learning signal you need to wire up.

## What can a user do with it

Three things, ordered by how much of the appliance they touch:

1. **Adaptive policy selection (the existing path).** Author a capsule
   with N options and a reward shape. POST request context to `/decide`,
   get a chosen option and a `decisionId`. POST `/feedback` later. The
   learner picks the best option per context from delayed outcomes. This
   is the work the original `examples/retry-tuning/` pack demonstrates
   and what the existing field deployment at MoEfolio.ai exercises.
2. **Operational decisions over computed features.** The capsule program
   *computes* its own features from a recent window the caller passes in
   (a metric history, a latency series, a fraud-rate series). EWMA
   forecasts, mean / stddev anomaly tests, percentile thresholds, all
   inside the same graph that runs the strategy node. Practically this
   collapses what would otherwise be several hundred lines of glue (a
   feature pipeline, a forecaster wrapper, a bandit library, an OOD
   check) into ~50 lines of Lycan source plus the one HTTP call to
   `/decide`. The three new demos in `Syntra/examples/predictive-autoscaling/`,
   `Syntra/examples/anomaly-routing/`, and
   `Syntra/examples/seasonal-fraud-threshold/` are the worked examples.
3. **Pull-style feature ingestion via the sidecar.** Run
   `Syntra/sidecar/syntra-ingest` next to Syntra. Configure it with the
   Prometheus / Datadog / SQL / file sources you already have. It polls,
   keeps a fresh snapshot, and exposes `/features/current`. Your
   integrating code grabs that snapshot, posts to `/decide`, gets a
   decision back, and reports outcome to `/feedback`. The sidecar is
   best-effort and stateless — it is not a metric store.

## What Syntra is not

Stating these explicitly because the previous framing left them
ambiguous, and a prospective user reading this document deserves to know
before they install. Ordered roughly by what you'd want clarified before
the install decision rather than after:

- **Not a managed service.** Self-hosted Docker container, single
  process, single binary, local-filesystem store under `syntra-store/`.
  Run behind a TLS proxy. Operationally hardened (auth tokens, rate
  limit, metrics, /ready, backup/restore) but the team running it is
  yours.
- **Single-node scale.** No clustering today. The default rate-limit is
  1000 decides/sec/principal; capsule complexity dominates real
  throughput. That envelope covers the vast majority of operational
  decision workloads — most SREs we've talked to are looking at tens
  to low hundreds of decisions per second on their actual hot path —
  but if you need to make six-figure decides/sec or run multi-region
  active-active, Syntra is not the appliance for that.
- **Not a metric collection / observability system.** The sidecar
  *reads* from Prometheus / Datadog / SQL — it does not replace them.
  Syntra never stores time series for its own sake; it stores decisions,
  feedback, learned weights, and an audit trail.
- **Not for one-shot decisions.** A decision that never receives a
  reward gives the learner nothing to update on. The capsule still
  runs, but it will not be smarter the second time.
- **Not a replacement for feature-flag / experiment platforms.** Those
  decide whether to ship X. Syntra decides which option to use once X
  is shipped, and adapts from outcomes.
- **Not a forecasting platform.** The only forecaster shipped is one
  pure kernel: `series.ewmaForecast`, one-step EWMA, one `alpha`
  parameter. No ARIMA. No Prophet. No deep forecasters. If you have a
  trained forecaster outside Syntra, the natural shape is to send its
  output to a capsule as a feature and let the capsule pick the action;
  if you don't have one and you need multi-step seasonal probabilistic
  forecasts, install a forecaster first.
- **Not a model platform.** No GPU. No training loop. No model
  registry. No fine-tuning. The learning that happens is the
  contextual-bandit update on the strategy node, plus the calibration
  of conformal intervals and OOD detectors.
- **Not for supervised problems with ground-truth labels at prediction
  time.** Use a model framework.

## How this differs from the prior framing

Earlier README and CHANGELOG copy led with "contextual bandit
appliance." That is accurate but it was load-bearing in a misleading
way: it implied the only thing a capsule does is pick an option from a
list, and that all the interesting work happens after the response
inside the user's application. Both halves are wrong.

The corrected framing:

- A Syntra capsule is a Lycan program. Lycan programs can compute. They
  call native kernels for I/O, stats, forecasts, SQL. The strategy node
  is one capability among 26.
- The choice the appliance returns can be informed by *computed*
  features — features the capsule derived from a history the caller
  POSTed, or read out of a sandboxed sqlite file, or fetched from an
  allowed host — not only by features the caller hand-built and passed
  in.
- The bandit layer is what makes the choice adaptive. The kernels are
  what make the choice well-informed.

Nothing in the Phase A–H bandit work is being rebuilt. The
`/decide` / `/feedback` API contract is unchanged. The capsule store
format is unchanged. The meta-bandit, drift detection, refusal, and
operational endpoints all continue to work.

This is a repositioning. It is the surface that changed, not the
appliance.

## Sources

- `Lycan/src/capabilities.rs` — the 26-capability registry, sandbox
  enforcement, EWMA, autoscale-recommend kernels
- `Lycan/README.md` — the Lycan language and runtime, native-capabilities
  table at the bottom
- `Syntra/examples/lycan-internals/demo_capability_pack.lycs` — single
  program touching file I/O, JSON, stats, EWMA, autoscale-recommend
- `Syntra/examples/lycan-internals/demo_autoscaler.lycs` — four
  competing scaling strategies behind a strategy node
- `Syntra/CHANGELOG.md` — Phase A–F (bandit core, refusal, drift,
  lifecycle), Phase G+H (hardening, multi-decision, observability)
