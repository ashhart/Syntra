# Agent Guide: Syntra

This file is for AI agents, maintainers, and collaborators working inside the Syntra repo.
For reading and summarizing the repository, start with `CONTEXT.md`.

## One-line identity

Syntra is the self-hosted adaptive decision runtime for running Lycan capsules
in real applications. The Lycan language core ships inside this repo as part
of the single `syntra` crate.

## Product boundary

Use this language:

- **Lycan** = the language: `.lycs` syntax, parser/compiler, graph binary
  format, graph executor, capability ABI, capsule format, verifier, and the
  language CLI.
- **Syntra** = the deployable runtime in this repo: Docker/compose deployment,
  HTTP API, admin console, tenant/job/capsule store, persistent memory,
  audit/decision/feedback logs, replay and simulation tooling.
- **Lycan Marketplace** = future distribution layer for signed capsules,
  capability packages, templates, and integrations.

Do not call this product "Lycan Studio". The browser UI is the admin console.

## Repo shape

One Rust crate at the root builds both binaries:

- `syntra` — appliance CLI: `serve`, `replay`, `simulate`, `author`, `proof-lab`.
- `lycan` — language CLI: run, `compile`, `decide`, `feedback`, `evolve`, REPL
  (entrypoint `src/bin/lycan.rs`).

Layout:

- `src/` — Lycan language core (`parser.rs`, `graph*.rs`, `learning.rs`,
  `meta_bandit.rs`, `capabilities.rs`, `server/`, `store.rs`, ...) plus the
  Syntra wrapper modules (`authoring.rs`, `capsule_compiler.rs`,
  `capsule_spec.rs`, `replay.rs`, `simulate.rs`, `proof_lab.rs`).
- `examples/lycan/` — language demos and compiled `.lyc` fixtures.
- `docs/lycan/` — language documentation and guide.
- `examples/` — product demos: install, decide, feedback, persistence, audit,
  sandbox.

## Runtime model

```text
client JSON
  -> HTTP API
  -> tenant / job / capsule lookup
  -> policy load
  -> Lycan graph execution
  -> decision response
  -> feedback
  -> memory update
  -> audit / decision / feedback logs
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
7. Keep demo scripts short, named, and focused on proof: install, decide,
   feedback, persistence, audit, sandbox.
8. Keep language examples small, runnable, and named by what they teach;
   prefer explicit policy and capability examples over hidden magic.
9. If adding API routes, update README and eventually OpenAPI docs.
10. Do not claim universal benchmark superiority; use measured language with
    hardware/test caveats.

## Useful commands

```bash
cargo build
cargo test -- --test-threads=1
cargo build --release

cp templates/env.example .env
docker compose up --build

./scripts/smoke-test.sh
./scripts/demo-boundary-api-tests.sh
./scripts/demo-sandbox.sh

# Lycan CLI
cargo run --bin lycan -- examples/lycan/hello.lycs
cargo run --bin lycan -- compile examples/lycan/hello.lycs
```

## Current TODO

- Expand admin console documentation.
- Keep security limitations honest in README and deployment docs.
- Add a proper language specification (grammar, value model, graph binary
  format, capability ABI, policy model, capsule format, learning semantics)
  under `docs/lycan/`.
