# Known issues

Tracking known runtime / presentation / documentation gaps that aren't
blocking but should be picked up in a future round. New entries are added
at the top; resolved entries are removed (commit history is the audit
trail).

For *deferred-but-planned* work (shape complete, wiring queued), see
`Syntra/docs/roadmap.md` instead.

## Open

### MAB vs VW headline number (2.67× lower regret) not reproduced at full scale

**Status:** Bin classification reproduces (A — competent: within constant
factor of VW on ≥7/9 cells), but the headline mean-ratio number drifted.
**Last measured:** May 2026, four runs of `syntra_vs_vw_mab/benchmark.py`
at 10 seeds × 2000 rounds × 9 cells, mean ratios:
- pre-fix (broken weighted-bucket override): 1.438 → bin **B**
- hard greedy override:                      0.955 → bin A (1 run)
- conditional fix (Binary→greedy, else soft): 1.194 / 1.239 → bin A (2 runs)
Documented Phase A-F baseline: ratio_mean=0.374 → 2.67× lower regret.

**Scope:** MAB vs VW benchmark only. Other documented benchmarks
(vaccine reward-blindness 4.36× vs documented 4.4×; outbreak pandemic
2/4 pass + 0.40 deaths vs documented 0.5) reproduce cleanly.

**Per-cell pattern:** consistent across runs. 8-9/9 cells stay within
1.5× VW (bin A), 0/9 cells beyond 2.5× VW. The gap to documented is
concentrated on **easy-difficulty cells with more arms** (5_easy ≈ 2.1,
10_easy ≈ 1.4-1.7) — exactly the cells where Thompson Sampling should
have its biggest advantage over VW's contextual learner. Hard cells
are ~1.0 in both runs and docs (uniformly-distributed arms → Syntra
and VW indistinguishable).

**Root cause identified post-fix (followup 24):** Syntra's
`rand_f64()` in `Lycan/src/learning.rs:2202` uses `SystemTime::now()`
+ thread id + an atomic counter as its entropy source. There is **no
external seeding hook**. So when the MAB benchmark passes `--seeds 10`
and seeds VW deterministically, Syntra's behavior is *not seeded* —
it depends entirely on wall-clock timing of each `/decide` call.

The Phase A-F documented headline of `ratio_mean=0.374` (2.67× lower
regret) was therefore one wall-clock realisation of a high-variance
distribution, not a reproducible measurement. Per-seed coefficient of
variation in this run:

| Cell | Syntra CV | VW CV |
|---|---|---|
| 2_easy | **1.39** | 0.22 |
| 5_easy | 0.54 | 0.26 |
| 10_easy | 0.42 | 0.22 |

Syntra's per-seed regret in 2_easy ranges 17.5 to 398 across 10
seeds. VW's range is 45.5 to 90.0. The bin classification (A — within
constant factor of VW on ≥7/9 cells) is stable across reruns because
that classification is robust to the per-cell variance. The
**mean-ratio headline number** is not — it's dominated by occasional
"unlucky seed" runs where Thompson's warmup samples happen to favour
the inferior arm by chance and the posterior takes a long time to
recover.

**Fix shape:** add `LYCAN_RNG_SEED` env var read at server startup;
plumb a seeded `StdRng` (or similar) through the `rand_f64` call site;
update the MAB benchmark to set it deterministically per cell. ~50-100
lines across `Lycan/src/learning.rs`, `Lycan/src/server/mod.rs`, and
the benchmark. With reproducible Syntra runs, the Phase A-F number is
either confirmed or refuted with confidence rather than swimming in
noise.

**Other secondary investigation targets** (likely smaller-magnitude
than RNG):
- Warmup overhead: 30 uniform-random selections × 90 cell-instances
  contribute ~10 regret each → ~1% of observed Syntra regret. Real
  but not the bulk.
- `apply_feedback` weight-delta asymmetry on binary rewards
  (`reward=0 → delta=0`). Currently irrelevant to selection because
  the conditional greedy override dominates.
- Code drift since Phase A-F (deleted `src/server.rs`, modified
  `src/learning.rs`, `src/graph_executor.rs`, `src/capabilities.rs`).
  Worth a `git log -p` audit once the RNG seeding is in place.

**Operator-facing status:** the published "2.67× lower regret" external
claim does not reproduce. With deterministic seeding now in place (the
`LYCAN_RNG_SEED` env var + the `POST /admin/rng/seed` admin endpoint,
plus `SYNTRA_DEMO_NO_TRAFFIC=1` to silence the demo container's
traffic generator so it doesn't interleave with benchmark requests),
the measured ratio is **0.946 mean → 1.06× lower regret vs VW**,
reproducible bit-exactly across runs (90/90 per-instance match between
two 10-seed × 2000-round runs). Bin A confirmed (5/9 cells Syntra
wins, 1/9 the 10_easy cell pulls the mean up).

Use **"bin-A competent with VW; Syntra wins on 5 of 9 cells with
reproducible 1.06× mean lower regret"** as the defensible claim. The
2.67× headline can now be either recovered or refuted with confidence
since A/B comparisons against any code change are deterministic.

### Outbreak benchmark weighted/ucb1 configs running at upper-tolerance bound

**Status:** Not a regression — within documented Phase A-F tolerance of "~1 death and ~$400M"
**Last observed:** May 2026 regression run
**Current numbers:** weighted=0.70 deaths, ucb1=1.00 deaths (baseline ~0.5)
**Risk:** If a future regression run shows further drift, the cumulative trend matters

If a regression run shows weighted or ucb1 at >1.5 deaths or any config at >$28B,
that's beyond the documented tolerance and needs investigation. Likely candidates
to investigate: RNG path changes from the server.rs refactor, OptionStats round-trip
changes from Item 2, ADWIN threshold changes from Phase I followup 19.

### ADWIN defaults are tuned from synthetic data

The per-layer ADWIN delta defaults (`capsule_adwin_delta=0.0005`,
`context_adwin_delta=0.002`) were chosen from synthetic
characterization runs in `Lycan/tests/change_detection_characterization.rs`
because we don't have production data to tune against. Real
workloads may need adjustment. If you observe capsule-level firing
before per-context-level on stable workloads, your delta values
likely need adjustment via `SafetyConfig.capsule_adwin_delta` and
`SafetyConfig.context_adwin_delta` (JSON keys `capsuleAdwinDelta` /
`contextAdwinDelta` in `learning.json`).

## Resolved this cycle

### Feature-context OOD detector unbounded memory growth — FIXED 2026-05-19

The feature-vector OOD detector is now fixed-size in the installed code:
`FeatureOodDetector` persists only `mean`, `m2`, `cov_inv`, dimensions,
counters, and tuning scalars. It does **not** persist per-observation
samples. State size is therefore O(d²), not O(decisions).

Regression guard: `feature_detector_state_shape_is_bounded_by_dimension`
drives 10,000 records through the detector and asserts the persisted
matrix/vector shapes remain bounded by feature dimension.

The historical growth profile remains useful as a cautionary capacity note,
but the open operator mitigation is no longer needed for current builds.

### Multi-AdaptiveChoice per-node learning — FIXED 2026-05-19

The 5C runtime path is wired. A graph with multiple `(choice ...)` /
`AdaptiveChoice` nodes now receives an independent `candidateId` per
`decisions[]` entry, persists per-node state under `memory.strategies`, and
accepts `decisionIndex` on `/feedback` to target a non-primary decision node.
Feedback without `decisionIndex` still targets `decisions[0]` for backwards
compatibility.

Regression guard: `multi_choice_feedback_can_target_second_decision_index`
installs a hand-authored two-choice capsule, drives it to Active, posts
feedback with `decisionIndex: 1`, and asserts the second node's meta-bandit
and candidate context state moved.

### Strategy-node install-time warning — FIXED 2026-05-19

The install path now calls `warn_if_strategy_nodes`, which decodes incoming
`.lyc` bytes and emits a `tracing::warn!` when the capsule contains legacy
`OpCode::Strategy` nodes. The warning is intentionally log-only: install
still succeeds and the HTTP response shape stays unchanged.

Regression guard: `install_with_strategy_node_succeeds_and_is_non_blocking`
builds a tiny Strategy-node graph in-process, installs it, and verifies the
stored graph remains intact.

### `/report` endpoint omits `algorithm` and `warmup` fields — FIXED 2026-05-18

`do_report` (`Lycan/src/server/inspect.rs`) now surfaces three previously-
omitted top-level fields in its JSON response:

- `warmup` — lifecycle object: `{state: "warmup"|"active"|"frozen",
  collected, target}` for warmup; `{state, characterization}` for
  active; `{state, reason}` for frozen.
- `algorithm` — the resolved `PickedAlgorithm` after warmup (e.g.
  `"Weighted { learning_rate: 0.1 }"`), or `null` during warmup.
- `metaBandit` — per-strategy-node summary keyed by node id, with
  `totalRounds`, `currentLeader`, and the 5/7-candidate list with
  `trials`, `meanReward`, `cumulativeReward` per candidate.

The state was always correct on disk; the formatter just didn't read
it back. The fix loads `warmup_state` (already loaded for the weight-
overlay logic) and `memory.metaBandit` and emits them. Verified end to
end against a freshly installed `predictive-autoscaling` capsule:
cold returned `{state: warmup, collected: 0, target: 30}` and
`algorithm: null`; post-warmup returned `algorithm: Weighted {...}`
plus the full 7-candidate meta-bandit summary.
