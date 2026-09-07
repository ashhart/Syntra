# Concepts: contextual bandits, in honest terms

This document is a concept tutorial. It answers two questions: what is a
contextual bandit, and when does it actually fit the problem you have? It
assumes you're an engineer who has not used one before. It does not assume
any background in reinforcement learning.

If you came here for a 30-minute walkthrough, skip to
[`tutorial.md`](tutorial.md). If you came here for the endpoint surface,
skip to [`api.md`](api.md). This file is the one to read when you're trying
to decide whether the bandit framing is right for your problem at all.

## The shape of the problem

Picture a service that handles two pages of decisions per minute. Each
request, your code has to pick one of K discrete options: which LLM to
route to, which retry policy to apply, which fraud threshold band to use,
which ranking weight to send a candidate through. The options are
distinguishable — they have different cost, different latency, different
quality — but which one is *best* depends on the request you're looking at
right now, and on conditions that change over the day, over the week, over
the lifetime of the deployment. The outcome — did this work? did the
customer come back? did the chargeback land? — resolves seconds to weeks
later, not at the moment you made the choice.

You want to optimize the cumulative outcome over time. Not the outcome of
the next decision in isolation — there's noise in any single decision and
you'll never resolve which option was "right" for one specific request —
but the running sum, the long-run average, the rate at which good things
happen relative to bad things across thousands and millions of decisions.

The instinct of most engineers, reaching for this shape of problem the
first time, is supervised learning. Collect labels, train a classifier or
regressor, predict the right option for each request. That instinct is
wrong for the kind of problem above. Supervised learning needs labels at
training time — a corpus of `(request, correct_option)` pairs. You do not
have those. At the moment a decision is made, nobody knows which option
was correct; the outcome that will tell you hasn't happened yet. After it
has happened, you only learn the outcome of the option you *picked*. The
other K−1 options were never tried for that request and never will be.
This is called *partial feedback* or *bandit feedback*, and it is the
thing that distinguishes the bandit problem from the classification
problem.

The second instinct is heuristics: hand-tune thresholds, hardcode the
mapping from context to option, ship it. This works for a while. It plateaus
for two reasons. The first is that the search space is bigger than
intuition can cover — once you have more than three or four context
features and four or five options, you cannot tune the K-by-features
mapping in your head. The second is drift. The traffic shape changes,
the upstream service changes, a new fraud vector appears, the LLM provider
deploys a new model under the same name. Your hand-tuned mapping was
right at the time you tuned it and is wrong now. The maintenance burden of
keeping a heuristic table current across a moving target is what makes
people go looking for adaptive infrastructure in the first place.

## Bandits and contextual bandits

A *multi-armed bandit* is the formal name for the problem of choosing
among K options under uncertainty, where each option has a reward
distribution you don't know in advance, and you learn about each option
only by trying it. The name comes from slot machines — multiple arms,
unknown payouts, finite budget of pulls. The mathematical problem is the
same shape as your problem: pick from K options, observe a noisy reward
from the one you picked, repeat, try to maximize the running total.

A *contextual* bandit is the same problem with side information. Before
each decision, you see a feature vector — the context — that describes
this particular request. The right option in general isn't a fixed
answer; it's a function of the context. A high-traffic, low-latency
context might want the cheap LLM. A low-traffic, accuracy-sensitive
context might want the expensive one. The bandit's job is to learn that
mapping from context to best option, not to learn one globally best
option.

Every bandit, contextual or not, has to balance two things: trying
options it hasn't sampled enough to be sure about (exploration), and
preferring options it currently believes are best (exploitation). Pure
exploitation locks in early, suboptimal beliefs and never recovers. Pure
exploration spends all of its budget on the bad options. The interesting
algorithms — the ones in the next section — are different recipes for
balancing the two, with different sensitivities to noise, different
warmup behavior, and different requirements on the shape of the reward.

Syntra is a contextual bandit appliance. You hand it the context for each
decision via `/decide`, it returns an option and a `decisionId`, and you
report the eventual outcome back via `/feedback` against that
`decisionId`. The shape of the API matches the shape of the problem.

## What "delayed feedback" means and why it matters

The textbook bandit assumes the reward arrives the instant you pull the
arm. Almost no production problem is shaped like that. Fraud loss
materializes when chargebacks come in, weeks after the transaction was
approved. LLM response quality is judged asynchronously, by another model
or by a downstream metric or by a user action that happens minutes to days
later. Even retry-policy outcomes — which look fast, because the retry
either succeeded or it didn't — depend on tail latency that you only see
after the request resolves, and on real-user impact that you only see at
aggregate scale.

Delayed feedback breaks a lot of bandit implementations that assume
synchronous reward. The decision and the feedback have to be linked
explicitly across the delay, the bandit has to keep making other decisions
in the meantime, and feedback can arrive out of order, late, or never. If
your bandit library can't handle the gap, you end up either blocking the
decision path on a reward that hasn't happened yet, or losing the
attribution between the decision and the eventual outcome.

Syntra's API is built around the gap. Every `/decide` response carries a
`decisionId`. Feedback is posted later, by `decisionId`, with no
assumption about when "later" is. You can deliver feedback in any order.
You can deliver it from a different process or a different machine than
the one that called `/decide`. You can deliver it minutes, hours, or days
afterward. The decision log and the feedback log are kept separately —
`decision.jsonl` and `feedback.jsonl` in the persistent store — and joined
on `decisionId`. This is the assumption every production bandit problem
needs and many bandit libraries don't make.

## Algorithm shapes, in honest terms

There is no one bandit algorithm. There are several, each good under
different conditions. Syntra's meta-bandit runs six in parallel — Thompson
sampling, UCB1, EpsilonGreedy, Weighted, Greedy, and LinUCB — and
converges on whichever performs best on your traffic, so in practice you
do not need to pick one. But understanding the candidates helps when you
look at `/report` and see the meta-bandit's selection over your six and
want to know what that means.

*Thompson sampling* maintains a posterior distribution over each option's
reward and picks an option by drawing a sample from each posterior and
choosing the highest. It explores in proportion to its uncertainty, which
is the property you want: an option the bandit knows is bad gets sampled
rarely; an option the bandit isn't sure about yet gets sampled often
enough to resolve the uncertainty. It works well on most stationary or
slowly-drifting problems with reasonably well-shaped rewards. It is less
good when the reward distribution is heavy-tailed in a way the posterior
doesn't capture, or when rewards are very sparse — the posterior takes a
long time to tighten.

*UCB1* (upper confidence bound) picks the option with the highest
optimistic estimate of its reward — current mean plus a confidence-width
bonus that shrinks as the option gets sampled. It has the cleanest
theoretical guarantees among the classical algorithms, but it is also
prone to over-exploring in the early rounds (the bonus dominates), and
its tuning is sensitive to the reward range. Good when reward is bounded
and roughly stationary; less good in non-stationary settings where the
mean-of-history isn't representative of the current regime.

*EpsilonGreedy* is the simplest: with probability ε, pick a random
option; with probability 1−ε, pick the one with the best observed mean.
It is robust, it is easy to reason about, and it is almost never the best
choice — but it is rarely catastrophically bad, which makes it a useful
baseline in the meta-bandit's portfolio. It will be the meta-bandit's
pick when the smarter algorithms haven't accumulated enough data to
differentiate themselves yet.

*LinUCB* is UCB applied to a linear model of reward as a function of the
context features. It's the only one of the six that uses the feature
vector. If you declare `contextSpec: features` in `learning.json`, the
meta-bandit's candidate list includes LinUCB; if you use the discrete
context default, LinUCB drops out of the candidate set automatically and
the meta-bandit runs five. LinUCB is good when reward really is roughly
linear in your features; it is bad when reward depends on feature
interactions or is highly non-linear, and it is sensitive to features
that are wildly out of scale or full of NaN/Inf (the implementation
defends against this but cannot rescue a feature set that doesn't
predict).

You will notice this section does not have equations. The intuition is
the goal here; the math is in the source. The honest summary is that no
single algorithm dominates and Syntra picks among them adaptively rather
than asking you to commit.

## Reward functions are the bottleneck

The hard part of running a bandit in production is almost never the
bandit. It is the reward function. The bandit can only optimize what you
tell it to score, and writing a reward function that scores what you
actually care about — without subtle blind spots — is harder than it
looks.

The companion writeup [`reward blindness`](../../writeup_reward_blindness.md)
documents a concrete case: five policies with a 4× spread in the outcome
they care about (47 to 186 deaths prevented) scoring within 0.056 points
of each other under a reward function that looks reasonable. The
mechanism was a counterfactual baseline that depended on the policy's own
behavior — when prevention succeeded, the credit pool shrank — and the
symptom was a reward function that could not rank the policies by the
outcome it was meant to optimize. The check that caught it was
monotonicity: build a ladder of strictly-better policies and verify the
reward scores increase monotonically along the ladder. If it doesn't, the
reward function is insensitive to the dimension you actually care about,
and no amount of clever bandit algorithm will recover from that. Before
you put a reward function in front of Syntra, run the monotonicity check.

## When Syntra is the right answer; when it isn't

Syntra fits when the problem has the bandit shape described above:
repeated discrete-option decisions, context-dependent best choice, and
outcomes that resolve with a delay. The four canonical examples — LLM
model routing, HTTP retry policy, fraud-threshold action bands, and
queue/route/ranking selection — are all in that shape, and the demo and
the retry-tuning example exercise the same machinery against the second.

It does not fit, and you should pick a different tool:

- **Forecasting, classification, regression.** You have labels at
  training time and you want to predict an outcome. Use a model
  framework. The bandit's exploration step is wasted budget when the
  supervised setup gives you full feedback for free.
- **Continuous-valued action spaces.** Syntra picks among a discrete set
  of options. If the action is a knob — set a temperature, set a price,
  set a timeout — the bandit framing is a forced quantization. There are
  algorithms (Bayesian optimization, continuous-armed bandits,
  policy-gradient RL) that handle this natively. Syntra is not one of
  them.
- **One-shot decisions.** Without a feedback loop, there is nothing for
  Syntra to learn from. A single high-stakes decision is a decision-theory
  problem, not a bandit problem.
- **Problems where you don't have a feedback signal.** If the outcome
  you care about cannot be measured and posted back to `/feedback`, no
  bandit can optimize it. Build the measurement first.
- **Reinforcement learning with state transitions.** If the action
  changes the state of the world in a way that affects future rewards —
  not just this decision's reward — you have an RL problem, not a bandit
  problem. Bandits assume each round is independent given the context.
- **Experiment / feature-flag platforms.** A bandit picks which of K
  options to use once you have decided to deploy them. An experiment
  platform tells you whether to deploy at all. These are adjacent tools,
  not substitutes.

A useful smoke test: can you write down, on one page, what feedback gets
posted to `/feedback` and how it's computed from the eventual outcome? If
the answer is yes and the answer survives the monotonicity check, the
bandit framing fits. If the answer is no, fix the measurement before
reaching for the bandit.

## What to read next

- [`tutorial.md`](tutorial.md) — a 30-minute walkthrough: install a
  capsule, drive it with the traffic generator, watch the lifecycle move
  from Warmup to Active, read the meta-bandit panel.
- [`api.md`](api.md) — the endpoint reference: `/decide`, `/feedback`,
  `/report`, `/contexts`, `/memory`, plus install, evolution, audit, and
  evaluate.
- [`operating.md`](operating.md) — the operator playbook: what to do when
  weights look wrong, how to read the audit log, when to re-warm, when to
  freeze.
- [`deployment.md`](deployment.md) — production deployment notes.
- [`examples/retry-tuning/`](../examples/retry-tuning/) — the canonical
  integration story, end to end, including the Python client that wraps
  `requests` and the fail-safe fallback path.

The README has the positioning and the marketing surface. This file is
the one to come back to when you're three weeks into running Syntra in
shadow mode and need to remember why the design choices are what they
are.
