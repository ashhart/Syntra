# memory.json growth profile

What `memory.json` looks like on disk over a long-running capsule and
what grows linearly with decision count versus what stays bounded.
Reference numbers for capacity planning, including a historical
feature-context OOD growth issue that is fixed in current builds.

## TL;DR

| Growth axis | Bounded? | Notes |
|---|---|---|
| `OptionStats` per (strategy, option) | Yes — fixed-size struct | ~64 bytes scalars + bounded `window: VecDeque<f64>` (capped at `config.window.size`) + bounded `signal_counts` / `objective_*` HashMaps (keyed by schema-defined names) |
| Strategy-bucket count per strategy node | By context cardinality | One bucket per `contextKey`. Discrete contexts: bounded by caller's `contextKey` set. Feature contexts: bounded by feature-vector hash cardinality. |
| Time-series feature window | Yes — capped at `window_size` from learning config | Per-feature `VecDeque<f64>` |
| OOD detector state | Yes — fixed-size for feature vectors; by key cardinality for discrete contexts | Feature detector persists Welford mean/covariance state (O(d²)), not samples. Discrete detector is keyed by context string. |
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
  allocated lazily plus OOD detector state from the historical build.
- Varying context, revisits (same context vectors as before):
  **1,330 B / decide** — strategy bucket count stays flat (the
  capsule is in warmup; no `/feedback` posted), but `memory.json`
  keeps growing. The growth on the revisit pass is the OOD
  detector's historical per-observation accumulation, not strategy
  state. Current builds should not exhibit this revisit growth.

## What is bounded

- **`OptionStats`** (defined `src/learning.rs:682`): fixed-size
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

## Historical issue — feature OOD detector growth

Earlier May 2026 profiling showed feature-context `memory.json` growing
by about 1.3 KB per repeated `/decide`, which pointed at per-observation
OOD detector persistence. Current builds no longer have that shape:
`FeatureOodDetector` stores only the running mean, covariance accumulator,
inverse covariance matrix, dimension, counters, and tuning scalars.

Current `/decide` still invokes:

```rust
// src/server/decide.rs (around line 283)
let det = memory.get_or_init_feature_ood(nid, d);
det.record(x);
if det.rebuild_due(100) {
    det.rebuild_cov_inv();
}
```

but `det.record(x)` updates fixed-size Welford state rather than appending
the observation. The regression test
`feature_detector_state_shape_is_bounded_by_dimension` drives 10,000
records through the detector and asserts persisted state remains bounded
by feature dimension.

## Recommended operator action

No OOD-specific rotation is required on current builds. Continue to size
storage for normal bounded `memory.json` state plus append-only
`decision.jsonl`, `feedback.jsonl`, and `audit.jsonl` retention. For
feature-context capsules, store growth is primarily driven by feature-vector
hash cardinality and decision-log retention rather than OOD samples.
