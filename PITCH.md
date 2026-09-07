# Syntra

**A self-hosted appliance for adaptive operational decisions.**

One Docker container next to your service. Local filesystem store. No
GPU, no model training, no managed cloud dependency. If Syntra is down
or refuses, your service falls back to the default policy it already
has — no new failure mode is introduced. If Syntra is up, your service
gets a learned choice per request and a place to send the feedback
signal when the outcome resolves.

Already running in shadow mode in production against a public AI
trading panel ([MoEfolio.ai](https://moefolio.ai/)) whose verdicts
resolve against the market across delayed windows.

## The shape of the problem

Your service makes the same kind of decision many times per minute:
which backend to route to, how aggressively to scale, which fraud
threshold band to apply, which retry policy to use, which LLM to call.
The right answer depends on context — load, latency, hour of day,
customer tier — and shifts when traffic shifts. The outcome arrives
later (seconds, days, weeks), and only for the option you picked.

Most teams handle this with hand-tuned heuristics, a small dashboard, a
quarterly review meeting, and a static threshold that was right when it
was tuned and is wrong now.

## What Syntra does

A Syntra capsule is a single Lycan program that, on every HTTP
`/decide`, can:

1. Read the recent series the caller posts — load history, latency
   window, fraud rate by hour — via the sandboxed `runtime.inputGet` /
   `file.readText` / `http.get` / `sql.sqliteQuery` kernels.
2. **Compute features in the same graph**: one-step EWMA forecast,
   mean / stddev / percentile, autoscale-recommend (`predicted_load` →
   target instance count). 26 native Rust kernels, policy-enforced
   sandbox.
3. Run an adaptive choice node over a list of operational policies.
   A meta-bandit picks the algorithm that performs best on this
   capsule's actual traffic; you don't tune which one. Three
   structural flavors are wired and pick themselves from the
   capsule's config — flat options (default), shared-state when
   options carry semantic similarity, and hierarchical when the
   action space factors into a tree (region × server-type, segment
   × creative). Same `/decide` API for all three.
4. Return the chosen policy label and a decision ID.

When the outcome resolves — minutes, hours, days later — your service
POSTs `/feedback` with the decision ID and the observed reward. The
appliance updates the strategy weights. ADWIN drift detectors re-warm
the learner when the traffic profile shifts.

That's the whole loop. One Docker container next to your application.
Local filesystem store. No GPU, no model training, no model registry.
The only thing you wire up is the feedback signal.

## Three concrete capsules

The three demos in `examples/` are end-to-end:

### Predictive autoscaling

POST a recent load history. The capsule runs
`series.ewmaForecast(history, 0.4)`, computes the 95th percentile, and
derives candidate instance counts via `ops.autoScaleRecommend`. A
strategy node picks among `hold`, `forecast_match`,
`forecast_headroom`, `p95_safe`. Feedback is `sla_met` and
`cost_efficiency` after the next window.

### Anomaly-aware API routing

POST a recent latency-series window. The capsule computes
`stats.mean` and `stats.stdDev`, derives a z-score for the most-recent
sample, and runs a strategy node over `primary`, `secondary`,
`degraded_cache_only`, `circuit_break`. Feedback is success rate and a
tail-latency penalty.

### Seasonal fraud threshold

POST a recent fraud-rate series. The capsule EWMA-forecasts the
next-window fraud rate, computes the recent p95, and runs a strategy
node over `loose`, `baseline`, `tight`, `very_tight`. Feedback is
`caught_fraud` and `false_positive_cost`, posted when the chargeback
window resolves days later. This is exactly the delayed-feedback shape
Syntra was built for.

Each demo: one `.lycs` program, one `learning.json`, one capsule
manifest, one README.

## What you do, step by step

```bash
# 1. Pull the image (self-contained).
docker run -d --name syntra -p 8787:8787 \
  -v $PWD/syntra-store:/store \
  -e SYNTRA_ADMIN_KEY=$ADMIN \
  ghcr.io/ashhart/syntra:demo

# 2. Compile and install a capsule.
lycan compile examples/predictive-autoscaling/program.lycs
curl -X POST http://localhost:8787/tenants/ops/jobs/scale/capsules/autoscaler/install \
  -H "Authorization: Bearer $ADMIN" \
  --data-binary @examples/predictive-autoscaling/program.lyc
curl -X PUT  http://localhost:8787/tenants/ops/jobs/scale/capsules/autoscaler/learning \
  -H "Authorization: Bearer $ADMIN" \
  -H "Content-Type: application/json" \
  --data-binary @examples/predictive-autoscaling/learning.json

# 3. Ask for a decision.
curl -X POST http://localhost:8787/tenants/ops/jobs/scale/capsules/autoscaler/decide \
  -H "Authorization: Bearer $ADMIN" \
  -d '{"load_history":[80,90,110,140,180,220],
       "current_instances":3, "target_per_instance":100,
       "min_instances":1, "max_instances":20,
       "features": {"hour":14.0, "current_instances":3, "load_trend":0.6}}'
# → {"decisionId": "...", "decisions": [{"option": "forecast_headroom", ...}]}

# 4. Apply the policy in your code, observe the outcome, post feedback.
curl -X POST http://localhost:8787/tenants/ops/jobs/scale/capsules/autoscaler/feedback \
  -H "Authorization: Bearer $ADMIN" \
  -d '{"decisionId": "...", "rewardComponents": {"sla_met": 1.0, "cost_efficiency": 0.7}}'
```

That is the whole integration surface for adopting Syntra against any
operational decision in your stack.

## What Syntra is not

So you know before you install:

- **Not a forecasting platform.** One kernel ships: one-step EWMA. No
  ARIMA, no Prophet, no deep forecasters.
- **Not a managed service.** Self-hosted Docker. Run behind a TLS proxy.
- **Not modern-data-stack scale.** Single-node, local-filesystem store,
  hot-path throughput in the hundreds to low thousands of decides per
  second on commodity hardware. No clustering today.
- **Not a metric store.** The optional `sidecar/` reads from
  Prometheus / Datadog / SQL but doesn't replace them.
- **Not a model platform.** No GPU, no training, no fine-tuning.
- **Not for one-shot decisions.** Without feedback flowing back, there
  is nothing to learn.

## What it is

A single binary, in a single container, that turns a recurring
operational decision into one your service can keep getting better at
— from the only signal you can credibly afford to wire up: delayed
feedback.

Read `POSITIONING.md` for the full ground-up statement. Read the three
demos in `examples/` for what a capsule looks like end-to-end. Drop
the container next to your service and run one decision through it.

Apache-2.0. Operationally hardened: scoped auth tokens, token-bucket
rate limit, Prometheus `/metrics`, `/ready` store-writability probe,
JSON structured logging, backup/restore as JSON bundles. Run behind a
TLS proxy.

`https://github.com/ashhart/Syntra` · `Apache-2.0`
