# Migrating from static if/else rules

If your service routes traffic, picks fraud thresholds, or chooses a
retry policy with a hand-written rule table — and you've been
hand-tuning those rules every quarter — replacing them with a Syntra
capsule is the most direct conversion path. The rules become the
**option list**; the context the rules read becomes the **feature
vector**; the outcome the rules were trying to optimise for becomes
the **reward**. The bandit replaces the human tuner.

## What you have today

A typical static-rules block looks like this:

```python
def pick_retry_policy(request):
    if request.tier == "enterprise":
        return "aggressive_retry"
    if request.tier == "free" and request.region == "us":
        return "no_retry"
    if request.observed_latency_ms > 800:
        return "circuit_break"
    return "default_retry"
```

Three things are true here:

1. The set of options (`aggressive_retry`, `no_retry`,
   `circuit_break`, `default_retry`) is fixed and known.
2. The features driving the decision (`tier`, `region`,
   `observed_latency_ms`) are observable at request time.
3. There is *some* downstream measurable outcome — request success,
   p95 latency, customer satisfaction — that resolves shortly after
   the policy is applied. You're already grading these rules
   manually in the quarterly review; the grader becomes the reward
   function.

## After Syntra

The capsule YAML:

```yaml
name: retry-policy
version: 0.1.0

options:
  - aggressive_retry
  - default_retry
  - no_retry
  - circuit_break

reward:
  type: continuous
  range: [0.0, 1.0]

context:
  features:
    - { name: tier_enterprise,    type: bool }
    - { name: region_us,          type: bool }
    - { name: observed_latency_ms, type: number, range: [0, 5000] }
```

The integration in your service:

```python
import requests

def pick_retry_policy(request):
    resp = requests.post(
        f"{SYNTRA_URL}/tenants/prod/jobs/retry/capsules/policy/decide",
        headers={"Authorization": f"Bearer {SYNTRA_TOKEN}"},
        json={
            "features": {
                "tier_enterprise":     request.tier == "enterprise",
                "region_us":           request.region == "us",
                "observed_latency_ms": request.observed_latency_ms,
            },
        },
        timeout=0.1,
    )
    decision = resp.json()
    # Remember decisionId so we can post the outcome later.
    request.store_decision_id(decision["decisionId"])
    return decision["decisions"][0]["chosen_option"]  # 0..3, matching options[]
```

When the outcome resolves (seconds, minutes, or hours later):

```python
def record_outcome(request, success: bool, latency_ms: float):
    requests.post(
        f"{SYNTRA_URL}/tenants/prod/jobs/retry/capsules/policy/feedback",
        headers={"Authorization": f"Bearer {SYNTRA_TOKEN}"},
        json={
            "decisionId": request.decision_id,
            "reward": 1.0 if success and latency_ms < 800 else 0.0,
        },
        timeout=0.5,
    )
```

## What changes operationally

- The quarterly tuning meeting goes away. The bandit re-balances
  weights as feedback arrives.
- You watch `/report` and `/admin/capsules` for which option is
  winning in which context. The dashboard's "Strategy Weights"
  card surfaces the live distribution.
- If your traffic shifts (new tier, new region, new latency regime),
  the per-context ADWIN detector fires and the bandit re-warms for
  that context only — without disturbing other contexts that are
  still converged.
- The old rules become the **warmup distribution**: in the first ~30
  rounds the bandit explores uniformly across options. If your old
  rules were good, that warmup quickly converges back to a similar
  policy. If they were bad, it diverges — and you find out cheaply.

## Failure modes to expect

- **Cold start**: the first ~30 decisions per context bucket are
  exploratory. If a context appears rarely (e.g. one tier × region
  combination that gets 2 requests/day), it may never converge. Mix
  rarely-seen contexts into a broader feature dimension or accept
  that they keep exploring.
- **Reward signal noise**: if your reward function flips bits at
  random (e.g. success-rate has high variance per request), the
  bandit needs more rounds. Wider confidence intervals on `/report`
  are the diagnostic.
- **Fallback path**: when Syntra is unreachable or refuses (see
  [Debugging refusals](../operations/debugging-refusals.md)), your
  service falls back to the static rules it already has. Don't
  remove the rules until you're confident the bandit has converged.
