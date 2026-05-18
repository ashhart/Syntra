# Outbreak benchmark — output schema

Documents the shape of `<output_dir>/summary.json` and `<output_dir>/seeds.csv`
written by `benchmark.py` (write site: `benchmark.py:759`). Describes what is
emitted today; not a contract — change the writer, update this file.

## `summary.json`

```jsonc
{
  "benchmark": "outbreak_early_warning_resilience",  // string, constant
  "seeds":   <int>,   // number of seeds run
  "weeks":   <int>,   // weeks per seed (typically 52)
  "regions": <int>,   // regions per seed (typically 4)

  "criteria": {
    "c1_beats_extremes":           { "pass": <bool>, "value": "<pct>", "threshold": ">=80%" },
    "c2_beats_adaptive":           { "pass": <bool>, "value": "<pct>", "threshold": ">=60%" },
    "c3_fewer_deaths_vs_threshold":{ "pass": <bool>, "value": "<pct>", "threshold": ">=50%" },
    "c4_cheaper_than_lockdown":    { "pass": <bool>, "value": "<pct>", "threshold": ">=90%" },
    "overall": { "pass": <bool>, "passed": <int>, "total": 4 }
  },

  "aggregate": {
    "<policy_name>": {
      "mean_deaths":          <float>,
      "mean_econ_cost_M":     <float>,   // millions of $
      "mean_score_original":  <float>,
      "mean_score_corrected": <float>,
      "mean_reward_original": <float>,
      "mean_reward_corrected":<float>
    }
    // …one entry per policy
  }
}
```

`<pct>` in the criteria block is a string like `"0%"` or `"100%"` (not a
number) — the writer formats it before serializing.

### Policy names appearing in `aggregate`

Hand-coded baselines plus Syntra:

```
none_always, lockdown_always, threshold, reactive, lagged_oracle,
myopic_oracle, proactive_t100_lvl1, proactive_t50_lvl1, proactive_t20_lvl1,
proactive_t5_lvl1, horizon_oracle, syntra
```

The set is policy-dependent — if a run adds or removes a baseline, the keys
under `aggregate` change. Don't hardcode the list when parsing; read the
keys out of the object.

### Where the load-bearing numbers live

- **The "2/4 pass" headline** is `criteria.overall.passed` / `criteria.overall.total`.
  Phase A-F baseline: 2/4 (c3 and c4 pass; c1 and c2 fail). Pass count >2
  on the same simulator is a real shift, not noise — investigate before
  citing as improvement.

- **Syntra's death count** is `aggregate.syntra.mean_deaths`. Phase A-F
  meta_bandit baseline: ~0.5. See [`Syntra/docs/known-issues.md`](../../../../docs/known-issues.md)
  for the drift band around weighted/ucb1 configs.

- **Syntra's economic cost** is `aggregate.syntra.mean_econ_cost_M` (millions).
  Phase A-F baseline: ~$26,300M.

- **Reward-blindness counterpart**: outbreak does **not** emit top-level
  `spread_original` / `spread_corrected` fields (those exist in the vaccine
  benchmark — see `../vaccine_allocation_resilience/SCHEMA.md`). For
  outbreak, derive the per-policy spread by reading
  `aggregate.<policy>.mean_score_original` / `mean_score_corrected` across
  all policies and taking `max - min` yourself.

### Process exit code

`benchmark.py` exits 0 if `criteria.overall.pass == true`, else 1. A failing
run still writes `summary.json` — don't gate parsing on the exit code.

## `seeds.csv`

Per-seed, per-policy raw values. Columns:

```
seed, policy, deaths, econ_cost_M, reward_original, reward_corrected,
score_original, score_corrected
```

Numeric columns are formatted with limited precision (`%.2f` / `%.3f` from
the writer) — parse as float, don't trust trailing digits beyond the
documented precision.
