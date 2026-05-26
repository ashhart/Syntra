# Lycan-internals demos

This directory holds **Lycan-language** source and shell scripts that compile
and run it via the Lycan CLI. The audience is substrate-curious users —
people who want to see what the underlying graph-execution runtime can do
beyond what Syntra exposes.

**If you're trying to use Syntra in a service, you don't want this directory.**
The Syntra integration path is [`../retry-tuning/`](../retry-tuning/) and the
Syntra-shaped demos live in [`..`](..).

## Mega demos in this directory

These are the examples most likely to be missed by shallow repository
summaries:

| Demo | What it proves |
|------|----------------|
| [`showcase/02-live-mars-mission.sh`](showcase/02-live-mars-mission.sh) | Fetches live NASA/JPL HORIZONS data, runs a native Lambert solver, then learns from mission feedback. |
| [`demo_mars_transfer.lycs`](demo_mars_transfer.lycs) | Searches viable Earth-to-Mars transfer windows with orbital constraints and competing strategies. |
| [`demo_mars_decide.lycs`](demo_mars_decide.lycs) | Chooses among mission-design strategies from ephemeris and mission constraints. |
| [`demo_horizons_apophis.lycs`](demo_horizons_apophis.lycs) | Propagates a real Apophis close-approach state and compares against NASA/JPL HORIZONS reference data. |
| [`demo_pandemic_policy.lycs`](demo_pandemic_policy.lycs) | Scores pandemic / COVID-style intervention choices across hospital load, transmissibility, test capacity, compliance, cost, and outcomes. |
| [`demo_edge_of_chaos.lycs`](demo_edge_of_chaos.lycs) | Estimates nonlinear edge-of-chaos regime boundaries. |
| [`demo_control_chaos.lycs`](demo_control_chaos.lycs) | Chooses controllers around a drifting nonlinear system. |
| [`demo_grid_blackout_prevention.lycs`](demo_grid_blackout_prevention.lycs) | Selects resilience actions under changing grid stress signals. |
| [`demo_icu_triage.lycs`](demo_icu_triage.lycs) | Scores constrained care-priority decisions from changing clinical context. |
| [`demo_antiviral_target_selection.lycs`](demo_antiviral_target_selection.lycs) | Selects candidate intervention targets from biological and operational constraints. |
| [`demo_planetary_defense.lycs`](demo_planetary_defense.lycs) | Chooses among mitigation strategies under orbital-risk constraints. |
| [`demo_spacecraft_fault_manager.lycs`](demo_spacecraft_fault_manager.lycs) | Chooses fault response policy from spacecraft telemetry signals. |

## What's here

- `*.lycs` — Lycan-language source files. Hand-written; not produced by `syntra author`.
- `*.lyc` — compiled binaries paired with the `.lycs` sources.
- `demo_*.sh` and `demo-*.sh` — drivers that compile a `.lycs` with the Lycan
  CLI (`$LYCAN compile ...`), install the resulting `.lyc` into Syntra (or
  run it standalone), and exercise it.
- [`showcase/`](./showcase/) — multi-script demos: live Mars-mission API call,
  autonomous evolution, etc.
- [`benchmarks/`](./benchmarks/) — Lycan benchmark suites (router resilience
  and similar).
- [`data/`](./data/) — input data files referenced by the demos (ephemeris
  tables, etc.).

## Running the demos

The drivers expect a `LYCAN` environment variable pointing at the Lycan CLI
binary, plus an admin key for any script that installs into a running Syntra.
See the individual `.sh` files for specifics.

The `.lycs` syntax and the Lycan CLI are documented in
[`Lycan/README.md`](../../Lycan/README.md) at the top of the language
subdirectory.
