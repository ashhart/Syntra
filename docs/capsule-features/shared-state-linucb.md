# Shared-state LinUCB with action embeddings

Syntra's default contextual bandit (per-option LinUCB) maintains
an independent `(A, b)` matrix-pair for every option. That isolation
buys clean math at the cost of a cold-start tax on every newly
introduced option: until you've fed observations through option N,
its `θ` is zero and its UCB score is the exploration-bonus floor.

Shared-state LinUCB replaces the per-option matrices with **a single
shared matrix** over the concatenated feature space `[x_context,
x_option]`. Each option carries a fixed `option_features` vector
that the capsule author provides at install time. At decide and
feedback time the bandit composes `x = [x_context, x_option]` and
runs the same Sherman-Morrison update against the shared `(A, b)`.

If your option set is a fixed handful of arbitrary labels, this is
not the feature you want. If your option set carries real semantic
similarity — and especially if it rotates over time — this is the
feature that pays for itself.

## The intuition in one paragraph

Per-option LinUCB learns one `θ_i` per option, so option N+1 starts
at `θ_{N+1} = 0` and pays cold-start until it's been pulled enough
times. Shared-state LinUCB learns one `θ` over the joint feature
space, so option N+1 — supplied with its action-feature vector at
registration time — gets `score(N+1, ctx) = [ctx, x_{N+1}] · θ̂`,
which is a non-trivial estimate from the moment it's registered.
The price is that all options share parameter mass; an option whose
action features look nothing like any trained option will not get a
useful prior either.

## When to use it

- **LLM model routing**, where each model has an embedding (cost,
  latency, accuracy, capability vector) that carries real semantic
  similarity. Adding a new model = supply its embedding =
  reasonable prior.
- **Server / container types**, where each flavour has a capability
  vector (vCPU, RAM, GPU class, networking tier). A new instance
  type inherits the model.
- **Retail items / variants**, where each item has a category /
  attribute vector and the option set genuinely rotates.
- **Anything with a meaningful "kind"** where the kind axis is
  low-dimensional and the option set is non-stationary.

## When NOT to use it

- **Arbitrary or low-information option features.** If you have to
  invent the features to make the shape compile, the shared state
  has nothing to learn from and per-option LinUCB is a better fit.
- **Tiny, fixed option set (≤ 5 options).** The cold-start saving
  is negligible. Per-option LinUCB or Thompson-on-Bernoulli is
  simpler and identically effective.
- **Very large option-feature dimension.** Compute scales with
  `(d_context + d_option)²` for storage and `(d_context +
  d_option)²` per Sherman-Morrison update. Practical for
  `d_total` ≤ ~64; beyond that, consider an external embedding /
  approximate inverse, or step back to per-option state.
- **Truly non-linear reward surfaces.** Shared-state LinUCB fits
  the **best linear approximation** in `[x_context, x_option]`.
  If your reward function is bilinear (e.g. `ctx · x_opt[0]`),
  feature-engineer cross-terms at the capsule layer. The worked
  example below shows what that looks like in a `.lycs` program.

## Worked example: feature-engineering cross-terms

Suppose your operational truth is bilinear: at low `workload`, option
features along axis 0 matter; at high `workload`, axis 1 matters.
Concretely, the true reward function is:

```text
r = w0 · workload + w1 · (1 - workload) · x_opt[0]
            + w2 · workload · x_opt[1]
```

Without cross-terms, shared-state LinUCB cannot represent the
interaction. With cross-terms, it can — you just have to emit them.

The fix is to **expand the per-option feature vector at the capsule
layer**: instead of declaring `option_features` with raw axis values,
the capsule's caller (or the capsule's `.lycs` program) emits the
cross-term-augmented vector at decide time. The shared θ then learns
the linear coefficients on those augmented features.

Concretely, in the `.lycs` program:

```lisp
;; Raw context: scalar workload from the request body.
($ ctx (!cap "runtime.inputGet" "features.workload"))

;; Per-option base features (declared once, fixed per option).
;; In a real capsule these would come from option_features in
;; learning.json. Here we show one option inline for the example.
($ x0 0.5)
($ x1 0.7)

;; Cross-terms. The caller posts these as features in the request
;; body so they enter shared-state LinUCB's context vector x.
;;   ctx_x0 = workload * x0
;;   ctx_x1 = workload * x1
($ ctx_x0 (* ctx x0))
($ ctx_x1 (* ctx x1))

;; The capsule's `(choice ...)` strategy node then runs over the
;; usual labelled options, but the bandit's feature vector now
;; carries the four-dimensional [workload, ctx_x0, ctx_x1, bias]
;; — which is enough for shared-state LinUCB to fit the bilinear
;; truth.
```

And in the capsule's `learning.json`, declare the **augmented**
feature schema explicitly:

```json
{
  "contextSpec": {
    "type": "features",
    "features": [
      {"name": "workload", "type": {"kind": "continuous", "range": [0.0, 1.0]}},
      {"name": "ctx_x0",   "type": {"kind": "continuous", "range": [0.0, 1.0]}},
      {"name": "ctx_x1",   "type": {"kind": "continuous", "range": [0.0, 1.0]}}
    ]
  },
  "sharedState": {
    "enabled": true,
    "dContext": 3,
    "dOption": 2,
    ...
  }
}
```

Then the request body posts the cross-terms alongside the raw context:

```json
{
  "features": {
    "workload": 0.7,
    "ctx_x0":   0.42,
    "ctx_x1":   0.49
  }
}
```

Shared-state LinUCB now has a feature space rich enough to represent
the bilinear truth. The trade-off is that `d_context` (and therefore
the design matrix dimension) grew by two — that's the cost of
faithfully expressing the interaction.

Rule of thumb: for every product of context × option dimension you
want the bandit to learn, emit one cross-term in the context. The
posterior θ then carries one weight per cross-term, and the algorithm
recovers the multiplicative structure that pure-linear features
miss. The technique is standard linear-model practice (interaction
features in regression, polynomial feature expansion in
scikit-learn) — shared-state LinUCB is no different. The capsule
layer is just the natural place to compute the products, since the
program already has `ctx` and `x_opt` in hand.

## YAML schema

The capsule introduces one new top-level key, `option_features`, a
map from option name to a fixed-length numeric vector. Every vector
must have the same length, which becomes `d_option`.

```yaml
name: my-capsule
options: [A, B, C, D, E, F]
option_features:
  A: [0.1, 0.1]
  B: [0.1, 0.9]
  C: [0.9, 0.1]
  D: [0.9, 0.9]
  E: [0.5, 0.5]
  F: [0.3, 0.7]
contexts: [workload]
reward: { type: continuous, range: [-1, 1] }
```

And `learning.json` adds a `sharedState` block alongside the
existing `contextSpec`:

```json
{
  "contextSpec": {
    "type": "features",
    "features": [
      {"name": "workload", "type": {"kind": "continuous", "range": [0.0, 1.0]}}
    ]
  },
  "sharedState": {
    "enabled": true,
    "dContext": 1,
    "dOption": 2,
    "lambda": 1.0,
    "scoreKind": "ucb",
    "alpha": 1.0,
    "optionFeatures": {
      "A": [0.1, 0.1],
      "B": [0.1, 0.9],
      "C": [0.9, 0.1],
      "D": [0.9, 0.9],
      "E": [0.5, 0.5],
      "F": [0.3, 0.7]
    }
  }
}
```

`scoreKind` is `"ucb"` (deterministic LinUCB) or `"lin_ts"` (linear
Thompson sampling). `alpha` is the LinUCB exploration coefficient
or, for LinTS, the `v` scale in `N(μ, v²·A⁻¹)`. `lambda` is the
initial regularisation; `1.0` is the safe default.

## The test capsule's generalisation result

The unit test
`shared_state_strategy::tests::shared_state_strategy_generalises_to_unseen_options`
in `Lycan/src/shared_state_strategy.rs` runs the following:

1. Register options A=[0.1, 0.1], B=[0.1, 0.9], C=[0.9, 0.1],
   D=[0.9, 0.9]. Skip E and F for now.
2. Simulate 300 decide/feedback rounds where context `ctx ~ U(0, 1)`,
   action selected by UCB at α=1.0, and true reward is
   `r = 0.10·ctx + 0.40·x_opt[0] + 0.60·x_opt[1] + ε`, with
   `ε ~ U(-0.025, 0.025)`.
3. Register E=[0.5, 0.5] and F=[0.3, 0.7]. **No observations are
   ever applied at E or F.**
4. Query the posterior mean for E and F at three contexts (0.0,
   0.5, 1.0) and compare against the true expected reward.

Observed result:

| option | ctx  | posterior_mean | true_expected | abs_diff |
|--------|------|---------------:|--------------:|---------:|
| E      | 0.00 | 0.4973         | 0.5000        | 0.0027   |
| E      | 0.50 | 0.5495         | 0.5500        | 0.0005   |
| E      | 1.00 | 0.6017         | 0.6000        | 0.0017   |
| F      | 0.00 | 0.5182         | 0.5400        | 0.0218   |
| F      | 0.50 | 0.5704         | 0.5900        | 0.0196   |
| F      | 1.00 | 0.6225         | 0.6400        | 0.0175   |

Max relative error across the six unseen probes is 4.0%. The
comparison baseline (per-option LinUCB at E or F with zero
observations) would return 0 for every probe — that is the
cold-start tax this feature exists to eliminate.

## Numerical stability

The wrapper inherits all guards from
`Lycan/src/linucb.rs` `LinUcbSharedState`:

- UCB exploration bonus is clamped at `10·α` (defends against
  collinear features / numerical drift).
- A non-finite score falls back to the posterior mean.
- LinTS Cholesky-failure falls back to posterior mean too.
- Sherman-Morrison `denom` is clamped to `≥ 1e-12`.
- A full Gauss-Jordan rebuild of `A⁻¹` is triggered every 1000
  updates to clear accumulated drift.

The module header in `Lycan/src/linucb.rs` is the canonical reference
for the math; do not re-derive it here. The only new guard at the
wrapper layer is **dimension consistency at registration time**:
`register_option` rejects any feature vector whose length doesn't
match `d_option`, and `validate()` cross-checks that
`d_context + d_option == shared.d_total`.

## Cross-links

- Test capsule: [`Syntra/examples/shared-state-action-embeddings/`](../../examples/shared-state-action-embeddings/)
- Wrapper module: `Lycan/src/shared_state_strategy.rs`
- Underlying math: `Lycan/src/linucb.rs` (`LinUcbSharedState`)
- Repository positioning: [`Syntra/POSITIONING.md`](../../POSITIONING.md)
- Concept of decisions inside a capsule:
  [`docs/concepts/operational-intelligence.md`](../concepts/operational-intelligence.md)
