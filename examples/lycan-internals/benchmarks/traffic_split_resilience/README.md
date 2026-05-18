# Traffic Split Resilience Benchmark

Third-domain validation of the reward-blindness pattern (documented in
`../../../../writeup_reward_blindness.md`) and the Phase A-F rate-adaptive
meta-bandit. Action space: A/B/n variant routing in a web-traffic simulator.

---

## Problem setup

Each simulated week, 500 requests arrive. Each request carries a customer tier
(`free`, `pro`, `enterprise`). The bandit picks which of 4 variants
(`variant_a` through `variant_d`) to route the user to. Reward per request is:

```
reward = conversion - 0.5 * cost    (clamped to [-1.0, 1.0])
```

where `conversion` is 0 or 1 (Bernoulli) and `cost` is the serving cost per
impression (variant_a: $0.00, variant_b: $0.01, variant_c: $0.05,
variant_d: $0.20). True conversion rates depend on `(tier, variant)` and are
encoded in the simulator with mild Gaussian noise (std 0.03).

**Regime shift.** At week 26, the conversion-rate table for the `enterprise`
tier is permuted: `variant_d` (previously best at 0.50) drops to 0.12 and
`variant_a` (previously worst at 0.15) rises to 0.48. Policies without
change-detection continue exploiting the pre-shift winner and accumulate
regret.

**Scale.** 52 weeks x 500 requests/week x 10 seeds = ~260,000 decisions per
seed. Default seed offset: 4000.

---

## Baselines

| Policy | Description |
|---|---|
| `always_a` | Always serve `variant_a`. Free-tier, zero serving cost. Lower bound on score. |
| `equal_split` | Uniform round-robin across all 4 variants regardless of tier or history. |
| `proportional_to_known_winners` | Oracle-ish: uses the pre-computed best variant per tier from the pre-shift table. Does not update after the regime shift, so degrades on enterprise post-week 26. |
| `epsilon_greedy_per_tier` | Manual epsilon-greedy bandit (epsilon=0.10) per tier. No change detection -- slow to recover post-shift. Represents "rolling your own." |
| `oracle` | Perfect-information: always picks the true best variant for each tier and week. Lower bound on regret. |

---

## Syntra integration

Syntra is included as a sixth policy when `--algorithm` is specified.

**Discrete context** (`--context-type discrete`): `contextKey = tier_name`
(one of `free`, `pro`, `enterprise`). The meta-bandit has 5 candidates.

**Feature context** (`--context-type features`): typed feature vector:
- `tier` (categorical: `free/pro/enterprise`)
- `hour_of_day` (cyclic, period 24; simulated as 8am on day 1 of each week)
- `recent_conversion_rate` (continuous [0,1]; running per-tier mean)

This feature spec enrolls LinUCB as a 6th candidate in the meta-bandit
portfolio.

**Feedback routing.** Each request produces a `decisionId` from `/decide`.
Feedback is routed via `{"decisionId": ..., "reward": ..., "signalKind": "final"}`.
This is the corrected path documented in the outbreak Phase A-F re-run: the
`decisionId` carries the `candidateId` from the `/decide` response into the
feedback handler, which is what lets `mb.record(candidate, reward)` run.
Without `decisionId`, the meta-bandit's trial counters stay at zero.

**Change detection.** PageHinkley (threshold 3.0, minDrift 0.05) with an
exploration boost of 30% for 8 weeks post-detection. The regime shift at week
26 should fire a `change_detected` event for the `enterprise` context bucket
within a few weeks of the shift.

---

## Pass/fail criteria

Pre-registered before running.

| Criterion | Condition | Threshold |
|---|---|---|
| c1 | Syntra beats `always_a` and `equal_split` on score | >= 80% of seeds |
| c2 | Syntra beats `epsilon_greedy_per_tier` on score | >= 60% of seeds |
| c3 | Syntra cumulative regret < `epsilon_greedy_per_tier`'s | >= 50% of seeds |
| c4 | Syntra per-request rate drop post-shift <= epsilon_greedy's (proxy for change-detection and recovery) | >= 50% of seeds |

c4 is not evaluable when `--weeks < 26`.

---

## Reward-blindness check

**Original reward.** `reward = conversion - 0.5 * cost`. This is not
policy-dependent in the same way as the outbreak counterfactual -- it is
absolute per-request reward. However, the corrected-score comparison still
applies: we compute the excess reward each policy earns over a fixed
`always_a` baseline for the same request stream.

**Corrected reward.** For each request, `corrected_reward = reward - ref_reward`
where `ref_reward` is what `always_a` would have earned on the same request
(same conversion draw). This measures how much each policy beats the free
baseline per impression.

**Pre-registration hypothesis.** In the traffic-split domain the reward
function is already well-shaped: the original reward is absolute
(not relative to a policy-dependent counterfactual), so the corrected spread
may be similar to the original spread rather than wider. This would be the
expected null result if the reward-blindness pattern does not reproduce here.
The key diagnostic is the **spread ratio** (`corrected_spread / original_spread`):

- Outbreak benchmark: ratio ~25x (corrected spread 1.40 vs original 0.056).
- Vaccine benchmark: ratio ~4.4x (corrected spread 0.56 vs original 0.13).
- Traffic split (predicted): ratio ~1.0 -- the original reward is already
  fixed-baseline (per-impression), so the corrected reward adds only a
  centering shift. The pattern should NOT reproduce in this domain.

This is the intended null hypothesis for the third-domain test: the
reward-blindness pattern is domain-conditional on whether the reward function
uses a policy-dependent counterfactual. The traffic-split domain uses a
fixed per-impression reward, so the blind spot does not arise here. Confirming
the null hypothesis in this domain strengthens the generalizability argument
for the positive results in the outbreak and vaccine domains.

---

## Results

*Populated after running the benchmark. See `results/` subdirectory.*

### How to run

```bash
# Baselines only (no Syntra server required):
cd Syntra/examples/lycan-internals/benchmarks/traffic_split_resilience
python3 benchmark.py --seeds 10 --weeks 52

# With Syntra (discrete context, meta-bandit):
syntra serve --addr 127.0.0.1:48799 --store /tmp/ab-bench-store \
  --admin-key dev-key &

python3 benchmark.py \
  --algorithm meta_bandit \
  --context-type discrete \
  --seeds 10 --weeks 52 \
  --syntra-url http://127.0.0.1:48799 \
  --admin-key dev-key \
  --output-dir results/meta_bandit_discrete

# With feature context (enrolls LinUCB):
python3 benchmark.py \
  --algorithm meta_bandit \
  --context-type features \
  --seeds 10 --weeks 52 \
  --syntra-url http://127.0.0.1:48799 \
  --admin-key dev-key \
  --output-dir results/meta_bandit_features

# Smoke run (3 seeds x 8 weeks):
python3 benchmark.py \
  --algorithm meta_bandit \
  --context-type discrete \
  --seeds 3 --weeks 8 \
  --syntra-url http://127.0.0.1:48799 \
  --admin-key dev-key \
  --output-dir results/smoke
```

### Syntax / simulator check (no Syntra):

```bash
python3 -m py_compile benchmark.py

python3 -c "
import sys; sys.path.insert(0, '.')
import benchmark
sim = benchmark.TrafficSplitSim(seed=42, weeks=4, requests_per_week=100)
print(sim.requests_for_week(0)[0])
"
```

---

## Meta-bandit candidate trial distribution

*To be filled after running with `--algorithm meta_bandit`.*

Expected: UCB leads trials (consistent with outbreak benchmark). The
reward stream is bounded continuous with positive mean (most requests
convert at >10% base rate), which the Phase B reward characterization
should classify as `BoundedContinuous` and use to pick UCB or Thompson
as the post-warmup algorithm.

If `--context-type features` is used, LinUCB (6th candidate) should
enroll and accumulate trials alongside the discrete-context candidates.

---

## Cross-domain comparison

| Domain | Action space | Reward type | Spread ratio | Pattern? |
|---|---|---|---|---|
| Outbreak (PHC) | Intervention level 0-4 | Policy-dependent counterfactual | ~25x | YES |
| Vaccine allocation | Dose allocation across regions | Policy-dependent counterfactual | ~4.4x | YES |
| Traffic split | A/B/n variant routing | Fixed per-impression | ~1.0 (predicted) | NO (null) |

The third-domain null confirms that the reward-blindness pattern is not
universal -- it is specific to reward functions that compute credit relative
to a policy-dependent counterfactual baseline.

---

## License

Apache-2.0. See `../../../../LICENSE`.
