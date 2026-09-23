# Lycan examples

Small programs in the Lycan language. In Syntra, Lycan is the language of
a capsule's optional feature program (derived features and action
exclusions, run in a sandbox before each decision); these examples show
the language itself. Run any of them with the `lycan` binary
(`cargo build --release` builds it):

```bash
./target/release/lycan examples/lycan/hello.lycs
```

| File | What it shows |
|---|---|
| `hello.lycs` | The smallest program: prints a line. |
| `fibonacci.lycs` | A recursive function. |
| `fizzbuzz.lycs` | Loops and conditionals. |
| `pipeline.lycs` | Filter, map and reduce with lambdas. |
| `calculator.lycs` | Reads lines from stdin until `q`. |
| `json-input.lycs` | `runtime.inputGet` on structured input: `lycan examples/lycan/json-input.lycs --input examples/lycan/request.json`. |
| `demo_adaptive_routing.lycs` | Latency statistics through native capabilities, then a `strategy` node choosing among three timeout policies. `--input` with a `{"latencies": [...]}` file replaces the embedded data. |
| `capability-policy/demo_capability_pack.lycs` | File, JSON, statistics, forecasting and autoscaling capabilities. It writes `/tmp/lycan_capability_pack_demo.json`. |

Each `.lyc` next to a `.lycs` is its compiled graph binary;
`tests/fixture_drift.rs` checks that they match a fresh
`lycan compile`. [docs/lycan](../../docs/lycan/README.md) documents the
language.
