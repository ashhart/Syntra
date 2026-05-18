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

**Likely investigation targets:**
- Warmup overhead: 30 random selections × 90 cell-instances = 2,700
  decisions where Syntra is doing uniform random. VW has no warmup
  equivalent; this is pure Syntra regret. Could test by setting
  warmup-target to 5 or 1 for this benchmark and rerunning.
- `apply_feedback` weight-delta asymmetry: `delta = clipped * learning_rate`
  means for binary rewards reward=0 produces delta=0 (no weight decrement).
  Currently irrelevant to selection because the conditional greedy
  override dominates, but could matter if the override is ever softened.
- Code drift since Phase A-F: working-tree had `D src/server.rs`,
  `M src/learning.rs`, `M src/graph_executor.rs`, `M src/capabilities.rs`
  when this session started. Any of those could have subtly shifted
  the Thompson update path.

**Operator-facing status:** the published "2.67× lower regret" external
claim does not currently reproduce. Use "bin-A competent with VW across
the 9-cell benchmark grid" as the defensible claim until the headline
number is recovered or the gap is explained.

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
