# Vaccine allocation — Phase A-F cross-domain check

The outbreak benchmark surfaced a reward-blindness pattern: under the
policy-dependent counterfactual reward, all policies score within 0.056 of
each other despite a 4× spread in actual deaths. The fix was a fixed-
counterfactual reward that produces a 1.40-point spread, ~25× wider.

The pre-existing writeup
([`writeup_reward_blindness.md`](../../../../../writeup_reward_blindness.md))
predicted that the same pattern would reproduce in the vaccine allocation
domain with smaller absolute magnitudes ("4.4× wider spread there vs 25×
here"). This run validates that prediction with a Phase A-F Syntra capsule
added as a sixth policy alongside the five hand-coded baselines.

## Setup

- 4 regions, 52 weeks, 10 seeds (3000–3009), 4 hand-coded allocation
  policies + myopic_oracle + Syntra (priority-region pick → 50/50 allocation
  rule).
- Syntra: Phase A-F `meta_bandit` learner. Two context modes tested
  (`discrete`: single global bucket; `features`: avg active per 100k +
  avg susceptible fraction + week_phase cyclic).
- Per-seed reset (matches outbreak pattern, hand-coded policies are stateless).
- Reward fed back to Syntra: mean of the 4 per-region original rewards
  per week.

## Result

| Policy                       | Deaths | Cost $M  | Orig reward | Corrected reward |
|---|---:|---:|---:|---:|
| equal_split                  |  184   |  56.87   | −14.54      | −10.44           |
| proportional_to_cases        |  138   |  56.87   | −14.62      | −9.99 ◄ best     |
| proportional_to_susceptible  |  195   |  56.87   | −14.53      | −10.55           |
| proactive_high_risk          |  184   |  56.87   | −14.55      | −10.44           |
| myopic_oracle                |  149   |  56.46   | −14.50 ◄ best| −9.99           |
| syntra (features, Phase A-F) |  169   |  56.87   | −14.57      | −10.29           |
| syntra (discrete, Phase A-F) |  189   |  56.87   | −14.54      | −10.48           |

**Original reward spread across policies: 0.128**
**Corrected reward spread across policies: 0.559** (4.4× wider)

The 4.4× ratio matches the writeup's prediction exactly. The reward-
blindness pattern reproduces cleanly in a second domain.

## Reading the table

- Deaths range from 138 (proportional_to_cases) to 195 (proportional_to_susceptible)
  — a 40% spread that reflects real differences in policy quality.
- The original reward function squashes that 40% spread into a 0.9% spread
  of total score (0.128 / 14.54). The policy ordering under the original
  reward is essentially noise.
- The corrected reward (fixed no-vaccine counterfactual) gives a 5.4% spread
  (0.559 / 10.44) that correctly orders policies by prevention.
- Syntra (features) lands between the worst and the best hand-coded
  policies on deaths (169 vs 138–195). It does not beat `proportional_to_cases`
  or `myopic_oracle` — those policies have direct access to oracle state
  (region case counts, R0) that Syntra only sees through delayed +
  noisy aggregate features.

## Why Syntra doesn't dominate

Three reasons:

1. **The reward function is the bottleneck.** A bandit cannot outperform the
   information content of the reward signal. The original reward has a
   policy-dependent counterfactual that collapses the credit pool when
   prevention works, so 138 deaths and 195 deaths look almost identical
   to the learner.
2. **Per-seed reset starves the meta-bandit.** Each seed has 52 weeks → 52
   feedbacks → only ~22 Active-state decisions per seed (after the
   30-feedback warmup). That's ~4 trials per candidate, deep in the
   exploration regime. The bandit cannot converge in one seed's worth of
   data. Compare to outbreak (208 decisions per seed, comfortable convergence).
3. **Coarse action space.** Syntra picks a priority region (K=4); the
   harness applies a fixed 50/50 allocation rule. The hand-coded policies
   produce arbitrary continuous allocations, including the myopic oracle
   that does 20-step greedy allocation per week. Coarsening the action
   space is what's required to fit the discrete-bandit shape; it costs
   ~30 deaths per seed against the oracle.

## Meta-bandit selection (final seed)

**features (22 trials, 6 candidates):**

| Candidate     | Trials | Mean reward |
|---|---:|---:|
| Thompson      |  2.0   | −0.0750 |
| UCB           |  4.9   | −0.0750 |
| Weighted      |  2.0   | −0.0747 |
| EpsilonGreedy |  4.0   | −0.0749 |
| Greedy        |  3.9   | −0.0749 |
| **LinUCB**    | **5.0**| −0.0749 |

**discrete (22 trials, 5 candidates):**

| Candidate     | Trials | Mean reward |
|---|---:|---:|
| Thompson      |  4.9   | −0.0749 |
| UCB           |  5.9   | −0.0748 |
| Weighted      |  2.0   | −0.0750 |
| EpsilonGreedy |  4.0   | −0.0750 |
| Greedy        |  5.0   | −0.0748 |

With only 22 trials per seed, candidate means are statistically tied
(all within 0.0003 of each other). The meta-bandit picks roughly
uniformly under these conditions, which is the correct behavior: when
candidates can't be distinguished, exploration should stay broad.

Both runs successfully transitioned Warmup → Active with reward
characterization installing `Weighted { learning_rate: 0.1 }` as the
post-warmup default (the reward stream is mostly negative — the
characterizer correctly identified it as bounded-negative-continuous).

## What this validates

1. **The reward-blindness pattern is not domain-specific.** Same shape in
   epidemiological intervention selection and vaccine allocation. The
   methodology check in `writeup_reward_blindness.md` (monotonicity against
   a known-better baseline) would catch this in either domain.
2. **The Phase A-F platform integrates cleanly into a new benchmark.** ~250
   lines added to the vaccine benchmark (SyntraClient, SyntraAllocPolicy,
   on-the-fly capsule authoring, meta_bandit CLI option). The platform
   surfaces (`/install`, `/learning`, `/decide`, `/feedback`) handle a
   second domain without modification.
3. **Cross-domain magnitudes vary as predicted.** Outbreak corrected/original
   ratio was 25×; vaccine is 4.4×. The pattern is general; the magnitude
   is domain-specific (depends on per-step action effect size and reward
   clipping shape, per the writeup's limitations section).

## Reproduction

```bash
syntra serve --addr 127.0.0.1:8787 --store /tmp/vaccine --admin-key dev-key &

cd Syntra/examples/lycan-internals/benchmarks/vaccine_allocation_resilience
python3 benchmark.py --algorithm meta_bandit --context-type features \
  --seeds 10 --weeks 52 --regions 4 --seed-offset 3000 \
  --syntra-url http://127.0.0.1:8787 --admin-key dev-key \
  --output-dir results/phase_a_f_meta_bandit_features
```

Wall time: ~13s per run (1.4 decisions per second; the simulator is the
bottleneck, not Syntra).
