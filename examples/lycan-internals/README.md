# Lycan-internals demos

This directory holds **Lycan-language** source and shell scripts that compile
and run it via the Lycan CLI. The audience is substrate-curious users —
people who want to see what the underlying graph-execution runtime can do
beyond what Syntra exposes.

**If you're trying to use Syntra in a service, you don't want this directory.**
The Syntra integration path is [`../retry-tuning/`](../retry-tuning/) and the
Syntra-shaped demos live in [`..`](..).

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
