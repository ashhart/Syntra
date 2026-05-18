# Migrating from a homegrown bandit library to Syntra

This guide is for engineers who wrote their own bandit implementation — in any
language, at any sophistication level — and are trying to decide whether to
replace it with Syntra. The answer is not always yes. This guide tries to give
you the signals that distinguish "you should switch" from "you should not", and
if you decide to switch, a concrete path through the migration.

## Background reading

- [Bandit primer](../concepts.md) — explains contextual bandits, delayed
  feedback, the exploration/exploitation tradeoff, and when the bandit framing
  fits at all.
- [30-minute walkthrough](../tutorial.md) — Syntra end-to-end: author a
  capsule, drive it, inspect what was learned.
- [API reference](../api.md) — every HTTP endpoint with request and response
  shapes.
- [Canonical Python integration](../../examples/retry-tuning/) — the reference
  integration pattern, fail-safe fallback, and unit tests.
- [Offline evaluation](../../examples/offline-eval/) — if that directory is
  present in your checkout, it contains IPS and Doubly Robust estimators for
  estimating what Syntra would have earned on your historical decision logs.

---

## Signs you should switch

### You are rewriting drift detection again

The first version was simple. The second version added a sliding window.
The third version added a t-test after someone noticed the window-based
version lagged on sharp regime changes. If your team is on the third iteration
of what is nominally a "quick infrastructure improvement" to the drift
detection layer, that is a strong signal that drift detection is an
underestimated problem and you want it maintained by someone whose job is
specifically that.

Syntra runs two-layer ADWIN drift detection by default: a capsule-level
detector that triggers re-warmup when the global reward distribution shifts,
and per-context detectors that reset only the affected context bucket on
narrower shifts. This is already built, already tested, and already handling
the edge cases — late feedback, context buckets that don't drift in sync,
the distinction between noise and regime change. You don't have to maintain it.

### The bandit lives in one service and other teams want access

A homegrown bandit is usually born inside a single service to solve a single
decision problem. Then a second team sees it working and asks to use it for
their decision. Now you have a versioning problem, a deployment coordination
problem, and a "who owns the memory state when they update the model" problem.
The single-service design doesn't compose.

Syntra's `tenant / job / capsule` hierarchy exists precisely for this. Adding
a second team means creating a new tenant or a new job — it's a POST request,
not a deployment. Each tenant gets isolated memory, isolated logs, and isolated
policy configuration. Your service's learned weights are not affected by
another team installing a new capsule under their tenant path.

### You need multi-tenant isolation and you don't have it

This is a specific version of the above. If your bandit's memory is shared
across tenants — even if the decision inputs are different — you are in a
situation where one tenant's traffic can shift the learned weights in a way
that affects another tenant's decisions. This is difficult to fix correctly in
a shared-memory design; it typically requires either a separate model per
tenant (which reintroduces the deployment problem) or careful partitioning of
the weight vector (which is subtle to get right under delayed feedback).
Syntra isolates state per tenant by construction.

### You need a "refuse when uncertain" gate and you don't have one

A bandit in Warmup or with sparse coverage of some context region will make
low-confidence decisions that look exactly like high-confidence decisions from
the caller's perspective — the response format is the same. If you have
production paths where a low-confidence option is worse than falling back to
a safe default, you need a way for the bandit to say "I don't know; use your
default". Most homegrown bandit implementations don't have this because it
wasn't in scope when the library was written.

Syntra's refusal gate (Phase E, opt-in) returns `{"refused": true}` when the
OOD score exceeds a threshold or when the prediction interval is too wide at
the configured coverage level. Your service code handles it with a single
conditional. The gate is disabled by default — you enable it with a `PUT` to
`/learning` and set `refusal.enabled = true` along with thresholds that match
your tolerance for uncertainty.

### The drift detector and the memory store and the API are all mixed together

If the drift detection logic, the weight update logic, the persistence layer,
and the HTTP API for your bandit are all in the same module — and you have to
touch all four to change any one of them — the architecture is coupled in a
way that makes every improvement more expensive than it should be. Syntra
separates these concerns cleanly. You can update the learning config without
touching the capsule. The persistence layer writes atomically to `memory.json`
on every feedback round. The API surface is stable and documented.

---

## Signs you should not switch

### Your bandit is deeply specialized to a single domain

Syntra picks among a discrete set of options using a general-purpose meta-
bandit. If your homegrown implementation uses domain knowledge baked into
the update rule — a structured prior over option rewards, a custom
parameterization of the reward distribution, problem-specific exploration
that exploits the geometry of your action space — that specialization
probably matters for your performance, and Syntra's general-purpose algorithms
won't replicate it. "Deeply specialized" is a high bar; most applications
don't actually need it. But if you spent months tuning the algorithm to the
specific statistics of your reward distribution and you can demonstrate that
the tuning earned measurable improvement, don't discard that.

### You need sub-millisecond latency and the HTTP hop won't allow it

Syntra's `/decide` endpoint is an HTTP round-trip. On localhost this is
typically 1-5ms; on a LAN between containers it is 10-30ms; over any
meaningful network distance it is higher. If your decision path has a budget
of hundreds of microseconds — high-frequency trading, real-time audio, some
game physics decision loops — an HTTP call is not in budget. There is no
configuration that removes the network hop; the appliance design requires it.
In that regime, an in-process bandit is the right answer regardless of how
much operational overhead it creates.

### Your reward function needs to be in the same process as the bandit

Some reward functions are expensive to compute and cannot be serialized into
a JSON scalar that you POST over HTTP. If the reward function requires access
to large in-memory data structures, GPU computation, or the results of a
model inference that itself takes seconds — and you cannot extract a scalar
reward to pass to `/feedback` — the decoupled Syntra model does not fit. The
`/feedback` endpoint accepts a scalar `reward` or named `components` that it
reduces to a scalar on the server. It cannot invoke your application's reward
computation.

### You have fewer than a few hundred decisions per week

The bandit's learning loop requires enough data to distinguish options. Syntra's
warmup phase requires approximately 30 feedback rounds before the meta-bandit
activates. In practice, for the learned weights to be meaningfully better than
random, you need a few hundred feedback rounds, and for the meta-bandit to
have selected a good candidate algorithm, you usually need a few thousand.
If your decision volume is low enough that you expect to accumulate that data
only over months, a static heuristic may be a more honest choice — not because
Syntra won't work, but because the human investment of migration and monitoring
outweighs the expected benefit at that volume.

---

## Pragmatic migration

If you have decided to switch, the migration does not require a cutover
weekend. The recommended path is a side-by-side shadow comparison.

### Week 1-2: run Syntra in shadow mode alongside your library

Deploy Syntra as a sidecar. Keep your homegrown bandit authoritative. For each
decision:

1. Send the request context to Syntra `/decide`. Record the `decisionId` and
   the option Syntra suggested. Do not use Syntra's suggestion — your existing
   bandit is still making the live decision.
2. Your existing bandit makes the decision as usual.
3. When the outcome resolves, post `/feedback` to Syntra with the `decisionId`
   and the observed reward.

This accumulates a learned history in Syntra under your actual traffic
distribution. After one to two weeks (or after you have collected a few
thousand feedback rounds — whichever comes later), you have enough state to
compare decisions.

The comparison you want is: on the requests where Syntra suggested a different
option than your library chose, what was the outcome? The decision logs at
`/decisions` and the feedback logs at `feedback.jsonl` in the store give you
the raw data. The offline-eval tooling in
[`../../examples/offline-eval/`](../../examples/offline-eval/) (see that
directory if present) provides IPS and Doubly Robust estimators that produce
a reward estimate for what Syntra's policy would have earned on your logged
traffic, accounting for the propensity mismatch between what your library
chose and what Syntra would have chosen. This is the methodologically honest
comparison — a simple accuracy-of-suggestion count understates Syntra's
performance in cases where your library's choices were systematically biased.

### What to do with your old bandit's logs

If your homegrown library has been logging decisions and outcomes in any form,
that history can be used to estimate what Syntra would have done retrospectively.
Format each logged decision as a CSV row with columns:

```
decision_id, context_key, action, propensity, reward
```

The `propensity` field is P(action | context) under your old bandit's policy —
the probability that your library would choose that option for that context.
For a deterministic policy, this is 1.0 for the chosen action and 0.0 for all
others; for an epsilon-greedy policy, it is `1 - epsilon + epsilon / K` for
the greedy action and `epsilon / K` for the rest. Load the CSV and run the
estimator:

```python
from syntra_ope import load_csv, EvalPolicy, evaluate

log = load_csv("your_decisions.csv")

# Build an EvalPolicy from Syntra's converged weights
eval_policy = EvalPolicy.from_json("syntra_ope/converged_policy.json")

result = evaluate(log, eval_policy)
print(result.to_dict())
```

This gives you an unbiased estimate of Syntra's expected reward on your
historical traffic, with bootstrap confidence intervals. If the estimate is
above your library's observed mean reward (`logging_policy_mean_reward` in the
output), Syntra is likely better; if it is within the confidence interval,
the policies are statistically indistinguishable on this data.

### Gradually shift traffic

Once shadow-mode comparison shows parity or improvement, shift a fraction of
live traffic to Syntra. The integration client pattern from the retry-tuning
example makes this a code change of a few lines: pass `shadow=False` for the
fraction of requests you want Syntra to control, leave `shadow=True` for the
rest. Monitor both populations through your existing observability stack.
The `/report` endpoint gives you Syntra's current option weights per context,
which you can compare against the corresponding weights in your old library.

### Deprecate the homegrown library after parity is proven

"Parity is proven" means: Syntra's live reward rate is not statistically below
your library's live reward rate over a period long enough to cover the dominant
feedback delays in your problem. For retry policies where feedback arrives
within seconds, a week of side-by-side data is usually enough. For fraud
thresholds where chargebacks arrive weeks later, you need to wait for the
feedback to resolve before drawing conclusions.

When you deprecate, keep your old library's decision logs. They are the
ground truth for any future audit of what the system decided and why during
the transition period. Syntra's own `audit.jsonl` records the install events,
config changes, and drift detections during the migration; together they tell
the complete story.

---

## What Syntra will not replicate

To be specific about what you lose when you switch:

**Custom update rules.** If your library uses a Bayesian update specific to
the parametric form of your reward distribution, Syntra's six candidate
algorithms probably don't implement that rule. Thompson sampling approximates
it in the binary case; LinUCB approximates it for linear-Gaussian features.
Neither is exact.

**In-process memory access.** Syntra communicates over HTTP. Your library
communicates through function calls. The latency difference is real.

**Arbitrary reward computation.** `/feedback` accepts a scalar or named
components; it does not invoke application code.

**Feature interactions.** Syntra's `contextSpec.features` does not support
quadratic or cubic feature products. If your library's performance depends on
them, precompute the interactions in your application before sending to
`/decide`, or accept that Syntra's LinUCB will not capture the same
correlations.

These are documented limitations, not roadmap items with promised timelines.
If any of them is a blocker for your use case, the right answer is to note it
and stay on your current implementation.

---

For endpoint details, see [api.md](../api.md). For what shipped in each
platform phase, what is known debt, and what the multi-node graph limitation
is in its current form, see [CHANGELOG.md](../../CHANGELOG.md).
