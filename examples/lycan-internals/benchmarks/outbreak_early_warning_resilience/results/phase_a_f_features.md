# Outbreak benchmark — feature-context Phase A-F variant

Re-running the outbreak benchmark with the capsule's `contextSpec` flipped
from `discrete` to `features`. The feature vector is:

| feature             | type         | range / values |
|---|---|---|
| `case_rate_per_100k`| continuous   | [0, 2000]      |
| `region_id`         | categorical  | {"0","1","2","3"} |
| `week_phase`        | cyclic       | period 24      |

Same 10 seeds × 52 weeks × 4 regions × seed-offset 2000 as the prior runs.

## Result

| Run                       | Deaths | Econ $M  | Score   | Meta-bandit winner |
|---|---:|---:|---:|---|
| meta_bandit / discrete    |  0.5   | 26,332   | −10.94  | UCB (152.3 trials of 386) |
| **meta_bandit / features**| **7.0**| **15,800**| **−6.66**| **LinUCB (147.5 of 386 = 38%)** |
| weighted / discrete       |  1.2   | 26,357   | −10.94  | UCB |
| epsilon_greedy / discrete |  0.8   | 25,982   | −11.01  | UCB |
| ucb / discrete            |  1.6   | 26,390   | −10.81  | UCB |

The feature-context variant accepts +6.5 more deaths to save **$10.5B in
economic cost** vs the best discrete config. Score improves from −10.94 to
−6.66 — roughly a 40% improvement on the score metric the failing criteria
care about. Criteria c1/c2 still fail (the reward function still clips
lives_saved at 1.0 and penalizes econ), but the magnitude is meaningfully
closer to the baselines.

## Meta-bandit candidate breakdown (final seed, meta_bandit / features)

| Candidate     | Trials | Share | Mean reward |
|---|---:|---:|---:|
| Thompson      |  24.4  |  6.3% | −0.299      |
| UCB           |  34.0  |  8.8% | −0.248      |
| Weighted      |  39.6  | 10.3% | −0.286      |
| EpsilonGreedy |  18.3  |  4.7% | −0.255      |
| Greedy        |  56.6  | 14.7% | −0.248      |
| **LinUCB**    | **147.5**| **38.2%** | **−0.248** |

LinUCB dominates trial allocation by a wide margin in feature-context mode
while tying for best mean reward. This is the meta-bandit doing its job:
it identified that LinUCB exploits the feature structure and routed
exploration there.

In the discrete sweeps, LinUCB doesn't enroll (the meta-bandit registers the
5-candidate `discrete_only` portfolio). The 5 discrete candidates' best
mean reward was −0.259 (Greedy in the ucb-pinned run), so LinUCB's −0.248
is a small but real improvement — and that improvement translates directly
into the bandit picking lighter interventions when the features support it.

## What changed in the bandit's behavior

The discrete bandit converged on "max intervention everywhere" — 0 to 2
deaths total at $26B cost. The feature-context bandit converged on a
context-dependent rule: heavy intervention when feature signals warrant it,
back off otherwise. The result is more deaths (7 vs 0.5) but ~40% lower
total cost. The reward function still pushes both toward the saturation
corner, but the feature-aware bandit reaches a less-extreme version of it.

## Reproduction

```bash
syntra serve --addr 127.0.0.1:8787 --store /tmp/outbreak --admin-key dev-key &

cd Syntra/examples/lycan-internals/benchmarks/outbreak_early_warning_resilience
python3 benchmark.py \
  --algorithm meta_bandit --context-type features \
  --seeds 10 --weeks 52 --regions 4 --seed-offset 2000 \
  --syntra-url http://127.0.0.1:8787 --admin-key dev-key \
  --output-dir results/phase_a_f_meta_bandit_features
```

Wall time: ~171s (about 28% longer than discrete, mostly LinUCB matrix
inversion overhead).
