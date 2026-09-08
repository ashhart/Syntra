# Syntra adaptive-policy baseline evaluation — 2026-09-08

- **Generated:** 2026-09-08 by `scripts/eval-report.sh` against commit `b4631b6`.
- **Host:** Apple M5 Max, Darwin 25.5.0 — one Apple silicon laptop.
- **Binary:** `target/release/syntra`, `cargo build --release` from this tree.
- **Primary sweep:** `--rounds 4000 --seeds 12 --seed 42` (seed list is `base … base+K-1`).

> ## Caveat — read before quoting anything on this page
>
> **No production data, and no MoEfolio data, is reproduced here.** Syntra has run
> in shadow mode against moefolio.ai, but **no MoEfolio decision log is checked into
> this repository** — the only decision data in-tree is synthetic test fixtures — so
> a measured production win-rate is not available to this script and none is claimed.
> Every number below comes either from `syntra simulate` scoring a capsule spec
> against a synthetic traffic spec whose true arm means are written in the YAML, or
> from a simulated HTTP session against a locally started `syntra serve`. They
> characterise mechanism behaviour on a reward generating process we chose, not field
> performance. The `simulate` numbers are seed-deterministic and portable across
> machines; the A/B harness wall-clock is machine-specific.

## Methodology

`syntra simulate` runs the capsule's resolved learning algorithm against synthetic
traffic. Each round it draws a context, selects an option, samples an observed
reward from that arm's *true* mean plus Gaussian noise, and adds
`best_true_mean − chosen_true_mean` to cumulative regret. Regret is therefore scored
against ground truth, not against the noisy observation.

`--compare-baseline <random|first-arm|epsilon-greedy:N>` scores a reference policy on
the **same** traffic spec, round count and seed list — same per-round context and
feature draw order, same regime shifts — under the same true-mean regret definition.
The comparator ladder needs no external binary, so the headline reproduces with only
a `cargo build --release`:

- `random` — uniform arm choice each round (the classical floor).
- `first-arm` — always the capsule's first option: what a non-adaptive deployment
  running a static default accumulates.
- `epsilon-greedy:<eps>` — ε-exploration over empirical means with optimistic
  initialisation, drawing observations under the same noise rule as the live policy.

`--compare-vw` still exists but is deliberately used for no headline here — see
finding 2.

### Reproducibility probe (phase 0)

The script opens by running the headline command twice, in two separate processes.
Every field the report quotes — regret, `regretTrace`, picks, refusal rate,
`perContextConvergence`, meta-bandit selections and leaders — must come back
byte-identical. `finalWeights` is compared with a tolerance and its measured drift
is reported, because the learning layer's float accumulation is not a pure function
of the seed.

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-auto.yaml --traffic evals/traffic/stationary.yaml --rounds 4000 --seeds 12 --seed 42 --format json
```

This run: reported fields `identical = true`,
`max |Δ finalWeights| = 1.1e-16`, mean cumulative regret
1,337.75. The probe is load-bearing, not decoration — see
finding 7: until this commit `--seed` did not pin the learner's own draws, so these
numbers moved by whole percent between runs.

## Traffic specs and capsules under test

Committed under `evals/traffic/` (traffic specs in `TrafficSpec` schema, `arms` =
true reward means index-aligned with the capsule's options) and
`evals/traffic/capsules/` (4-option capsules):

| Traffic | True arm means | noise σ | Contexts (weights) | Regime shifts |
| --- | --- | ---: | --- | --- |
| `stationary.yaml` | 0.90, 0.50, 0.30, 0.10 | 0.05 | z_low 0.5 / z_mid 0.35 / z_high 0.15 | none |
| `regime-shift.yaml` | 0.90, 0.50, 0.30, 0.10 | 0.05 | z_low 0.5 / z_mid 0.35 / z_high 0.15 | @600 → 0.20/0.35/0.55/0.85; @1400 → 0.30/0.72/0.45/0.25 |
| `sparse-reward.yaml` | 0.050, 0.040, 0.010, 0.005 | 0.25 | cold_start 0.55 / returning 0.3 / power_user 0.15 | none |

Each spec runs green against its capsule, e.g.:

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-auto.yaml --traffic evals/traffic/stationary.yaml --rounds 4000 --seeds 12 --seed 42 --format json
```

## Cumulative regret vs built-in baselines

Config: `--rounds 4000 --seeds 12 --seed 42`. Lower is better;
`Syntra Δ` = baseline − syntra, so positive means Syntra accumulated less regret than
that reference policy. One command per row group; the baseline flag rotates.

### `stationary`

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-auto.yaml --traffic evals/traffic/stationary.yaml --rounds 4000 --seeds 12 --seed 42 --compare-baseline <random|first-arm|epsilon-greedy:0.1> --format json
```

| Config | Algorithm (resolved) | Syntra regret | σ | Baseline | Baseline regret | σ | Syntra Δ | shareBest (last 500) |
| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| simpleWeighted (auto) | simpleWeighted | 1,337.75 | 29.80 | `random` | 1,801.80 | 22.08 | 464.05 | 0.430 |
|  |  |  |  | `first-arm` | 0.00 | 0.00 | -1,337.75 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 182.67 | 11.46 | -1,155.08 |  |
| thompson | thompson | 43.88 | 1.65 | `random` | 1,801.80 | 22.08 | 1,757.92 | 0.989 |
|  |  |  |  | `first-arm` | 0.00 | 0.00 | -43.88 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 182.67 | 11.46 | 138.78 |  |
| ucb1 | ucb1 | 175.47 | 2.03 | `random` | 1,801.80 | 22.08 | 1,626.33 | 0.983 |
|  |  |  |  | `first-arm` | 0.00 | 0.00 | -175.47 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 182.67 | 11.46 | 7.20 |  |

### `regime-shift`

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-auto.yaml --traffic evals/traffic/regime-shift.yaml --rounds 4000 --seeds 12 --seed 42 --compare-baseline <random|first-arm|epsilon-greedy:0.1> --format json
```

| Config | Algorithm (resolved) | Syntra regret | σ | Baseline | Baseline regret | σ | Syntra Δ | shareBest (last 500) |
| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| simpleWeighted (auto) | simpleWeighted | 1,090.84 | 20.70 | `random` | 1,317.60 | 10.66 | 226.76 | 0.359 |
|  |  |  |  | `first-arm` | 1,612.00 | 0.00 | 521.16 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 705.48 | 26.64 | -385.37 |  |
| thompson | thompson | 420.36 | 18.46 | `random` | 1,317.60 | 10.66 | 897.25 | 0.974 |
|  |  |  |  | `first-arm` | 1,612.00 | 0.00 | 1,191.64 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 705.48 | 26.64 | 285.12 |  |
| ucb1 | ucb1 | 182.70 | 1.65 | `random` | 1,317.60 | 10.66 | 1,134.91 | 0.981 |
|  |  |  |  | `first-arm` | 1,612.00 | 0.00 | 1,429.30 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 705.48 | 26.64 | 522.78 |  |

### `sparse-reward`

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-sparse.yaml --traffic evals/traffic/sparse-reward.yaml --rounds 4000 --seeds 12 --seed 42 --compare-baseline <random|first-arm|epsilon-greedy:0.1> --format json
```

| Config | Algorithm (resolved) | Syntra regret | σ | Baseline | Baseline regret | σ | Syntra Δ | shareBest (last 500) |
| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| ucb1 (auto, sparse_continuous) | ucb1 | 81.07 | 3.91 | `random` | 95.09 | 1.36 | 14.02 | 0.488 |
|  |  |  |  | `first-arm` | 0.00 | 0.00 | -81.07 |  |
|  |  |  |  | `epsilon-greedy:0.1` | 27.95 | 17.05 | -53.12 |  |

Two notes for reading these tables:

- `first-arm` regret is exactly 0.00 on `stationary` and `sparse-reward`: arm index 0
  *is* the best arm in those specs by construction, so the comparator is degenerate
  there. Its informative number is on `regime-shift`, where a static default that
  never changes its mind pays 1,612.00 regret against a stream that moved.
- `epsilon-greedy:0.1` is the strongest built-in on the stationary stream
  (182.67 regret). Against it, ucb1 wins (175.47), thompson
  wins by a wide margin (43.88), and the weighted `auto`
  configuration loses badly (1,337.75) — see the convergence
  section, which is about exactly that gap.

## Meta-bandit strategy-selection ablation

`simulate` runs the meta-bandit candidates against the same observed rewards it feeds
the live algorithm (in the simulator the meta-bandit is instrumented, not in control)
and reports `metaBanditSelections` plus `metaBanditLeader`. That is the ablation
question: given only this stream's own rewards, which candidate would the outer
bandit have backed — and does the answer depend on which algorithm the inner policy
happened to be running?

Cells are the fraction of the 4000×12 rounds on which that candidate was
consulted; `leaders` is the distribution of final leaders across the 12 seeds.

| Traffic | Config | EpsilonGreedy | Greedy | Thompson | Ucb | Weighted | Final leaders across seeds |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| stationary | simpleWeighted (auto) | 0.189 | 0.242 | 0.132 | 0.201 | 0.236 | `Weighted`×6, `Greedy`×3, `Ucb`×2, `Thompson`×1 |
| stationary | thompson | 0.189 | 0.119 | 0.244 | 0.282 | 0.166 | `EpsilonGreedy`×6, `Ucb`×2, `Weighted`×2, `Greedy`×1, `Thompson`×1 |
| stationary | ucb1 | 0.206 | 0.150 | 0.271 | 0.113 | 0.259 | `Weighted`×4, `Thompson`×3, `Ucb`×3, `EpsilonGreedy`×1, `Greedy`×1 |
| regime-shift | simpleWeighted (auto) | 0.182 | 0.175 | 0.137 | 0.239 | 0.266 | `Weighted`×5, `Thompson`×3, `Ucb`×2, `EpsilonGreedy`×1, `Greedy`×1 |
| regime-shift | thompson | 0.198 | 0.277 | 0.206 | 0.152 | 0.168 | `Greedy`×4, `Thompson`×3, `Weighted`×3, `EpsilonGreedy`×2 |
| regime-shift | ucb1 | 0.199 | 0.167 | 0.190 | 0.156 | 0.289 | `EpsilonGreedy`×3, `Ucb`×3, `Weighted`×3, `Thompson`×2, `Greedy`×1 |
| sparse-reward | ucb1 (auto, sparse_continuous) | 0.169 | 0.297 | 0.133 | 0.284 | 0.117 | `Greedy`×4, `Ucb`×3, `Weighted`×3, `EpsilonGreedy`×1, `Thompson`×1 |

Averaged over all 7 (traffic, config) cells × 12 seeds =
84 final-leader draws (1 of 7 cells end in a tie for first), the outer bandit lands on
`Weighted` 31.0 % of the time and `Ucb`
17.9 % — no candidate is close to a majority, and no
cell is won by more than 6 of 12 seeds.

Two honest readings:

- The outer bandit's *ordering* of candidates is not stable at this round count: the
  consultation fractions stay inside 0.11–0.30 for every candidate on
  every stream, which means the meta-bandit keeps hedging rather than committing. It
  is still exploring on 4000-round streams.
- The leader does not reliably track which algorithm the inner policy is running:
  of the 7 cells whose inner algorithm names a candidate (`auto` on
  continuous is `Weighted`, plus `thompson` and `ucb1`), only 2 has that candidate as its *sole* plurality leader. `metaBanditLeader` in
  `simulate` output must therefore not be read as "the strategy the capsule is
  using". It is the outer bandit's own opinion, scored on the same rewards, and on
  this evidence it is a weak signal at this sample size.

## Convergence investigation: `shareBestArmLast500 = 0.404`

The pre-existing observation being investigated: a 4-arm capsule, true arm rewards
`0.9, 0.5, 0.3, 0.1`, 500 rounds, `algorithm: {type: auto}`, reporting
`shareBestArmLast500` ≈ 0.404 — the policy picks the best arm only ~40 % of the time,
long after it has enough samples to rank the arms. Two explanations were on the
table: a slow learner that would eventually converge, or a selection rule that never
tries to. We ran the same stream out to 20 000 rounds and swept
`learning.min_exploration`.

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-weighted-minex00.yaml --traffic evals/traffic/stationary.yaml --rounds <500|2000|8000|20000> --seeds 12 --seed 42 --format json
```

| Config | min_exploration | rounds | regret / round | shareBest mean | min–max across seeds | final weight vector |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| weighted | 0.0 | 500 | 0.1748 | 0.638 | 0.564–0.684 | `[0.739, 0.214, 0.037, 0.010]` |
| weighted | 0.0 | 2,000 | 0.1200 | 0.756 | 0.726–0.780 | `[0.758, 0.222, 0.010, 0.010]` |
| weighted | 0.0 | 8,000 | 0.1055 | 0.749 | 0.696–0.790 | `[0.736, 0.244, 0.010, 0.010]` |
| weighted | 0.0 | 20,000 | 0.1031 | 0.771 | 0.716–0.856 | `[0.785, 0.191, 0.014, 0.010]` |
| simpleWeighted (auto) | 0.05 | 500 | 0.3508 | 0.387 | 0.320–0.424 | `[0.415, 0.253, 0.183, 0.150]` |
| simpleWeighted (auto) | 0.05 | 2,000 | 0.3354 | 0.412 | 0.360–0.474 | `[0.428, 0.235, 0.188, 0.149]` |
| simpleWeighted (auto) | 0.05 | 8,000 | 0.3331 | 0.418 | 0.384–0.464 | `[0.440, 0.220, 0.188, 0.151]` |
| simpleWeighted (auto) | 0.05 | 20,000 | 0.3325 | 0.404 | 0.344–0.452 | `[0.377, 0.261, 0.203, 0.159]` |
| weighted | 0.2 | 500 | 0.4298 | 0.277 | 0.226–0.310 | `[0.280, 0.257, 0.235, 0.228]` |
| weighted | 0.2 | 2,000 | 0.4258 | 0.279 | 0.234–0.314 | `[0.299, 0.243, 0.234, 0.224]` |
| weighted | 0.2 | 8,000 | 0.4260 | 0.285 | 0.260–0.312 | `[0.287, 0.253, 0.237, 0.222]` |
| weighted | 0.2 | 20,000 | 0.4257 | 0.282 | 0.248–0.326 | `[0.273, 0.252, 0.246, 0.229]` |
| weighted | 0.4 | 500 | 0.4454 | 0.255 | 0.200–0.286 | `[0.262, 0.253, 0.244, 0.242]` |
| weighted | 0.4 | 2,000 | 0.4412 | 0.260 | 0.226–0.294 | `[0.270, 0.247, 0.244, 0.239]` |
| weighted | 0.4 | 8,000 | 0.4415 | 0.263 | 0.232–0.288 | `[0.262, 0.254, 0.245, 0.239]` |
| weighted | 0.4 | 20,000 | 0.4406 | 0.262 | 0.234–0.310 | `[0.260, 0.251, 0.249, 0.240]` |
| thompson | 0.05 | 500 | 0.0369 | 0.931 | 0.922–0.944 | `[0.566, 0.150, 0.143, 0.142]` |
| thompson | 0.05 | 2,000 | 0.0154 | 0.985 | 0.972–0.992 | `[0.564, 0.148, 0.145, 0.144]` |
| thompson | 0.05 | 8,000 | 0.0088 | 0.987 | 0.980–0.996 | `[0.565, 0.148, 0.144, 0.144]` |
| thompson | 0.05 | 20,000 | 0.0074 | 0.987 | 0.978–0.994 | `[0.562, 0.147, 0.147, 0.143]` |
| ucb1 | 0.05 | 500 | 0.1499 | 0.725 | 0.720–0.730 | `[0.523, 0.191, 0.148, 0.137]` |
| ucb1 | 0.05 | 2,000 | 0.0772 | 0.956 | 0.946–0.966 | `[0.551, 0.159, 0.146, 0.144]` |
| ucb1 | 0.05 | 8,000 | 0.0221 | 1.000 | 1.000–1.000 | `[0.567, 0.144, 0.144, 0.144]` |
| ucb1 | 0.05 | 20,000 | 0.0088 | 1.000 | 1.000–1.000 | `[0.566, 0.145, 0.145, 0.145]` |

**Three measurements settle it.**

1. *It is not slow convergence.* The `auto` configuration sits at shareBest
   0.387 after 500 rounds and 0.404 after
   20,000 rounds, and regret per round barely moves
   (0.351 → 0.332). Forty times more
   traffic buys no convergence: the cumulative regret curve is a straight line.
2. *It is the selection rule, and the floor sets the ceiling.* `algorithm: auto` on a
   continuous reward resolves to `simpleWeighted`, whose `selectionMode: weighted`
   samples an option **proportionally to its weight** and never switches to argmax.
   Its best-arm share is therefore bounded by the best arm's weight share, and the
   observed cap tracks `learning.min_exploration` monotonically downward: shareBest
   0.771 at floor 0.0, 0.404 at 0.05,
   0.282 at 0.2, 0.262 at 0.4 — while regret
   per round rises in lockstep (0.103 → 0.441).
   Raising an exploration floor should hurt a policy that has already converged; it
   does not rescue one whose selection rule is a sampler. The final weight vectors
   make the mechanism visible: at floor 0.0 the weights separate
   `[0.785, 0.191, 0.014, 0.010]`, at floor 0.4 they are flattened
   `[0.260, 0.251, 0.249, 0.240]` — the floor is renormalised into
   every weight each update, so it *is* the steady-state selection distribution.
3. *Greedy-resolved algorithms on the identical stream do converge.* Thompson
   reaches shareBest 0.987 at 0.0074 regret per
   round; UCB1 reaches 1.000 at 0.0088, same
   20,000 rounds, same seeds.

**Conclusion — reported as a finding, and it is a real weakness of the default
mapping.** The 0.404 figure is *expected behaviour of the resolved configuration*: a
weighted sampler exploiting proportionally to weight forever, so a best-arm share
equal to its weight share is the rule working as coded and the learner is not
broken. But the mapping that puts a capsule there by default is worth acting on:

- `CapsuleSpec::resolved_algorithm` sends `reward.type: continuous` → `Weighted`. A
  capsule declaring only `algorithm: {type: auto}` therefore ships a policy that never
  stops exploring, and its cumulative regret grows linearly
  (≈0.33/round at 20,000 rounds against
  0.007 for Thompson and 0.009 for UCB1 on the
  same traffic and seeds — a 45× gap
  against Thompson and 38× against UCB1,
  widening for every extra
  round the deployment runs.
- Tuning `min_exploration` is not the fix. Dropping it to 0.0 raises the weighted
  ceiling to shareBest 0.771 and cuts regret/round to
  0.103, but the policy still never goes greedy. The
  operator-visible fix is to declare `algorithm: {type: thompson}` (or `ucb`) for
  continuous-reward capsules when exploitation is the goal — which is what these
  tables recommend, and what the eval capsules under `evals/traffic/capsules/` show.

Per-context share from the same 4 000-round weighted runs:

| Context | Traffic share | shareBest (mean over seeds) |
| --- | ---: | ---: |
| `z_low` | 0.5 | 0.438 |
| `z_mid` | 0.35 | 0.408 |
| `z_high` | 0.15 | 0.440 |

The spread across contexts is 0.032 — negligible — and the highest share
belongs to `z_high`, which carries 0.15 of the traffic, not to
`z_low` with 0.5. So the 0.43 ceiling is *not* a per-context
sample-starvation effect that more traffic, or per-context state, would fix. Every
context plateaus at the same place the shared weight vector plateaus, which is the
signature of a selection rule with a hard ceiling rather than a learner waiting for
data.

## Offline-policy-evaluation round-trip (IPS + doubly robust)

This phase checks the *measuring instrument*, `examples/offline-eval` (`syntra_ope`),
against data whose answer is known analytically.

A seeded ε-greedy behaviour policy (ε = 0.2, seed 20260908,
4000 rounds) plays `evals/traffic/stationary.yaml` and logs
`decision_id,context_key,action,propensity,reward`. The target policy is the true best
arm per context (`primary`), so its
value is known: 0.9000. A bug in the log generator or
the estimator shows up immediately as a miss.

```bash
python3 scripts/eval-report.sh   # phase C generates this log in-process with seed 20260908
python3 examples/offline-eval/evaluate.py ope_log.csv \
    --mode static --policy-json ope_policy.json \
    --bootstrap 400 --bootstrap-seed 42 --format json
```

Behaviour-policy arm counts over the log: `primary` 3421, `secondary` 185, `degraded_cache_only` 201, `circuit_break` 193.

| Estimator | Estimated value of the target policy | 95 % bootstrap CI | True value | Abs. error | CI covers truth |
| --- | ---: | --- | ---: | ---: | :---: |
| IPS | 0.9048 | [0.8956, 0.9145] | 0.9000 | 0.0048 | yes |
| DR | 0.8991 | [0.8977, 0.9004] | 0.9000 | 0.0009 | yes |

The uncorrected mean reward of the same log is 0.8121
(analytic value of the ε-greedy mixture: 0.8100), so
the naive read understates the target policy by 0.0879.
Both importance-weighted estimators land on the known answer inside their own
bootstrap intervals, so the IPS/DR round-trip is sound.

### What the first version of this phase got wrong

The first log this phase generated was biased, and the round-trip is what caught it.
The propensity column originally recorded the probability of the *branch* that
produced each row (`ε/n` for any exploration draw) instead of the action's *marginal*
probability. An exploration draw that happens to land on the greedy arm still has
total probability `1−ε+ε/n`; logging `ε/n` for those rows inflates their IPS weight by
17.0× and biases the estimate upward on exactly the rows that
matter. Scored on the same log, both ways:

| Propensity logged | IPS estimate | True value | Abs. error |
| --- | ---: | ---: | ---: |
| marginal `1−ε+ε/n` / `ε/n` (what the CSV now carries) | 0.9047 | 0.9000 | 0.0047 |
| branch-conditional `1−ε` / `ε/n` (the original bug) | 1.8268 | 0.9000 | 0.9268 |

204 of 4000 rows were
affected, and the distorted estimate sits far outside its own confidence interval —
which is the practical argument for running OPE against a synthetic stream with a
known answer before trusting an OPE number about anything real.
Warnings emitted by the evaluator on the corrected log: none.

## A/B harness round-trip (live `syntra serve`)

`examples/ab-harness` drives two capsules over HTTP against a running server. This
phase is not a claim about which example capsule learns better — with
2 seeds × 120 rounds the paired test cannot separate them,
and the table says so. It is a check that the instrument works end-to-end against the
real server: author → install → decide → feedback → aggregate.

```bash
LYCAN_RNG_SEED=7 syntra serve --addr 127.0.0.1:<port> --store <tmp> --admin-key <key>
PATH=$PWD/target/release:$PATH python3 examples/ab-harness/ab_harness.py \
    examples/ab-harness/example_capsule_a.yaml examples/ab-harness/example_capsule_b.yaml \
    examples/ab-harness/example_traffic.yaml \
    --rounds 120 --seeds 2 --seed-offset 1000 \
    --tenant eval --job ab --output-dir <tmp>
```

| Metric | Capsule A | Capsule B |
| --- | ---: | ---: |
| mean cumulative reward | 57.2326 | 56.9759 |
| stderr across seeds | 2.9202 | 0.7660 |
| mean regret vs oracle | 8.7675 | 9.0241 |
| refusal rate | 0.0000 | 0.0000 |
| per-seed B−A | -2.411, +1.897 | |

Harness verdict: winner `a`, `confidence_b_better_at_95pct = false`, paired t-test p = 0.9245. Refusal rate 0.0000 on both sides, which is the point
of the phase (see finding 4).

## Findings

1. **The headline number no longer depends on an external binary.**
   `--compare-baseline random|first-arm|epsilon-greedy:<eps>` scores a reference
   policy on the same true arm means, rounds and seeds as the live policy and emits
   `baselineComparison` in the same JSON; `--compare-vw` still works as an optional
   extra. Pinned by `tests/eval_baselines.rs`: analytic `first-arm` regret, exactly
   zero regret for all three comparators on an all-equal stream, the unbiased-random
   identity, and the ε-greedy-below-random ordering.
2. **`--compare-vw` is not a Vowpal Wabbit baseline today.** The existing path
   generates its actions from an internal uniform-random stream, computes regret from
   that stream, shells out to `vw --cb_explore` only to check it exits 0, and
   discards VW's predictions; with `vw` absent it degrades to a `[warn]` and exit 0.
   Recorded rather than quietly rewritten — changing that output's meaning is a
   contract decision for the maintainers, and the built-in comparators make it
   unnecessary for the headline. Anyone tempted to quote `vwComparison` as "VW"
   would in fact be quoting a random policy.
3. **`shareBestArm = 0.404` is behaviour, and the `auto` mapping is the weakness.**
   Weighted selection never goes greedy: shareBest tracks the exploration floor and
   regret stays linear out to 20,000 rounds, while Thompson and UCB1 on the
   same traffic and seeds converge to ≈1.0 with one to two orders of magnitude less
   regret per round. Continuous-reward capsules should not ship on `auto` when the
   operator wants exploitation.
4. **The A/B harness could not see any decisions before this report.** Its arm
   extractor compared `decisions[0].chosen_option` — which `/decide` returns as an
   option **index** — against option **names**, so every round was tallied as a
   refusal: the pre-fix run on this machine reported refusal rate 1.0000 and mean
   cumulative reward 0.0000 for both capsules, with an all-zero `seeds.csv`. It now
   maps the index back into the capsule's option list (names are still accepted), and
   the table above is the first populated result that harness has produced here.
5. **`simulate` fails closed on malformed input — and the reported exit-0 symptom is
   not reproducible on this commit.** A reward list whose length does not match the
   capsule's option count already exits 1 through the engine's validation, for both
   `--true-arm-rewards` and `--traffic`; `tests/eval_baselines.rs` pins that exit
   status on both paths so it cannot regress. What *was* fail-open, one layer down,
   is now fixed and pinned: non-numeric entries in `--true-arm-rewards` were silently
   dropped, shrinking the arm list and turning a typo into a different experiment;
   they now abort with exit 2, as do malformed `--compare-baseline` arguments.
6. **The OPE round-trip earned its keep by catching a bug in itself.** The propensity
   column in the phase-C log initially recorded branch-conditional probabilities;
   IPS came back 1.827 against a known
   0.900. Fixed to marginal propensities, IPS is
   0.905. Worth remembering the next time
   an OPE number about production traffic looks plausible.
7. **`simulate --seed` did not pin the run, and nothing checked it.** The traffic
   RNG honoured `--seed`; the learner did not. Thompson samples, the weighted
   roulette and the meta-bandit tie-breaks come from `learning::rng`, which falls
   back to SystemTime entropy, and only `cli_serve` ever called `seed_rng` (via
   `LYCAN_RNG_SEED`) — setting that variable had no effect on `simulate`. Eight
   identical runs of the headline command on this machine, before the fix, spread
   mean cumulative regret from 1,329.33 to 1,355.23, and the reported final weight
   vectors moved with them, so any number quoted from this tool was a single
   unrepeatable draw. `run_traffic` now seeds the shared PRNG per seed-run and hands
   it back to entropy afterwards; the phase-0 probe above re-checks it on every
   regeneration and `tests/eval_baselines.rs::repeated_invocation_is_reproducible`
   pins it. Residual, measured: `finalWeights` still drifts at last-ulp scale across
   processes (float accumulation inside the learning layer is not a pure function of
   the seed), so the probe tolerates that one field rather than claiming full-JSON
   identity. Caveat for readers of older Syntra documents: numbers produced by
   `simulate` before this change are not reproducible from their stated seeds.
8. **What this report cannot say.** No production win-rate, no MoEfolio numbers, no
   live-traffic regret: that data is not in this repository and was not invented to
   fill the gap. Everything above is scored against a reward generating process
   written in YAML, which makes it a mechanism check and a regression baseline, not
   evidence of field performance.

## How to reproduce

```bash
cargo build --release                 # once
bash scripts/eval-report.sh           # ~30-60 s; rewrites this report + the JSON artifact
```

Overrides: `ROUNDS`, `SEEDS`, `BASE_SEED`, `AB_ROUNDS`, `AB_SEEDS`, `OPE_ROUNDS`,
`OPE_EPSILON`, `OPE_BOOTSTRAP`, `OPE_SEED`, `RNG_SEED`, `REPORT_DATE`. The `simulate`
phases are seed-deterministic and machine-independent; the A/B phase is deterministic
given `LYCAN_RNG_SEED` plus sequential request order (its wall-clock is not), and the
OPE bootstrap is pinned with `--bootstrap-seed 42`.

One table by hand:

```bash
target/release/syntra simulate evals/traffic/capsules/routing-4arm-auto.yaml --traffic evals/traffic/stationary.yaml --rounds 4000 --seeds 12 --seed 42 --compare-baseline random --format json
```

Machine-readable copy of every measurement above: `2026-09-08-adaptive-policy-baseline.json`.

