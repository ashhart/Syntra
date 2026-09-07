# Learning layer reference

Everything below is configurable per capsule via `PUT /tenants/.../learning`.

## Bandit algorithms

| Algorithm | Notes |
|---|---|
| `simpleWeighted` | Proportional sampling from learned weights. Default. |
| `epsilonGreedy` | Random explore with prob ε, otherwise pick highest scored mean. |
| `ucb1` | Upper-confidence bound; supports Gupta-Koren-Talwar corruption bonus. |
| `thompsonSampling` | Gaussian posterior on continuous rewards (Box-Muller sampling). |
| `softmax` | Boltzmann selection over scored options; tune with `temperature`. |

The configured algorithm is called by `select_option` and applied via soft-peak (chosen weight ← max + ε, renormalized) so the runtime `AdaptiveChoice` greedy picker respects it while preserving the underlying weight gradient.

## Adaptive features

- **Exponential decay** — count-based half-life (per-feedback) and wall-clock half-life. Older stats weigh less.
- **Sliding window** — last N rewards per option. Drives windowed mean, variance, CVaR.
- **Reward clipping** — `safety.rewardClip` bounds incoming reward to ±value. Defends against extreme/poisoned feedback.
- **Trimmed-mean robust stats** — drop the top/bottom fraction of the window. For workloads with adversarial outliers.
- **Page-Hinkley change detection** — two-sided CUSUM. Alarm boosts exploration; doesn't wipe state.
- **Model-surprise change detection** — alternative: fires when too many observations land outside the posterior interval.
- **Risk-sensitive CVaR** — selection score = `(1-blend)*mean + blend*CVaR_α`. For tail-sensitive workloads.
- **Conformal prediction sets** — `predictionSet` and `setWidth` returned alongside `/decide`. Calibrated coverage.
- **Delayed-feedback fusion** — feedback can tag `signalKind` (e.g. surrogate/interim/final); per-signal noise variance fuses into a per-option posterior. Pattern: Impatient Bandits (McDonald et al. KDD 2023).
- **Multi-objective Pareto** — vector rewards; `pareto_frontier` filters dominated options before selection.

## Operating modes

- `mode: "highThroughput"` — `snapshotOnFeedback=false`, `journalOnFeedback=false`. Use for >1k decisions/sec workloads.
- `mode: "highAssurance"` — snapshots and journal entries on every feedback, `rewardClip=1.0`. For audited / safety-critical use.

## Related endpoints

- `GET /tenants/.../chaos` — composite instability score (weight entropy, change-point rate, posterior variance, exploration-boost activity, prediction-set width).
- `POST /tenants/.../evaluate` — surrogate-index off-policy evaluation against an alternative learning config (does not re-run decisions).
