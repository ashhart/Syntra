# Anomaly-aware routing

A Syntra capsule that turns a recent latency history into a routing
decision.

The capsule's Lycan program computes `stats.mean` and `stats.stdDev`
over the latency window the caller POSTs, derives a z-score for the
most recent latency sample, and runs a strategy node over four routing
policies. Syntra learns from `/feedback` which policy is the right
choice under which context.

This is one of three demos that show the *operational kernels* Lycan
ships — `stats.mean`, `stats.stdDev` — feeding directly into the
adaptive choice Syntra exposes over HTTP. The repository copy lives
at [`examples/anomaly-routing/`](https://github.com/ashhart/Syntra/tree/main/examples/anomaly-routing).

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
    runtime.inputGet latency_history / current_latency
    |
    stats.mean / stats.stdDev over the window
    |
    z_score = (current_latency - mean) / stddev   (0 on flat window)
    |
    strategy node picks one:
        primary | secondary | degraded_cache_only | circuit_break
    |
    chosen route label
```

The z-score and the per-window mean/stddev are computed every decide
so they are visible in `lycan inspect` output and the `decision.jsonl`
log.

## Install

```bash
# 1. Compile the .lycs to a graph binary
lycan compile program.lycs

# 2. Install into Syntra
curl -X POST "$SYNTRA/tenants/edge/jobs/route/capsules/router/install" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     --data-binary @program.lyc

# 3. Attach the learning config (feature-context + refusal)
curl -X PUT "$SYNTRA/tenants/edge/jobs/route/capsules/router/learning" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     --data-binary @learning.json
```

## Decide

The caller supplies the recent latency history (used by the program)
and the feature context (used by the bandit). `z_score` in `features`
should be the same value the program would compute — pre-derive it on
the caller side, or post a placeholder and rely on the program's
logged value for offline inspection.

```bash
curl -X POST "$SYNTRA/tenants/edge/jobs/route/capsules/router/decide" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -H "Content-Type: application/json" \
     -d '{
       "latency_history": [120, 135, 128, 142, 130, 138, 125, 140, 132, 412],
       "current_latency": 412,
       "features": {
         "z_score":         2.74,
         "hour":            14.0,
         "current_latency": 412.0
       }
     }'
```

Response (actual shape, captured from an `e2e dev-mode` run):

```json
{
  "ok": true,
  "decisionId": "dec_8a3c1f...",
  "decisions": [
    {
      "node_id": 47,
      "chosen_option": 3,
      "confidence": 0.30,
      "weights": [0.15, 0.27, 0.28, 0.30]
    }
  ],
  "stdout": [
    "lat_mean: 155.5",
    "lat_stddev: 85.89",
    "z_score: 2.99",
    "decision: route via circuit_break"
  ],
  "refused": false
}
```

`chosen_option` is the **zero-based index** into the strategy node's
options:

| Index | Route                   |
|-------|-------------------------|
| 0     | `primary`               |
| 1     | `secondary`             |
| 2     | `degraded_cache_only`   |
| 3     | `circuit_break`         |

The caller maps the index to a routing decision and applies it in its
own request layer.

## Feedback

When you observe the outcome (request succeeded or failed, observed
tail latency over the next N requests), post the components form to
`/feedback`:

```bash
curl -X POST "$SYNTRA/tenants/edge/jobs/route/capsules/router/feedback" \
     -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
     -d '{
       "decisionId": "dec_9f1b2e...",
       "rewardComponents": {
         "success_rate":         1.0,
         "tail_latency_penalty": 180.0
       }
     }'
```

The capsule's reward shape (`success_rate * 0.7 -
tail_latency_penalty * 0.3`, with `tail_latency_penalty` normalized
by a 2000 ms budget) lives in `capsule.yaml` and Syntra applies it.

## What to expect

- **Warmup (~30 feedback rounds)** uses uniform-random selection.
- **After warmup** the meta-bandit transitions to Active and runs all
  seven candidates in parallel under meta-bandit selection.
- **Convergence on a clear winner takes another ~30–50 rounds.**
- The **LinUCB** candidate uses the feature-context (`z_score`,
  `hour`, `current_latency`) so it can learn that, e.g.,
  `circuit_break` wins when `z_score > 3` and `primary` wins when
  `z_score` is near zero.
- **ADWIN drift detection** will re-warm the capsule if your traffic
  profile shifts (deploy, migration, new region).

## What this isn't

- **Not a circuit breaker.** It picks a *policy label*. Your routing
  layer still has to apply that label.
- **Not an anomaly detection product.** `stats.mean` and
  `stats.stdDev` over a caller-supplied window give you a 3σ-style
  signal; that signal is one feature the bandit conditions on, not
  a calibrated detector.
- **Not a replacement for a service mesh / proxy.** It's an adaptive
  layer that learns *which routing policy* works under which
  latency-shape context — it does not move bytes.

## Related

- [Predictive autoscaling](predictive-autoscaling.md) — sister demo
  using EWMA forecast + percentile to pick a scaling policy.
- [Seasonal fraud threshold](seasonal-fraud-threshold.md) — sister
  demo using EWMA on a fraud-rate series to drive threshold
  adjustment.
- [Kernel concept](../concepts/kernel.md) — the 26 building blocks
  this capsule's program is composed from.
