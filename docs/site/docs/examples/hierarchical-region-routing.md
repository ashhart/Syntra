# Hierarchical region routing

Hierarchical-bandit demo capsule. The action space is the cross
product of two natural decisions:

| Level | Decision      | Options                                  |
|-------|---------------|------------------------------------------|
| 1     | `region`      | `us`, `eu`                               |
| 2     | `server_type` | `us_small`/`us_medium`/`us_large`, `eu_*`|

Two regions × three server types = six leaf actions. The
hierarchical framing keeps one bandit per non-leaf level: reward
credit flows along the chosen path so siblings within the same parent
share information, unlike a flat six-arm bandit.

Repository copy: [`examples/hierarchical-region-routing/`](https://github.com/ashhart/Syntra/tree/main/examples/hierarchical-region-routing).

## Status

**Wired end to end.** Phase I followups 4–8 (May 2026) closed the
runtime branches. The capsule installs via `syntra author` and
decides through `do_decide_hierarchical`; `/feedback` propagates
reward across every level via
`HierarchicalCapsuleState::apply_feedback`; `/admin/capsules` reports
`scoringMode: "hierarchical"` with real leaf labels.

## Files

- `capsule.yaml` — the CapsuleSpec input. Carries `name`, `version`,
  `options:` (flat leaf-name list), `reward:`, and
  `hierarchical_options:` (the nested tree the runtime walks at
  decide time).
- `program.lyc`, `program.lycs` — auto-emitted by `syntra author`.
  Hierarchical capsules **do not execute their graph** at decide time
  in v1; selection happens entirely outside the executor. The `.lyc`
  is decorative for legacy compat + inspection.
- `hierarchical_spec.json` — sidecar emitted by `syntra author` from
  the `hierarchical_options:` block.
- `manifest.json` — references `hierarchical_spec.json` in its
  `sidecars` array.
- `learning.json` — minimal: discrete context, refusal disabled.

## Build + install

```bash
cd Syntra/examples/hierarchical-region-routing
syntra author capsule.yaml --out-dir .

curl -X POST "$SYNTRA/tenants/demo/jobs/region/capsules/router/install" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     --data-binary @program.lyc

# The hierarchical_spec sidecar is what tells the runtime to use the
# tree shape rather than falling back to flat AdaptiveChoice.
curl -X PUT "$SYNTRA/tenants/demo/jobs/region/capsules/router/hierarchical_spec" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @hierarchical_spec.json

curl -X PUT "$SYNTRA/tenants/demo/jobs/region/capsules/router/learning" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @learning.json
```

## Decide

```bash
curl -X POST "$SYNTRA/tenants/demo/jobs/region/capsules/router/decide" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     -d '{}'
```

Response (actual shape from a fresh install):

```json
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

The caller reads `leafName` (or maps `path` through the spec) to know
which option to apply. `perLevelCandidateIds` records which
meta-bandit candidate fired at each level — useful for inspection /
audit.

## Feedback

```bash
curl -X POST "$SYNTRA/tenants/demo/jobs/region/capsules/router/feedback" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     -d '{"decisionId": "dec_...", "reward": 0.83}'
```

The reward is applied unchanged at every level along `path`. Both the
root meta-bandit (region decision) and the chosen branch's
meta-bandit (server-type decision) update from the same observed
reward.

## What to expect

Validated end to end on a fresh install: 100 rounds rewarding only
the `us_medium` leaf path `[0, 1]` at 1.0 and every other leaf at
0.0 converges cleanly:

- **Root bucket `d0|`**: weights → `[0.94, 0.06]` (us preferred at ~94%)
- **us sub-bucket `d1|0`**: weights → `[0.05, 0.91, 0.04]` (medium dominates at ~91%)
- **eu sub-bucket `d1|1`**: stays near-uniform (16 rounds, all reward 0 —
  no signal to differentiate eu's three leaves)
- **Last 30 of 100 rounds histogram**: `us_medium` chosen 26/30 times (87%)

The per-level meta-bandits each carry their own 7-candidate portfolio
(Thompson, UCB, Weighted, EpsilonGreedy, Greedy, LinUCB, LinTS).

## v1 limitations

- The capsule's `.lyc` graph is **not executed** at decide time.
  `runtime.publish` calls inside a hierarchical capsule do not fire.
- Refusal / OOD / conformal calibration not yet wired for hierarchical.
- `apply_feedback` credits the per-level meta-bandit's current leader
  as a greedy proxy rather than the candidate actually selected at
  decide time. Threading the per-level candidate id back into
  feedback is queued.

## See also

- [Strategy node](../concepts/strategy-node.md) — the three adaptive
  flavors.
- [Shared-state action embeddings](shared-state-action-embeddings.md)
  — the other non-default flavor.
