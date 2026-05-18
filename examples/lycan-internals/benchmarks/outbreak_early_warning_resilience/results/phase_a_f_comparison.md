# Outbreak benchmark — Phase A-F comparison

Re-running the outbreak-early-warning benchmark against a Phase A-F Syntra
capsule (warmup → reward characterization → rate-adaptive meta-bandit) and
comparing against three legacy single-algorithm configs and the hand-coded
baselines.

**Config:** 10 seeds (2000–2009) × 52 weeks × 4 regions. Same simulator and
same baselines as `results/v3_full/`.

## Results

### Syntra under four algorithm configurations

| Configuration   | Deaths (mean) | Econ cost $M | Original score | Δ vs `v3_full` |
|---|---:|---:|---:|---:|
| weighted        | 1.2 | 26,357 | −10.94 | 8.6 fewer deaths, $15B more econ |
| epsilon_greedy  | 0.8 | 25,982 | −11.01 | 9.0 fewer deaths, $15B more econ |
| ucb             | 1.6 | 26,390 | −10.81 | 8.2 fewer deaths, $15B more econ |
| **meta_bandit (Phase A-F)** | **0.5** | **26,332** | **−10.94** | **9.3 fewer deaths, $15B more econ** |
| v3_full (legacy, archived)  | 9.8 | 10,993 | −4.48 | — |

The four Phase A-F configurations are statistically indistinguishable on every
output dimension. Deaths span 0.5–1.6, economic cost spans
$25.98B–$26.39B, score spans −10.81 to −11.01 — within noise of each other
across 10 seeds.

### Hand-coded baselines (identical across all configs — same seeds, deterministic sim)

| Policy              | Deaths | Econ $M | Score    |
|---|---:|---:|---:|
| none_always         |  826   |     0   |  −0.61   |
| lockdown_always     |    0   | 179,447 | −62.38   |
| threshold           |  233   |   134   |  −0.12   |
| reactive            |  477   |   125   |  −0.31   |
| lagged_oracle       |  197   |   199   |  −0.12   |
| myopic_oracle       |  186   |   178   |  −0.10   |
| proactive_t5_lvl1   |   47   |   330   |  −0.16   |
| horizon_oracle      |  368   |   112   |  −0.20   |

### Pass/fail criteria (all four Phase A-F configs)

| Criterion                                  | Pass | Value |
|---|:---:|:---:|
| c1: Syntra beats none + lockdown on score  | ✗ | 0% |
| c2: Syntra beats threshold + reactive      | ✗ | 0% |
| c3: Syntra fewer deaths than threshold     | ✓ | 100% |
| c4: Syntra cheaper than lockdown_always    | ✓ | 100% |

Overall: 2/4 pass — same outcome as `v3_full`. The pass/fail surface is
**unchanged** by the move to the Phase A-F learner.

## Phase A-F features observed in the meta_bandit run

- **Warmup → Active transition fired.** All 10 seeds completed warmup
  within the first 30 feedbacks. Reward characterization classified the
  reward stream as `BoundedContinuous { min: −0.499, max: 0.0 }` and
  installed `UCB { c: 2.0 }` as the post-warmup algorithm in all 10
  seeds — consistent reward shape, consistent pick.
- **Meta-bandit recorded trials.** After 386 total feedback rounds on the
  final seed of the `meta_bandit` run, candidate trials were:
  - Thompson: 20.2  (mean reward −0.286)
  - Ucb:     152.3  (mean reward −0.268) ← leader
  - Weighted: 28.5  (mean reward −0.307)
  - EpsilonGreedy: 26.9 (mean reward −0.269)
  - Greedy:   92.4  (mean reward −0.271)

  UCB led on trials in all four runs (weighted: 98.8, epsilon_greedy: 198.6,
  ucb: 114.0, meta_bandit: 152.3). The meta-bandit's choice is stable and
  algorithm-pin-independent on this benchmark.
- **LinUCB did not appear** because the capsule uses discrete context keys
  (`region_{i}_{low|moderate|high|critical}`). The meta-bandit registers the
  5-candidate `discrete_only` portfolio; LinUCB is a feature-context-only
  candidate. Switching the capsule to a feature-vector context spec
  (e.g. `case_rate_per_100k` as continuous + `region_id` as categorical)
  would enroll LinUCB, but that's a separate experiment.
- **Refusal stayed off** (`refusal.enabled = false`) as intended for a
  benchmark — every request gets a decision.
- **Change detection** is enabled in `learning.json` (PageHinkley, threshold
  3.0). No `change_detected` events fired across the 10 seeds — the
  reward stream is stationary modulo noise, and the detector correctly
  did not trip.

## What the run actually shows

The Phase A-F learner converges harder on the reward-function-optimal policy
than the legacy run did. The result is that Syntra prevents almost all
deaths (0.5–1.6 across runs, vs 9.8 in `v3_full`) by aggressively choosing
high-intervention levels. The cost: ~$26B in economic damage, ~2.4× the
legacy run's $11B.

The four Phase A-F configurations produce essentially identical outcomes
because they are all converging on the same policy in the same way. The
algorithm choice — meta-bandit, weighted, epsilon-greedy, ucb — moves
the result by at most 1 death and $400M out of 26,000.

This is the reward-blindness pattern from
[`writeup_reward_blindness.md`](../../../../writeup_reward_blindness.md):
the reward function clips `lives_saved` at 1.0 (100 lives over the
benchmark's scale), so once a policy prevents enough deaths to saturate
that ceiling, the gradient flattens. Every choice that prevents nearly all
deaths gets the same lives reward; the only remaining signal is the
econ-cost penalty, which prefers _not_ to intervene. The bandit finds
the corner where both forces are minimized — heavy intervention applied
just often enough — and stays there. Algorithm choice doesn't matter
because there's no gradient left for it to climb.

The legacy `v3_full` run achieved a less extreme outcome (more deaths,
less spend) because its feedback was routed via `strategyId + option`
rather than `decisionId`, which silently bypassed the candidate-context
path. With `decisionId` feedback wired in, every candidate sees the full
reward stream and the meta-bandit converges faster — producing the
clearer reward-blindness signal.

## Benchmark change required to surface Phase A-F

The benchmark previously called `/feedback` with `{"strategyId": node_id,
"option": option, "reward": ..., "contextKey": ...}`. That payload
bypassed the `chosen_candidate` extraction in `do_feedback`, which means
`mb.record(candidate, reward)` was never invoked and the meta-bandit's
trial counters stayed at zero across the whole run. Switching to
`{"decisionId": ..., "reward": ..., "signalKind": ...}` (still preserving
`strategyId`/`option` as a fallback for capsules without a decision log
entry) routes the feedback through the candidate-context bucket and
exercises the post-Phase-B path. That's the only benchmark change beyond
the new `--algorithm meta_bandit` CLI option.

## How to reproduce

```bash
syntra serve --addr 127.0.0.1:8787 --store /tmp/outbreak --admin-key dev-key &

cd Syntra/examples/lycan-internals/benchmarks/outbreak_early_warning_resilience
for alg in weighted epsilon_greedy ucb meta_bandit; do
  python3 benchmark.py --algorithm $alg \
    --seeds 10 --weeks 52 --regions 4 --seed-offset 2000 \
    --syntra-url http://127.0.0.1:8787 --admin-key dev-key \
    --output-dir results/phase_a_f_$alg
done
```

Each run takes ~134s wall clock. Total sweep: ~9 minutes.
