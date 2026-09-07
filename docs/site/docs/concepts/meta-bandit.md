# Meta-bandit

The **meta-bandit** is Syntra's answer to "which bandit algorithm
should I use?"

The answer is: don't pick. Run seven in parallel, give each one a slice
of the traffic, watch which one is winning on this capsule's actual
reward distribution, and converge on it.

## What runs

Once a capsule transitions out of Warmup into Active, the meta-bandit
runs seven candidate algorithms in parallel on the same strategy node:

- **Thompson sampling** — maintains a posterior distribution over each
  option's reward and picks by drawing one sample from each posterior.
  Explores in proportion to uncertainty. Works well on most stationary
  or slowly-drifting problems with reasonably well-shaped rewards.
- **UCB1** — picks the option with the highest optimistic estimate
  (mean + confidence-width bonus). Cleanest theoretical guarantees
  among the classical algorithms; over-explores early; sensitive to
  reward range.
- **EpsilonGreedy** — with probability ε, pick a random option; with
  probability 1−ε, pick the best observed mean. Robust, easy to reason
  about, rarely the best choice but rarely catastrophically bad.
- **Weighted** — softmax-style probabilistic sampling proportional to
  estimated mean reward.
- **Greedy** — exploit-only baseline. Picks the highest-mean option
  every time. Included as the meta-bandit's worst-case anchor.
- **LinUCB** — UCB applied to a linear model of reward as a function
  of context features. Only fires when `contextSpec: features` is
  declared. Good when reward is roughly linear in features.
- **LinTS** — Thompson sampling applied to a linear model. Pairs with
  LinUCB; preferred when the posterior over feature-coefficient θ
  matters more than the upper confidence bound.

LinUCB and LinTS drop out of the candidate set automatically when the
capsule uses `contextSpec: discrete` — they need a feature vector to
operate.

## How the meta-bandit picks

The meta-bandit is itself a bandit. The "arms" are the seven
candidates above. The "reward" is the cumulative reward each candidate
has accumulated on this capsule's traffic.

A **rate-adaptive exploration schedule** governs how aggressively the
meta-bandit explores its candidate set early versus how aggressively
it exploits the leader late. Concretely: each strategy node holds one
meta-bandit state per `contextKey` (discrete) or per feature
neighbourhood (continuous), and the per-decide candidate selection is
softmax over cumulative-reward estimates with a temperature that
decays as more rounds resolve.

Two things to internalize:

1. **You do not pick the algorithm.** Even the Warmup phase picks an
   initial active algorithm from the reward characterization (UCB(c=2.0)
   for bounded-continuous, Thompson for Bernoulli, etc.). Once Active,
   the meta-bandit overrides any `algorithm:` you put in
   `learning.json` and picks per-decide.
2. **The convergence is per-capsule.** Two capsules in the same Syntra
   instance can converge on different candidates because their reward
   shapes differ. A retry-tuning capsule with binary success / fail
   reward will land on Thompson. A latency-optimizing capsule with
   continuous reward will probably land on LinUCB or UCB1.

## Convergence shape

In controlled tests, a clear winner emerges in roughly 30–50 rounds
after Warmup ends:

> In a controlled 100-round run where option 2 received reward 1.0 and
> the other three received 0.1, option 2 was chosen 14/25 times in
> rounds 26–50, 20/25 in 51–75, and 22/25 in 76–100 — 62/100 overall.
> Its weight climbed from `0.25` to `0.81`. The remaining ~40% of
> picks are the meta-bandit's other candidates exploring, which is
> intentional — the `min_exploration` floor keeps the bandit from
> fully locking in.
>
> — `examples/predictive-autoscaling/README.md`

The `min_exploration` floor is configurable via `learning.json`'s
`safety.minExploration`. The default is `0.02`. Lower it and the
bandit will converge harder on the leader at the cost of late
discovery if a better option appears. Raise it and the bandit will
keep a more even spread.

## Inspecting the meta-bandit

The meta-bandit's per-candidate trials and cumulative reward live in
`memory.json` under each strategy node's per-context state. The
admin console's meta-bandit panel renders this; for programmatic
access:

```bash
curl -s -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  "$SYNTRA/tenants/.../capsules/.../memory" | jq '.strategies[0].metaBandit'
```

For a quick read, `/report` is cheaper but does not surface meta-bandit
state — that is a known presentation gap, not a runtime issue. Inspect
`/memory` while debugging convergence.

The `decisions.jsonl` log records `candidateId` on every decision the
meta-bandit served, so you can also slice by candidate in offline
analysis:

```bash
# Which candidates served decisions in the last 1k rounds?
jq -r .candidateId < syntra-store/.../decision.jsonl | tail -1000 | sort | uniq -c
```

## What the meta-bandit is not

- **Not a stacking ensemble.** It does not average the seven
  candidates' predictions. It picks one candidate per decision and
  uses that one candidate's option choice. The other six observe the
  same outcome but it does not bias their internal state away from
  what they would have decided.
- **Not free of warmup.** Each candidate needs samples to differentiate
  itself. The first few hundred decides under the meta-bandit may look
  noisy — that is the candidates trying their hand in turn, not a bug.
- **Not a substitute for a sane reward function.** The bandit can only
  optimize what you tell it to score. If two policies that produce 4×
  different real-world outcomes score within 0.05 of each other under
  your reward function, no clever meta-bandit will recover from that.
  Run the monotonicity check first.

## Where to go next

- [Strategy node](strategy-node.md) — what the meta-bandit lives
  inside.
- [Drift detection](drift.md) — what re-warms the meta-bandit when the
  reward distribution shifts.
- [Refusal](refusal.md) — when the meta-bandit's confidence is too low
  to act on.
- [Predictive autoscaling demo](../examples/predictive-autoscaling.md)
  — convergence numbers from a real run.
