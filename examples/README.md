# Examples

These examples target the current (v1) server API and are being rebuilt
around the v2 decision core.

- [`llm-routing/`](llm-routing/), [`demo-llm-model-routing.sh`](demo-llm-model-routing.sh)
  and [`demo-governed-llm-routing.sh`](demo-governed-llm-routing.sh): choose a
  model route per request and learn from delayed quality, latency and cost.
- [`anomaly-routing/`](anomaly-routing/), [`predictive-autoscaling/`](predictive-autoscaling/),
  [`seasonal-fraud-threshold/`](seasonal-fraud-threshold/), [`retry-tuning/`](retry-tuning/),
  [`queue-selection/`](queue-selection/), [`fraud-tuning/`](fraud-tuning/): capsules
  that compute a signal before choosing an action.
- [`replay/`](replay/), [`offline-eval/`](offline-eval/), [`ab-harness/`](ab-harness/):
  v1 evaluation tooling. See [CONTEXT.md](../CONTEXT.md) for its limits.
- [`lycan/`](lycan/): small Lycan language programs (hello, fibonacci,
  fizzbuzz, calculator, pipeline, runtime input, capability pack).

The science demos, proof lab and self-evolution examples moved to the
separate Lycan Lab repository.
