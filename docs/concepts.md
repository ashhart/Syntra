# Concepts: contextual bandits, in plain terms

This page answers three questions: what a contextual bandit is, how Syntra
runs one, and when the framing fits your problem. It assumes no background
in reinforcement learning. For commands, start with
[quickstart.md](quickstart.md); for the endpoints, [api.md](api.md).

## The shape of the problem

A service makes the same kind of decision many times a minute. For each
request it picks one of K options: which model answers, which backend
serves, which retry policy applies, which offer is shown. The options
differ in cost, latency and quality, and which one is best depends on the
request in front of you and on conditions that change over the day, the
week and the life of the deployment. The outcome (did it work, was the
answer good, did the customer come back) arrives seconds to weeks after
the choice.

You want the outcome to be good on average over thousands and millions of
decisions, not on any single one. A single decision is noisy, and you will
never learn which option would have been right for that one request.

Supervised learning does not fit this. It needs labeled pairs of
`(request, correct option)`, and you never get them. When you decide,
nobody knows the right option, and afterwards you only see the outcome of
the option you picked. The other K-1 were not tried for that request. This
is partial feedback, or bandit feedback, and it is what separates this
problem from classification.

Hand-written rules work for a while and then stop working, for two
reasons. Once there are more than a few context features and a few
options, the mapping is too large to tune by hand. And it drifts. Traffic
changes, an upstream service changes, a provider ships a new model under
the same name. A table that was right when you tuned it is wrong now, and
keeping it right is what sends people looking for something that adapts.

## Bandits and contextual bandits

A multi-armed bandit is the formal name for choosing among K options whose
payoffs you do not know, learning about an option only by trying it. A
contextual bandit adds side information. Before each decision you see a
context that describes this request, and the best option is a function of
that context rather than one fixed answer. The cheap model may be right for
short chat messages and wrong for long code reviews; the learner's job is
to learn that mapping.

Every bandit balances exploration (trying options it is unsure about) and
exploitation (choosing what it currently believes is best). Pure
exploitation locks in early mistakes; pure exploration wastes traffic on
bad options.

Syntra's API follows the shape of the problem: `POST .../decide` with the
context returns an action, the probability it was chosen with and a
`decisionId`; later, `POST .../reward` with that `decisionId` reports how
it went.

## Delayed feedback

Textbook bandits assume the reward arrives the moment you choose. Real
outcomes arrive later. A grader scores an answer minutes later, a
chargeback lands weeks later, a success is only reported when it happens.
Meanwhile the service keeps deciding.

Syntra joins a reward to its decision by `decisionId`, whenever it
arrives, in any order, from any process. Decisions and rewards are both
stored in `syntra.db`. A capsule keeps the first reward per decision
(`rewards: "first"`) or adds them all (`"sum"`, with idempotency keys so a
retried reward counts once). When your application reports only successes
(a click, a purchase), `reward.default` with `reward.waitSeconds` gives every decision
that got no reward in time a default one, so the model also learns what a
miss looks like.

## How Syntra decides

A capsule is one decision point. Its spec lists the actions (ids and
optional features), the exploration and learner settings, the reward
range and the mode.

- **Features.** The context and each action's features are JSON. They are
  flattened (nested objects become dotted names; numbers stay numbers;
  strings and booleans become indicators such as `task=code`), hashed into
  a fixed number of slots (`2^learner.bits`), and crossed, so the model
  scores each action with weights on context-feature-by-action-feature
  pairs. That
  is how it learns that `task=code` favors the large model, and how it
  generalizes to contexts it has not seen exactly.
- **Learner.** One online least-squares model per capsule. It predicts the
  reward of each action for a context and updates on every reward.
  `learner.learningRate` (default 0.5) trades speed for steadiness, and no
  value wins everywhere. In `examples/learning_bench.rs`, lowering it from
  0.5 to 0.1 left a stationary problem unchanged, cost a problem whose
  best actions change halfway 6% of the oracle's reward (0.985 to 0.921),
  and cost a 200-item catalog 3% (0.792 to 0.770). On the simulated LLM
  routing example (rewards with noise, routes 0.04 apart), 0.1 was better:
  the learned route stopped swinging from one published model to the next
  and the run earned 1% more. For noisy rewards and close actions start
  at 0.05 to 0.1; for drifting problems keep 0.5; and evaluate a change
  on your own logs before promoting it.
- **Exploration.** The default is SquareCB. The predicted best action gets
  most of the probability, and every other action gets a probability that falls
  with the gap between its prediction and the best one's, and with the
  number of updates so far. `epsilonGreedy` is the alternative. Either way
  a floor keeps every eligible action at or above `floor / K`.
- **Modes.** `learner` serves the learned policy and keeps learning.
  `baselineExplore` serves your incumbent action (`baselineAction` in the
  request) most of the time and spreads `baselineEpsilon` over all actions,
  a safe way to start logging next to an existing rule. `frozen` serves
  the learned policy and records rewards without learning.

The draw is made with a logged seed, so the server can reproduce any
decision exactly, including decisions SDKs make in-process.

## Why every decision logs its probability

Because each decision is drawn at random from a known distribution, the
log can answer a counterfactual question. What would another policy have
earned on this same traffic? Off-policy evaluation reweights each logged
reward by how much more (or less) likely the other policy was to choose
the logged action. Syntra reports several estimators: IPS (plain
reweighting, unbiased but noisy), SNIPS (normalized, steadier), DM (a
reward model's prediction, steady but only as good as the model) and DR,
doubly robust, which combines the model with the reweighted correction
and is the one the recommended gate uses (`lift.dr.lower >= 0` means the
candidate beats what was served at the 95% level). DM comes without an
interval: its error is mostly the model's, which resampling the log does
not measure. Intervals that held the model fixed covered the true value in
14% of simulated datasets, against 95% for DR's
([benchmarks](../benchmarks/README.md)).

This only works if two things hold, and Syntra enforces both:

- Every action the other policy might choose must have had a nonzero
  probability when the decision was logged. The exploration floor
  guarantees it.
- The logged probabilities must be the ones actually used. The server logs
  them itself, and it replays every decision an SDK uploads and refuses
  the ones that do not reproduce. A decision whose model the server no
  longer has (after a restart) cannot be replayed. Syntra keeps it, so its
  reward still trains the model, but marks it unverified and leaves it out
  of evaluation.

The report says how much to trust it: confidence intervals, the effective
sample size (how many rows the reweighting effectively rests on), weight
diagnostics, and a verdict in words. A policy that would choose actions
the log rarely took gets a wide interval, which a gate on the lower bound
refuses.

## Reward functions are the bottleneck

The hard part of running a bandit is rarely the bandit. It is the reward:
the learner optimizes exactly what you score, and a score that looks
reasonable can be blind to what you care about. Before you trust a reward
function, build a small ladder of cases where you know which outcome is
better and check that the reward ranks them in that order. If it does
not, no learner will recover from it. Keep the pieces of the reward (for
example quality, cost and latency) in the reward's `detail` so you can
recompute and audit them later.

## When Syntra fits, and when it does not

It fits repeated choices among a discrete set of options, where the best
option depends on context and the outcome can be measured and reported,
even late. Examples: model routing, backend or queue selection, retry and timeout
policies, ranking which offer or article to show.

Pick something else for:

- **Prediction with labels.** If you know the right answer at training
  time, use supervised learning; exploration would waste traffic.
- **Continuous actions.** Syntra chooses among discrete actions. You can
  bucket a price or a timeout into actions, but Bayesian optimization or
  continuous-action methods handle knobs natively.
- **One-off decisions.** Without repetition there is nothing to learn
  from.
- **No measurable outcome.** If you cannot compute and report a reward,
  build the measurement first.
- **Actions that change future state.** If today's action changes
  tomorrow's situation (not just today's reward), you have a
  reinforcement-learning problem; bandits assume each decision stands on
  its own given its context.
- **Deciding whether to ship at all.** An experimentation platform answers
  "is B better than A, should we launch it"; a bandit decides which of the
  launched options to use for each request. They complement each other.

A useful test is whether you can write down, on one page, the reward for
each decision and how you compute it from the outcome. If you can, and it
ranks known cases correctly, the framing fits.

## Read next

- [quickstart.md](quickstart.md): decide, reward, decide in-process,
  evaluate and promote, on one machine.
- [examples/llm-routing](../examples/llm-routing/): model routing on
  simulated models, learning and then evaluated from the store.
- [operating.md](operating.md) and [deployment.md](deployment.md).
- [design/v2-decision-core.md](design/v2-decision-core.md): the learner,
  exploration, feature hashing and the evaluation protocol in detail.
