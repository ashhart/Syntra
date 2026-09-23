# Syntra demos

Every demo below runs against the v2 server. The scripts exit 0 only when
all their checks pass, and the three under `scripts/` are also run by
`tests/demo_smoke.rs` on every test run, which asserts their headline
results.

| Demo | Run | What it shows |
|------|-----|---------------|
| Watch it learn | `cargo run --release --bin syntra -- demo` | A server on a fresh temporary store with simulated LLM-routing traffic (made-up quality and cost per route and task). Prints the admin console's address and key, and every ten seconds the share of the best possible expected reward the capsule earns. Ctrl-C stops it. |
| LLM routing | [examples/llm-routing](examples/llm-routing/) | `syntra.llm.ModelRouter` routing 4,000 requests between three simulated model routes with delayed quality grades, decisions made in-process and verified by the server, learning per task, then `syntra evaluate --store` on the logs: DR estimates of the constant policies land on the simulation's known values, and a gate refuses always-large. Needs the Python extension (`sdk/python/scripts/develop.sh`). |
| Containment matrix | [scripts/demo-containment.py](scripts/demo-containment.py) | 14 attack vectors from a feature program wired to every I/O capability: file and symlink escapes toward a canary outside the sandbox, a policy that tries to widen its file root, SSRF to metadata, RFC1918 and the admin console, plain http to an allowed host, exfiltration, policy flips, a compute-budget abort. 24/24 checks; every denial is a 500 audited as `execution_denied` with its request id. |
| TLS gateway | [scripts/demo-tls-gateway.py](scripts/demo-tls-gateway.py) | The server behind a TLS reverse proxy: spec, program install, decide and reward over TLS, wrong CA and hostname mismatch rejected, plain HTTP refused. 8/8 checks. |
| Agent governor | [scripts/demo-agent-governor.py](scripts/demo-agent-governor.py) | Agent tool calls decided by a learning capsule: a budget rail in the feature program (`exclude.<id>`) that forces `block` with probability 1, learned allow/hold per agent from rewards, byte-identical model after a restart, tenant isolation. 10/10 checks. |

[scripts/smoke-test.sh](scripts/smoke-test.sh) is a quicker check of a
build: 32 checks of auth, decide and reward, idempotency, scoped tokens, a
feature program and its sandbox, metrics, evaluation, restart persistence,
backup and doctor, in about a second.

The v1 demos (LLM model routing and governed routing with a replay gate,
both shell scripts, and anomaly-aware routing) went away with the v1
API. Model routing with delayed feedback is now `examples/llm-routing`,
the replay gate is `syntra evaluate` and `POST .../promote`, and
computing signals before a decision is a feature program, as in the
containment and agent-governor demos. The science demos (Mars transfer
search, edge of chaos, pandemic policy scoring and others), the proof lab
and self-evolving capsules live in the separate Lycan Lab repository.
