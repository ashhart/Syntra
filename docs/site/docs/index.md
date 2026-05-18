---
title: Syntra
hide:
  - toc
---

<div class="syntra-hero" markdown>

# Syntra

**A self-hosted appliance for adaptive operational decisions.**

<p class="lede" markdown>
One Docker container next to your service. Local filesystem store. No GPU,
no model training, no managed cloud dependency. If Syntra is down or
refuses, your service falls back to the default policy it already has —
no new failure mode is introduced. If Syntra is up, your service gets a
learned choice per request and a place to send the feedback signal when
the outcome resolves.
</p>

<div class="syntra-cta" markdown>
[Try it in 30 minutes](quickstart.md){ .md-button .md-button--primary }
[Read the pitch](https://github.com/ashhart/Syntra/blob/main/PITCH.md){ .md-button }
[Browse the docs](concepts/index.md){ .md-button }
</div>

</div>

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

1. **Read** the recent series the caller posts — load history, latency
   window, fraud rate by hour — via the sandboxed `runtime.inputGet` /
   `file.readText` / `http.get` / `sql.sqliteQuery` kernels.
2. **Compute features in the same graph**: one-step EWMA forecast,
   mean / stddev / percentile, autoscale-recommend (`predicted_load` →
   target instance count). 26 native Rust kernels, policy-enforced
   sandbox.
3. **Run an adaptive choice node** over a list of operational policies.
   A meta-bandit picks the algorithm that performs best on this
   capsule's actual traffic; you don't tune which one. Three structural
   flavors are wired and pick themselves from the capsule's config —
   flat options (default), shared-state when options carry semantic
   similarity, and hierarchical when the action space factors into a
   tree. Same `/decide` API for all three.
4. **Return** the chosen policy label and a decision ID.

When the outcome resolves — minutes, hours, days later — your service
POSTs `/feedback` with the decision ID and the observed reward. The
appliance updates the strategy weights. ADWIN drift detectors re-warm
the learner when the traffic profile shifts.

That is the whole loop. One Docker container next to your application.
Local filesystem store. No GPU, no model training, no model registry.
The only thing you wire up is the feedback signal.

## Three concrete capsules

The three demos in [`examples/`](examples/index.md) are end-to-end:

- **[Predictive autoscaling](examples/predictive-autoscaling.md)** —
  POST a recent load history. The capsule runs `series.ewmaForecast`,
  computes the 95th percentile, and derives candidate instance counts
  via `ops.autoScaleRecommend`. A strategy node picks among `hold`,
  `forecast_match`, `forecast_headroom`, `p95_safe`.
- **[Anomaly-aware API routing](examples/anomaly-routing.md)** — POST a
  recent latency-series window. The capsule computes `stats.mean` and
  `stats.stdDev`, derives a z-score, and routes among `primary`,
  `secondary`, `degraded_cache_only`, `circuit_break`.
- **[Seasonal fraud threshold](examples/seasonal-fraud-threshold.md)**
  — POST a recent fraud-rate series. The capsule EWMA-forecasts the
  next-window fraud rate and picks a threshold-adjustment policy.
  Feedback resolves days later when the chargeback window closes.

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
- **Not a managed service.** Self-hosted Docker. Run behind a TLS
  proxy.
- **Not modern-data-stack scale.** Single-node, local-filesystem store,
  hot-path throughput in the hundreds to low thousands of decides per
  second on commodity hardware. No clustering today.
- **Not a metric store.** The optional `sidecar/` reads from
  Prometheus / Datadog / SQL but does not replace them.
- **Not a model platform.** No GPU, no training, no fine-tuning.
- **Not for one-shot decisions.** Without feedback flowing back, there
  is nothing to learn.

## Where next

- The [quickstart](quickstart.md) walks through `docker run` → install →
  decide → feedback in 30 minutes.
- The [concepts pages](concepts/index.md) explain capsules, kernels,
  strategy nodes, the meta-bandit, drift, and refusal in honest terms.
- The [domain packs](examples/index.md) are end-to-end worked
  capsules — copy whichever is closest to your problem.
- The [API reference](reference/api.md) is the full endpoint surface.
