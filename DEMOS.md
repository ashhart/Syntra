# Syntra demos

These run against the v2 server and are smoke-tested by
`tests/demo_smoke.rs`:

| Demo | Path | What it shows |
|------|------|---------------|
| Containment matrix | [scripts/demo-containment.py](scripts/demo-containment.py) | 14 attack vectors from a feature program wired to every I/O capability: file and symlink escapes toward a canary outside the sandbox, a policy that tries to widen its file root, SSRF to metadata, RFC1918 and the admin console, plain http to an allowed host, exfiltration, policy flips, a compute-budget abort. 24/24 checks; every denial is a 500 audited as `execution_denied` with its request id. |
| TLS gateway | [scripts/demo-tls-gateway.py](scripts/demo-tls-gateway.py) | The server behind a TLS reverse proxy: spec, program install, decide and reward over TLS, wrong CA and hostname mismatch rejected, plain HTTP refused. |
| Agent governor | [scripts/demo-agent-governor.py](scripts/demo-agent-governor.py) | Agent tool calls decided by a learning capsule: a budget rail in the feature program (`exclude.<id>`) that forces `block` with probability 1, learned allow/hold per agent from rewards, byte-identical model after a restart, tenant isolation. |

Not yet ported to the v2 API (they call v1 routes and fail against the
current server):

| Demo | Path | What it shows |
|------|------|---------------|
| LLM model routing | [examples/demo-llm-model-routing.sh](examples/demo-llm-model-routing.sh) | Three model routes and two contexts learned from delayed feedback, persisted across restart. |
| Governed LLM routing | [examples/demo-governed-llm-routing.sh](examples/demo-governed-llm-routing.sh) | Shadow run and replay gate. The replay uses simulated per-action rewards; see [CONTEXT.md](CONTEXT.md) for why this is not yet an off-policy evaluation. |
| Anomaly-aware routing | [examples/anomaly-routing/](examples/anomaly-routing/) | Latency statistics computed in the capsule before a routing choice. |

The science demos (Mars transfer search, edge of chaos, pandemic policy
scoring and others), the proof lab and self-evolving capsules moved to the
separate Lycan Lab repository.
