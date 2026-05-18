# Syntra A/B Harness

Simulation-driven head-to-head comparison of two Syntra capsules.

Both capsules receive the **exact same simulated traffic** -- same context
sequence, same random seed, same latent reward function -- so any performance
difference is attributable to the capsule's learning algorithm, not to luck of
the draw.  The harness runs over multiple independent seeds and applies a
paired t-test to determine statistical significance.

## Directory layout

```
ab-harness/
  ab_harness.py           -- main CLI and all harness logic
  README.md               -- this file
  example_traffic.yaml    -- example traffic specification
  example_capsule_a.yaml  -- baseline capsule spec
  example_capsule_b.yaml  -- variant capsule spec
  example_run.sh          -- convenience runner script
  tests/__init__.py
  tests/test_harness.py   -- unit tests (no live server required)
```

## Requirements

- Python 3.8+
- `syntra` binary on PATH (for capsule authoring)
- A running Syntra server (for the harness itself; tests run without one)
- **PyYAML is optional**: traffic specs can be written as JSON to avoid the
  dependency.  See the note in `example_traffic.yaml`.

No third-party Python packages are used beyond PyYAML (optional).  All math
uses `statistics`, `math`, and `random` from the standard library.

## CLI

```
python3 ab_harness.py <capsule_a.yaml> <capsule_b.yaml> <traffic_spec> \
    --rounds N --seeds K \
    --syntra-url <url> --admin-key <key> \
    [--output-dir <dir>] [--verbose]
```

Or via the installed entry point:

```
syntra-ab capsule_a.yaml capsule_b.yaml traffic.json \
    --rounds 1000 --seeds 10 \
    --syntra-url http://localhost:8787 --admin-key dev-key
```

### Arguments

| Argument | Default | Description |
|---|---|---|
| `capsule_a` | (required) | Path to capsule A YAML spec |
| `capsule_b` | (required) | Path to capsule B YAML spec |
| `traffic_spec` | (required) | Path to traffic spec (JSON or YAML) |
| `--rounds` | 200 | Decide/feedback rounds per seed |
| `--seeds` | 10 | Number of independent seeds |
| `--seed-offset` | 1000 | First seed value |
| `--syntra-url` | `http://localhost:8787` | Syntra server URL (or `$SYNTRA_URL`) |
| `--admin-key` | `dev-key` | Admin key (or `$SYNTRA_ADMIN_KEY`) |
| `--tenant` | `ab` | Syntra tenant (reset between seeds) |
| `--job` | `main` | Syntra job identifier |
| `--output-dir` | `results/run_<ts>/` | Output directory |
| `--verbose` | off | Print per-round errors to stderr |

## Traffic spec

Traffic specs can be written as **JSON or YAML**.  YAML requires PyYAML; JSON
works with stdlib only.

### Schema

```json
{
  "arms": ["option_a", "option_b", "option_c"],

  "true_rewards": {
    "option_a": 0.30,
    "option_b": 0.55,
    "option_c": 0.40
  },

  "noise_std": 0.10,

  "regime_shifts": [
    {
      "at_round": 500,
      "new_rewards": {"option_b": 0.35, "option_c": 0.60}
    }
  ],

  "context_sequence": ["low", "medium", "high"]
}
```

**Fields:**

- `arms` (required): list of arm names. Both capsule YAML files must declare
  exactly these option names.
- `true_rewards` (required): per-arm base reward.  Can be:
  - `{"arm": number}` -- context-independent
  - `{"arm": {"ctx_key": number, "__default__": number}}` -- context-dependent
  - A list of numbers in the same order as `arms`
- `noise_std` (optional, default 0.0): Gaussian noise std on observed reward
- `regime_shifts` (optional): list of `{at_round, new_rewards}` mid-run shifts
- `context_sequence` (optional): determines the context key each round.
  - A list: cycled by round index
  - `{"distribution": "uniform", "values": [...]}`: random uniform sampling
  - Omit for context-free operation

## How the simulation works

For each seed:

1. Both capsules are authored via `syntra author` and installed on the Syntra
   server at different paths within the same tenant:
   - A: `/tenants/<tenant>/jobs/<job>/capsules/a`
   - B: `/tenants/<tenant>/jobs/<job>/capsules/b`
2. For each round:
   a. A context key is drawn (same for both capsules).
   b. Both capsules are called via `/decide` with the same context.
   c. The traffic model computes observed rewards for each chosen arm
      (independently noised -- they may have chosen different arms).
   d. `/feedback` is sent to both capsules with their respective rewards.
3. The tenant is deleted between seeds to reset all learning state.

## Output

The harness writes to `--output-dir`:

- `summary.json`: aggregate metrics across all seeds
- `seeds.json`: per-seed result objects
- `seeds.csv`: per-seed results in CSV format

### Per-seed result

```json
{
  "seed": 1000,
  "a": {
    "cumulative_reward": 412.3,
    "mean_per_round": 0.41,
    "refusals": 0,
    "regret_vs_oracle": 87.7
  },
  "b": {
    "cumulative_reward": 446.1,
    "mean_per_round": 0.45,
    "refusals": 0,
    "regret_vs_oracle": 53.9
  },
  "head_to_head": {
    "b_minus_a_cumulative": 33.8,
    "b_won_round_pct": 0.62
  }
}
```

### Aggregate result

```json
{
  "rounds": 1000,
  "seeds": 10,
  "winner": "b",
  "a": {"mean_cumulative": 412.3, "stderr": 8.4, "regret_mean": 87.7, "refusal_rate": 0.0},
  "b": {"mean_cumulative": 446.1, "stderr": 9.1, "regret_mean": 53.9, "refusal_rate": 0.0},
  "p_value_paired_t": 0.0021,
  "confidence_b_better_at_95pct": true
}
```

**Metrics explained:**

- `cumulative_reward`: sum of all observed rewards over the run
- `regret_vs_oracle`: oracle_cumulative - actual_cumulative.  Oracle picks the
  best arm every round (after any regime shifts) with no noise.
- `refusals`: rounds where the capsule returned no decision (counted; reward
  treated as 0 for that round)
- `p_value_paired_t`: two-tailed paired t-test over per-seed cumulative
  rewards.  Implemented with stdlib only (no scipy).
- `confidence_b_better_at_95pct`: true iff B wins on mean and p < 0.05

## Running the tests

Tests exercise all pure math and parsing without a live server:

```bash
cd /path/to/ab-harness
python3 -m py_compile ab_harness.py          # syntax check
PYTHONPATH=. python3 -m pytest tests/ -v     # run tests
```

## Example

```bash
# With a running Syntra server:
./example_run.sh

# Or manually:
python3 ab_harness.py \
    example_capsule_a.yaml \
    example_capsule_b.yaml \
    example_traffic.yaml \
    --rounds 1000 --seeds 10 \
    --syntra-url http://localhost:8787 \
    --admin-key dev-key
```

## License

Apache-2.0.
