# Syntra vs VW MAB benchmark — output schema

Documents the shape of `<output_dir>/summary.json` written by `benchmark.py`
(write site: `benchmark.py:319`). Describes what is emitted today; not a
contract — change the writer, update this file.

No companion `seeds.csv` for this benchmark — all per-seed data is in
`per_instance` inside `summary.json`.

## `summary.json`

```jsonc
{
  "config": {
    "seeds":  <int>,   // seeds per cell
    "rounds": <int>    // rounds per seed (typically 2000)
  },

  "per_instance": [
    {
      "n_arms":      <int>,    // 2, 5, or 10
      "difficulty":  "<str>",  // "easy" | "medium" | "hard"
      "seed":        <int>,    // raw seed value
      "seed_idx":    <int>,    // 0..seeds-1 within the cell
      "syntra_regret": <float>,
      "vw_regret":     <float>,
      "ratio":         <float>,// syntra_regret / vw_regret (lower is better for syntra)
      "syntra_time_s": <float>,// wall-clock seconds for syntra over `rounds`
      "vw_time_s":     <float>
    }
    // …one entry per (n_arms × difficulty × seed) cell
  ],

  "per_cell": {
    "<n_arms>_<difficulty>": {     // e.g. "2_easy", "5_hard", "10_medium"
      "syntra_regret": <float>,    // mean across seeds in this cell
      "vw_regret":     <float>,    // mean across seeds in this cell
      "ratio_mean":    <float>     // mean of `ratio` across seeds in this cell
    }
    // …9 cells total: {2,5,10} × {easy,medium,hard}
  },

  "bin": "<str>"   // pre-registered bin label; see below
}
```

### Bin labels

Computed from `per_cell` (logic at `benchmark.py:295-316`):

| Condition                                     | `bin` value |
|---|---|
| ≥7/9 cells with `ratio_mean ≤ 1.5`            | `"A — competent: within constant factor of VW on ≥7/9 cells"` |
| ≥7/9 cells with `ratio_mean ≤ 2.5` (but not A)| `"B — approximately competent: ≤2.5× VW on ≥7/9, but >1.5× on some"` |
| >2 cells with `ratio_mean > 2.5`              | `"C — core has issues, investigate"` |
| >2 cells with `ratio_mean < 1.0`              | `"D — suspicious: Syntra better than VW on multiple cells"` |
| otherwise                                     | `"Mixed — falls between bins, report numbers, no clean label"` |

The label is verbatim — including the em-dash — and contains a non-ASCII
character. Match against the prefix (`bin.startswith("A")`), not the whole
string.

### Where the load-bearing numbers live

- **The "2.7× lower regret" headline** is `1 / mean(per_cell.ratio_mean)`.
  Concretely: take the nine `ratio_mean` values from `per_cell`, average
  them, take the reciprocal. Phase A-F baseline (Syntra-Thompson):
  `1 / 0.374 ≈ 2.67×`. Not stored directly in the JSON — derive on read.

- **Per-cell competence** is `per_cell["<arms>_<diff>"].ratio_mean`. A
  cell with `ratio_mean > 1.5` is outside the "competent" band on that
  workload; ratio_mean > 2.5 means investigate. The bin label collapses
  these per-cell judgements into one verdict.

- **Per-seed raw regret** is in `per_instance`. Use it for distribution
  plots, scatter, or to investigate which seeds drove a high `ratio_mean`
  in a given cell.

- **Timing** (`syntra_time_s`, `vw_time_s`) is wall-clock for the whole
  `rounds`-long run. VW is dramatically faster — that's not a bug, it's a
  documented cost of running Syntra through the HTTP appliance. Don't
  compare per-decision latency from these fields; they include HTTP
  round-trip and seed-setup overhead.

### Process exit code

Always 0 on completion. The benchmark prints `PRE-REGISTERED OUTCOME: <bin>`
to stdout but does not gate the exit code on it.
