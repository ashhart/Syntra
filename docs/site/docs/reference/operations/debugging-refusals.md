# Debugging refusals

"Why is Syntra refusing my decisions?" is the most common
operational question, and the answer is always one of three
specific reasons. Each one is reported in the `/decide` response
body and in the capsule's audit log; each one has a different
diagnosis path.

## What "refused" looks like

When the capsule's `refusal.enabled = true` and the lifecycle is
`Active`, Syntra can decline to commit to a decision. The response
shape is:

```json
{
  "ok": true,
  "decisionId": "dec_...",
  "refused": true,
  "decisions": [],
  "confidence": {
    "oodScore": 0.87,
    "intervalWidth": null,
    "coverage": 0.95,
    "refused": true,
    "refusalReason": "ood"
  }
}
```

`refusalReason` is the diagnostic. There are exactly three values.

## Reason 1: `"ood"` — out of distribution

**What it means**: the feature vector on this request looks unlike
anything the bandit has seen during its training. The
Mahalanobis-distance OOD detector inside the capsule scored above
the configured `refusal.ood_threshold` (default `0.5`).

**Where to look**:

```bash
curl -s ".../tenants/<t>/jobs/<j>/capsules/<c>/memory" | \
    jq '.strategies[].feature_ood'
```

The `mean` and `inv_cov` keys describe the feature distribution
the bandit has *seen*. If your request's features fall far outside
this — for example, a `latency_ms` of `50_000` when the bandit has
only ever seen values in `[0, 2000]` — that's the OOD signal.

**Fixes** (pick one):

- **Genuinely novel context**: the bandit hasn't been trained on
  this regime yet. Let your service fall back to a default policy
  for this request; the OOD detector records the new observation
  and the next request like it will be less novel. Over time the
  bandit catches up.
- **Threshold too tight**: if you see refusals on traffic that
  looks normal to you, raise `refusal.ood_threshold` in the
  capsule's `learning.json` (e.g. from `0.5` to `0.8`). Restart not
  required — the new value takes effect on the next `/decide`.
- **Feature drift**: if a feature's distribution genuinely shifted
  (e.g. a deploy changed the latency baseline), the OOD detector
  will fire on everything new. Force the bandit to re-warm by
  posting a `DELETE` to
  `/tenants/<t>/jobs/<j>/capsules/<c>/memory`, then re-run warmup.

## Reason 2: `"interval_too_wide"` — bandit is unsure

**What it means**: the conformal prediction interval for the chosen
arm is wider than `refusal.max_interval_width`. The bandit is
saying "I could pick this option, but the confidence band is too
wide to commit to it."

**Where to look**:

The `intervalWidth` field in the `/decide` response is the actual
computed width. Compare it to your `refusal.max_interval_width`
setting in `learning.json`.

```bash
curl -s ".../decide" -d '...' | jq '.confidence'
```

**Fixes**:

- **More data**: this is the right behavior in early training. Keep
  feeding `/feedback`; the interval narrows as observations
  accumulate. The bandit refuses fewer requests as confidence
  builds.
- **Loosen the threshold**: raise `refusal.max_interval_width` in
  `learning.json`. Useful when the reward signal is genuinely
  noisy and the bandit can't ever get below the current threshold.
- **Reduce option count**: if you have 20 options and only 3 are
  ever competitive, splitting per-arm calibration data 20 ways
  means each arm stays uncertain. Drop options that aren't
  earning trials.

## Reason 3: `"insufficient_calibration_data"` — not enough feedback

**What it means**: the conformity calibrator for the chosen arm
has fewer than 30 observations (the calibration target). Without
30 residuals, it can't compute a meaningful interval, so refusal
is the safe default.

**Where to look**:

```bash
curl -s ".../tenants/<t>/jobs/<j>/capsules/<c>/memory" | \
    jq '.strategies[].contexts[].stats[] | {tries, posterior_var}'
```

If `tries < 30` on the chosen arm in the chosen context, that's
the cause.

**Fixes**:

- **Wait it out**: feedback the bandit's exploratory decides; the
  calibrator gets its 30 observations and the refusals stop.
- **Lower the target**: if 30 is too high for your traffic rate,
  drop `conformal.min_calibration` in `learning.json` (e.g. to
  10). Quality of the interval degrades but refusals stop sooner.
- **Disable refusal for this capsule**: set `refusal.enabled =
  false` in `learning.json` if the safety guarantees aren't worth
  the operational cost. The bandit still chooses but no longer
  declines.

## Refusals are usually working

Refusal is the safety net the bandit asks for when it doesn't have
enough evidence. If you're seeing refusals during the first few
hundred decides per capsule, that's the system working as designed.
If you're still seeing refusals after thousands of decides per
context, one of the three diagnoses above will tell you which
parameter to nudge.

## When to call this an outage

A capsule that refuses **everything** for hours is broken — usually
because feedback isn't flowing. Check:

1. The audit log for `feedback_on_refused` events (operators
   sometimes post feedback against refused decisions, which is a
   no-op but recorded).
2. The capsule's warmup state — if it's stuck in `Warmup` the
   calibrator hasn't started building intervals yet.
3. The decision log for an early run of consecutive refused
   decisions; that pattern usually means the OOD detector fired on
   the first request and every subsequent one looks similar.

A capsule that refuses **occasionally** during normal operation is
behaving correctly. The Prometheus counter
`syntra_refusals_total{reason=...}` is the right metric to alert
on — alert when the rate stays above ~5% for more than 30 minutes,
not on every individual refusal.
