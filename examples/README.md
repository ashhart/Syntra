# Examples

- [`llm-routing/`](llm-routing/): routing requests between three
  **simulated** model routes with `syntra.llm.ModelRouter` (Python,
  in-process decisions), watching it learn, then scoring other policies on
  the logged traffic with `syntra evaluate --store`.
  `python3 examples/llm-routing/llm_routing.py`
- [`learning_bench.rs`](learning_bench.rs): how close the decision engine
  gets to the best policy on simulated contextual bandits with a known
  optimum (fixed segments, segments whose best action changes halfway, and
  a 200-item catalog). No server needed; about 3 seconds.
  `cargo run --release --example learning_bench`
- [`bench_decide.rs`](bench_decide.rs): latency and throughput of HTTP
  `/decide` (and `/reward`) against a running server, over keep-alive
  connections.
  `cargo run --release --example bench_decide -- --addr 127.0.0.1:8787 --key <key> --concurrency 8 --requests 20000 --reward-every 1`
- [`bench_local.rs`](bench_local.rs): in-process decide latency with the
  Rust `LocalDecider`, and how fast the server verifies the uploads.
  `cargo run --release --example bench_local -- --addr 127.0.0.1:8787 --key <key> --threads 8 --decisions 100000`
- [`lycan/`](lycan/): small programs in the Lycan language, the language of
  Syntra's optional feature programs.

The benchmarks create their own capsules (`bench/bench/router`,
`bench/bench/local`) on the server you point them at; run them against a
throwaway store. Their numbers depend on the machine, so report them with
the hardware.

[DEMOS.md](../DEMOS.md) lists the end-to-end demos, including the three
scripts `tests/demo_smoke.rs` runs.
