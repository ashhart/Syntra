# Fraud threshold tuning

Adaptive fraud-blocking threshold selection driven by a Syntra
capsule.

Repository copy: [`examples/fraud-tuning/`](https://github.com/ashhart/Syntra/tree/main/examples/fraud-tuning).

## What it does

Most fraud systems use a single, hand-tuned risk threshold (e.g.
block every transaction with score > 0.7). This is a permanent
compromise: a threshold that is too tight causes false positives and
frustrated customers; one that is too loose lets fraud slip through.

`syntra_fraud.FraudClient` lets Syntra pick the threshold
dynamically, per decision, based on current merchant context:

1. Computes a feature vector describing the merchant's recent
   behaviour (recent fraud rate, transaction volume per hour, average
   ticket size, hour of day).
2. Calls Syntra `/decide` with those features. Syntra returns one of
   five threshold options (`block_at_0_5` through `block_at_0_9`).
3. Compares the transaction's risk score against the chosen threshold
   and returns a `ScoreDecision` with `block: bool` and
   `decision_id`.
4. After the true outcome is known, `report_outcome()` posts a reward
   to `/feedback`.

Over many transactions, Syntra's meta-bandit learns which threshold
works best under which merchant conditions and converges.

## Quick start

```bash
docker run --rm -p 8787:8787 -p 8080:8080 syntra:demo
cd Syntra/examples/fraud-tuning
pip install -e .
```

```python
import os
from syntra_fraud import FraudClient

client = FraudClient(
    syntra_url="http://localhost:8787",
    capsule_path="/tenants/myteam/jobs/fraud/capsules/threshold",
    admin_key=os.environ["SYNTRA_ADMIN_KEY"],
)

decision = client.score({
    "merchant_id":     "merch_42",
    "risk_score":      0.73,
    "ticket_size_usd": 120.0,
})

if decision.block:
    reject_transaction()
else:
    process_transaction()

# Later, when the true outcome is known:
client.report_outcome(
    decision.decision_id,
    was_fraud=False,
    merchant_id="merch_42",
    ticket_size_usd=120.0,
    blocked=decision.block,
)
```

## Feature derivation

The library maintains a lock-protected, per-merchant rolling window
(last 100 observations). From those observations it derives:

- `recent_fraud_rate` — fraction of recent transactions that were
  labelled fraudulent.
- `transaction_volume_per_hour` — count of transactions seen in the
  last 60 minutes.
- `avg_ticket_size_usd` — mean ticket value over the window.
- `hour` — current UTC hour (cyclic feature, period 24).

These four features are sent to Syntra `/decide`. The capsule
installed by `setup_capsule.py` declares a feature-context learning
config, so Syntra runs all meta-bandit candidates including LinUCB.

## Reward formula

`report_outcome()` maps the (block, was_fraud) pair to a reward in
`[-1, 1]`:

| Outcome                     | Reward                                  |
|-----------------------------|-----------------------------------------|
| Correct block (real fraud)  | +1.0                                    |
| Correct allow (legit)       | +1.0                                    |
| False positive (block legit)| -false_positive_cost / 200.0 (default -0.25) |
| Missed fraud (allow fraud)  | -fraud_loss_cost / 200.0 (default -1.0) |

Both cost parameters are arguments to `report_outcome()` so you can
tune the trade-off for your business without touching the library.

## Fail-safe behavior

- Syntra unreachable → use `fallback_threshold` (default: 0.7).
- Syntra returns `refused: true` → use `fallback_threshold`; the
  decision ID is still forwarded so the bandit's audit log records
  the attempt.
- Syntra returns a malformed response → use `fallback_threshold`.
- Feedback POST fails → silently swallowed.

A Syntra outage degrades adaptive threshold selection to a fixed
fallback until Syntra recovers. It does not block transactions or
raise exceptions into your application.

## Customization points

- **Threshold options** — edit `CAPSULE_SPEC` in `setup_capsule.py` to
  define different threshold names, then re-run `setup_capsule.py`.
- **`Threshold.from_option`** — replace the default mapping in
  `__init__.py` with a custom `_DEFAULT_THRESHOLDS` list.
- **Feature window** — pass `window=N` to `_MerchantTracker` (default
  100).
- **Fallback threshold** — pass `fallback_threshold=0.8` to
  `FraudClient`.
- **Cost parameters** — pass `false_positive_cost` and
  `fraud_loss_cost` to `report_outcome()` to reflect your business
  economics.

## See also

- [HTTP retry tuning](retry-tuning.md) — the canonical integration
  pattern this mirrors.
- [Seasonal fraud threshold](seasonal-fraud-threshold.md) — the
  kernel-based capsule that computes its own features from a posted
  fraud-rate window instead of relying on the integrating library's
  rolling stats.
