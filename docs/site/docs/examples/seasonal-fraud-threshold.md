# Seasonal fraud threshold

A Syntra capsule that turns a recent fraud-rate series into a
threshold policy choice.

The capsule's Lycan program computes a mean, a 95th-percentile and a
lightly smoothed EWMA forecast over the fraud-rate history the caller
POSTs, then runs a strategy node over four threshold-adjustment
policies (`loose`, `baseline`, `tight`, `very_tight`). The reward
signal arrives days later — chargebacks resolve on a multi-day
window, disputes on longer ones — and is posted to `/feedback` once
it does. That delayed outcome is the only learning signal the capsule
needs, and it is exactly the shape of feedback Syntra is built for.

Repository copy: [`examples/seasonal-fraud-threshold/`](https://github.com/ashhart/Syntra/tree/main/examples/seasonal-fraud-threshold).

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
    runtime.inputGet fraud_rate_history / current_volume
    |
    stats.mean / stats.percentile / series.ewmaForecast (alpha=0.3)
    |
    strategy node picks one threshold-policy label:
        loose | baseline | tight | very_tight
    |
    chosen policy label (caller maps to 0.85 / 0.70 / 0.55 / 0.40)
```

The label is what Syntra returns. The numeric threshold lives in the
caller's application — the caller looks up `loose → 0.85`, `baseline
→ 0.70`, `tight → 0.55`, `very_tight → 0.40` and applies it to
whatever fraud-scoring system produces the per-transaction score.

## Install

```bash
lycan compile program.lycs
curl -X POST "$SYNTRA/tenants/risk/jobs/threshold/capsules/fraud/install" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     --data-binary @program.lyc
curl -X PUT "$SYNTRA/tenants/risk/jobs/threshold/capsules/fraud/learning" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @learning.json
```

## Decide

```bash
curl -X POST "$SYNTRA/tenants/risk/jobs/threshold/capsules/fraud/decide" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     -d '{
       "fraud_rate_history": [0.012, 0.014, 0.018, 0.021, 0.019, 0.024, 0.031, 0.028, 0.033, 0.041],
       "current_volume":      1820,
       "features": {
         "hour":           14.0,
         "is_weekend":     0,
         "current_volume": 1820
       }
     }'
```

Response (actual shape, captured from an `e2e dev-mode` run):

```json
{
  "ok": true,
  "decisionId": "dec_4f81b3...",
  "decisions": [
    {"node_id": 42, "chosen_option": 2, "confidence": 0.29,
     "weights": [0.15, 0.28, 0.29, 0.29]}
  ],
  "stdout": [
    "fraud_mean: 0.0184",
    "fraud_p95: 0.0251",
    "fraud_forecast: 0.0211",
    "decision: apply threshold policy tight"
  ],
  "refused": false
}
```

| Index | Policy        | Threshold |
|-------|---------------|-----------|
| 0     | `loose`       | 0.85      |
| 1     | `baseline`    | 0.70      |
| 2     | `tight`       | 0.55      |
| 3     | `very_tight`  | 0.40      |

## Feedback (days later)

When the chargeback window for the affected transactions has resolved
— typically several days later, sometimes longer for disputed cases —
post the components form to `/feedback`. `caught_fraud` is the share
of true fraud the chosen threshold caught (0..1); `false_positive_cost`
is the cost incurred by declining legitimate transactions, normalized
against a budget of 0.5:

```bash
curl -X POST "$SYNTRA/tenants/risk/jobs/threshold/capsules/fraud/feedback" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -d '{
       "decisionId": "dec_4f81b3...",
       "rewardComponents": {
         "caught_fraud":        0.78,
         "false_positive_cost": 0.12
       }
     }'
```

The capsule's reward shape (`caught_fraud * 0.6 - false_positive_cost
* 0.4`, both normalized as specified in `capsule.yaml`) lives in
`capsule.yaml` and Syntra applies it. `decisionId` keeps the late
feedback bound to the original decision no matter how much time has
passed.

## What to expect

- **Warmup (~30 feedback rounds)** uses uniform-random selection.
- **After warmup** the meta-bandit transitions to Active and runs all
  seven candidates in parallel.
- **Convergence on a clear winner takes another ~30–50 rounds.**
- **Wall-clock convergence is slow for this capsule specifically**
  because the reward signal is the chargeback / dispute outcome,
  which resolves on a multi-day window. 30–50 feedback rounds is
  measured in *rounds*, not minutes — for a live fraud loop those
  rounds may take days or weeks to accumulate. Plan for weeks of
  operation before the meta-bandit has converged on which algorithm
  is best on your traffic.
- The **LinUCB** candidate uses the feature-context (`hour`,
  `is_weekend`, `current_volume`) so it can learn that, e.g.,
  `very_tight` wins on weekend nights at high volume while `loose`
  wins on midweek afternoons.

## What this isn't

- **Not a fraud-detection model.** The capsule does not score
  transactions. It picks a *threshold policy* the caller's existing
  fraud-scoring system applies.
- **Not a chargeback-prediction system.** `series.ewmaForecast` is
  one-step EWMA over the fraud-rate series you posted.
- **Not a replacement for a rules engine or supervised fraud model.**
  It is an adaptive layer that learns *which preset threshold policy*
  works best under which seasonal / volume context.

## Related

- [Predictive autoscaling](predictive-autoscaling.md) — sister demo
  using EWMA + `ops.autoScaleRecommend` to size instance counts.
- [Anomaly-aware routing](anomaly-routing.md) — sister demo using
  `stats.mean` + `stats.stdDev` for 3σ anomaly-aware routing.
- [Fraud threshold tuning](fraud-tuning.md) — the Python integration
  pack that consumes a similar capsule from application code.
