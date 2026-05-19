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

### OOD detector accumulates per-observation state unbounded (feature-context capsules)

**Status:** Real growth bug — `memory.json` increases ≈1.3 KB per `/decide`
on a feature-context capsule, even when the same feature vector is observed
repeatedly. At 1 decide/sec this is ≈110 MB/day, ≈3.4 GB/month.
**Last measured:** May 2026, via predictive-autoscaling stress test
(see `Syntra/docs/operations/memory-profile.md`).
**Scope:** Feature-context capsules only. Discrete-context capsules are
bounded by `contextKey` cardinality (confirmed empirically: 4,300 same-
context decides grew memory.json by 11 KB, dominated by float-precision
serialization noise).
**Symptom:** `OptionStats` itself is fixed-size and not the offender;
the growth is in `memory.feature_ood_for(nid)`. `det.record(x)` is
called every `/decide` (`Lycan/src/server/decide.rs:283`) and accumulates
state that is not bucketed by feature-vector hash.

**Likely fix shape:** cap the OOD detector's stored observation window
at N samples — matching its `rebuild_due(100)` cadence is a natural
choice since records beyond that window aren't read by the current
scorer anyway. Numerical care needed to keep the covariance estimate
stable. Not scoped to this round; would also need a Lycan-side test
characterizing OOD detection quality under a bounded window.

**Operator mitigation:** see `Syntra/docs/operations/memory-profile.md`
("Recommended operator action"). Periodic capsule-state rotation works
as a stopgap.

### Multi-AdaptiveChoice graphs: decide response wired, memory/learning is not

**Status:** Partial wiring. A `.lycs` program with two or more
`(choice ...)` blocks compiles to multiple `AdaptiveChoice` nodes, and
`/decide` returns one entry per node in `decisions[]` with independent
`chosen_option` and `weights`. **However**, `/memory` records only one
strategy (the primary, lowest-index node). The meta-bandit learning,
context buckets, and feedback accounting all happen against decisions[0]
only. The second-and-later AdaptiveChoice nodes are effectively
choosing uniformly at random across the lifetime of the capsule.
**Last verified:** May 2026, via a hand-authored two-choice capsule.
**Reference:** `Lycan/src/server/helpers.rs:59-61` accurately describes
the gap ("primary today means decisions[0]... when multi-AdaptiveChoice
support (debt item 5C) lands"). The `per_node_candidates` HashMap in
`decide.rs:340` is set up but the persistence path only stores the
primary node's state in `memory.json`.
**Impact:** YAML-authored capsules don't trigger this — `emit_lycan_source`
in `capsule_compiler.rs` always emits a single `(choice ...)`. Only
hand-authored `.lycs` capsules with multiple `(choice ...)` blocks hit it.
**Likely fix shape:** in `do_feedback` and the memory-persistence side
of `do_decide`, iterate over `all_choice_nodes` rather than just
`primary_choice_node`; allocate per-node strategy buckets in
`memory.strategies`. Out of scope for this round.

### Strategy-node install-time warning never landed

**Status:** Documentation referenced a `warn_if_strategy_nodes` helper
in `Syntra/src/capsule_compiler.rs` that doesn't exist. The file has
zero references to `Strategy`, `OpCode::Strategy`, `warn!`, or
`eprintln!`. The CHANGELOG's only "Item 1" reference is to a separate
greedy-lock investigation.
**Why this is mostly fine in practice:** the YAML compiler
(`emit_lycan_source`) only ever emits `(choice ...)` forms, which
compile to `OpCode::AdaptiveChoice`, never `OpCode::Strategy`. So no
YAML-authored capsule can produce a Strategy node by accident. The
warning would only fire for hand-authored `.lycs` capsules installed
via the raw `/install` endpoint.
**Status remains open** because if hand-authored installs do happen
(they're a supported path), an install-time hint pointing users at
`(choice ...)` would be a real ergonomic win. The right home is in
the `/install` handler (`Lycan/src/server/admin.rs` or wherever the
`.lyc` bytes are decoded), not in `capsule_compiler.rs`. Out of scope
for this round.

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
