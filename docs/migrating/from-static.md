# Migrating from a static retry / fallback policy to Syntra

This guide is for engineers who already have a working static policy — "retry
three times with 100ms exponential backoff", or "always use model B for
enterprise users" — and want to understand what actually changes when they
adopt Syntra. The short answer is: less than you think, and the parts that
change happen below your application's control flow.

## Background reading

- [Bandit primer](../concepts.md) — explains contextual bandits, delayed
  feedback, and why heuristic tables plateau.
- [30-minute walkthrough](../tutorial.md) — end-to-end: author a capsule,
  drive it, read what was learned.
- [API reference](../api.md) — every endpoint described with request/response
  shapes.
- [Canonical Python integration](../../examples/retry-tuning/) — the
  `RetryClient` example that this guide refers to throughout.
- [Offline evaluation](../../examples/offline-eval/) — if that directory is
  present in your checkout, it contains IPS/DR tooling for estimating what
  Syntra would have done on your historical logs before you go live.

---

## What changes

The single biggest change is that you stop maintaining a lookup table and start
maintaining a reward function. Those are different things.

With a static policy, someone on your team decided — at some point, probably
under time pressure — that three retries with 100ms backoff was right. They
tuned it against the failure rates they saw that week, on the endpoints they
had time to characterize, with the upstream SLAs that were in force then. The
decision was captured in a config file or a constant in the source and it has
been there ever since. When the upstream changed, nobody updated it. When a new
endpoint was added, it inherited the same policy. When traffic patterns shifted
to favor a different retry cadence on Tuesday afternoons, the static policy
stayed static.

With Syntra, you stop tuning the mapping and start defining what "a good
outcome" means. You write a reward function — in the retry-tuning case, that
means: success is 1.0, failure is -1.0, and total latency subtracts a small
penalty. Syntra handles the mapping from request context to which policy option
to apply, and it updates that mapping continuously as outcomes arrive.

The cognitive model is different. You are not thinking "what retry count is
right for endpoint X". You are thinking "what does success mean for this
category of request, and how do I measure it." Once the reward function is
sound, Syntra's job is to find the mapping. Your job is to make sure the reward
function is measuring what you actually care about. The [concepts.md reward
blindness section](../concepts.md#reward-functions-are-the-bottleneck) is worth
reading before you define yours.

## What stays the same

Your application code does not change in a meaningful way. The control flow —
make request, handle failure, return response — stays entirely in your service.
Syntra does not sit in the request path in any load-bearing sense. The
integration shape from the retry-tuning example illustrates this precisely: the
`RetryClient` is a thin wrapper around `requests` that calls `/decide` to pick
a policy, executes your HTTP call with that policy, then calls `/feedback` with
the outcome. If Syntra is unreachable, the client falls back to a configured
static policy and your request succeeds anyway. An outage in Syntra degrades
adaptive retry to "always fall back" — it does not break anything.

During shadow mode, the regression surface is zero: Syntra observes and learns
from decisions your service is already making, but it does not influence any
of them. You can run for two weeks in shadow mode, build up a history of
learned weights, inspect the `/report` and `/contexts` endpoints, and only
then flip to letting Syntra's suggestions drive live behaviour. That transition
is a one-line code change in the client.

## Implementation steps

### Step 1: author the capsule

Write your existing policy options as a YAML spec. For the retry-tuning case,
the canonical spec looks like this — taken directly from
[`examples/retry-tuning/setup_capsule.py`](../../examples/retry-tuning/setup_capsule.py):

```yaml
name: retry-tuning
options:
  - none
  - single
  - triple
  - exponential_fast
  - exponential_slow
reward:
  type: continuous
  range: [-1.0, 1.0]
```

Your existing static policy is somewhere in this list. If you have been using
"triple retries with exponential backoff", that maps to `exponential_slow` or
`triple` depending on your timing. The point is that you start from your
existing options rather than inventing new ones. Syntra will learn which of
the listed options performs best under which conditions; you do not need to
know the answer in advance.

Compile it:

```bash
syntra author retry-tuning.yaml --out-dir ./retry-capsule/
```

Smoke-test convergence locally before touching any real infrastructure:

```bash
syntra simulate retry-tuning.yaml \
  --rounds 5000 \
  --true-arm-rewards "0.0,0.5,0.7,0.8,0.4" \
  --seed 42
```

This drives a synthetic trial with five options and prints the convergence
trace. The expected output shows the bandit abandoning the lower-reward options
within a few hundred rounds. If it doesn't, the reward range or the option
ordering is probably wrong.

### Step 2: install and configure feature context

POST the compiled binary to Syntra and attach the feature-context learning
config. The feature set shown here is the one from the retry-tuning example
— `recent_failure_rate`, `p99_latency_ms`, and `hour` — which maps closely to
the context your existing static policy ignores:

```bash
curl -X POST $SYNTRA_URL/tenants/myteam/jobs/retry/capsules/router/install \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @./retry-capsule/program.lyc

curl -X PUT $SYNTRA_URL/tenants/myteam/jobs/retry/capsules/router/learning \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "contextSpec": {
      "type": "features",
      "features": [
        {"name": "recent_failure_rate", "type": {"kind": "continuous", "range": [0.0, 1.0]}},
        {"name": "p99_latency_ms",      "type": {"kind": "continuous", "range": [0.0, 5000.0]}},
        {"name": "hour",                "type": {"kind": "cyclic", "period": 24.0}}
      ]
    }
  }'
```

The `cyclic` kind for `hour` is important: it encodes the hour as sin/cos so
the model understands that hour 23 and hour 0 are adjacent, rather than
treating time as a linear scale that wraps discontinuously.

### Step 3: wire in the Python client in shadow mode

Install the integration library and point a `RetryClient` at your capsule:

```python
import os
from syntra_retry import RetryClient

client = RetryClient(
    syntra_url=os.environ["SYNTRA_URL"],
    capsule_path="/tenants/myteam/jobs/retry/capsules/router",
    admin_key=os.environ["SYNTRA_ADMIN_KEY"],
    fallback_policy="triple",   # your existing static policy
)

# In your request handler:
response = client.request("GET", "https://api.example.com/users")
```

In shadow mode, the `RetryClient` calls `/decide` to get a suggestion, then
ignores it and applies `fallback_policy` instead — but it still calls
`/feedback` with the real outcome. This is how the bandit accumulates a
learning history without influencing live behaviour. The
[`RetryClient` source](../../examples/retry-tuning/) shows the exact control
flow and the fallback chain.

Shadow mode is the default until you pass `shadow=False` to the constructor.
There is no other change required. The application code that calls
`client.request(...)` is identical in shadow and live modes.

### Step 4: watch the dashboard for approximately 1,000 requests

Open the admin console at `http://localhost:8080` (if using the demo image) or
at your Syntra appliance's `/admin` endpoint. Within the first 30 feedback
rounds the lifecycle flips from Warmup to Active and the meta-bandit panel
starts populating. You are watching for two things:

1. **Context coverage.** Hit `/contexts` to confirm that requests are landing
   in the context buckets you expect. A capsule with feature context creates
   synthetic buckets from the encoded feature vectors; the bucket distribution
   should reflect your actual traffic shape.

2. **Weight drift.** In `/report`, the option weights start at roughly even
   (1/K each) and should start separating within a few hundred feedback rounds.
   If your static policy happened to be the best option uniformly, you will see
   it pull ahead. If another option is better in some context, you will see the
   weights diverge by context. Either finding is useful information.

1,000 requests is a rough threshold, not a hard one. Sparse feedback (i.e.,
feedback that arrives days after the decision) means you need more calendar
time, not necessarily more requests, before the weights are meaningful.

## Common pitfalls

### Delayed feedback breaks attribution

The most common mistake when migrating from a static policy is to implement
the feedback call in a way that loses the `decisionId`. The `decisionId` is
returned by `/decide` and must be passed back to `/feedback` when the outcome
arrives. If your outcome resolves in a different process, a different
microservice, or after a queue hop, you need to propagate the `decisionId`
through that path and post it back from whatever system knows the eventual
result. A feedback posted without a `decisionId` (using the raw
`strategyId/option/contextKey` form documented in the API) bypasses the
decision-log lookup and loses refusal accounting and meta-bandit
context-binding — use it only as a last resort.

### Reward function design is harder than it looks

A reward of 1.0 for success and -1.0 for failure is a reasonable starting
point but it is not always right. If some failure modes cost ten times what
others do — a 503 that recovers on retry versus a timeout that pins a thread
for five seconds — your reward function should reflect that. The `reward_spec`
endpoint lets you define named components with weights:

```bash
curl -X PUT $SYNTRA_URL/tenants/myteam/jobs/retry/capsules/router/reward_spec \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"success": 1.0, "latency_ms": -0.002, "cost": -0.5}'
```

Then `/feedback` callers can post components instead of a scalar:

```json
{"decisionId": "dec_...", "components": {"success": 1.0, "latency_ms": 1240}}
```

The server reduces them to a scalar using the spec. The monotonicity check
described in [concepts.md](../concepts.md#reward-functions-are-the-bottleneck)
is worth running before you put any reward function in front of real traffic:
build a ladder of policies you believe are strictly ordered by quality and
verify the reward scores increase along the ladder.

### Shadow mode is not free

Shadow mode accumulates a learned history, but that history is based on
outcomes from your static policy, not from Syntra's suggestions. This means
the learned weights in shadow mode are an estimate of which option would have
performed best given the decisions your static policy actually made — they
are not a direct counterfactual of what Syntra would have chosen. The
[offline-eval directory](../../examples/offline-eval/) (see that directory if
present) contains IPS and Doubly Robust estimators that can use your
shadow-mode decision log to produce an unbiased estimate of what Syntra's
policy would have earned, accounting for the mismatch between what was chosen
and what was observed.

### Live mode is a one-way door in the short term

Once you flip to live mode and the bandit starts influencing decisions, the
learned weights reflect a mix of your static policy's history and the bandit's
own choices. Rolling back to shadow mode is easy — it is a config change —
but the learned state doesn't roll back with it. If you're not confident in
the reward function or the feature set, stay in shadow mode until you are.
There is no penalty for leaving shadow mode on for a month.

---

For any endpoint behaviour not covered here, see [api.md](../api.md). For
what shipped in each platform phase and the known debt list, see
[CHANGELOG.md](../../CHANGELOG.md).
