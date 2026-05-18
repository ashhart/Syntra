# Domain packs

End-to-end worked capsules. Each one ships in the Syntra repository at
`examples/{name}/` with one `capsule.yaml`, one `program.lycs`, one
`learning.json`, a manifest, and a README. The pages below adapt those
READMEs for the docs site.

There are three flavors of pack:

## Operational-intelligence demos

These three exercise the *kernel-feature-derivation-to-strategy-node*
pattern: the capsule's Lycan program reads a window the caller posts,
derives features from it inside the graph (`stats.*`, `series.ewmaForecast`,
`ops.autoScaleRecommend`), and routes that into the strategy node.
Start here.

- [Predictive autoscaling](predictive-autoscaling.md) — EWMA forecast +
  autoscale-recommend driving a four-policy scaling choice.
- [Anomaly-aware routing](anomaly-routing.md) — mean / stddev / z-score
  driving a four-policy routing choice.
- [Seasonal fraud threshold](seasonal-fraud-threshold.md) — EWMA on a
  fraud-rate series driving a four-policy threshold choice. Reward
  delayed by days.

## Integration packs

Python libraries that consume Syntra over HTTP. The canonical pattern
is `RetryClient` — drop-in for `requests`; everything else mirrors its
shape.

- [HTTP retry tuning](retry-tuning.md) — the canonical Python
  integration. Drop-in for `requests`. Five retry policies.
- [LLM model routing](llm-routing.md) — `cheap_fast` / `balanced` /
  `expensive_accurate` per request. Quality / latency / cost reward.
- [Fraud threshold tuning](fraud-tuning.md) — pick a block threshold
  per merchant per request.
- [Queue selection](queue-selection.md) — pick a backend queue per
  request from a list of K.
- [Language clients](language-clients.md) — Go, Node, Java, Rust
  clients that mirror the Python pattern.

## Advanced bandit flavors

Capsules that exercise the alternative adaptive flavors — shared-state
and hierarchical — beyond the default per-option meta-bandit.

- [Hierarchical region routing](hierarchical-region-routing.md) — 2×3
  region × server-type tree. Reward propagates along the path.
- [Shared-state action embeddings](shared-state-action-embeddings.md)
  — shared θ over `[context, option_features]`; new options inherit
  non-zero priors.

## Other examples in the repository

A few more demos live in `examples/` that are not covered as individual
pages:

- `examples/demo-llm-model-routing.sh` — three model routes, two
  contexts, persistence across restart.
- `examples/demo-static-policy-vs-syntra.sh` — focused
  static-vs-adaptive proof.
- `examples/offline-eval/` — IPS and doubly-robust off-policy
  estimators.
- `examples/ab-harness/` — A/B simulation harness.
- `examples/lycan-internals/` — substrate-level demos exercising
  Lycan kernels directly (autoscaler, capability pack, webhook load).
