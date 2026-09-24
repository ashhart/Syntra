# Off-policy evaluation: Syntra and Open Bandit Pipeline

Target: epsilon-greedy (0.1) on the true best action, true value 0.7702 (Monte Carlo SE 3.5e-05). 200 independent datasets per cell; columns are the logging policy and the number of logged rows. Logging: uniform: uniform logging (beta = 0); aligned: logging softmax(3q), leaning towards the target; opposed: logging softmax(-3q), leaning away from the target. No weight clipping anywhere (Syntra `--w-max inf`, OBP `lambda_=inf`).

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

Mean seconds per dataset, one process each: n = 1,000: syntraWriteRows 0.02, syntraEvaluate 0.02, obpRegression 0.06, obpEstimatesAndIntervals 0.08; n = 10,000: syntraWriteRows 0.15, syntraEvaluate 0.13, obpRegression 0.18, obpEstimatesAndIntervals 0.46; n = 100,000: syntraWriteRows 1.48, syntraEvaluate 1.32, obpRegression 1.84, obpEstimatesAndIntervals 2.58.

Wall time 461 s on 12 processes; Apple M5 Max (macOS), release build.
