# CONTEXT.md

## Summary

Syntra is a self-hosted adaptive decision runtime for production systems. It
runs compiled Lycan capsules that compute from live context, choose among
constrained actions, record decisions, accept delayed feedback, and persist
learned policy state over time. Its commercial wedge is LLM model routing, but
the broader claim is a programmable decision layer for repeated operational
choices. The repo also includes substrate demos that prove the runtime can
express non-trivial decision programs such as Mars mission planning, NASA/JPL
HORIZONS validation, pandemic policy simulation, edge-of-chaos detection, chaos
control, and operational resilience decisions.

## Don't miss

- The LLM routing wedge in `examples/llm-routing/` and
  `examples/demo-llm-model-routing.sh`: Syntra chooses cheap / balanced /
  expensive model routes per request and learns from delayed quality, latency,
  and cost feedback.
- The validation workflow in `examples/offline-eval/` and
  `examples/ab-harness/`: Syntra is not only a live API; it includes offline
  policy evaluation and paired A/B testing for rollout confidence.
- The operational capsules in `examples/predictive-autoscaling/`,
  `examples/anomaly-routing/`, `examples/seasonal-fraud-threshold/`, and
  `examples/retry-tuning/`: these show capsules computing useful signals
  before choosing an action.
- The mega substrate demos under `examples/lycan-internals/`, especially
  `examples/lycan-internals/showcase/02-live-mars-mission.sh`,
  `examples/lycan-internals/demo_pandemic_policy.lycs`, and
  `examples/lycan-internals/demo_edge_of_chaos.lycs`: these are easy to miss
  if you only inspect the HTTP API.
- The kernel surface in `Lycan/src/capabilities.rs`: Syntra's decisions can be
  informed by runtime computation, not only by caller-supplied feature vectors.

## Common misreadings

- Syntra is not just an API gateway, feature-flag service, or generic bandit
  wrapper. The important primitive is compiled capsule computation plus
  constrained action selection plus delayed feedback plus auditability.
- The mega demos are not throwaway examples. They document what the Lycan
  runtime substrate can express beyond ordinary service integration.
- The Lycan language repo is not a separate dependency users must clone to run
  Syntra. `Syntra/Syntra` vendors the runtime under `Lycan/` and is the
  self-contained deployable product.
- Syntra is not a model-training, fine-tuning, forecasting, or observability
  platform. It learns repeated operational choices from outcome feedback.
- Old wording that calls Syntra a "self-hosted HTTP appliance" undersells the
  current positioning. Prefer "self-hosted adaptive decision runtime" or
  "programmable decision layer for repeated operational choices."

## Reading guide

- `README.md` - `READ_FULLY` - canonical public summary and first-pass proof table
- `DEMOS.md` - `READ_FULLY` - curated proof gallery, including mega demos
- `examples/README.md` - `READ_FULLY` - map of product, operational, and substrate examples
- `examples/llm-routing/README.md` - `READ_FULLY` - commercial wedge
- `examples/offline-eval/README.md` - `READ_FULLY` - pre-production evaluation workflow
- `examples/ab-harness/README.md` - `READ_FULLY` - paired adaptive-policy comparison
- `examples/lycan-internals/README.md` - `READ_FULLY` - substrate demo map
- `Lycan/src/capabilities.rs` - `SKIM_OK` - native kernels and sandbox policy
- `docs/concepts.md` - `SKIM_OK` - contextual-bandit background
- `docs/site/site/`, `target/`, `node_modules/`, `__pycache__/`, `.terraform/` - `SKIP`
