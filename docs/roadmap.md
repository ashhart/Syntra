# Roadmap

This document is the explicit index of capabilities whose foundation is
complete but whose runtime wiring is deferred. Tracking it here so
work picked up across sessions doesn't drop on the floor.

Resolved items move out to `CHANGELOG.md`. Hard bugs surface in
`known-issues.md`. This file is for "shape exists, integration
queued" only.

## Queued

### Hierarchical bandits — runtime wiring

**Status**: foundation + spec/install layer complete (steps 1+2),
runtime branches in `server.rs` + store sidecar still queued (steps
3–5).

**What's done** (Item 2 prep + autonomous-tick #2, May 2026):

- `Lang/src/hierarchical.rs` — the math layer: `HierarchicalSpec`,
  `enumerate_paths`, `resolve_path`, `propagate_reward`. 13 tests
  passing. Reviewed and not flagged for foundation issues.
- `Lang/src/hierarchical_state.rs` — the persisted state wrapper:
  `HierarchicalCapsuleState`, lazy per-`HierState` bandit buckets,
  `select_path`, `apply_feedback`, JSON round-trip. 7 tests passing.
- `Syntra/examples/hierarchical-region-routing/` — worked test capsule
  (2 parents × 3 children). The in-process 200-round simulation
  converges cleanly: root weights `[0.91, 0.09]` on the rewarded
  parent; nested level weights `[0.04, 0.92, 0.04]` on the rewarded
  child. Meta-bandit `total_rounds` advances on both levels (200 and
  152 respectively, reflecting per-level activation counts).
- `Syntra/docs/capsule-features/hierarchical-bandits.md` — concept
  doc + when-to-use / when-not.
- **NEW (step 1)**: `CapsuleSpec.hierarchical_options:
  Option<HierarchicalSpec>` field with validation. Mutually exclusive
  with `decisions[]`; `options` must equal
  `enumerate_paths().map(resolve_path)` for legacy compat. 5 new
  tests in `Syntra/src/capsule_spec.rs`.
- **NEW (step 2)**: `capsule_compiler` emits a `hierarchical_spec.json`
  sidecar at install time for hierarchical capsules; `manifest.json`
  carries a `sidecars` array pointing at it. Graph emission unchanged
  (single AdaptiveChoice over flattened leaves) — the recursion will
  happen at decide-time when steps 3–4 land. 2 new tests in
  `Syntra/src/capsule_compiler.rs`.
- **NEW (step 5)**: `LycanStore` now exposes
  `load_hierarchical_spec_in_job(...)`,
  `load_hierarchical_state_in_job(...)`, and
  `save_hierarchical_state_in_job(...)` against the matching sidecars
  on disk. Same Option-on-missing pattern as the existing
  `load_warmup_state_in_job` helpers, atomic-write on save. 3 new
  tests covering spec load, state save/load round-trip (structural
  assertion — `serde_json` loses 1 ULP of f64 precision on numeric
  Value round-trip but tree shape, bucket keys, and weights within
  1e-9 are exact), and the absence-of-sidecar path returning None for
  legacy flat capsules.
- **NEW (step 3)**: `server.rs::do_decide` now early-dispatches to
  `do_decide_hierarchical` when the capsule's `hierarchical_spec.json`
  sidecar is present. The hierarchical handler loads the spec + state,
  walks the tree via `HierarchicalCapsuleState::select_path`, persists
  the updated state, and writes a decision-log entry with `path`,
  `leafName`, `perLevelCandidateIds`, and `kind: "hierarchical"`. New
  install-side endpoints `GET / PUT /tenants/.../hierarchical_spec`
  let an operator upload the compile-output sidecar into the runtime
  store after `/install`. Plus the matching
  `LycanStore::save_hierarchical_spec_in_job` with validation.
  Verified end to end against a 2x3 tree: 30 decides distribute
  near-uniformly across the six leaves (6/6/5/5/4/4) with three
  buckets allocated (`d0|`, `d1|0`, `d1|1`) and a 9.4 KB
  `hierarchical_state.json` persisted on disk. `/decide` response
  carries the new shape:
  `{"algorithm":"hierarchical","decisions":[{"kind":"hierarchical","leafName":"eu_large","path":[1,2],"perLevelCandidateIds":["EpsilonGreedy","Greedy"]}]}`.

**v1 limitations to know**:
- The capsule's `.lyc` graph is **not executed** for hierarchical
  decides. That means `runtime.publish` calls inside a hierarchical
  capsule won't fire and `!cap` calls in the program body never run.
  Hierarchical selection happens entirely outside the executor; the
  graph node is decorative for legacy compat / inspection. Lifting
  this is a follow-up — see "Future polish" below.
- Warmup gating is bypassed for hierarchical. The per-level
  meta-bandits handle their own exploration via the standard
  rate-adaptive schedule, so a separate capsule-level warmup is
  redundant. `/report`'s warmup field will still update on feedback
  (step 4) for /report consistency.
- Refusal / OOD / conformal calibration are not yet wired for
  hierarchical capsules.

- **NEW (step 4)**: `do_feedback_hierarchical` in `server.rs` —
  same early-dispatch pattern as the decide side. Parses reward
  (supports `reward`, `components`, or `outcome`), updates warmup
  state, looks up the decision by `decisionId`, extracts the recorded
  `path`, calls `HierarchicalCapsuleState::apply_feedback(&path,
  &path, reward)` to propagate the observed reward across every
  level, persists the updated state, writes audit + feedback log
  entries. Verified end to end against a 2×3 hierarchical capsule
  rewarding only the `us_medium` leaf path `[0, 1]`:
  - Root bucket `d0|`: weights converge to `[0.94, 0.06]` (us
    dominates at 93.5%).
  - us sub-bucket `d1|0`: weights converge to
    `[0.05, 0.91, 0.04]` (medium dominates at 90.8%).
  - Leaf-pick histogram in the last 30 of 100 rounds: `us_medium`
    chosen 26/30 times (87%).

**Status: DONE.** Hierarchical bandits are reachable through `/decide`
and `/feedback` end to end with weight convergence on the rewarded
leaf path. The third adaptive flavor (alongside meta-bandit-over-
per-option-LinUCB and shared-state LinUCB) is now functionally
complete. The v1 limitations documented under step 3 still apply
(graph not executed, no `runtime.publish` in hierarchical capsules,
no refusal/OOD) and are tracked separately under "Future polish"
below.

### Future polish (not blocking the third adaptive flavor)

- Wire graph execution into the hierarchical `/decide` path so
  `runtime.publish` and other `!cap` calls in the capsule's `.lycs`
  fire during a hierarchical decide. Currently the graph is loaded but
  not executed; the bandit-level path resolution happens entirely
  outside the executor.
- Wire refusal / OOD / conformal calibration for hierarchical
  capsules. Today they always return `refused: false`.
- ~~Record per-level candidate ids at decide time and thread them back
  into `apply_feedback`~~ — **DONE** (Phase I followup 14, May 2026).
  New `HierarchicalCapsuleState::apply_feedback_with_candidates`
  takes a `per_level_candidates: &[CandidateId]` argument; the server's
  `do_feedback_hierarchical` recovers the ids from the decision
  event's `perLevelCandidateIds` field and threads them through.
  Length-mismatch falls back to the greedy proxy as a data-integrity
  safeguard. Original `apply_feedback` remains for math-layer tests
  that don't track candidate provenance.

**Concrete integration plan** (from the prep agent's report):

1. **`Syntra/src/capsule_spec.rs`** — add optional field around line 50:
   ```rust
   #[serde(default, skip_serializing_if = "Option::is_none", rename = "hierarchicalOptions")]
   pub hierarchical_options: Option<lycan::hierarchical::HierarchicalSpec>,
   ```
   Validation in `CapsuleSpec::validate` (around line 185): call
   `hierarchical_options.validate()` and propagate the error;
   require `options` to match `enumerate_paths().map(resolve_path)`
   for legacy compat; reject when `decisions` is also set
   (mutually exclusive).

2. **`Syntra/src/capsule_compiler.rs`** — in `compile_to_dir`
   (around line 14): detect `spec.hierarchical_options.is_some()`,
   keep emitting one flat `AdaptiveChoice` over the leaf names from
   `enumerate_paths().map(resolve_path)`, persist
   `hierarchical_options.to_json()` to a new sidecar file
   `hierarchical_spec.json` in the compiled-capsule directory.

3. **`Lang/src/server/decide.rs` `do_decide`**: after
   `load_memory_in_job`, attempt
   `state.store.load_hierarchical_state_in_job(...)`. If present:
   skip the flat AdaptiveChoice branch; call
   `HierarchicalCapsuleState::select_path(...)`; write the
   `decision.jsonl` event with `path`, `leafName`,
   `perLevelCandidateIds`; replace single `"option"` in the
   response with `"option": leaf_name, "path": path,
   "perLevelCandidateIds": [...]`; save via
   `save_hierarchical_state_in_job`.

4. **`Lang/src/server/feedback.rs` `do_feedback`**: after
   resolving the decision-id lookup, branch on whether the
   decision record carries a `path` field. If yes: load
   `HierarchicalCapsuleState`, call `apply_feedback(&path,
   &chosen_per_level, reward)`, save back, emit `feedback.jsonl`
   with per-level `(state, reward)` updates.

5. **`Lang/src/store.rs`** (around line 447, the memory-sidecar
   block): add `load_hierarchical_state_in_job` /
   `save_hierarchical_state_in_job` mirroring the `memory.json`
   pair against a new sidecar file `hierarchical_state.json`.
   Rationale for sidecar over folding into `memory.json`: the
   bandit shapes are structurally different; sidecar lets capsules
   toggle between flat and hierarchical without merge logic.

**Validation criterion when wiring lands**: install the
`hierarchical-region-routing` capsule, send 100 `/decide`
+ `/feedback` rounds rewarding only the `[us-east, medium]` path,
confirm via `/memory` that root weights converge to `[~0.9, ~0.1]`
and the us-east bucket's child weights converge to
`[~0.04, ~0.92, ~0.04]`. (These are the same shape the in-process
test already produced; the runtime test is just the same numbers
arriving through HTTP.)

**Estimated effort**: ~300 lines across 4 files, plus an integration
test. 2–3 hours focused.

**Why deferred**: Item 2 was scoped to land "one capability cleanly
rather than both half-broken." Shared-state LinUCB was prioritised
because its generalisation property (the validation criterion in 2b)
is a *runtime* property that can only be proven at the API boundary.
Hierarchical's property (clean convergence under reward propagation)
was already proven at the math layer in the in-process test; the
runtime wiring just routes that math through HTTP.

## Resolved

(Items move here when they ship and then drop entirely once they
appear in `CHANGELOG.md`.)

— none —
