# Lycan Showcase Demos

This folder is the public demo surface.

The rest of `examples/` is intentionally large: fixtures, experiments, regression scripts, compiled binaries, science models, and development checks. This folder keeps the story tight.

## The Five

| Demo | Script | What It Proves |
|------|--------|----------------|
| 01 | `01-apps-learn-contexts.sh` | One capsule can learn different winners for Context A, B, and C. |
| 02 | `02-live-mars-mission.sh` | Lycan can fetch live NASA/JPL Horizons data, run a native Lambert solver, and learn from mission feedback. |
| 03 | `03-autonomous-evolution.sh` | Lycan can accept a better proposal, reject a wrong one, snapshot, and journal the attempt. |
| 04 | `04-sandbox-red-team.sh` | File escape and SSRF attempts are blocked by runtime policy. |
| 05 | `05-runtime-appliance-memory.sh` | The API appliance installs a capsule, learns from feedback, restarts, and keeps memory. |

Run everything:

```bash
./examples/showcase/run-all.sh
```

Run one:

```bash
./examples/showcase/01-apps-learn-contexts.sh
```

## Naming Rule

Showcase demos are named for the claim they prove, not the implementation detail.

Good:

```text
02-live-mars-mission.sh
04-sandbox-red-team.sh
```

Avoid:

```text
demo_runtime_capability_pack_v2_final_test.sh
```

## What Stays Outside This Folder

- Unit and integration tests stay in `tests/`.
- Exhaustive API regressions stay as `examples/demo-api-regression.sh`.
- Boundary torture suites stay as `examples/demo-boundary-api-tests.sh`.
- Raw `.lycs` programs stay in `examples/`.
- Compiled `.lyc` fixtures stay where current tests expect them.
