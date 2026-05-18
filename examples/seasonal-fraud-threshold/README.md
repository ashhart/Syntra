# seasonal-fraud-threshold

A Syntra capsule that turns a recent fraud-rate series into a threshold
policy choice.

The capsule's Lycan program computes a mean, a 95th-percentile and a
lightly smoothed EWMA forecast over the fraud-rate history the caller
POSTs, then runs a strategy node over four threshold-adjustment policies
(`loose`, `baseline`, `tight`, `very_tight`). The reward signal arrives
days later — chargebacks resolve on a multi-day window, disputes on
longer ones — and is posted to `/feedback` once it does. That delayed
outcome is the only learning signal the capsule needs, and it is exactly
the shape of feedback Syntra is built for.

This is one of three demos that show the *operational kernels* Lycan
ships — `series.ewmaForecast`, `stats.percentile`, `stats.mean` —
feeding directly into the adaptive choice that Syntra exposes over HTTP.
See `Syntra/POSITIONING.md` for the broader framing.

## Files

| File           | Purpose                                              |
|----------------|------------------------------------------------------|
| `capsule.yaml` | Bandit-side manifest: options, reward shape          |
| `program.lycs` | The Lycan program: kernels + strategy node           |
| `learning.json`| `contextSpec` + `refusal` for `PUT /learning`        |
| `README.md`    | This file                                            |

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
caller's application — the caller looks up `loose → 0.85`,
`baseline → 0.70`, `tight → 0.55`, `very_tight → 0.40` and applies it
to whatever fraud-scoring system produces the per-transaction score.
All three computed features (`fraud_mean`, `fraud_p95`, `fraud_forecast`)
are logged at decision time so they are visible in `lycan inspect`
output and the `decision.jsonl` log.

## Install

```bash
# 1. Compile the .lycs to a graph binary
lycan compile program.lycs

# 2. Install into Syntra
curl -X POST "$SYNTRA/tenants/risk/jobs/threshold/capsules/fraud/install" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     --data-binary @program.lyc

# 3. Attach the learning config (feature-context + refusal)
curl -X PUT "$SYNTRA/tenants/risk/jobs/threshold/capsules/fraud/learning" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @learning.json
```

## Decide

The caller supplies the recent fraud-rate history (used by the program)
and the feature context (used by the bandit):

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
    {
      "node_id": 42,
      "chosen_option": 2,
      "confidence": 0.29,
      "weights": [0.15, 0.28, 0.29, 0.29]
    }
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

`chosen_option` is the **zero-based index** into the strategy node's
options as they appear in `program.lycs`:

| Index | Policy        | Threshold |
|-------|---------------|-----------|
| 0     | `loose`       | 0.85      |
| 1     | `baseline`    | 0.70      |
| 2     | `tight`       | 0.55      |
| 3     | `very_tight`  | 0.40      |

The caller maps the index to the numeric threshold and applies it to
its fraud-scoring layer for the next window. The program's computed
`fraud_mean` / `fraud_p95` / `fraud_forecast` values appear verbatim in
the `stdout` array of the response.

## Feedback

When the chargeback window for the affected transactions has resolved
— typically several days later, sometimes longer for disputed cases —
post the components form to `/feedback`. `caught_fraud` is the share of
true fraud the chosen threshold caught (0..1); `false_positive_cost` is
the cost incurred by declining legitimate transactions, normalized
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

The capsule's reward shape
(`caught_fraud * 0.6 - false_positive_cost * 0.4`, both normalized as
specified in `capsule.yaml`) lives in `capsule.yaml` and Syntra applies
it. `decisionId` keeps the late feedback bound to the original decision
no matter how much time has passed.

## What to expect

- **Warmup (~30 feedback rounds)** uses uniform-random selection — every
  policy is picked with probability `1/n` (so `0.25` each for these four)
  and the weights shown in `/decide` responses stay at the uniform prior.
  Warmup is collecting reward samples to characterize the reward shape
  so the meta-bandit can pick a starting algorithm.
- **After warmup** the meta-bandit transitions to Active, picks an
  initial algorithm from the characterization (UCB(c=2.0) for a
  bounded-continuous reward like this capsule's), and runs all seven
  candidates — Thompson, UCB1, EpsilonGreedy, Weighted, Greedy, LinUCB,
  LinTS — in parallel under meta-bandit selection.
- **Convergence on a clear winner takes another ~30–50 rounds** after
  warmup when one policy dominates reward — i.e., ~60–80 rounds total
  from a cold install. In a sibling capsule's controlled 100-round test
  (same selection stack), the winning option received reward 1.0 and the
  others 0.1; the winner was picked ~22/25 times by rounds 76–100 and
  62/100 overall, with its weight climbing from `0.25` to `0.81`. The
  remaining ~40% are the meta-bandit's other candidates exploring — the
  `min_exploration` floor keeps the bandit from fully locking in.
- **Wall-clock convergence is slow for this capsule specifically**
  because the reward signal is the chargeback / dispute outcome,
  which resolves on a multi-day window. 30–50 feedback rounds is
  measured in *rounds*, not minutes — for a live fraud loop those
  rounds may take days or weeks to accumulate. Plan for weeks of
  operation before the meta-bandit has converged on which algorithm
  is best on your traffic.
- The **LinUCB** candidate uses the feature-context (`hour`,
  `is_weekend`, `current_volume`) so it can learn that, e.g.,
  `very_tight` wins on weekend nights at high volume while `loose` wins
  on midweek afternoons.
- **ADWIN drift detection** will re-warm the capsule if your fraud
  profile shifts (new attack pattern, payment-provider change,
  geographic expansion) — selection returns to uniform-random while
  the new reward shape is characterized.
- The live weights, warmup counter, and meta-bandit candidate trials /
  cumulative-reward state are visible via `GET /memory` and in
  `memory.json` / `warmup.json` on disk. The `/report` endpoint does
  not currently surface meta-bandit or warmup state — that is a known
  presentation gap, not a runtime issue. Inspect `/memory` while
  debugging convergence.

## What this isn't

- Not a fraud-detection model. The capsule does not score transactions.
  It picks a *threshold policy* the caller's existing fraud-scoring
  system applies. Bring your own scorer.
- Not a chargeback-prediction system. `series.ewmaForecast` is one-step
  EWMA over the fraud-rate series you posted. It does not predict
  individual chargebacks; it is a feature the capsule uses to inform
  the policy choice.
- Not a replacement for a rules engine or supervised fraud model. It is
  an adaptive layer that learns *which preset threshold policy* works
  best under which seasonal / volume context — it is not the system of
  record for fraud decisions.

## Related

- `Syntra/POSITIONING.md` — the operational positioning this demo is part of
- `Syntra/examples/predictive-autoscaling/` — sister demo using EWMA +
  `ops.autoScaleRecommend` to size instance counts
- `Syntra/examples/anomaly-routing/` — sister demo using `stats.mean` +
  `stats.stdDev` for 3σ anomaly-aware routing
- `Syntra/sidecar/` — sidecar that scrapes Prometheus / Datadog / SQL /
  file sources and exposes `/features/current` for capsules like this one
