# Hierarchical bandits

A hierarchical-bandit capsule expresses its action space as a tree
rather than a flat list. Each non-leaf decision is itself a bandit, and
the leaves are the terminal actions Syntra returns to the caller. This
is the bandit-only analog of hierarchical reinforcement learning: there
are no temporal rollouts, only nested discrete choices, with credit
flowing along the chosen path.

**Status**: wired end to end through `/decide` and `/feedback` (Phase
I followups 4–10, May 2026). The third adaptive flavor alongside
meta-bandit-over-per-option-LinUCB and shared-state LinUCB. See
`Syntra/docs/roadmap.md` for the v1 limitations that carry forward
(graph not executed for hierarchical decides; refusal/OOD not wired;
per-level candidate id uses a greedy proxy in feedback).

## When to use this

Use a hierarchical capsule when the action space has natural grouping
and the groups carry information you don't want the bandit to discover
from scratch. Concrete case: 5 regions × 4 server types = 20 leaf
actions. A flat 20-arm bandit learns each (region, server type) pair
independently. A hierarchical capsule keeps one bandit at the region
level and one bandit per region at the server-type level; if `medium`
is generally a good size, every region's bandit sees that signal
without needing its own 20-arm exploration budget.

## When not to use this

Skip it when the grouping is artificial. If the reward depends only on
the leaf identity and not on intermediate structure — for example, six
ad-creative variants that share no meaningful family — the hierarchical
wrapper just adds bookkeeping. A flat capsule is faster to converge
because every observation updates exactly one arm, not one per level.
The grouping needs to carry real shared structure for the hierarchy to
pay off.

## YAML schema

A hierarchical capsule is a `CapsuleSpec` with two related fields:

- `options:` — the *flat* leaf-name list. Required for legacy compat.
  Must equal `hierarchical_options.enumerate_paths().map(resolve_path)`
  in order. The CapsuleSpec validator enforces this.
- `hierarchical_options:` — the nested tree. Every entry is either a
  branch (with `name` + `sub_capsule`) or a bare leaf
  (`- name: small`).

```yaml
name: hierarchical-region-routing
version: 0.1.0

# Flat view (legacy compat) — must match enumerate_paths.
options:
  - us_small
  - us_medium
  - us_large
  - eu_small
  - eu_medium
  - eu_large

reward:
  type: continuous
  range: [-1.0, 1.0]

# Tree view (what the runtime walks at decide time).
hierarchical_options:
  options:
    - name: us
      sub_capsule:
        options: [us_small, us_medium, us_large]
        reward: { type: continuous, range: [-1.0, 1.0] }
    - name: eu
      sub_capsule:
        options: [eu_small, eu_medium, eu_large]
        reward: { type: continuous, range: [-1.0, 1.0] }
  reward: { type: continuous, range: [-1.0, 1.0] }
```

Sub-tree leaf names need to be **globally unique** so the flat
`options[]` list (which is the concatenation of every leaf in
traversal order) doesn't have duplicates. The convention shown above —
prefix each sub_capsule's leaves with the parent name — is the most
readable way to keep them disjoint.

Validation rules (enforced by `HierarchicalSpec::validate` in
`Lang/src/hierarchical.rs`, plus the CapsuleSpec-level rules in
`Syntra/src/capsule_spec.rs::validate_hierarchical`):

- Maximum nesting depth: 4.
- Minimum branching factor at every level: 2.
- Option names within a branch must be unique.
- Total reachable leaf count: 256.
- A `continuous` or `sparse_continuous` reward must declare a `range`
  with strict `lo < hi`.
- `hierarchical_options` is mutually exclusive with `decisions[]`
  (the sequential-DAG shape from Phase 3C).
- Flat `options` must equal `enumerate_paths().map(resolve_path)`.

## Install flow

Three steps. `syntra author` emits the `hierarchical_spec.json`
sidecar; the install pipeline uploads both the `.lyc` and the sidecar:

```bash
# 1. Compile.
syntra author capsule.yaml --out-dir .

# 2. Install the .lyc.
curl -X POST "$SYNTRA/tenants/<t>/jobs/<j>/capsules/<c>/install" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     --data-binary @program.lyc

# 3. Upload the hierarchical_spec sidecar. Without this step the
#    runtime falls back to a flat AdaptiveChoice over the leaf names
#    — same shape, but no per-level reward propagation.
curl -X PUT "$SYNTRA/tenants/<t>/jobs/<j>/capsules/<c>/hierarchical_spec" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @hierarchical_spec.json
```

After step 3, `GET /admin/capsules` reports
`scoringMode: "hierarchical"` for the capsule with real leaf labels in
`options`.

## /decide and /feedback shape

`/decide` returns the full path the bandit walked, the resolved leaf
name, and the per-level candidate id (which algorithm in the
meta-bandit portfolio fired at each level):

```json
POST /tenants/.../capsules/<name>/decide
{}

200 OK
{
  "ok": true,
  "decisionId": "dec_57a2270571f0c2fa",
  "decisions": [{
    "node_id": 0,
    "kind": "hierarchical",
    "path": [0, 1],
    "leafName": "us_medium",
    "perLevelCandidateIds": ["EpsilonGreedy", "Greedy"]
  }],
  "algorithm": "hierarchical",
  "warmup": {"state": "warmup", "collected": 0, "target": 30},
  "refused": false
}
```

The caller acts on `leafName` (or maps `path` through the spec).
`perLevelCandidateIds` records which meta-bandit candidate fired at
each level for audit / debugging.

`/feedback` accepts the usual reward / components / outcome form. The
runtime looks up the decision by `decisionId`, recovers the stored
path, and calls `HierarchicalCapsuleState::apply_feedback(&path,
&path, reward)` to propagate the observed reward to every level:

```json
POST /tenants/.../capsules/<name>/feedback
{
  "decisionId": "dec_57a2270571f0c2fa",
  "reward": 0.83
}

200 OK
{
  "ok": true,
  "kind": "hierarchical",
  "decisionId": "dec_57a2270571f0c2fa",
  "path": [0, 1],
  "reward": 0.83,
  "levelsUpdated": 2
}
```

`levelsUpdated` equals `path.len()` for valid feedback; a mismatch is
the signal that the persisted spec changed between decide and
feedback (treat as a configuration error).

## Credit assignment

Two propagation modes are supported, controlled by the spec's optional
`reward_propagation` field at the root of `hierarchical_options`:

**Full** (default). The same reward is applied unchanged at every
level along the chosen path. If the leaf earns `0.83`, both the
root-level bandit's `us` arm and the us-level bandit's `medium` arm
see `0.83` as their observed reward for that round. This is the
simplest scheme — siblings under a successful parent share its lift;
siblings under a failing parent share its penalty.

It is also the **noisiest** scheme. A leaf that earns `0.9` because
the chosen size was right, even though the chosen region was wrong,
still credits the wrong region at `0.9`.

**Discounted { factor: f64 }**. Per-level reward at depth `d` along a
length-`N` path is `reward * factor.powi(N - 1 - d)`. The deepest
level (the leaf decision) sees the full reward; every shallower level
is attenuated by an extra factor of `factor`. Useful when:

- the top-level decision is robust and you want it to explore less
  aggressively based on leaf-level reward noise;
- you have evidence that most reward variance comes from
  leaf-specific structure rather than from the top-level choice;
- you want the root meta-bandit's exploration schedule to depend
  less on which particular leaf got picked under it.

YAML form (top-level of `hierarchical_options`):

```yaml
hierarchical_options:
  options: [...]
  reward: { type: continuous, range: [-1, 1] }
  reward_propagation:
    mode: discounted
    factor: 0.5
```

Or for full propagation (the default, equivalent to omitting the
field):

```yaml
  reward_propagation:
    mode: full
```

**Worked example.** A 2-level capsule with `factor: 0.5`. A leaf
reward of `1.0` for path `[us, medium]` results in:

- The leaf-level bucket (depth 1, `d1|0` in the persisted state)
  credits `medium` with `1.0 * 0.5^0 = 1.0`.
- The root-level bucket (depth 0, `d0|`) credits `us` with
  `1.0 * 0.5^1 = 0.5`.

`factor = 1.0` is mathematically equivalent to `Full`. `factor`
values outside `(0, 1]` are accepted (negative or >1 are unusual but
not validated against) — pick a value that matches the per-level
reward dynamic you observe in your traffic, or stick with `Full`
until you have specific evidence the root should attenuate.

The choice between propagation modes is a tuning question that
depends on the noise structure of your reward signal. Roadmap notes
on more sophisticated schemes (eligibility traces, doubly-robust
estimators) remain — `Discounted` is the practical middle ground.

## Selection inside each level (v1)

Each level holds its own `MetaBandit` (the same five-candidate
discrete portfolio as flat capsules — Thompson, UCB, Weighted,
EpsilonGreedy, Greedy) and its own per-arm weights. The meta-bandit
drives selection-history accounting at each level; the actual arm
pick is a weighted-random draw over the level's weights, regardless
of which meta-bandit candidate was selected for the current round.
The candidate id at each level is recorded in the decision event
(`perLevelCandidateIds`) for audit, but the same per-arm weight
distribution drives selection across all candidates inside a level.
True per-candidate selection — so e.g. the root-level LinUCB candidate
sees the root level's feature context while the us-level Thompson
candidate operates on a different state — is queued under "Future
polish" in `Syntra/docs/roadmap.md`.

## Validated convergence

A 100-round end-to-end run against the 6-leaf 2×3 demo capsule
rewarding only the `us_medium` leaf at `1.0` and every other leaf at
`0.0` converges cleanly:

| Bucket | Description           | Final weights              | totalRounds |
|--------|-----------------------|----------------------------|-------------|
| `d0|`  | root (region)         | `[0.94, 0.06]`             | 100         |
| `d1|0` | us subtree            | `[0.05, 0.91, 0.04]`       | 84          |
| `d1|1` | eu subtree (no signal)| stays near uniform         | 16          |

In the last 30 rounds, `us_medium` was chosen 26/30 times (87%). The
root learned `us > eu`; the us subtree learned `medium > small/large`;
the eu subtree stayed flat because it saw 16 rounds all rewarded at
`0.0` and had no signal to differentiate its leaves.

## Worked example and persistence

See [`Syntra/examples/hierarchical-region-routing/`](../../examples/hierarchical-region-routing/)
for the 2 × 3 = 6 leaf demo capsule. It installs via `syntra author`
plus the `PUT /hierarchical_spec` upload step and runs end to end
against the same Syntra binary that serves flat / shared-state
capsules. The README in that directory captures a full
install-and-drive walkthrough with actual response shapes.

The persisted state lives in
[`Lang/src/hierarchical_state.rs`](../../../Lang/src/hierarchical_state.rs)
as `HierarchicalCapsuleState` — one bandit bucket per reachable
`HierState`, allocated lazily on first selection, JSON-serialisable
for the sidecar store at `hierarchical_state.json` next to
`current.lyc` and `memory.json`. The dashboard's `/api/state` surfaces
a per-bucket summary (`hierarchical.buckets[]` with `key`, `depth`,
`currentLeader`, `leaderMean`, `totalRounds`, `weights`) so the chart
in Region 2 can render one line per HierState.

## Where this fits in the appliance

Hierarchical bandits are a structural choice for the *action* side of
a capsule. They compose cleanly with everything described in
[`Syntra/docs/concepts/operational-intelligence.md`](../concepts/operational-intelligence.md):
the capsule's Lycan program still computes features, the strategy node
still sees them, the only difference is that the strategy node walks a
tree rather than a flat option list. They are positioned alongside
contextual features in [`Syntra/POSITIONING.md`](../../POSITIONING.md)
as one of the structural levers for capsules whose flat option space
would be either too large or too coarse.
