# Wiring up delayed feedback

The most common Syntra integration question: **the outcome of a
decision resolves hours or days after the decision was made — how do
I post feedback against that decision when it finally lands?**

The mechanism is a single field in the `/decide` response:

```json
{
  "decisionId": "dec_57a2270571f0c2fa",
  ...
}
```

That string is opaque, stable, and pairs the decision with its
eventual outcome. You persist it next to whatever event the outcome
will eventually update — a fraud chargeback, an SLA window close,
a refund — and POST it back to `/feedback` when that event resolves.

## The pattern

```python
# 1. Decide.
resp = syntra.post(f"/tenants/prod/jobs/fraud/capsules/threshold/decide",
                   json={"features": {...}})
decision_id = resp.json()["decisionId"]
chosen_threshold = resp.json()["decisions"][0]["chosen_option"]

# 2. Persist the decisionId alongside the business event.
db.transactions.insert({
    "txn_id":      txn.id,
    "decision_id": decision_id,
    "threshold":   chosen_threshold,
    "decided_at":  now(),
    # ... rest of the business row
})

# 3. ...later, when the outcome resolves...
chargeback_event = wait_for_chargeback_or_settlement(txn.id, days=14)

# 4. Feedback.
syntra.post(f"/tenants/prod/jobs/fraud/capsules/threshold/feedback",
            json={
                "decisionId": db.transactions.find(txn.id).decision_id,
                "reward":     reward_from(chargeback_event),
            })
```

The `decisionId` round-trip is the only piece that has to flow
through your existing system. Syntra keeps the rest of the state —
which feature vector was active at decide time, which candidate
algorithm fired, which option was chosen — keyed on that ID in its
on-disk decision log (`decisions.jsonl`).

## How long can the gap be?

Days, weeks. The Syntra side stores the decision event indefinitely
in `decisions.jsonl`. If you post `/feedback` 90 days after the
`/decide`, it still finds the event and applies the reward. The
only practical limits are:

- **Storage**: each decision event is ~1–2 KB on disk. At 1000
  decides/day that's ~1 GB/year per capsule. Use `DELETE
  /tenants/<t>/jobs/<j>/capsules/<c>/logs` to purge old events
  before they accumulate beyond what you care about.
- **Drift**: if the model has substantially re-warmed (ADWIN
  detected drift between the decide and the feedback), the reward
  is still applied — but the bandit has moved on. The
  `changeDetected` field in the `/feedback` response tells you when
  that happened.

## What about partially-resolved outcomes?

You have two options:

1. **Wait for the final outcome**, then post one feedback. Cleanest;
   what most integrations end up doing.
2. **Post intermediate feedback** with `signalKind` (delayed-feedback
   mode). For example, "an early indicator resolved positively" as a
   weak signal, then the final outcome as a strong signal:

   ```python
   syntra.post(".../feedback", json={
       "decisionId": dec_id,
       "reward": 0.6,
       "signalKind": "interim",  # or any string your capsule recognizes
   })
   ```

   Requires `delayed_feedback.enabled = true` in the capsule's
   learning config. The bandit weights the partial signal less than
   the final outcome.

## Failure modes

- **Lost decisionId**: if your DB row is dropped before the outcome
  resolves, you can't post feedback. The decision still happened,
  the option was still chosen — but the bandit learns nothing from
  it. This is a *missing signal*, not an *incorrect signal*; the
  bandit's variance increases on the un-reinforced arm. Set
  expectations: missing feedback is fine occasionally, harmful
  systematically.
- **Out-of-order feedback**: posting feedback for decision A *after*
  posting feedback for decision B that happened later is fine —
  Syntra applies each one independently. There's no implied
  ordering.
- **Refused decisions**: if the decision was refused (see
  [Debugging refusals](../operations/debugging-refusals.md)), the
  `/feedback` call returns a 200 with `noted` set to a string
  explaining the bandit state was unchanged. You can safely call
  `/feedback` against any historical `decisionId` without
  branching.
