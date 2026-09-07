# Drift detection

A bandit that has converged on the option that wins under last
quarter's traffic is going to keep picking that option even after the
traffic has shifted. Whatever produced the old winner — a load shape,
a customer mix, an upstream provider's behaviour — does not stay
constant in production.

**Drift detection** is the mechanism by which Syntra notices a regime
shift and re-warms the learner before it makes too many decisions
against the old assumption.

## Two scopes

Syntra runs **ADWIN** (ADaptive WINdowing) detectors at two scopes,
both on the reward stream:

### Capsule-level ADWIN

One detector per strategy node, fed by every reward that arrives via
`/feedback`. When the detector flags a change-point — the empirical
reward mean inside a recent window differs from the historical window
by more than the configured threshold — the capsule **re-warms**:

- Lifecycle returns to `Warmup`.
- The meta-bandit's candidate selection returns to uniform sampling
  for the new warmup target (default ~30 rounds).
- The strategy weights snapshot to `snapshots/` before reset (you can
  inspect the pre-drift state).
- An `audit.jsonl` event is appended: `change_detected`.

This is the right move when the *whole* reward landscape has changed.
A new region went live, a payment provider deployed a new model under
the same name, your service migrated to a different LLM API tier.

### Per-context ADWIN

One detector per `(nodeId, contextKey)` (discrete) or per feature
neighbourhood (features), fed by the reward stream filtered to just
the decisions that landed in that bucket. When this detector flags a
change-point, only that **single bucket** is reset:

- The per-context bandit state for that bucket resets to the uniform
  prior.
- The rest of the capsule keeps its current weights.
- An `audit.jsonl` event is appended: `context_drift`.

This is the right move when one context bucket starts behaving
differently — a single customer's fraud profile shifted, one
geographic region's latency distribution changed — while the rest of
the capsule's traffic is still in the regime it knew.

## Configuration

Drift detection is configured under `changeDetection` in
`learning.json`:

```json
{
  "changeDetection": {
    "enabled": false,
    "threshold": 5.0,
    "minDrift": 0.05,
    "explorationBoost": 0.25,
    "boostDuration": 50,
    "method": "pageHinkley",
    "surpriseKSigma": 2.5,
    "surpriseFractionThreshold": 0.30
  }
}
```

The fields most operators touch:

- `enabled` — off by default. ADWIN incurs constant per-feedback
  bookkeeping; turn it on once you have enough traffic that drift
  matters.
- `threshold` — the ADWIN δ confidence threshold. Higher = more
  conservative (fewer false positives, slower to detect real
  drift).
- `minDrift` — minimum effect size to flag. A 0.05 minDrift means a
  change-point is only considered if the mean shifted by at least 5%
  of the reward range.
- `explorationBoost` and `boostDuration` — instead of a full re-warm,
  optionally just boost the bandit's exploration rate by this much
  for this many rounds after detection. Useful when you suspect drift
  but don't want to discard learned weights.

`method: "pageHinkley"` is an alternative single-window detector that
ships alongside ADWIN. ADWIN is preferred for capsule-level; Page–
Hinkley is the fall-back for very-low-traffic capsules where ADWIN's
window growth is slow.

## When drift fires (and when it doesn't)

Drift detection is **reward-based**, not feature-based. A change in
the input distribution alone does not re-warm the capsule. What
re-warms the capsule is a change in the *reward distribution given
the bandit's current behaviour*.

That has implications:

- **A new option being added does not trigger drift.** The bandit's
  reward distribution just gains a new option whose weight starts at
  the prior. The other options are unaffected.
- **Pure feature drift without reward drift does not trigger.** If
  your `hour-of-day` feature shifts because of a daylight-savings
  rollover but the reward each option produces is unchanged, the
  bandit doesn't re-warm. It learns the new feature distribution
  naturally as decisions accumulate.
- **A reward-shape change without a mean shift can trigger.** ADWIN
  is on the mean; if the variance widens dramatically at the same
  mean, the detector won't fire. The OOD detector in [refusal](refusal.md)
  is the complementary signal for distribution-shape changes.

## What an operator does when drift fires

Drift is a signal, not a failure. The audit log carries the event, the
admin console renders it, and the capsule's lifecycle moves back into
Warmup automatically. The operator's job is not to react urgently — it
is to **understand** why drift fired:

1. Read the `audit.jsonl` `change_detected` entry. It records the
   window means before and after the change-point.
2. Cross-reference with the `decision.jsonl` log for the same window
   to see which options were dominant and which got penalized.
3. Check `feedback.jsonl` for the actual reward shape that came in.
   Is it a real upstream change, or a reward-spec bug, or a feedback
   pipeline outage?
4. If real: let the capsule re-warm naturally. The meta-bandit will
   re-converge in another ~30–50 rounds on whatever now wins.
5. If artifact: identify the cause, fix it, and consider replaying
   the pre-drift snapshot via `/snapshots`.

## What drift detection is not

- **Not anomaly detection per request.** ADWIN looks at the *reward
  stream over time*, not individual inputs. For per-request
  out-of-distribution scoring, see [refusal](refusal.md).
- **Not free.** A capsule with drift detection enabled does more work
  per `/feedback`. For capsules with very low traffic, the bookkeeping
  is not free relative to the per-decide cost.
- **Not a substitute for monitoring.** Drift detection runs on the
  reward stream — the data Syntra sees. Outages in the upstream
  service that *prevent* feedback from reaching Syntra do not look
  like drift; they look like silence. Monitor the rate of incoming
  feedback separately.

## Where to go next

- [Refusal](refusal.md) — the per-request OOD / confidence companion
  to drift's stream-level detection.
- [Meta-bandit](meta-bandit.md) — what re-converges after a drift
  re-warm.
- [Operations stub](../reference/operations.md) — operator runbook for
  drift events (in progress).
