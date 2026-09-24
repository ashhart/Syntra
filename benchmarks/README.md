# Benchmarks against Vowpal Wabbit and Open Bandit Pipeline

Three comparisons with the tools Syntra is most often measured against:
Vowpal Wabbit (VW), the standard contextual-bandit learner, and Open Bandit
Pipeline (OBP), the standard library for off-policy evaluation. Each script
runs with one command, writes its raw numbers to `results/`, and prints the
tables quoted here. Every number below was measured on one machine, an
Apple M5 Max (macOS), release build, at commit `64db123` on 2026-09-24.
Other jobs were running on the machine at the time; the latency section
says how that was handled.

| Script | Question | Wall time here |
|---|---|---|
| `learning_vs_vw.py` | Learning online on the simulated problems of `examples/learning_bench.rs`, how close does each learner get to the best policy? | 7 min on 16 processes |
| `ope_vs_obp.py` | On logged data whose true answer is known, how far off are the off-policy estimates, and do the 95% intervals contain the truth 95% of the time? | 8 min on 12 processes |
| `latency_vs_vw.py` | How long does one decision take when called from Python? | 10 s |

## Results in brief

Where Syntra did better:

- At default settings it beat VW in five of six combinations of problem and
  exploration, on both final performance and regret, and tied in the sixth.
  The largest gap was the problem whose best actions change halfway through:
  with SquareCB, Syntra earned 0.985 of the oracle's expected reward over the
  last 10% of rounds, VW 0.896.
- Its doubly robust (DR) estimates were as accurate as OBP's DR with a
  correctly specified reward model, and more accurate than OBP's DR with the
  additive logistic regression of OBP's examples: 6.5% against 9.6% mean
  relative error on 1,000 rows logged by a policy that avoids the target's
  actions.
- Its 95% intervals for IPS, SNIPS and DR contained the true value in 94.3%
  to 94.5% of 1,800 datasets. Its SNIPS intervals were about half as wide as
  OBP's SNIPW intervals, which contained the truth every time because they
  are too wide.
- A decision from Python took 0.83 µs at the median and 2.0 µs at p99, with
  the action drawn and the decision queued for upload. VW's `predict` took
  8.0 µs and 11.8 µs on prebuilt text and returns only the probabilities.

Where it was level:

- IPS and SNIPS agree with OBP's IPW and SNIPW to within 1.4e-12 relative
  on identical data, as they should.
- On the stable four-segment problem with SquareCB, Syntra and VW at their
  defaults were within noise of each other.

Where it did worse:

- VW's defaults are not its best on these problems. With its settings tuned
  on held-out seeds, VW beat Syntra on the 200-item catalog with SquareCB
  (0.799 against 0.793, with lower regret too) and finished higher on the
  four-segment problem (0.995 against 0.986 with SquareCB, 0.972 against
  0.965 with epsilon-greedy) with no significant difference in regret.
  Syntra's only setting exposed by learning_bench is its learning rate, so
  it had less to tune.
- Syntra's fixed 5% exploration floor, which explains most of its lead on
  the changing problem, also caps it on stable ones: VW without a floor
  reached 0.995 or better on 13 of 30 seeds of the four-segment problem,
  where Syntra stays between 0.979 and 0.989.
- Syntra's DM intervals contained the truth in only 13.6% of datasets. They
  hold the reward model fixed, as Syntra's documentation says, and its
  linear model cannot fit the logistic rewards used here. OBP's DM with a
  correctly specified model was the most accurate estimator of all, which
  says more about knowing the true model than about either tool.

## Setting up

```bash
uv venv --python 3.10 .venv-bench
uv pip install --python .venv-bench/bin/python -r benchmarks/requirements.txt

.venv-bench/bin/python benchmarks/learning_vs_vw.py
.venv-bench/bin/python benchmarks/ope_vs_obp.py
.venv-bench/bin/python benchmarks/latency_vs_vw.py
```

The scripts build what they need with cargo in release mode and honour
`CARGO_TARGET_DIR`. The first two run their independent simulations in
parallel on all but two logical CPUs; `--jobs` changes that. Python 3.10 is
deliberate. obp 0.5.7, the latest release, declares `python < 3.11` and pins
torch 1.12, scikit-learn 1.1.3, pandas 1.5.2, numpy 1.23.5 and scipy 1.9.3,
so on Python 3.11 or 3.12 the resolver can only pick obp 0.4.1 from 2021.
The one pin added on top of obp's is matplotlib 3.7.5, because seaborn
0.11, which obp requires, fails to import with matplotlib 3.9 or later.
`requirements.txt` pins every package that was installed.

| Component | Version |
|---|---|
| Syntra | commit `64db123`, `--release`, rustc 1.98.0-nightly (2026-06-25) |
| Vowpal Wabbit | 9.11.6, the PyPI wheel |
| Open Bandit Pipeline | 0.5.7 |
| Python | 3.10.20 |
| numpy, scipy, scikit-learn | 1.23.5, 1.9.3, 1.1.3 |

VW 9.11.6 was published the day before these runs. As a spot check, the
default and matched VW configurations below were also run on three seeds
per problem with VW 9.10.0 from 2024: 29 of 36 runs gave identical results,
and the 7 that differed were SquareCB runs that moved in both directions.

## 1. Online learning against Vowpal Wabbit

```bash
.venv-bench/bin/python benchmarks/learning_vs_vw.py   # --rounds 20000 --seeds 30 --tune-seeds 10
```

### How it works

The problems are the three environments of `examples/learning_bench.rs`,
each with a known optimum:

- `segments`: four user segments and three actions with Bernoulli rewards;
  the best action of each segment beats the next best by 0.1 to 0.3.
- `drift`: the same, but every segment's means rotate by one action
  halfway through the run.
- `catalog`: 20 of 200 items offered per request, each with two numeric
  features; the reward is 0.9 minus 0.8 times the distance between the
  user's and the item's features, clamped to [0.05, 0.95].

The two metrics are learning_bench's. Final share is the mean expected
reward of the chosen actions over the last 10% of rounds, divided by the
oracle's. Regret is the mean over all rounds of the best expected reward
minus the chosen one. Every row runs 20,000 rounds on the same 30 seeds,
1000 to 1029. A seed fixes the contexts, the item sets, the reward draws and
the sampling draws, so both learners face the same problem round by round
and the differences can be paired by seed.

Syntra's numbers come from learning_bench itself. `learning_seeds/` is a
small crate that compiles `examples/learning_bench.rs` unchanged (its build
script strips the `//!` module docs, which `include!` rejects) and prints
one result per seed. The script checks that the per-seed means reproduce
learning_bench's printed table exactly, for every learning rate it runs.

VW runs on `envs.py`, a Python port of the environments, the SplitMix64
generator, Syntra's sampler and exploration floor, and the run loop. The
port is exact: learning_bench's uniform policy run through it gives results
identical to the last bit to Syntra's on all 90 pairs of environment and
seed, and the script stops if one differs. VW returns a probability for each
action; the harness draws the action with Syntra's sampler from Syntra's
draw stream.

Four kinds of rows:

- **Defaults.** VW with `--cb_explore_adf --squarecb -q ca` and
  `--cb_explore_adf --epsilon 0.1 -q ca` and its default learning settings;
  Syntra as learning_bench runs it.
- **VW tuned.** For each problem, the setting with the lowest mean regret on
  10 separate tuning seeds, 2000 to 2009, then run on the 30 report seeds.
  The SquareCB grid has 48 points: `-l 0.1`, `0.5` or `2` with the default
  `--power_t 0.5`, or `-l 0.01`, `0.03` or `0.1` with `--power_t 0`; times
  `--gamma_scale` 1, 10, 100 or 1000; times SquareCB's own minimum
  probability `--epsilon` 0 or 0.05. The epsilon-greedy grid has the six
  learning-rate schedules.
- **Syntra tuned.** Its learning rate, 0.1, 0.25, 0.5, 1 or 2, chosen the
  same way. learning_bench exposes no other setting.
- **VW matched.** VW's defaults plus the three changes that make its setup
  the same as Syntra's apart from the regression learner: Syntra's 5% floor
  mixed into VW's probabilities before sampling, `--ignore_linear c`, and,
  for epsilon-greedy, probability 1 in the label so the update is
  unweighted.

### What is held equal, and what is not

In every VW row the problems, seeds and random draws are the same as
Syntra's. So are the features: a context namespace `c`, with strings as
`name=value` indicators, numbers as values and zeros dropped, and an action
namespace `a` with the `id=<id>` indicator and the item features, crossed
by `-q ca`, plus a constant, in 2^18 hashed slots. Rewards of 1 or 0 go to
VW as costs of -1 or 0. Both use SquareCB with gamma = 10 n^0.5, n being the
number of updates so far, and SquareCB's updates are unweighted in both,
since VW sets the logged probability to 1 before learning. Both learn with
normalized adaptive gradient steps at a base rate of 0.5.

The differences in the default rows:

| | Syntra | VW | Removed in the matched rows |
|---|---|---|---|
| Exploration floor | Mixes 5% uniform into every distribution, p = 0.95 p + 0.05 / K. Its "epsilon-greedy 0.1" therefore spreads 0.145 of the probability uniformly | None by default | Yes, the harness applies Syntra's floor to VW's distribution |
| Epsilon-greedy update weights | Unweighted (`learner.importance: none`) | 1/p, VW's MTR default | Yes, probability 1 in the label |
| Context-only features | Left out; they cannot change the ranking | Linear terms of `c` kept | Yes, `--ignore_linear c` |
| Update rule | NAG, Algorithm 2 of Ross, Mineiro and Langford (2013) | VW's default adaptive, normalized and importance-invariant update; each update also carries weight 1/K (events over actions) | No |
| Hashing | FNV-1a with the MurmurHash3 finalizer | MurmurHash3 | No; with at most about 600 features in 2^18 slots, collisions are rare in both |
| Prediction range | Clamped to [0, 1] before exploring | Clipped to the range of costs seen, [-1, 0] once both rewards have occurred | No; the same after the first few rounds |

### Results

Final share of the oracle's expected reward, mean ± standard error over 30
seeds:

| Configuration | segments | drift | catalog |
|---|---:|---:|---:|
| Syntra SquareCB, defaults | 0.9852 ± 0.0004 | 0.9853 ± 0.0005 | 0.7927 ± 0.0014 |
| VW `--squarecb`, defaults | 0.9864 ± 0.0028 | 0.8962 ± 0.0099 | 0.7448 ± 0.0022 |
| Syntra SquareCB, learning rate tuned | 0.9861 ± 0.0003 | 0.9853 ± 0.0005 | 0.7927 ± 0.0014 |
| VW `--squarecb`, tuned | 0.9947 ± 0.0017 | 0.9580 ± 0.0030 | 0.7988 ± 0.0015 |
| VW `--squarecb`, matched to Syntra | 0.9734 ± 0.0018 | 0.9720 ± 0.0018 | 0.7363 ± 0.0020 |
| Syntra epsilon-greedy 0.1, defaults | 0.9644 ± 0.0005 | 0.9630 ± 0.0006 | 0.7830 ± 0.0010 |
| VW `--epsilon 0.1`, defaults | 0.9494 ± 0.0028 | 0.9421 ± 0.0037 | 0.7534 ± 0.0023 |
| Syntra epsilon-greedy 0.1, learning rate tuned | 0.9651 ± 0.0004 | 0.9630 ± 0.0006 | 0.7830 ± 0.0010 |
| VW `--epsilon 0.1`, tuned | 0.9716 ± 0.0013 | 0.9421 ± 0.0037 | 0.7837 ± 0.0027 |
| VW `--epsilon 0.1`, matched to Syntra | 0.9651 ± 0.0004 | 0.9638 ± 0.0005 | 0.7650 ± 0.0011 |
| Uniform random (either) | 0.7590 ± 0.0007 | 0.7574 ± 0.0006 | 0.5974 ± 0.0014 |

Regret per round, same runs:

| Configuration | segments | drift | catalog |
|---|---:|---:|---:|
| Syntra SquareCB, defaults | 0.01537 ± 0.00023 | 0.02788 ± 0.00081 | 0.19831 ± 0.00061 |
| VW `--squarecb`, defaults | 0.01462 ± 0.00145 | 0.06631 ± 0.00362 | 0.23206 ± 0.00103 |
| Syntra SquareCB, learning rate tuned | 0.01439 ± 0.00044 | 0.02788 ± 0.00081 | 0.19831 ± 0.00061 |
| VW `--squarecb`, tuned | 0.01576 ± 0.00139 | 0.04325 ± 0.00105 | 0.19347 ± 0.00095 |
| VW `--squarecb`, matched to Syntra | 0.02603 ± 0.00048 | 0.04076 ± 0.00090 | 0.23643 ± 0.00104 |
| Syntra epsilon-greedy 0.1, defaults | 0.03011 ± 0.00024 | 0.03670 ± 0.00036 | 0.20369 ± 0.00041 |
| VW `--epsilon 0.1`, defaults | 0.04923 ± 0.00061 | 0.05654 ± 0.00090 | 0.23068 ± 0.00129 |
| Syntra epsilon-greedy 0.1, learning rate tuned | 0.02776 ± 0.00023 | 0.03670 ± 0.00036 | 0.20369 ± 0.00041 |
| VW `--epsilon 0.1`, tuned | 0.02928 ± 0.00083 | 0.05654 ± 0.00090 | 0.21318 ± 0.00093 |
| VW `--epsilon 0.1`, matched to Syntra | 0.02824 ± 0.00025 | 0.03914 ± 0.00066 | 0.21810 ± 0.00057 |
| Uniform random (either) | 0.17498 ± 0.00019 | 0.17527 ± 0.00021 | 0.32366 ± 0.00028 |

Tuned settings (lowest mean regret on the tuning seeds): segments SquareCB: VW `-l 2 --gamma_scale 10`, Syntra 0.1; segments epsilon-greedy: VW `-l 0.1`, Syntra 0.25; drift SquareCB: VW `-l 2 --gamma_scale 100 --epsilon 0.05`, Syntra 0.5; drift epsilon-greedy: VW `-l 0.5`, Syntra 0.5; catalog SquareCB: VW `-l 2 --gamma_scale 100`, Syntra 0.5; catalog epsilon-greedy: VW `-l 0.1 --power_t 0`, Syntra 0.5.

Paired over the 30 seeds, Syntra minus VW, a difference counts when the 95%
t-interval excludes zero (the full intervals are in
`results/learning_vs_vw.md`):

| Environment | Exploration | Defaults | Both tuned | VW matched to Syntra |
|---|---|---|---|---|
| segments | SquareCB | no significant difference | VW higher final share | Syntra better on both |
| segments | epsilon-greedy | Syntra better on both | VW higher final share | VW better on both |
| drift | SquareCB | Syntra better on both | Syntra better on both | Syntra better on both |
| drift | epsilon-greedy | Syntra better on both | Syntra better on both | Syntra lower regret |
| catalog | SquareCB | Syntra better on both | VW better on both | Syntra better on both |
| catalog | epsilon-greedy | Syntra better on both | Syntra lower regret | Syntra better on both |

learning_bench's own output for the default rows, with the 5 seeds used in
the main README and with the 30 used here:

```text
20000 rounds, 5 seeds, learning rate default; share of the oracle's expected reward over the last 10% of rounds, and mean regret per round

| Environment | Policy | Final share of oracle | Regret per round |
|---|---|---|---|
| segments | squarecb | 0.986 | 0.0155 |
| segments | epsilon-greedy 0.1 | 0.965 | 0.0303 |
| segments | uniform | 0.755 | 0.1756 |
| drift | squarecb | 0.985 | 0.0278 |
| drift | epsilon-greedy 0.1 | 0.963 | 0.0363 |
| drift | uniform | 0.761 | 0.1756 |
| catalog | squarecb | 0.792 | 0.1980 |
| catalog | epsilon-greedy 0.1 | 0.782 | 0.2044 |
| catalog | uniform | 0.596 | 0.3239 |
```

```text
20000 rounds, 30 seeds, learning rate default; share of the oracle's expected reward over the last 10% of rounds, and mean regret per round

| Environment | Policy | Final share of oracle | Regret per round |
|---|---|---|---|
| segments | squarecb | 0.985 | 0.0154 |
| segments | epsilon-greedy 0.1 | 0.964 | 0.0301 |
| segments | uniform | 0.759 | 0.1750 |
| drift | squarecb | 0.985 | 0.0279 |
| drift | epsilon-greedy 0.1 | 0.963 | 0.0367 |
| drift | uniform | 0.757 | 0.1753 |
| catalog | squarecb | 0.793 | 0.1983 |
| catalog | epsilon-greedy 0.1 | 0.783 | 0.2037 |
| catalog | uniform | 0.597 | 0.3237 |
```

### What the numbers say

- On `drift`, the gap at the defaults is mostly exploration. VW's SquareCB
  has no floor, so its exploration shrinks as gamma grows and it notices
  late that the best actions moved. With Syntra's floor and feature set,
  the matched row, VW reaches 0.972 instead of 0.896; with its own minimum
  probability `--epsilon 0.05`, which the tuning chose, 0.958.
- On `catalog` it is not. VW with Syntra's floor does no better than at its
  defaults, 0.736 against 0.745; a higher learning rate does. Tuned, VW's
  SquareCB uses `-l 2` on all three problems, and on `catalog` that puts it
  ahead of Syntra, whose best learning rate there was its default 0.5. VW's
  epsilon-greedy went the other way, to `-l 0.1` or a constant 0.1 on two of
  the three.
- The floor is a trade. On `segments`, VW's SquareCB without a floor
  finished at 0.995 or better on 13 of 30 seeds but at 0.942 and 0.948 on
  the two worst. On those two, one segment was still getting a wrong action
  in about half and in nine tenths of its final rounds. Syntra's seeds all
  fell between 0.979 and 0.989: seven times less spread, and a lower
  ceiling.
- With the setup made identical, the matched rows, Syntra's learner came
  out ahead in five of six cases, on `drift` with epsilon-greedy only on
  regret. VW's was ahead with epsilon-greedy on `segments`, by 0.001 of
  final share and 0.002 of regret. The gaps are largest with SquareCB, up
  to 0.056 of final share on `catalog`. SquareCB's probabilities depend on
  the predicted differences between actions, not only on which action ranks
  first, so it is the more sensitive of the two to how a learner's
  predictions converge. These runs do not show which part of VW's update
  is responsible.
- Syntra's "epsilon-greedy 0.1" is epsilon-greedy 0.145 in VW's terms,
  because the default floor applies on top. The label in learning_bench
  and in the main README hides that.

## 2. Off-policy evaluation against Open Bandit Pipeline

```bash
.venv-bench/bin/python benchmarks/ope_vs_obp.py   # --repeats 200 --sizes 1000,10000,100000
```

### How it works

The logged data come from OBP's `SyntheticBanditDataset`: 10 actions,
5-dimensional standard normal contexts, Bernoulli rewards. The expected
reward is OBP's `logistic_reward_function` with the coefficients of seed
12345, the dataset's default; its logits are standardized with a mean and
standard deviation measured once on a million contexts, which puts the
average reward at 0.50 and the best action's at 0.80. The logging policy is
the dataset's softmax of beta times the expected reward:

- `uniform`: beta = 0;
- `aligned`: beta = 3, which favours the actions the target favours;
- `opposed`: beta = -3, which avoids them; the largest importance weight
  in a dataset averages 47 at 1,000 rows and 69 at 100,000.

The target policy is epsilon-greedy with epsilon 0.1 on the truly best
action. Its true value, 0.7702, comes from 4 million fresh contexts (Monte
Carlo standard error 3.5e-05). Each of the 200 repeats per cell draws a new
dataset and hands the same rows to both tools.

Syntra gets the rows as JSONL and runs
`syntra evaluate --policy target-column --w-max inf --bootstrap 1000 --folds 5`:
DM, IPS, SNIPS and DR, with DR's reward model Syntra's own linear model on
context, action and their crosses, cross-fitted over 5 folds, and 95%
percentile-bootstrap intervals that hold the fitted model fixed. OBP runs
`InverseProbabilityWeighting`, `SelfNormalizedInverseProbabilityWeighting`,
`DirectMethod` and `DoublyRobust`, without clipping, with a
`RegressionModel` cross-fitted over 5 folds and one of two base models:

- `LogisticRegression(C=100, max_iter=10000)` on the context and a one-hot
  action, as in OBP's examples. It is additive, so it cannot represent the
  context-action interaction in the true reward;
- the same logistic regression on pairwise products of those inputs, which
  contains the true reward function.

OBP's intervals come from its `estimate_interval` with 1,000 resamples.

Two things in obp 0.5.7 would have made the truth ill-defined, so the script
works around them. Its default `z_score=True` standardizes the logits with
the statistics of each generated batch, and computes `x - mean / std` rather
than `(x - mean) / std`, so the expected reward of a context depends on the
batch it was drawn in; the script passes `z_score=False` and standardizes
with fixed constants instead. And `obtain_batch_bandit_feedback` draws the
logged actions with `sample_action_fast(pi_b, random_state=self.random_state)`,
which reseeds from the same integer on every call, so repeated calls on one
dataset object reuse the same uniform draws; with uniform logging every
repeat would log the same actions. The script builds a new dataset with its
own `random_state` for every repeat instead.

### Results

Mean relative error, |estimate - truth| / truth averaged over datasets:

| Estimator | uniform, 1,000 | uniform, 10,000 | uniform, 100,000 | aligned, 1,000 | aligned, 10,000 | aligned, 100,000 | opposed, 1,000 | opposed, 10,000 | opposed, 100,000 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Syntra DM | 3.46% | 0.83% | 0.55% | 2.37% | 0.73% | 0.36% | 6.33% | 1.26% | 0.78% |
| Syntra IPS | 7.32% | 2.74% | 0.77% | 5.45% | 1.71% | 0.53% | 16.16% | 5.11% | 1.64% |
| Syntra SNIPS | 3.56% | 1.29% | 0.38% | 2.84% | 0.85% | 0.27% | 6.60% | 2.03% | 0.66% |
| Syntra DR | 3.67% | 1.25% | 0.37% | 2.84% | 0.86% | 0.26% | 6.50% | 2.09% | 0.64% |
| OBP IPW | 7.32% | 2.74% | 0.77% | 5.45% | 1.71% | 0.53% | 16.16% | 5.11% | 1.64% |
| OBP SNIPW | 3.56% | 1.29% | 0.38% | 2.84% | 0.85% | 0.27% | 6.60% | 2.03% | 0.66% |
| OBP DM (LR, additive) | 26.73% | 26.68% | 26.71% | 15.92% | 16.00% | 15.96% | 39.18% | 38.79% | 38.91% |
| OBP DR (LR, additive) | 3.89% | 1.49% | 0.42% | 2.99% | 0.90% | 0.28% | 9.58% | 2.81% | 0.92% |
| OBP DM (LR, interactions) | 2.61% | 0.68% | 0.24% | 1.95% | 0.58% | 0.19% | 3.42% | 0.93% | 0.29% |
| OBP DR (LR, interactions) | 3.66% | 1.25% | 0.37% | 2.85% | 0.85% | 0.26% | 6.79% | 2.05% | 0.64% |

Share of 95% intervals containing the true value (percentile bootstrap, 1,000 resamples):

| Estimator | uniform, 1,000 | uniform, 10,000 | uniform, 100,000 | aligned, 1,000 | aligned, 10,000 | aligned, 100,000 | opposed, 1,000 | opposed, 10,000 | opposed, 100,000 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Syntra DM | 15.5% | 22.0% | 10.5% | 25.5% | 18.0% | 11.0% | 6.0% | 9.0% | 5.0% |
| Syntra IPS | 96.5% | 91.5% | 97.0% | 94.5% | 95.5% | 94.5% | 93.0% | 93.5% | 93.5% |
| Syntra SNIPS | 94.5% | 93.0% | 94.5% | 93.5% | 93.5% | 94.5% | 94.0% | 94.0% | 97.0% |
| Syntra DR | 95.5% | 93.5% | 96.0% | 94.0% | 92.5% | 94.5% | 95.0% | 92.0% | 97.5% |
| OBP IPW | 96.5% | 93.0% | 96.5% | 93.5% | 94.5% | 94.0% | 93.0% | 92.5% | 94.5% |
| OBP SNIPW | 100.0% | 100.0% | 100.0% | 100.0% | 100.0% | 100.0% | 100.0% | 100.0% | 100.0% |
| OBP DR (LR, additive) | 94.0% | 93.5% | 95.5% | 94.0% | 93.5% | 97.0% | 93.0% | 92.5% | 96.5% |
| OBP DR (LR, interactions) | 94.5% | 93.0% | 94.5% | 92.0% | 93.5% | 96.0% | 93.0% | 94.0% | 95.5% |

Mean width of those intervals:

| Estimator | uniform, 1,000 | uniform, 10,000 | uniform, 100,000 | aligned, 1,000 | aligned, 10,000 | aligned, 100,000 | opposed, 1,000 | opposed, 10,000 | opposed, 100,000 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Syntra DM | 0.016 | 0.004 | 0.001 | 0.015 | 0.004 | 0.001 | 0.018 | 0.004 | 0.001 |
| Syntra IPS | 0.302 | 0.096 | 0.030 | 0.200 | 0.063 | 0.020 | 0.545 | 0.174 | 0.055 |
| Syntra SNIPS | 0.142 | 0.045 | 0.014 | 0.102 | 0.032 | 0.010 | 0.242 | 0.077 | 0.024 |
| Syntra DR | 0.144 | 0.044 | 0.014 | 0.102 | 0.032 | 0.010 | 0.252 | 0.075 | 0.024 |
| OBP IPW | 0.302 | 0.096 | 0.030 | 0.199 | 0.063 | 0.020 | 0.545 | 0.174 | 0.055 |
| OBP SNIPW | 0.304 | 0.096 | 0.030 | 0.199 | 0.063 | 0.020 | 0.555 | 0.175 | 0.055 |
| OBP DR (LR, additive) | 0.163 | 0.051 | 0.016 | 0.106 | 0.033 | 0.011 | 0.326 | 0.102 | 0.032 |
| OBP DR (LR, interactions) | 0.143 | 0.044 | 0.014 | 0.102 | 0.032 | 0.010 | 0.249 | 0.075 | 0.024 |

Pooled over all 1,800 datasets: Syntra DM 13.6% (±0.8); Syntra IPS 94.4% (±0.5); Syntra SNIPS 94.3% (±0.5); Syntra DR 94.5% (±0.5); OBP IPW 94.2% (±0.5); OBP SNIPW 100.0% (±0.0); OBP DR (LR, additive) 94.4% (±0.5); OBP DR (LR, interactions) 94.0% (±0.6).

Per cell: logged mean reward, effective sample size and largest importance weight (Syntra's diagnostics, averaged over datasets), and Syntra DR's absolute error minus OBP DR's on the same data as a share of the true value, with a 95% t-interval (negative favours Syntra):

| Logging | n | Logged mean | ESS | Max weight | Syntra DR - OBP DR (additive) | Syntra DR - OBP DR (interactions) |
|---|---:|---:|---:|---:|---:|---:|
| uniform | 1,000 | 0.500 | 120 | 9.1 | -0.22% [-0.53, +0.09] | +0.01% [-0.15, +0.16] |
| uniform | 10,000 | 0.500 | 1204 | 9.1 | -0.24% [-0.35, -0.14] | +0.01% [-0.02, +0.03] |
| uniform | 100,000 | 0.500 | 12056 | 9.1 | -0.05% [-0.08, -0.02] | +0.00% [-0.00, +0.01] |
| aligned | 1,000 | 0.612 | 245 | 6.6 | -0.14% [-0.31, +0.02] | -0.00% [-0.09, +0.08] |
| aligned | 10,000 | 0.612 | 2448 | 7.0 | -0.05% [-0.10, -0.00] | +0.00% [-0.01, +0.02] |
| aligned | 100,000 | 0.613 | 24489 | 7.4 | -0.02% [-0.03, -0.00] | +0.00% [-0.00, +0.00] |
| opposed | 1,000 | 0.388 | 40 | 46.7 | -3.07% [-3.88, -2.26] | -0.28% [-0.65, +0.08] |
| opposed | 10,000 | 0.392 | 396 | 59.6 | -0.72% [-0.98, -0.45] | +0.04% [-0.00, +0.09] |
| opposed | 100,000 | 0.391 | 3962 | 68.6 | -0.28% [-0.37, -0.20] | -0.00% [-0.01, +0.01] |

Agreement on identical data (largest relative difference over all datasets): syntra/ips vs obp/ipw: 1.1e-13; syntra/snips vs obp/snipw: 1.4e-12.

### What the numbers say

- IPS and SNIPS are the same estimators in both tools, and on identical
  rows they agree to 1e-13 and 1e-12 relative. Their errors and interval
  coverage match accordingly.
- Syntra's DR, with its linear reward model, was as accurate as OBP's DR
  with the correctly specified model: the paired differences are
  under 0.3% of the true value and none is significant. Against OBP's
  additive model its mean error was lower in all nine cells, significantly
  in seven, and most where the logging policy avoids the target's actions:
  6.5% against 9.6% mean relative error at 1,000 rows.
- Pooled over all 1,800 datasets, Syntra's bootstrap intervals contained the
  true value 94.4% of the time for IPS, 94.3% for SNIPS and 94.5% for DR
  (±0.5%). OBP's IPW and DR intervals did the same, 94.0% to 94.4%.
- OBP's SNIPW intervals contained the truth in 100% of datasets and were as
  wide as its IPW intervals, about twice as wide as Syntra's SNIPS
  intervals. OBP bootstraps SNIPW's per-row terms with the normalizing mean
  held at its full-sample value, which loses the variance reduction that
  self-normalization brings; Syntra recomputes the ratio in every resample.
- Syntra's DM intervals are not confidence intervals for the policy's value.
  They contained the truth in 13.6% of datasets. They hold the reward model
  fixed, which Syntra's documentation states, so they ignore both the
  model's bias (its linear model misses the logistic shape of the reward)
  and its variance: at 1,000 rows the DM estimates varied from dataset to
  dataset with a standard deviation of 0.020 to 0.034 while the intervals
  were only 0.015 to 0.018 wide.
- Timing was not the point of this benchmark, but for scale: at 100,000 rows
  `syntra evaluate` took 1.3 s per dataset for all four estimators, the
  cross-fitted model and 1,000 bootstrap resamples, plus 1.5 s for Python to
  write the JSONL. OBP took 1.8 s to fit its two regression models and 2.6 s
  for its estimates and intervals. The two do different amounts of work.

## 3. Decision latency from Python

```bash
.venv-bench/bin/python benchmarks/latency_vs_vw.py   # --calls 200000 --warmup 20000
```

### How it works

The script builds `syntra` and the Python extension
(`sdk/python/scripts/develop.sh`), starts `syntra serve` on a new store in a
temporary directory, and creates a capsule with the three actions of the
`segments` problem and default settings, which means SquareCB. Both models
first learn from the same 2,000 simulated rounds: Syntra through the HTTP
API, VW in process. Then each variant runs 20,000 untimed calls and 200,000
timed ones, in ten blocks that alternate between the variants, with the
garbage collector off during the timed loops. Every call is timed on its
own with `time.perf_counter_ns`, whose clock ticks every 41.7 ns on this
machine. The contexts cycle through 1,000 pre-generated `segments`
contexts, such as `{"segment": "pro", "hour": 13}`.

What one call does:

- Syntra's `LocalDecider.decide(context)` converts the dict, flattens and
  hashes the features, predicts all three actions, builds the SquareCB
  distribution with the floor, draws the action with a fresh seed, makes a
  decision id, queues the decision for upload and returns a `Decision`.
  With the default `sync_interval` a background thread uploads every
  second, and the server replays each uploaded decision; all 440,000
  decisions made by the two deciders were accepted. Both deciders get a
  `max_queue` that holds the whole run. With the default of 100,000, a loop
  at this speed fills the upload queue in 0.14 s, before the first sync, and
  `decide` then raises `upload queue is full`.
- VW's `predict(lines)` parses four lines of text (the shared context and
  three actions), predicts, builds the SquareCB distribution and returns
  it as a list. It draws no action and logs nothing. The text is built in
  advance in the first VW row; the next rows add drawing the action in
  Python, or building the text from the same dict.
- VW's `predict` on a pre-parsed example reuses one parsed example per
  context. A new request cannot do that, so it is a lower bound on VW's
  Python path.

### Results

| Call | p50 | p90 | p99 | p99.9 | mean |
|---|---:|---:|---:|---:|---:|
| Syntra `LocalDecider.decide(dict)`, background sync every 1 s (default) | 0.83 | 0.96 | 2.04 | 4.67 | 0.90 |
| Syntra `LocalDecider.decide(dict)`, no background thread | 0.83 | 0.96 | 2.38 | 6.04 | 0.91 |
| VW `predict(lines)`, text built in advance | 8.00 | 8.21 | 11.75 | 23.58 | 8.17 |
| VW `predict(lines)` + drawing the action in Python | 8.25 | 8.46 | 11.25 | 22.79 | 8.35 |
| VW: build the text from the dict, then `predict` | 9.04 | 9.29 | 12.21 | 24.08 | 9.16 |
| VW `predict` on a pre-parsed example (no parsing; lower bound) | 2.42 | 2.54 | 3.17 | 4.00 | 2.44 |
| Empty call (timing-loop overhead) | 0.04 | 0.04 | 0.04 | 0.08 | 0.04 |

Other jobs were running on the machine: the one-minute load average was
13.2 before the timed loops and 12.3 after, on 18 logical CPUs, while this
benchmark used one core for the calls plus the server. Three more runs over
the next minutes gave the same medians within 0.13 µs for every variant; the
p99 of Syntra's calls ranged from 1.9 to 2.4 µs and of VW's text path from
11.1 to 18.3 µs.

### What the numbers say

- A Syntra decision from Python took about a tenth of the time of VW's
  standard text path, 0.83 µs against 8.0 µs at the median, while also
  drawing the action and queueing the decision for upload. Most of VW's
  time is parsing: on a pre-parsed example it takes 2.4 µs, still about
  three times Syntra's full call.
- The background upload thread made no consistent difference, at the
  median or in the tail.
- This is one small case: two context features and three actions. Both
  costs grow with the number of features and actions, and a server-side
  decision over HTTP is a different path, measured in the main README.

## Caveats

- Simulations are not your traffic. The learning problems are
  learning_bench's, small and chosen before this comparison; the OPE data
  follow a logistic reward model from the family that OBP's second
  regression model fits.
- The tuning is modest and uneven: 54 VW settings against 5 Syntra
  learning rates, chosen on 10 seeds. VW's SquareCB gamma and minimum
  probability were tuned; Syntra's exploration settings were not.
- Standard errors and t-intervals treat the 30 seeds and the 200 datasets
  as independent samples, which they are by construction.
- The latency numbers are for one machine with other work running, and for
  Python only.

## Files

- `learning_vs_vw.py`, `envs.py`, `learning_seeds/`: the learning benchmark,
  the Python port of learning_bench's environments, and the per-seed runner
  for Syntra.
- `ope_vs_obp.py`: the off-policy evaluation benchmark.
- `latency_vs_vw.py`: the latency benchmark.
- `common.py`: shared helpers.
- `requirements.txt`: the pinned Python environment.
- `results/`: each script's JSON output (every run, the settings and the
  versions) and Markdown tables.
