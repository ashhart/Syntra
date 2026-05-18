# `runtime.publish` — exposing kernel outputs to the dashboard

A Lycan capsule's program computes intermediate values — EWMA
forecasts, percentile thresholds, derived z-scores — before the
strategy node picks among options. Those values are normally only
visible in `!p` log output, which means they survive in `lycan
inspect` traces but never reach the operator running a dashboard
against `/decisions`. `runtime.publish` names a computed value and
stores it on the current decision's journal entry so the Syntra
dashboard (and any `/decisions` consumer) can render it, audit it,
and reason about it alongside the chosen option.

## Signature and supported types

```lisp
(!cap "runtime.publish" name value)
```

- `name` is a string. Calling with the same `name` twice within the
  same `/decide` overwrites the earlier value.
- `value` is one of: number (int or float), string, bool, null.
  **Arrays are rejected.** **Non-finite floats** (`NaN`, `±Inf`) are
  rejected.

The call returns `null`. It is `Effectful` in the kernel registry but
does not require any capsule-policy permission — the only side effect
is a write to the per-decision in-memory buffer the runtime owns.

## Where the values land

Each successful `runtime.publish` call appends one field to the
current decision's `published` map. That map is flushed into:

- the `published` object on the JSON returned by `POST /decide`,
- the `published` field on the matching record in `GET /decisions`
  (the JSONL feed), and
- the corresponding entry in `decisions.jsonl` on disk.

The Syntra dashboard's Region 5 (Live kernel outputs) polls
`/decisions`, reads the `published` map of the most recent
decision, and renders each `(name, value)` pair as a card.

The buffer is initialised empty at the start of every `/decide` and
discarded after the strategy node fires, so a `runtime.publish` call
in capsule A cannot leak into capsule B's decision entry, and one
decide's values cannot leak into another's.

## Worked example: predictive-autoscaling

The `predictive-autoscaling` capsule computes three intermediate
values — the recent mean load, the recent 95th percentile, and a
one-step EWMA forecast — and then offers the bandit four candidate
instance counts to choose between. The forecast and the percentile
matter operationally: the operator wants to see the *number* the
program is reacting to, not just the option label the bandit
returned.

The capsule's `.lycs` emits, just before the `(choice ...)` node:

```lisp
;; Publish kernel outputs to the decision journal.
(!cap "runtime.publish" "forecast"              load_forecast)
(!cap "runtime.publish" "p95"                   load_p95)
(!cap "runtime.publish" "recommended_instances" (policy_forecast_headroom))
```

The third call publishes the result of the `forecast_headroom`
policy — the most-illustrative of the four scaling candidates — so
the dashboard can show "the headroom-policy recommendation" alongside
"the bandit's actual selection." The bandit may pick differently;
publishing both is the point.

A `/decide` response then carries a `published` object alongside the
usual fields:

```json
{
  "decisionId": "01HSXEK9...",
  "option": "policy_forecast_headroom",
  "confidence": 0.27,
  "weights": [0.21, 0.24, 0.29, 0.26],
  "published": {
    "forecast": 134.41,
    "p95": 155,
    "recommended_instances": 2
  }
}
```

## When to use it

Any computed value that's useful to render in the dashboard or audit
after the fact:

- forecasts and projections (`series.ewmaForecast` outputs),
- threshold values and percentile cuts (`stats.percentile`),
- derived feature dimensions (z-scores, ratios, normalisations),
- recommended actions the strategy node will pick *between*,
- anything you'd otherwise reach for `!p` to log.

If the dashboard or an external auditor should see it, publish it.

## When NOT to use it

Don't publish high-volume internal scratch state. Each
`runtime.publish` call adds one JSON field to the decision-journal
entry; a capsule that publishes 50 values per `/decide` makes the
JSONL bulky and the dashboard cluttered.

The capability is for *interpretable* intermediate values — the ones
a human reading `decisions.jsonl` should care about. Loop counters,
intermediate accumulators, and per-step debug values belong in `!p`
output, not in the published buffer.

## CLI / test behaviour

Outside Syntra — when you run `lycan decide` against a `.lyc` file
directly — there is no per-decision buffer. `runtime.publish`
detects the missing buffer and silently no-ops, returning `null`.

The same `.lycs` program therefore runs identically in CLI and in
Syntra: in CLI the call succeeds but the values are dropped; in
Syntra the call succeeds and the values surface on the decision
entry. There is no separate "CLI mode" of the capability — the
runtime context is what differs.

## Cross-links

- Mental model for kernels inside a capsule:
  [`../concepts/operational-intelligence.md`](../concepts/operational-intelligence.md).
- The cleanest demo that uses this capability:
  [`../../examples/predictive-autoscaling/`](../../examples/predictive-autoscaling/).
- Other capsule features:
  [`shared-state-linucb.md`](shared-state-linucb.md),
  [`hierarchical-bandits.md`](hierarchical-bandits.md).
- Capability spec in the kernel registry: search
  `Lang/src/capabilities.rs` for `runtime.publish`.
