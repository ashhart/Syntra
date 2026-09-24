# Online learning: Syntra and Vowpal Wabbit

20000 rounds, seeds 1000-1029 (30 seeds) for every row; tuning on seeds 2000-2009. Mean ± standard error across seeds. Final share: mean expected reward over the last 10% of rounds as a share of the oracle's. Regret: mean over all rounds of (best mean - chosen mean).

## segments (4 segments x 3 actions)

| Learner | Configuration | Final share of oracle | Regret per round |
|---|---|---:|---:|
| Syntra | SquareCB (default) | 0.9852 ± 0.0004 | 0.01537 ± 0.00023 |
| Syntra | same, learning rate tuned: 0.1 | 0.9861 ± 0.0003 | 0.01439 ± 0.00044 |
| VW | `--cb_explore_adf --squarecb -q ca` (defaults) | 0.9864 ± 0.0028 | 0.01462 ± 0.00145 |
| VW | same, tuned: `-l 2 --gamma_scale 10` | 0.9947 ± 0.0017 | 0.01576 ± 0.00139 |
| VW | matched to Syntra (see notes) | 0.9734 ± 0.0018 | 0.02603 ± 0.00048 |
| Syntra | epsilon-greedy 0.1 (default) | 0.9644 ± 0.0005 | 0.03011 ± 0.00024 |
| Syntra | same, learning rate tuned: 0.25 | 0.9651 ± 0.0004 | 0.02776 ± 0.00023 |
| VW | `--cb_explore_adf --epsilon 0.1 -q ca` (defaults) | 0.9494 ± 0.0028 | 0.04923 ± 0.00061 |
| VW | same, tuned: `-l 0.1` | 0.9716 ± 0.0013 | 0.02928 ± 0.00083 |
| VW | matched to Syntra (see notes) | 0.9651 ± 0.0004 | 0.02824 ± 0.00025 |
| either | uniform random | 0.7590 ± 0.0007 | 0.17498 ± 0.00019 |

## drift (best actions rotate halfway)

| Learner | Configuration | Final share of oracle | Regret per round |
|---|---|---:|---:|
| Syntra | SquareCB (default) | 0.9853 ± 0.0005 | 0.02788 ± 0.00081 |
| Syntra | same, learning rate tuned: 0.5 | 0.9853 ± 0.0005 | 0.02788 ± 0.00081 |
| VW | `--cb_explore_adf --squarecb -q ca` (defaults) | 0.8962 ± 0.0099 | 0.06631 ± 0.00362 |
| VW | same, tuned: `-l 2 --gamma_scale 100 --epsilon 0.05` | 0.9580 ± 0.0030 | 0.04325 ± 0.00105 |
| VW | matched to Syntra (see notes) | 0.9720 ± 0.0018 | 0.04076 ± 0.00090 |
| Syntra | epsilon-greedy 0.1 (default) | 0.9630 ± 0.0006 | 0.03670 ± 0.00036 |
| Syntra | same, learning rate tuned: 0.5 | 0.9630 ± 0.0006 | 0.03670 ± 0.00036 |
| VW | `--cb_explore_adf --epsilon 0.1 -q ca` (defaults) | 0.9421 ± 0.0037 | 0.05654 ± 0.00090 |
| VW | same, tuned: `-l 0.5` | 0.9421 ± 0.0037 | 0.05654 ± 0.00090 |
| VW | matched to Syntra (see notes) | 0.9638 ± 0.0005 | 0.03914 ± 0.00066 |
| either | uniform random | 0.7574 ± 0.0006 | 0.17527 ± 0.00021 |

## catalog (20 of 200 items, 2-D features)

| Learner | Configuration | Final share of oracle | Regret per round |
|---|---|---:|---:|
| Syntra | SquareCB (default) | 0.7927 ± 0.0014 | 0.19831 ± 0.00061 |
| Syntra | same, learning rate tuned: 0.5 | 0.7927 ± 0.0014 | 0.19831 ± 0.00061 |
| VW | `--cb_explore_adf --squarecb -q ca` (defaults) | 0.7448 ± 0.0022 | 0.23206 ± 0.00103 |
| VW | same, tuned: `-l 2 --gamma_scale 100` | 0.7988 ± 0.0015 | 0.19347 ± 0.00095 |
| VW | matched to Syntra (see notes) | 0.7363 ± 0.0020 | 0.23643 ± 0.00104 |
| Syntra | epsilon-greedy 0.1 (default) | 0.7830 ± 0.0010 | 0.20369 ± 0.00041 |
| Syntra | same, learning rate tuned: 0.5 | 0.7830 ± 0.0010 | 0.20369 ± 0.00041 |
| VW | `--cb_explore_adf --epsilon 0.1 -q ca` (defaults) | 0.7534 ± 0.0023 | 0.23068 ± 0.00129 |
| VW | same, tuned: `-l 0.1 --power_t 0` | 0.7837 ± 0.0027 | 0.21318 ± 0.00093 |
| VW | matched to Syntra (see notes) | 0.7650 ± 0.0011 | 0.21810 ± 0.00057 |
| either | uniform random | 0.5974 ± 0.0014 | 0.32366 ± 0.00028 |

## Final share of the oracle's expected reward, all environments

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

## Regret per round, all environments

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

## Paired differences, Syntra minus VW

Same seeds, so both learners see the same contexts and reward draws; 95% t-intervals over 30 seed pairs. Positive share and negative regret favour Syntra.

| Environment | Exploration | Comparison | Δ final share | Δ regret | Reading |
|---|---|---|---:|---:|---|
| segments | SquareCB | defaults | -0.001 [-0.007, +0.004] | +0.0007 [-0.0023, +0.0038] | no significant difference |
| segments | SquareCB | both tuned | -0.009 [-0.012, -0.005] | -0.0014 [-0.0043, +0.0015] | VW higher final share |
| segments | SquareCB | VW matched | +0.012 [+0.008, +0.015] | -0.0107 [-0.0117, -0.0096] | Syntra better on both |
| segments | epsilon-greedy | defaults | +0.015 [+0.009, +0.021] | -0.0191 [-0.0205, -0.0178] | Syntra better on both |
| segments | epsilon-greedy | both tuned | -0.006 [-0.009, -0.004] | -0.0015 [-0.0031, +0.0000] | VW higher final share |
| segments | epsilon-greedy | VW matched | -0.001 [-0.001, -0.000] | +0.0019 [+0.0013, +0.0024] | VW better on both |
| drift | SquareCB | defaults | +0.089 [+0.069, +0.109] | -0.0384 [-0.0461, -0.0308] | Syntra better on both |
| drift | SquareCB | both tuned | +0.027 [+0.021, +0.033] | -0.0154 [-0.0174, -0.0133] | Syntra better on both |
| drift | SquareCB | VW matched | +0.013 [+0.010, +0.017] | -0.0129 [-0.0145, -0.0113] | Syntra better on both |
| drift | epsilon-greedy | defaults | +0.021 [+0.014, +0.028] | -0.0198 [-0.0217, -0.0180] | Syntra better on both |
| drift | epsilon-greedy | both tuned | +0.021 [+0.014, +0.028] | -0.0198 [-0.0217, -0.0180] | Syntra better on both |
| drift | epsilon-greedy | VW matched | -0.001 [-0.002, +0.000] | -0.0024 [-0.0036, -0.0013] | Syntra lower regret |
| catalog | SquareCB | defaults | +0.048 [+0.043, +0.053] | -0.0337 [-0.0357, -0.0318] | Syntra better on both |
| catalog | SquareCB | both tuned | -0.006 [-0.010, -0.002] | +0.0048 [+0.0024, +0.0073] | VW better on both |
| catalog | SquareCB | VW matched | +0.056 [+0.052, +0.061] | -0.0381 [-0.0402, -0.0360] | Syntra better on both |
| catalog | epsilon-greedy | defaults | +0.030 [+0.025, +0.034] | -0.0270 [-0.0297, -0.0243] | Syntra better on both |
| catalog | epsilon-greedy | both tuned | -0.001 [-0.006, +0.005] | -0.0095 [-0.0116, -0.0074] | Syntra lower regret |
| catalog | epsilon-greedy | VW matched | +0.018 [+0.015, +0.021] | -0.0144 [-0.0157, -0.0132] | Syntra better on both |

Verdicts from the intervals above, per environment and exploration:

| Environment | Exploration | Defaults | Both tuned | VW matched to Syntra |
|---|---|---|---|---|
| segments | SquareCB | no significant difference | VW higher final share | Syntra better on both |
| segments | epsilon-greedy | Syntra better on both | VW higher final share | VW better on both |
| drift | SquareCB | Syntra better on both | Syntra better on both | Syntra better on both |
| drift | epsilon-greedy | Syntra better on both | Syntra better on both | Syntra lower regret |
| catalog | SquareCB | Syntra better on both | VW better on both | Syntra better on both |
| catalog | epsilon-greedy | Syntra better on both | Syntra lower regret | Syntra better on both |

## learning_bench output

`cargo run --release --example learning_bench -- --rounds 20000 --seeds 5`:

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

`cargo run --release --example learning_bench -- --rounds 20000 --seeds 30`:

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

`cargo run --release --example learning_bench -- --rounds 20000 --seeds 30 --learning-rate 0.1`:

```text
20000 rounds, 30 seeds, learning rate 0.1; share of the oracle's expected reward over the last 10% of rounds, and mean regret per round

| Environment | Policy | Final share of oracle | Regret per round |
|---|---|---|---|
| segments | squarecb | 0.986 | 0.0144 |
| segments | epsilon-greedy 0.1 | 0.965 | 0.0281 |
| segments | uniform | 0.759 | 0.1750 |
| drift | squarecb | 0.906 | 0.0642 |
| drift | epsilon-greedy 0.1 | 0.949 | 0.0616 |
| drift | uniform | 0.757 | 0.1753 |
| catalog | squarecb | 0.770 | 0.2132 |
| catalog | epsilon-greedy 0.1 | 0.764 | 0.2173 |
| catalog | uniform | 0.597 | 0.3237 |
```

`cargo run --release --example learning_bench -- --rounds 20000 --seeds 30 --learning-rate 0.25`:

```text
20000 rounds, 30 seeds, learning rate 0.25; share of the oracle's expected reward over the last 10% of rounds, and mean regret per round

| Environment | Policy | Final share of oracle | Regret per round |
|---|---|---|---|
| segments | squarecb | 0.986 | 0.0138 |
| segments | epsilon-greedy 0.1 | 0.965 | 0.0278 |
| segments | uniform | 0.759 | 0.1750 |
| drift | squarecb | 0.985 | 0.0357 |
| drift | epsilon-greedy 0.1 | 0.964 | 0.0407 |
| drift | uniform | 0.757 | 0.1753 |
| catalog | squarecb | 0.789 | 0.2002 |
| catalog | epsilon-greedy 0.1 | 0.780 | 0.2055 |
| catalog | uniform | 0.597 | 0.3237 |
```

`cargo run --release --example learning_bench -- --rounds 20000 --seeds 30 --learning-rate 0.5`:

```text
20000 rounds, 30 seeds, learning rate 0.5; share of the oracle's expected reward over the last 10% of rounds, and mean regret per round

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

Checks: benchmarks/learning_seeds reproduces every learning_bench table above; the Python port's uniform policy equals learning_bench's on all 90 (environment, seed) pairs. Wall time 392 s (215 s of it VW and the port, 16 processes); Apple M5 Max (macOS), release build.
