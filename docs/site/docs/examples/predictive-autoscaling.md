# Predictive autoscaling

A Syntra capsule that turns a recent load history into a scaling
decision.

The capsule's Lycan program computes an EWMA forecast and a
95th-percentile load from the data the caller POSTs, derives four
candidate instance counts via `ops.autoScaleRecommend`, and runs a
strategy node over the four scaling policies. Syntra learns from
`/feedback` which policy is the right choice under which context.

This is one of three demos that show the *operational kernels* Lycan
ships — `series.ewmaForecast`, `stats.percentile`,
`ops.autoScaleRecommend` — feeding directly into the adaptive choice
Syntra exposes over HTTP. The repository copy lives at
[`examples/predictive-autoscaling/`](https://github.com/ashhart/Syntra/tree/main/examples/predictive-autoscaling).

## Files

| File           | Purpose                                              |
|----------------|------------------------------------------------------|
| `capsule.yaml` | Bandit-side manifest: options, reward shape          |
| `program.lycs` | The Lycan program: kernels + strategy node           |
| `learning.json`| `contextSpec` + `refusal` for `PUT /learning`        |
| `README.md`    | The repo-side README                                 |

## How the program is shaped

```
request body
    |
    runtime.inputGet load_history / current_instances / target_per_instance ...
    |
    stats.mean / stats.percentile / series.ewmaForecast (alpha=0.4)
    |
    ops.autoScaleRecommend for each of four candidate sizings
    |
    strategy node picks one:
        hold | forecast_match | forecast_headroom | p95_safe
    |
    chosen instance count
```

All four candidates are computed every decide so the per-option counts
are visible in `lycan inspect` output and the `decision.jsonl` log.

## Install

```bash
# 1. Compile the .lycs to a graph binary
lycan compile program.lycs

# 2. Install into Syntra
curl -X POST "$SYNTRA/tenants/ops/jobs/scale/capsules/autoscaler/install" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     --data-binary @program.lyc

# 3. Attach the learning config (feature-context + refusal)
curl -X PUT "$SYNTRA/tenants/ops/jobs/scale/capsules/autoscaler/learning" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @learning.json
```

## Decide

The caller supplies the recent load history (used by the program) and
the feature context (used by the bandit):

```bash
curl -X POST "$SYNTRA/tenants/ops/jobs/scale/capsules/autoscaler/decide" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     -d '{
       "load_history":         [82, 88, 95, 110, 132, 158, 174, 188, 201, 215],
       "current_instances":    3,
       "target_per_instance":  100,
       "min_instances":        1,
       "max_instances":        20,
       "features": {
         "hour":              14.0,
         "current_instances": 3,
         "load_trend":        0.6
       }
     }'
```

Response (actual shape, captured from an `e2e dev-mode` run):

```json
{
  "ok": true,
  "decisionId": "dec_571a25a7d52da32c",
  "decisions": [
    {
      "node_id": 71,
      "chosen_option": 2,
      "confidence": 0.34,
      "weights": [0.33, 0.05, 0.27, 0.34]
    }
  ],
  "stdout": [
    "load_forecast: 193.18",
    "policy_hold: 3",
    "policy_forecast_match: 2",
    "policy_forecast_headroom: 3",
    "policy_p95_safe: 3",
    "decision: scale to 3 instances"
  ],
  "warmup": { "collected": 0, "state": "warmup", "target": 30 },
  "refused": false
}
```

`chosen_option` is the **zero-based index** into the strategy node's
options as they appear in `program.lycs`:

| Index | Policy                |
|-------|-----------------------|
| 0     | `policy_hold`         |
| 1     | `policy_forecast_match` |
| 2     | `policy_forecast_headroom` |
| 3     | `policy_p95_safe`     |

The caller maps the index to a policy and applies it in its own
infrastructure code (invoke the cloud-provider scaling API with the
count the policy implies). The program's per-option computed counts
appear verbatim in the `stdout` array of the response.

## Feedback

When you observe the outcome (SLA met or breached, cost reasonable or
wasteful), post the components form to `/feedback`:

```bash
curl -X POST "$SYNTRA/tenants/ops/jobs/scale/capsules/autoscaler/feedback" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -d '{
       "decisionId": "dec_8c2a1f...",
       "rewardComponents": {
         "sla_met":         1.0,
         "cost_efficiency": 0.65
       }
     }'
```

The capsule's reward shape (`sla_met * 0.7 + cost_efficiency * 0.3`)
lives in `capsule.yaml` and Syntra applies it.

## What to expect

- **Warmup (~30 feedback rounds)** uses uniform-random selection —
  every option is picked with probability `1/n` (so `0.25` each for
  these four) and the weights shown in `/decide` responses stay at the
  uniform prior. Warmup is collecting reward samples to characterize
  the reward shape (e.g. `BoundedContinuous`) so the meta-bandit can
  pick a starting algorithm.
- **After warmup** the meta-bandit transitions to Active, picks an
  initial algorithm from the characterization (UCB(c=2.0) for a
  bounded-continuous reward like this capsule's), and runs all seven
  candidates — Thompson, UCB1, EpsilonGreedy, Weighted, Greedy,
  LinUCB, LinTS — in parallel under meta-bandit selection.
- **Convergence on a clear winner takes another ~30–50 rounds** after
  warmup when one option dominates reward — i.e., ~60–80 rounds from
  a cold install. In a controlled 100-round run where option 2
  received reward 1.0 and the other three received 0.1, option 2 was
  chosen 14/25 times in rounds 26–50, 20/25 in 51–75, and 22/25 in
  76–100 — 62/100 overall. Its weight climbed from `0.25` to `0.81`.
  The remaining ~40% of picks are the meta-bandit's other candidates
  exploring; the `min_exploration` floor keeps the bandit from fully
  locking in.
- The **LinUCB** candidate uses the feature-context (`hour`,
  `current_instances`, `load_trend`) so it can learn that, e.g.,
  `forecast_headroom` wins when `load_trend > 0` and `forecast_match`
  wins on flat traffic.
- **ADWIN drift detection** will re-warm the capsule if your traffic
  profile shifts (deploy, migration, new region) — selection returns
  to uniform-random while the new reward shape is characterized.
- The live weights, warmup counter, and meta-bandit candidate trials
  / cumulative-reward state are visible via `GET /memory` and in
  `memory.json` / `warmup.json` on disk. The `/report` endpoint does
  not currently surface meta-bandit or warmup state — a known
  presentation gap, not a runtime issue. Inspect `/memory` while
  debugging convergence.

## What this isn't

- **Not a real autoscaler.** It picks a *policy*. Your infrastructure
  layer still has to call AWS / GCP / Kubernetes to actually change
  capacity.
- **Not a forecaster.** `series.ewmaForecast` is one-step EWMA. The
  capsule uses it as a feature; production decisions should still be
  cross-checked against your existing alerting / capacity planning.
- **Not a replacement for cluster autoscaler / HPA.** It's an adaptive
  layer that learns *which scaling strategy* works for your workload
  — it is not a control loop.

## Related

- [Anomaly-aware routing](anomaly-routing.md) — sister demo using
  `stats.mean` + `stats.stdDev` for 3σ anomaly-aware routing.
- [Seasonal fraud threshold](seasonal-fraud-threshold.md) — sister
  demo using EWMA on a fraud-rate series to drive threshold
  adjustment.
- [Kernel concept](../concepts/kernel.md) — the 26 building blocks
  this capsule's program is composed from.
