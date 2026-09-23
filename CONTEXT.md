# Context

Read this first to summarize the repository.

## What Syntra is

A self-hosted contextual-bandit decision service, in one Rust crate. An
application asks a capsule (one decision point, addressed as
`tenant/job/capsule`) which of several actions to take, acts, and reports a
reward later. Syntra learns from the rewards and logs every decision with
the probability distribution it was drawn from and its seed, so the log
supports off-policy evaluation, which estimates what another policy would
have earned before it serves. `POST .../promote` applies a spec change
only if it passes gates on that evaluation. There is also an Azure
Personalizer-compatible API (rank, reward, activate, service
configuration) and an importer for Personalizer and Vowpal Wabbit DSJSON
logs.

This is v2. It replaced the v1 learning stack (meta-bandits, strategy
weights, per-context buckets, `memory.json`, JSONL logs, `/report`,
`/contexts`, `/memory`) and refuses v1 stores. The CHANGELOG has the
history.

## How a decision flows

```text
POST .../decide {context, actions?}          (or an SDK decides in-process)
  -> auth: admin key or scoped token, scope check, rate limit   src/server/{routes,auth}.rs
  -> capsule runtime: spec + model in memory                    src/server/runtime.rs
  -> optional Lycan feature program under its execution policy  (derived features, exclusions)
  -> engine: hashed features -> learner scores -> exploration PMF -> seeded draw   src/decision/
  -> answer {decisionId, action, probability, ranking}
  -> write-behind queue -> syntra.db (decision with PMF and seed)   src/server/writer.rs, src/eventstore/
POST .../reward {decisionId, reward}  (any time later)
  -> learner update -> syntra.db; periodic model snapshots
SDK uploads (decisions:batch, rewards:batch)
  -> replayed on the published model before they are stored     src/server/upload.rs
POST .../evaluate, .../promote, `syntra evaluate`
  -> DM, IPS, SNIPS, DR with bootstrap intervals over the log    src/ope/
```

The process is disposable; the store (`syntra.db` plus spec, policy and
program files) is not. A restart rebuilds each model exactly from its
latest snapshot plus the rewards logged after it.

## Where things are

- `src/decision/`: the engine (features, learner, SquareCB and
  epsilon-greedy exploration, specs).
- `src/eventstore/`: the SQLite event store (decisions, rewards, model
  snapshots, audit).
- `src/server/`: the HTTP server (hyper): routes, auth, decide, reward,
  uploads, evaluate and promote, the Personalizer API, metrics, the admin
  console (`console.html`), specs from files, the default-reward sweeper.
- `src/ope/`: off-policy evaluation and gates. `src/client.rs`: the Rust
  `LocalDecider`. `src/import.rs`: DSJSON import. `src/demo.rs`:
  `syntra demo`. `src/backup.rs`, `src/doctor.rs`, `src/store.rs`:
  backups, store checks, the file layout.
- Lycan, the language of optional feature programs: `src/lexer.rs`,
  `parser.rs`, `graph_compiler.rs`, `verifier.rs`, `graph_executor/`,
  `capabilities/` (with the sandbox), `context.rs` (execution policy),
  `src/bin/lycan.rs` (the `lycan` CLI).
- `sdk/python` (PyO3: `LocalDecider`, `Client`, `syntra.llm.ModelRouter`),
  `sdk/typescript` (HTTP client).
- `docs/`: [quickstart](docs/quickstart.md), [concepts](docs/concepts.md),
  [operating](docs/operating.md), [deployment](docs/deployment.md),
  [api](docs/api.md) and [openapi.yaml](docs/openapi.yaml),
  [Personalizer migration](docs/migrating/personalizer.md),
  [design](docs/design/v2-decision-core.md), [Lycan](docs/lycan/README.md).
- `examples/` (LLM routing on simulated models, benchmarks, Lycan
  programs), `scripts/` (smoke test and three demos, see
  [DEMOS.md](DEMOS.md)), `deploy/helm/syntra`, `tests/`, `fuzz/`.

## How it is checked

`cargo test` runs the suites in `tests/`: the server and auth routes,
crash recovery under load, local evaluation, OPE, promotion, the
Personalizer API, DSJSON import, specs from files, the doctor and CLI,
security regressions, Lycan conformance vectors and fixtures, and
`openapi_drift.rs` (the router and `docs/openapi.yaml` must agree).
`demo_smoke.rs` runs the three demo scripts. `fuzz/` has five fuzz
targets. The Python and TypeScript SDKs have their own tests against a
real server, and CI lints and templates the Helm chart.

## Limits

- One server per store: SQLite with one writer, no replication.
- Feature programs run in the server process under a policy-enforced
  sandbox, not behind an OS boundary; `max_memory_bytes` is not enforced.
- Local evaluation needs the Rust or Python SDK; TypeScript decides over
  HTTP until its WebAssembly core exists. SDKs cannot decide locally for
  capsules with a feature program.
- The learner is linear in hashed feature crosses; rewards that depend
  nonlinearly on feature matches are only partly captured (see the
  learning benchmark in the README).
- No external security review; run it behind TLS and do not expose it to
  the public internet ([SECURITY.md](SECURITY.md)).

The science demos, proof lab, self-evolving capsules and the Lycan
interpreter moved to the separate Lycan Lab repository (split at commit
`15f5441`).
