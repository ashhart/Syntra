# Agent Guide: Syntra

This file is for AI agents, maintainers, and collaborators working inside the Syntra repo.
To read and summarize the repository, start with `CONTEXT.md`.

## One-line identity

Syntra is a self-hosted contextual-bandit decision service. It logs every
decision with its propensity, learns from rewards, and gates changes on
off-policy evaluation. The Lycan language, used for optional feature
programs, ships in the same `syntra` crate.

## Product boundary

Use this language:

- **Lycan** = the language: `.lycs` syntax, parser/compiler, graph binary
  format, graph executor, capability ABI and sandbox, capsule format
  (`.lycap`), verifier, and the `lycan` CLI.
- **Syntra** = the decision service in this repo: decision core, event store
  (`syntra.db`), HTTP API, admin console, off-policy evaluation and promotion,
  the Personalizer-compatible API, SDKs (Rust and Python in-process,
  TypeScript over HTTP), the `syntra` CLI, the Docker image and the Helm
  chart.
- **Lycan Marketplace** = future distribution layer for signed capsules,
  capability packages, templates, and integrations.

Do not call this product "Lycan Studio". The browser UI is the admin console.

## Repo shape

One Rust crate at the root builds both binaries:

- `syntra`: `serve`, `demo`, `status`, `stop`, `health`, `doctor`, `backup`,
  `restore`, `import`, `evaluate` (entrypoint `src/main.rs` -> `syntra::run`
  in `src/lib.rs`).
- `lycan`: run (compile + verify + graph executor), `compile`, `explain`,
  `inspect`, `dump`, `stats`, `capabilities`, `serve` (the same server as
  `syntra serve`), `capsule create/verify/inspect/run` (entrypoint
  `src/bin/lycan.rs`).

Layout:

- `src/decision/` (engine, features, learner, exploration, spec),
  `src/eventstore/` (SQLite event store), `src/server/` (HTTP server,
  admin console, uploads, evaluate/promote, Personalizer API),
  `src/ope/` (off-policy evaluation), `src/client.rs` (Rust
  `LocalDecider`), `src/import.rs`, `src/demo.rs`, `src/backup.rs`,
  `src/doctor.rs`, `src/store.rs` (file layout).
- Lycan core: `lexer.rs`, `parser.rs`, `graph*.rs`, `verifier.rs`,
  `graph_executor/`, `capabilities/`, `context.rs` (execution policy),
  `binary.rs`, `capsule.rs`.
- `sdk/python`, `sdk/typescript`: client SDKs.
- `docs/`: quickstart, concepts, operating, deployment, API (with
  `openapi.yaml`), Personalizer migration, the v2 design, and `docs/lycan/`
  (language documentation and normative spec).
- `examples/`: LLM routing on simulated models, benchmarks, Lycan programs
  and compiled `.lyc` fixtures (`examples/lycan/`).
- `scripts/`: `smoke-test.sh` and the demos `tests/demo_smoke.rs` runs.
- `deploy/helm/syntra`: the Helm chart.

The science demos, proof lab, self-evolving capsules, the tree-walking
interpreter/REPL and the real-time control experiments moved to the separate
Lycan Lab repository on 2026-09-23 (split at commit `15f5441`).

## Runtime model

```text
client JSON (or an SDK deciding in-process and uploading)
  -> HTTP API: credential, scope, rate limit
  -> tenant / job / capsule runtime (spec and model in memory)
  -> optional feature program under the capsule's execution policy
  -> features -> learner scores -> exploration PMF -> seeded draw
  -> decision response (action, probability, decisionId)
  -> decision logged with its PMF and seed (write-behind to syntra.db)
  -> reward, later -> model update -> reward logged
  -> snapshots and audit trail; evaluate / promote read the log
```

The container is disposable. The store is sacred.

## Working rules for agents

1. Treat parser, graph format, capability ABI, and capsule format changes as
   language changes: keep the verifier fail-closed and update `docs/lycan/`.
2. Never add `.env`, API keys, production databases, Docker volumes, local
   stores, or `target/` artifacts.
3. Preserve fail-closed security behavior: no admin key means no startup
   unless explicit dev mode exists.
4. Preserve policy enforcement on server execution paths and the capability
   sandbox (allow-listed hosts, private networks denied, sandboxed file IO).
5. Preserve tenant/job/capsule isolation.
6. Use "admin console", not "admin studio".
7. Keep demo scripts short, named, and focused on proof: decide, reward,
   persistence, audit, sandbox, evaluation.
8. Keep language examples small, runnable, and named by what they teach;
   prefer explicit policy and capability examples over hidden magic.
9. If adding API routes, update `docs/openapi.yaml` (`tests/openapi_drift.rs`
   fails otherwise), the route table in `docs/api.md`, and the README.
10. Do not claim universal benchmark superiority; use measured language with
    hardware/test caveats.

## Useful commands

```bash
cargo build
cargo test -- --test-threads=1
cargo build --release

./target/release/syntra demo               # a server with simulated traffic
./scripts/smoke-test.sh                    # 32 checks against a real server
python3 scripts/demo-containment.py        # the sandbox's containment matrix

# Python SDK: build the extension, then test against a debug server
cargo build --bin syntra
sdk/python/scripts/develop.sh
python3 -m unittest discover -s sdk/python/tests -v

# Docker
cp templates/env.example .env              # then set LYCAN_ADMIN_KEY
docker compose up --build

# Lycan CLI
cargo run --bin lycan -- examples/lycan/hello.lycs
cargo run --bin lycan -- compile examples/lycan/hello.lycs
```

## Current TODO

- A Postgres event store and more than one decide node (today: one server
  per SQLite store).
- The TypeScript SDK's in-process decider (a WebAssembly build of the Rust
  core); local evaluation for capsules with a feature program.
- Keep security limitations honest in README, SECURITY.md and the
  deployment docs; the admin console needs a dedicated security review.
