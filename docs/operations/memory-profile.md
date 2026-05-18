# memory.json growth profile

What `memory.json` looks like on disk over a long-running capsule and
what grows linearly with decision count versus what stays bounded.
Reference numbers for capacity planning and for the open OOD-growth
issue tracked in `Syntra/docs/known-issues.md`.

## TL;DR

| Growth axis | Bounded? | Notes |
|---|---|---|
| `OptionStats` per (strategy, option) | Yes — fixed-size struct | ~64 bytes scalars + bounded `window: VecDeque<f64>` (capped at `config.window.size`) + bounded `signal_counts` / `objective_*` HashMaps (keyed by schema-defined names) |
| Strategy-bucket count per strategy node | By context cardinality | One bucket per `contextKey`. Discrete contexts: bounded by caller's `contextKey` set. Feature contexts: bounded by feature-vector hash cardinality. |
| Time-series feature window | Yes — capped at `window_size` from learning config | Per-feature `VecDeque<f64>` |
| **OOD detector state** | **NO** | Grows linearly with every decide call. Discussed below. |
| Decision log (`decision.jsonl`) | Append-only | Not part of `memory.json`; separate sidecar |

## Methodology — May 2026 measurement

Capsule under test: `predictive-autoscaling` (3 features:
`hour`, `current_instances`, `load_trend`). Feature-context capsule.
Driven via HTTP `/decide` against a `lycan serve --dev-mode` instance.
No `/feedback` posted on the second-stage measurements, so strategy
stats and the warmup counter do not advance — the growth observed is
attributable to the decide path alone.

Measurements taken on macOS (Darwin 25.5.0), debug-build `lycan`,
`/tmp/syntra-task3-store` on local APFS. Numbers are byte counts of
`memory.json` as observed by `wc -c`, not in-memory footprint.

## Results

```text
Stage                                                memory.json    Δ vs prior
─────────────────────────────────────────────────── ────────────  ───────────
Fresh install (predictive-autoscaling)                    60,241 B          —
After 4,300 fixed-context /decide rounds                  71,512 B    +11,271 B
After 500 varying-context decides (∼500 unique ctx)      976,734 B   +905,222 B
After another 500 varying-context decides (REVISITS)   1,641,924 B   +665,190 B
```

Per-decide growth, by stage:
- Fixed context: **2.6 B / decide** — float-precision noise in
  the single bucket's serialized weights/stats. Effectively flat.
- Varying context, novel: **1,810 B / decide** — new buckets being
  allocated lazily plus OOD detector accumulating per-observation
  state.
- Varying context, revisits (same context vectors as before):
  **1,330 B / decide** — strategy bucket count stays flat (the
  capsule is in warmup; no `/feedback` posted), but `memory.json`
  keeps growing. The growth on the revisit pass is the OOD
  detector's per-observation accumulation, not strategy state.

## What is bounded

- **`OptionStats`** (defined `Lycan/src/learning.rs:682`): fixed-size
  scalar fields plus a `VecDeque<f64>` window. Window length is
  bounded at `config.window.size` (truncation in
  `learning.rs:1958`). HashMaps inside `OptionStats`
  (`signal_counts`, `objective_rewards`, `objective_counts`) are
  keyed by names defined in the capsule's reward/objective schema —
  bounded by schema cardinality, not by decide count.

- **`OptionState`** received exponential decay in Phase C3
  (`option_state_forgetting` config, default `0.999`). State
  variables stay numerically bounded.

- **TimeSeries feature window** (`feature_schema.rs::TimeSeriesWindow`):
  fixed-cap `VecDeque<f64>` per time-series feature.

- **Strategy contexts**: bounded by the number of distinct
  `contextKey` values the caller emits. In discrete mode this is
  fully under the caller's control; in feature mode it is bounded
  by the cardinality of the encoded feature vector, which is bounded
  by the feature granularity declared in `learning.json`.

## What is not bounded — OOD detector

Each `/decide` against a feature-context capsule invokes:

```rust
// Lycan/src/server/decide.rs (around line 283)
let det = memory.get_or_init_feature_ood(nid, d);
det.record(x);
if det.rebuild_due(100) {
    det.rebuild_cov_inv();
}
```

`det.record(x)` accumulates state per call. The empirical data
above shows that even when the request context is re-observed
(same feature vector), `memory.json` keeps growing by ≈1.3 KB
per decide. The OOD detector's internal storage is not bounded by
feature-vector cardinality.

At 1 decide/sec, this is ≈110 MB per day, ≈3.4 GB per month. A
production capsule running a year would blow through any reasonable
operator-visible store budget.

This affects **feature-context capsules only**. Discrete-context
capsules track the OOD signal via `discrete_ood_for` which appears
to be bucket-counted rather than per-observation accumulated; the
fixed-context measurement above implicitly tested this (the same
discrete bucket was hit 4,300 times with ≈2.6 B/decide growth,
which is float-noise, not real accumulation).

The fix space is well-trodden: cap the OOD detector's window at N
samples (likely matching its `rebuild_due(100)` cadence — `record()`
already triggers a rebuild every 100 calls, so the persisted state
beyond that window is unused for scoring) and pop oldest on insert.
Numerical care needed: the covariance estimate that `rebuild_cov_inv`
computes must remain stable as the population evolves. **This is
not in scope for this document — see `known-issues.md` for the
open ticket.**

## Recommended operator action

For feature-context capsules:

- Plan store growth at ≈1.3 KB × `decide rate` × `retention seconds`.
- Snapshot or rotate `memory.json` (or the whole capsule directory)
  at scheduled intervals; the persisted state is dominated by the
  OOD detector, so a "reset OOD state every N hours" loop is a
  viable mitigation until the upstream fix lands.
- Use `syntra status` and `syntra stop` (Phase I followup 22) to
  inspect / restart cleanly when needed.

For discrete-context capsules: no action needed. Growth is bounded
by `contextKey` cardinality.
