# Syntra Examples

This repo keeps the public example surface deliberately small.

## Focused Proof

```bash
./examples/demo-static-policy-vs-syntra.sh
```

This installs a compiled Lycan capsule into Syntra, takes an initial decision with neutral weights, sends delayed feedback, and proves the learned memory survives restart.

## LLM Model Routing

```bash
./examples/demo-llm-model-routing.sh
```

This is the clearest AI-app adoption demo. Syntra chooses between `cheap_fast`, `balanced`, and `expensive_accurate` model routes. It learns separate winners for `support-low-cost` and `legal-high-accuracy` contexts, then proves those weights survive restart.

## Docker Proof

```bash
./examples/docker-quickstart/demo-docker-quickstart.sh
```

This builds a disposable Syntra container, installs the same compiled capsule, verifies auth, sends feedback, restarts the container, and proves memory persisted in the Docker volume.

## API Demo

```bash
export LYCAN_ADMIN_KEY=...
export SYNTRA_URL=http://localhost:8787
./examples/curl/api-demo.sh
```

This is the small curl-oriented demo for an already-running Syntra server.

## Capsule Fixture

- `demo_takeaway_demand.lycs` is the readable Lycan source.
- `demo_takeaway_demand.lyc` is the compiled capsule installed by the demos.
- `demo_llm_model_router.lycs` is the readable LLM routing source.
- `demo_llm_model_router.lyc` is the compiled capsule for the LLM routing demo.

Syntra serves compiled `.lyc` capsules. Use the Lycan language repo when you want to author or compile new capsules.
