# Syntra Feature Walkthroughs

Seven self-contained Python scripts that demonstrate each new Syntra feature
end-to-end. They complement the prose in `docs/tutorial.md` by giving
copy-pasteable scripts you can run against a live Syntra instance. No
third-party dependencies -- stdlib only (`urllib.request`, `json`,
`threading`, etc.).

## Prerequisites

A running Syntra instance and the `syntra` CLI binary on your PATH:

```bash
export SYNTRA_URL=http://localhost:8787
export SYNTRA_ADMIN_KEY=<your admin key>
```

Scripts 01-06 require the `syntra` binary for capsule compilation (scripts
call `syntra author`). Script 07 only needs the server reachable.

## Walkthroughs

| # | Script | What it demonstrates |
|---|--------|----------------------|
| 01 | `01_scoped_tokens.py` | Mint Read, TenantAdmin, and Admin tokens via `POST /admin/tokens`; verify each token is denied on a forbidden route (403). |
| 02 | `02_continuous_action_pricing.py` | Configure `actionSpace: {type: continuous, range: [10,60], buckets: 5}` and verify the `chosenAction` midpoints (15, 25, 35, 45, 55) are returned correctly. |
| 03 | `03_multi_objective_feedback.py` | Drive 60 decide/feedback cycles with `components`-based rewards (quality, latency_ms, cost_usd); inspect per-(option, component) Q estimates from `/memory`. |
| 04 | `04_batched_feedback.py` | Post 1000 events via `POST /feedback/batch` and compare wall-clock time against 1000 individual `POST /feedback` calls; print speedup ratio. |
| 05 | `05_backup_and_restore.py` | Drive feedback to Active state, call `POST /admin/backup` to save a bundle, then `POST /admin/restore` to replay it; verify learned weights survived via `/report`. |
| 06 | `06_rate_limit_handling.py` | Open 50 parallel threads hammering `/decide`; on 429 the client reads `Retry-After` and backs off; shows all requests eventually succeed with zero loss. |
| 07 | `07_metrics_scrape.py` | `GET /metrics`, parse the Prometheus exposition with a hand-rolled parser, and surface a dashboard: top capsule by decide volume, p99 latency, refusal rate, meta-bandit trial distribution. |

## Running a single walkthrough

```bash
cd /path/to/Syntra/examples/walkthroughs
python3 01_scoped_tokens.py --syntra-url $SYNTRA_URL --admin-key $SYNTRA_ADMIN_KEY
```

Each script accepts `--help` for the full argument list.

## Running all walkthroughs in sequence

```bash
cd /path/to/Syntra/examples/walkthroughs

for script in 01_scoped_tokens.py \
              02_continuous_action_pricing.py \
              03_multi_objective_feedback.py \
              04_batched_feedback.py \
              05_backup_and_restore.py \
              06_rate_limit_handling.py \
              07_metrics_scrape.py; do
  echo "=== $script ==="
  python3 "$script" \
    --syntra-url "$SYNTRA_URL" \
    --admin-key  "$SYNTRA_ADMIN_KEY"
  echo
done
```

Or via make if you add a `Makefile`:

```makefile
run-all:
	for f in 0*.py; do python3 $$f --syntra-url $(SYNTRA_URL) --admin-key $(SYNTRA_ADMIN_KEY); done
```

## Running the smoke tests (no live Syntra needed)

```bash
cd /path/to/Syntra/examples/walkthroughs
for f in *.py; do python3 -m py_compile "$f"; done
PYTHONPATH=. python3 -m pytest tests/ -v
```

All 9 tests should pass (7 import-and-callable checks + 1 metrics parser unit
test + 1 midpoint formula check) with no Syntra instance running.

## Notes on feature dependencies

- **`02_continuous_action_pricing.py`** requires the `actionSpace` field to be
  wired in the server's `learning` endpoint and the `chosenAction` key to be
  present in `/decide` responses (Phase 3A feature).
- **`03_multi_objective_feedback.py`** requires `reward_spec` upload via
  `PUT /reward_spec` and the server to track `objectiveRewards` /
  `objectiveCounts` in memory per option.
- **`05_backup_and_restore.py`** requires `POST /admin/backup` and
  `POST /admin/restore` (Phase G/H admin endpoints).
- **`06_rate_limit_handling.py`** requires the server's `RateLimiter` to be
  configured with a meaningful limit; if no limit is set, all decides succeed
  on the first try and the throttled count will be 0 (the script still passes).
- **`07_metrics_scrape.py`** expects metric family names beginning with
  `syntra_` or `lycan_`; the parser is name-agnostic and will work with
  whatever prefix the server uses.

## License

Apache-2.0.
