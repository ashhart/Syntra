# Refusal

A bandit is happy to return *some* option for every request, no matter
how confident or unconfident it is in any of them. That is the
default. For most operational decisions it is the right default —
returning *something* is more useful than returning nothing.

But there are requests where it is not. The input is out-of-distribution
relative to anything the bandit has seen. The reward posterior is so
wide that every option's confidence interval overlaps with every
other's. In those cases, the honest answer is **"I don't know — fall
back to your default."**

**Refusal** is the opt-in mechanism that lets the bandit say that.

## What refusal returns

When refusal is enabled and the capsule is Active, `/decide` can
return a refused response in place of a decision:

```json
{
  "ok": true,
  "decisionId": "dec_e1f2a3b4c5d60718",
  "contextKey": "rush_hour",
  "warmup": {"state": "active", "algorithm": "Thompson"},
  "decisions": [],
  "refused": true,
  "oodScore": 0.92,
  "confidence": {
    "oodScore": 0.92,
    "intervalWidth": 0.62,
    "coverage": 0.95,
    "refused": true,
    "refusalReason": "ood"
  }
}
```

Three things to notice:

- `decisions` is empty. There is no `chosen_option` to act on.
- `refused: true` is the signal your integration code checks. Every
  Syntra client library — Python, Go, Node, Java, Rust — wraps this
  and falls back to a `fallback_policy` you configure on the client.
- `refusalReason` is one of `"ood"`, `"interval_too_wide"`, or
  `"insufficient_calibration_data"`. The first two are real signals;
  the third is the bootstrap path before the calibrator has enough
  data.

## Two signals: OOD score, interval width

Refusal uses two confidence signals in parallel.

### Out-of-distribution score (`oodScore`)

A per-context OOD detector tracks the distribution of inputs the
bandit has seen for each context bucket. New inputs are scored against
that distribution; very-novel inputs get a high `oodScore` (close to
1.0).

For discrete contexts the detector tracks the frequency of each
contextKey; for feature contexts it tracks the feature-vector
distribution (per-feature percentile, joint quantile).

Refusal fires when `oodScore > refusal.oodThreshold` (default `0.8`).

### Prediction-interval width (`intervalWidth`)

Once the capsule is Active, **split-conformal prediction intervals**
wrap the bandit's reward predictions. Each prediction carries a
calibrated interval; the width of that interval at the configured
`coverage` (default 0.95) is the bandit's expressed uncertainty.

Refusal fires when `intervalWidth > refusal.maxIntervalWidth` (default
`0.5`).

## Configuration

Refusal is **disabled by default**. Enable it in `learning.json`:

```json
{
  "refusal": {
    "enabled": true,
    "coverage": 0.95,
    "maxIntervalWidth": 0.5,
    "oodThreshold": 0.8
  }
}
```

Fields:

- `enabled` — gate. Off by default; turn it on when the capsule has
  enough traffic that the calibrator and OOD detector have
  characterized the normal distribution.
- `coverage` — the conformal coverage target. `0.95` means "95% of
  observed rewards should fall inside the interval." Higher coverage
  = wider intervals = more refusals.
- `maxIntervalWidth` — fraction of the reward range. `0.5` on a reward
  in `[-1, 1]` means refuse when the interval is wider than 1.0 units.
- `oodThreshold` — fraction. Refuse when `oodScore` exceeds this.
  Lower = more aggressive refusal.

PUT it at any time:

```bash
PUT /tenants/{t}/jobs/{j}/capsules/{c}/learning
```

## The lifecycle interaction

Refusal **never fires during Warmup.** The bootstrap path needs
unconditional data flow to characterize the reward distribution; if
the bandit refuses on its own training data, the calibrator never
gets enough samples to be useful.

Refusal **also doesn't fire when calibration data is insufficient.**
The first few hundred Active-state decisions, you may see
`refusalReason: "insufficient_calibration_data"` flagged but
`refused: false` — the system is telling you it would have abstained
if it had been confident enough to abstain.

## How your integration handles refusal

Every official client library has a fallback path:

```python
# from syntra_retry/__init__.py — pattern is identical for queue, fraud, llm
decision = client.choose(...)
if decision.refused:
    policy = client.fallback_policy
else:
    policy = decision.policy
```

Three failure modes flow into the same fallback:

1. Syntra **unreachable** → use `fallback_policy`.
2. Syntra returns `refused: true` → use `fallback_policy`.
3. Syntra returns a **malformed response** → use `fallback_policy`.

A Syntra outage degrades adaptive selection to "always fall back" until
the service recovers. It does not break the request flow.

## Auditing refusal

`audit.jsonl` records every refused decision with the score that
triggered it. The dashboard surfaces the per-context refusal rate so
you can see whether refusal is firing on a stable minority of edge-case
inputs or whether it has spiked because of a regime shift.

If you POST `/feedback` against a `decisionId` that was refused, the
event is recorded but does **not** mutate the bandit — `audit.jsonl`
appends a `feedback_on_refused` entry. This keeps the refusal
honest: you can still learn from the outcome (for monitoring and
offline analysis) without letting refused decisions bias the bandit's
own weights.

## What refusal is not

- **Not a circuit breaker.** It does not control the rate of decisions
  — every request still gets a response, just sometimes with
  `refused: true`. If you want to throttle inbound traffic, do that
  at the proxy layer.
- **Not a perfect classifier of "bad inputs".** The OOD detector is a
  statistical signal, not a guarantee. Some unusual-but-correct inputs
  will be refused; some genuinely-bad inputs at low rate will not be.
- **Not free.** Conformal intervals require calibration data and
  per-decision interval-width computation. The OOD detector requires
  per-decision scoring. For very-low-volume capsules the cost is
  small but nonzero.
- **Not a substitute for input validation.** If the caller is sending
  malformed feature vectors (NaN, Inf, out-of-range), the runtime
  defends against it upstream of refusal. Refusal is for inputs that
  are *syntactically* valid but *semantically* out of the training
  distribution.

## Where to go next

- [Drift detection](drift.md) — the stream-level confidence companion
  to refusal's per-request signal.
- [Strategy node](strategy-node.md) — where refusal sits in the decide
  path.
- [API reference: Decide](../reference/api.md#decide) — the full
  request/response surface including the refusal block.
