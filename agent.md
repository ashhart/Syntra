# Agent Guide: Syntra

This file is for AI agents, maintainers, and collaborators working inside the Syntra repo.

## One-line identity

Syntra is the self-hosted Docker/API/admin appliance for running Lycan capsules in real applications.

## Product boundary

Use this language:

- **Lycan** = the language.
- **Syntra** = the runtime appliance in this repo.
- **Lycan Marketplace** = future distribution layer for signed capsules, capability packages, templates, and integrations.

Do not call this product "Lycan Studio". The browser UI is the admin console.

## What belongs here

This repo owns operational runtime concerns:

- Docker and docker-compose deployment
- HTTP API
- Admin console
- Tenant/job/capsule store
- Persistent memory
- Audit, decision, feedback, and evolution logs
- API demos and smoke tests
- Deployment documentation
- Security hardening for the appliance

## What belongs in Lycan Lycan

Language and runtime-core work belongs in the Lycan language repo:

- `.lycs` syntax
- parser/compiler
- graph binary format
- graph executor
- capability ABI
- capsule format
- verifier
- CLI language commands
- language examples and specification

This repo builds Syntra as a standalone appliance binary. It depends on the released Lycan runtime source at build time; runtime Docker deployments do not require a local Lycan checkout.

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

1. Keep Docker/API/admin/store work in this repo.
2. Do not change language semantics here unless the same change is coordinated in the Lycan language repo.
3. Never add `.env`, API keys, production databases, Docker volumes, local stores, or `target/` artifacts.
4. Preserve fail-closed security behavior: no admin key means no startup unless explicit dev mode exists.
5. Preserve policy enforcement on server execution paths.
6. Preserve tenant/job/capsule isolation.
7. Use "admin console", not "admin studio".
8. Keep demo scripts short, named, and focused on proof: install, decide, feedback, persistence, audit, sandbox.
9. If adding API routes, update README and eventually OpenAPI docs.

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
```

## Current TODO

- Expand admin console documentation.
- Keep security limitations honest in README and deployment docs.
