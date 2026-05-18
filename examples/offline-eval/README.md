# syntra-ope: Offline Policy Evaluation for Syntra capsules

`syntra-ope` estimates how a Syntra bandit capsule would have performed on a
dataset of decisions already logged by an existing system — without deploying
Syntra into production first.

## What it does

Given a CSV of historical decisions (action chosen, propensity of that action
under the logging policy, and observed reward), `syntra-ope` estimates the
counterfactual: "if Syntra had been making these decisions instead, what mean
reward would it have achieved?"

Two estimators are provided:

- **IPS (Inverse Propensity Score):** reweights each logged reward by
  `pi_eval(action|context) / logging_propensity`. Unbiased when propensities
  are correct. Can have high variance if logging and eval policies diverge.

- **DR (Doubly Robust):** combines IPS with a learned reward model (per
  (context, action) sample mean). Consistent if either the propensity model
  *or* the reward model is correct — hence doubly robust. Usually lower
  variance than pure IPS in practice.

Both estimators come with bootstrap confidence intervals (5th/95th percentile
by default, B=200 resamples).

## When is this the right tool?

Use `syntra-ope` when:
- You have a production or shadow log with propensity annotations (you know
  how likely the logging system was to pick each action it picked).
- You want to de-risk a Syntra deployment by estimating its value before
  running an A/B test.
- You want to compare multiple candidate converged policies offline.

Do not use it when:
- You have no propensity data (IPS and DR are both undefined).
- Your action space is continuous (these estimators work on discrete actions).
- The logging policy is fully deterministic with propensity 1.0 everywhere —
  coverage is insufficient; consider collecting some exploration data first.

## What the estimates mean

`logging_policy_mean_reward` is the raw mean reward from the log — what the
current system actually achieved on average.

`eval_policy_estimates.ips.mean` is the IPS estimate of what Syntra would
have achieved. If this is higher than the logging mean, Syntra is predicted to
outperform the current system.

The confidence interval `[ci_5, ci_95]` tells you how much uncertainty remains
given your log size. A wide CI means you need more data before trusting the
estimate. A narrow CI where the lower bound exceeds the logging mean is a
strong signal that Syntra genuinely helps.

## Layout

```
syntra_ope/__init__.py    # library: IPS, DR, bootstrap CI, EvalPolicy, load_csv
evaluate.py               # CLI entry point
example_data.csv          # tiny synthetic dataset (25 rows, 3 contexts, 2 actions)
examples/
  converged_policy.json   # example converged-policy lookup table
example_basic.py          # minimal demonstration (no Syntra required)
setup.py                  # pip-installable as syntra-ope
tests/
  test_estimators.py      # 8 unit tests
README.md                 # this file
```

## Quick start

### Static mode (no Syntra server required)

```bash
cd Syntra/examples/offline-eval
pip install -e .

# Run the CLI
python evaluate.py example_data.csv \
    --policy-json examples/converged_policy.json \
    --mode static \
    --format json

# Or run the minimal example
python example_basic.py
```

### Bandit mode (live Syntra required)

Bandit mode replays the log row-by-row against a running Syntra instance. On
each row, it calls `/decide`, then feeds back the logged reward so the bandit
evolves. The eval policy is the table of Syntra's choices per context key.

```bash
python evaluate.py example_data.csv \
    --capsule path/to/capsule.yaml \
    --mode bandit \
    --syntra-url http://localhost:8787 \
    --admin-key dev-key \
    --format json
```

## CSV schema

```
decision_id,context_key,action,propensity,reward
dec_001,low_risk,policy_a,0.5,0.8
dec_002,high_risk,policy_b,0.3,0.4
...
```

- `decision_id`: unique row identifier (string).
- `context_key`: the discrete context the logging system used. This must match
  Syntra's contextKey vocabulary if you plan to use bandit mode.
- `action`: the action the logging policy chose.
- `propensity`: the probability the logging policy assigned to that action.
  **This column must not be blank** — the tool errors clearly if it is.
- `reward`: the observed reward (float, any scale, but consistent across rows).

## Converged-policy JSON (static mode)

```json
{
  "low_risk": "policy_a",
  "medium_risk": "policy_a",
  "high_risk": "policy_b"
}
```

This is a mapping from context key to the single best action. It represents
what a Syntra bandit converges to after sufficient feedback. You can obtain it
by reading Syntra's `/state` endpoint after a run, or by running the bandit
mode once and exporting the resulting policy table.

## Running tests

```bash
cd Syntra/examples/offline-eval
PYTHONPATH=. python -m pytest tests/ -v
```

## License

Apache-2.0.

## Reference

Miroslav Dudik, John Langford, Lihong Li. "Doubly Robust Policy Evaluation
and Learning." Proceedings of the 28th International Conference on Machine
Learning (ICML 2011).
