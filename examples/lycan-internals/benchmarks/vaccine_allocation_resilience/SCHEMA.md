# Vaccine allocation benchmark — output schema

Documents the shape of `<output_dir>/summary.json` and `<output_dir>/seeds.csv`
written by `benchmark.py` (write site: `benchmark.py:727`). Describes what is
emitted today; not a contract — change the writer, update this file.

## `summary.json`

```jsonc
{
  "benchmark":   "vaccine_allocation_resilience",  // string, constant
  "seeds":       <int>,   // number of seeds run
  "seed_offset": <int>,   // first seed (e.g. 3000 means seeds 3000…3009)
  "weeks":       <int>,   // weeks per seed (typically 52)
  "regions":     <int>,   // regions per seed (typically 4)

  "aggregate": {
    "<policy_name>": {
      "mean_deaths":          <float>,
      "mean_cost_M":          <float>,   // millions of $ (NOT mean_econ_cost_M)
      "mean_reward_original": <float>,
      "mean_reward_corrected":<float>
    }
    // …one entry per policy
  },

  "spread_original":  <float>,   // max-min of per-policy mean_reward_original
  "spread_corrected": <float>    // max-min of per-policy mean_reward_corrected
}
```

Note: the vaccine schema **does not** have a `criteria` block. Outbreak does;
vaccine does not. Don't conflate them — the user-facing prior-session
recollection that "vaccine has criteria" is wrong against the actual file
shape today.

### Policy names appearing in `aggregate`

```
equal_split, proportional_to_cases, proportional_to_susceptible,
proactive_high_risk, myopic_oracle, syntra
```

When run with both context modes (`discrete` and `features`) the `syntra`
entries appear in separate output directories, not as separate keys in one
file. The key inside `aggregate` is always `"syntra"`.

### Where the load-bearing numbers live

- **The reward-blindness ratio** — the 4.4× number cited in
  `writeup_reward_blindness.md` — is `spread_corrected / spread_original`,
  with both at the **top level** of `summary.json` (NOT inside `aggregate`).
  Reproduced this cycle: `0.559 / 0.128 ≈ 4.37`.

- **Field naming differs from outbreak**: vaccine uses `mean_cost_M`,
  outbreak uses `mean_econ_cost_M`. If parsing both benchmarks with shared
  code, branch on the benchmark name.

- **No score fields**: vaccine's `aggregate` entries don't carry
  `mean_score_original` / `mean_score_corrected` (outbreak's do). The
  vaccine reward fed back to Syntra is the mean of per-region original
  rewards per week; only the reward fields are emitted.

- **Spread, not pass/fail**: vaccine reports the magnitude of the
  reward-shape problem (`spread_*`), not a pass/fail counter.
  `summary.json` has no `criteria.overall.passed` field — don't look for it.

### Process exit code

Always 0 on completion. No criteria-based gating.

## `seeds.csv`

Per-seed, per-policy raw values. Columns:

```
seed, policy, deaths, cost_M, reward_original, reward_corrected
```

(No score columns; vaccine doesn't emit `score_original` / `score_corrected`.)

Numeric columns formatted with `%.3f` / `%.4f` from the writer — parse as
float, don't rely on more precision than that.
