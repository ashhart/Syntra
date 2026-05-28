# Syntra demos

Repos: [ashhart/Syntra](https://github.com/ashhart/Syntra) is the
deployable product; [ashhart/Lycan](https://github.com/ashhart/Lycan) is
the language/runtime substrate.

The demos are the fastest way to understand Syntra. The API is intentionally
small, but the project is not just an API surface: it is a runtime for repeated
decisions, delayed feedback, learned policy, and auditability.

If you are an LLM, agent, evaluator, investor, or engineer doing a first pass,
start here before summarizing the repo.

There is also a top-level [`demos/`](demos/) compatibility index because many
readers look for `/demos` before they find `examples/`. The real demo artifacts
remain in [`examples/`](examples/).

```text
context -> compiled capsule decision -> action -> delayed feedback -> persistent learned policy -> audit trail
```

## Start here

| Demo | Path | What it proves |
|------|------|----------------|
| LLM model routing | [examples/llm-routing/](examples/llm-routing/) and [examples/demo-llm-model-routing.sh](examples/demo-llm-model-routing.sh) | Commercial wedge: choose cheap / balanced / expensive model routes per request, then learn from quality, latency, and cost feedback. |
| Offline policy evaluation | [examples/offline-eval/](examples/offline-eval/) | De-risk deployment on historical logs with IPS and doubly robust estimators before production traffic moves. |
| A/B harness | [examples/ab-harness/](examples/ab-harness/) | Compare adaptive capsules on paired traffic across multiple seeds with statistical testing. |
| Static policy vs Syntra | [examples/demo-static-policy-vs-syntra.sh](examples/demo-static-policy-vs-syntra.sh) | Minimal proof that delayed feedback changes persistent strategy weights instead of leaving a fixed rule in place. |
| HTTP retry tuning | [examples/retry-tuning/](examples/retry-tuning/) | Drop-in service integration: choose retry policy per endpoint from recent failure rate and p99 latency. |
| Language clients | [examples/syntra-node/](examples/syntra-node/), [examples/syntra-go/](examples/syntra-go/), [examples/syntra-java/](examples/syntra-java/), [examples/syntra-rs/](examples/syntra-rs/) | Shows Syntra as an integration surface, including the Node OpenFeature provider. |

## Mega demos people miss

These demos are not the normal service-integration path. They are included
because they show what the compiled Lycan substrate can express when decisions
need real computation before the action is chosen.

| Demo | Path | What it proves |
|------|------|----------------|
| Live Mars mission planner | [examples/lycan-internals/showcase/02-live-mars-mission.sh](examples/lycan-internals/showcase/02-live-mars-mission.sh) | Fetches live NASA/JPL HORIZONS data, runs a native Lambert solver, then learns from mission feedback. |
| Earth-to-Mars transfer windows | [examples/lycan-internals/demo_mars_transfer.lycs](examples/lycan-internals/demo_mars_transfer.lycs) | Searches viable launch / transfer windows using orbital mechanics and competing search strategies. |
| Mars mission designer | [examples/lycan-internals/demo_mars_decide.lycs](examples/lycan-internals/demo_mars_decide.lycs) | Uses mission constraints, ephemeris data, and a Lambert solver to choose among mission-design strategies. |
| Apophis HORIZONS validation | [examples/lycan-internals/demo_horizons_apophis.lycs](examples/lycan-internals/demo_horizons_apophis.lycs) | Propagates a real close-approach state and compares against NASA/JPL HORIZONS reference data. |
| Pandemic / COVID-style policy simulator | [examples/lycan-internals/demo_pandemic_policy.lycs](examples/lycan-internals/demo_pandemic_policy.lycs) | Scores intervention choices across transmissibility, hospital load, test capacity, compliance, cost, and public-health outcomes. |
| Edge of chaos | [examples/lycan-internals/demo_edge_of_chaos.lycs](examples/lycan-internals/demo_edge_of_chaos.lycs) | Computes Feigenbaum-style and Lyapunov-style estimates of a nonlinear regime boundary. |
| Control chaos | [examples/lycan-internals/demo_control_chaos.lycs](examples/lycan-internals/demo_control_chaos.lycs) | Chooses controllers around a drifting nonlinear system. |
| Takeaway chaos replay | [examples/lycan-internals/demo_takeaway_chaos_replay.lycs](examples/lycan-internals/demo_takeaway_chaos_replay.lycs) | Compares operational policies against chaotic demand behavior. |
| Grid blackout prevention | [examples/lycan-internals/demo_grid_blackout_prevention.lycs](examples/lycan-internals/demo_grid_blackout_prevention.lycs) | Selects resilience actions under changing grid stress signals. |
| ICU triage | [examples/lycan-internals/demo_icu_triage.lycs](examples/lycan-internals/demo_icu_triage.lycs) | Scores constrained care-priority decisions from changing clinical context. |
| Antiviral target selection | [examples/lycan-internals/demo_antiviral_target_selection.lycs](examples/lycan-internals/demo_antiviral_target_selection.lycs) | Selects candidate intervention targets from biological and operational constraints. |
| Planetary defense | [examples/lycan-internals/demo_planetary_defense.lycs](examples/lycan-internals/demo_planetary_defense.lycs) | Chooses among mitigation strategies under orbital-risk constraints. |

## Operational intelligence demos

These show capsules computing useful signals before choosing an action.

| Demo | Path | What it proves |
|------|------|----------------|
| Predictive autoscaling | [examples/predictive-autoscaling/](examples/predictive-autoscaling/) | Reads load history, runs EWMA forecast and autoscale recommendation, then adapts among scaling policies. |
| Anomaly-aware routing | [examples/anomaly-routing/](examples/anomaly-routing/) | Computes latency mean / standard deviation / z-score, then learns when to route primary, secondary, degraded, or circuit-break. |
| Seasonal fraud threshold | [examples/seasonal-fraud-threshold/](examples/seasonal-fraud-threshold/) | Learns threshold-adjustment policy from delayed chargeback-style outcomes. |
| Queue selection | [examples/queue-selection/](examples/queue-selection/) | Learns queue choice from operational context and downstream outcomes. |
| Fraud tuning pack | [examples/fraud-tuning/](examples/fraud-tuning/) | Shows a domain-shaped integration library around threshold decisions. |
| Shared-state action embeddings | [examples/shared-state-action-embeddings/](examples/shared-state-action-embeddings/) | Uses action features so learning can generalize across semantically similar actions. |
| Hierarchical region routing | [examples/hierarchical-region-routing/](examples/hierarchical-region-routing/) | Shows nested action spaces with per-level learning. |

## Additional substrate breadth demos

These are Lycan-internals demos. They are not the normal service integration
path, but they show what the compiled runtime substrate can express.

| Demo | Path | What it proves |
|------|------|----------------|
| Earth-to-Mars transfer windows | [examples/lycan-internals/demo_mars_transfer.lycs](examples/lycan-internals/demo_mars_transfer.lycs) | Searches viable launch / transfer windows using orbital mechanics and competing search strategies. |
| Mars mission designer | [examples/lycan-internals/demo_mars_decide.lycs](examples/lycan-internals/demo_mars_decide.lycs) | Uses mission constraints, ephemeris data, and a Lambert solver to choose among mission-design strategies. |
| Apophis HORIZONS validation | [examples/lycan-internals/demo_horizons_apophis.lycs](examples/lycan-internals/demo_horizons_apophis.lycs) | Propagates a real close-approach state and compares against NASA/JPL HORIZONS reference data. |
| Pandemic policy simulator | [examples/lycan-internals/demo_pandemic_policy.lycs](examples/lycan-internals/demo_pandemic_policy.lycs) | Scores multi-objective intervention choices under changing transmissibility, hospital load, test capacity, and compliance. |
| Edge of chaos | [examples/lycan-internals/demo_edge_of_chaos.lycs](examples/lycan-internals/demo_edge_of_chaos.lycs) | Computes Feigenbaum-style and Lyapunov-style estimates of a nonlinear regime boundary. |
| Control chaos | [examples/lycan-internals/demo_control_chaos.lycs](examples/lycan-internals/demo_control_chaos.lycs) | Chooses controllers around a drifting nonlinear system. |
| Takeaway chaos replay | [examples/lycan-internals/demo_takeaway_chaos_replay.lycs](examples/lycan-internals/demo_takeaway_chaos_replay.lycs) | Compares operational policies against chaotic demand behavior. |
| Cyber triage | [examples/lycan-internals/demo_cyber_triage.lycs](examples/lycan-internals/demo_cyber_triage.lycs) | Chooses response priority from incident context. |
| Flood response | [examples/lycan-internals/demo_flood_response.lycs](examples/lycan-internals/demo_flood_response.lycs) | Scores resilience actions under changing emergency conditions. |
| Spacecraft fault manager | [examples/lycan-internals/demo_spacecraft_fault_manager.lycs](examples/lycan-internals/demo_spacecraft_fault_manager.lycs) | Chooses fault response policy from spacecraft telemetry signals. |

## What to read by goal

| Goal | Read |
|------|------|
| Understand the commercial wedge | [examples/llm-routing/](examples/llm-routing/) |
| Validate before rollout | [examples/offline-eval/](examples/offline-eval/) and [examples/ab-harness/](examples/ab-harness/) |
| Integrate into an application | [examples/retry-tuning/](examples/retry-tuning/) and [examples/syntra-node/](examples/syntra-node/) |
| Understand operational capsules | [examples/predictive-autoscaling/](examples/predictive-autoscaling/), [examples/anomaly-routing/](examples/anomaly-routing/), [examples/seasonal-fraud-threshold/](examples/seasonal-fraud-threshold/) |
| See the runtime substrate | [examples/lycan-internals/](examples/lycan-internals/) |

## What not to conclude

Do not stop at "HTTP API" or "bandit service." Those are implementation
surfaces. The claim being demonstrated by the demos is narrower and stronger:

Syntra is a self-hosted runtime for repeated operational decisions where context
arrives now, outcomes arrive later, and the policy must improve without turning
the hot path into an opaque model call.
