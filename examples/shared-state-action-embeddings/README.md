# shared-state-action-embeddings

A worked capsule that demonstrates **shared-state LinUCB** with
per-option action-feature vectors. The bandit learns a single
parameter vector θ over the concatenated `[x_context, x_option]`
space, so an option whose action features overlap with trained
options inherits a non-zero prior instead of starting from scratch.

## Why this capsule exists

Per-option LinUCB (Syntra's default for feature-vector contexts)
keeps one independent matrix and response vector per option. When
you add a 7th option to a 6-option capsule, the new option's matrix
is fresh — its UCB score is driven entirely by the exploration
bonus until it accumulates enough observations. For a capsule whose
option set rotates (LLM models, server flavours, retail variants),
that's a real cost: every rotation pays the cold-start tax.

Shared-state LinUCB pays the cold-start once, on the **shared**
matrix, and never again per option. A new option's prior is
`x · θ̂` where `x = [x_context, x_option]` — the model has learned
how the option features map to rewards, so an option whose features
sit in the convex hull of trained ones inherits a sensible estimate
from day zero.

The supporting test in `Lang/src/shared_state_strategy.rs`
(`shared_state_strategy_generalises_to_unseen_options`) demonstrates
this. After 300 decide / feedback rounds against options A/B/C/D,
it registers E and F and queries `posterior_mean` at three
contexts. The estimates land within ~4% of the true expected reward
in the linear-truth case; the assertion threshold is 30% to leave
RNG-seed headroom.

## Files

| File | What it is |
|------|------------|
| `capsule.yaml` | Capsule definition with six options and a 2-D `option_features` vector per option. |
| `program.lycs` | Minimal Lycan source — pulls `workload` from the request body, hands selection to the `(choice ...)` node. |
| `learning.json` | Learning config with `sharedState.enabled: true`, the `optionFeatures` map echoed for the runtime, and the LinUCB hyperparameters. |

## Status

The Lang-side wrapper (`Lang/src/shared_state_strategy.rs`) is
shipped and tested. The Syntra-side wiring — extending
`capsule_spec.rs` to parse `option_features`, the
`learning.json::sharedState` block, and a `do_decide` branch that
routes through `SharedStateOptionStrategy::select` — is the next
step on the main thread. See
`Syntra/docs/capsule-features/shared-state-linucb.md` for the
integration plan and the doc-level discussion of when shared state
is the right call.

## Worked /decide flow (post-integration)

```
POST /tenants/edge/jobs/route/capsules/shared-state-action-embeddings/decide
Authorization: Bearer $TOKEN
Content-Type: application/json

{
  "features": { "workload": 0.42 }
}
```

Response:

```json
{
  "option": "B",
  "decisionId": "...",
  "scores": {
    "A": 0.31, "B": 0.59, "C": 0.27,
    "D": 0.55, "E": 0.43, "F": 0.47
  }
}
```

POST `/feedback` with the observed reward:

```json
{
  "decisionId": "...",
  "reward": 0.7
}
```

The reward applies a Sherman-Morrison update on the shared θ. Every
option — including E and F, which were not chosen — sees its
posterior mean shift, because they share parameter mass with B.
That sharing is the whole point.

## When NOT to use this capsule shape

- If the option features are arbitrary (e.g. one-hot per option),
  shared-state LinUCB degenerates to a slightly-more-expensive
  version of per-option LinUCB. Use the default per-option flavour.
- If the option set is fixed and tiny (≤ 5 options), the cold-start
  saving doesn't outweigh the cost of authoring features. Stick
  with per-option LinUCB or Thompson sampling on Bernoulli rewards.
- If the option set is very large, the shared `(d_context +
  d_option)²` matrix may be expensive. The number worth checking
  before deploying is `(d_context + d_option)² * 8 bytes` for the
  state size and the per-decide cost of a matrix-vector product at
  that dimension.
