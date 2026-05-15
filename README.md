# Syntra

Syntra is a self-hosted adaptive decision layer for applications.

It helps services learn from delayed feedback using contextual bandits, without putting an LLM, Python ML stack, or external personalization SaaS in the hot path.

Use it for repeated decisions where the best option depends on context and the outcome arrives later:

- which LLM model should handle this request?
- which retry / timeout policy should this customer path use?
- which queue, route, ranking, or threshold should win for this job?
- which strategy performs best for this tenant, region, or workload?

Syntra runs as a Docker/API appliance beside your app. Your service sends context to `/decide`, keeps its own production path in control, then posts `/feedback` when the real outcome matures. Syntra persists the learned weights, exposes them through the API and admin console, and lets you promote behaviour only after you can see it working.

The Docker image is self-contained at runtime. It does not require a local Lycan checkout to run.

## Powered By Lycan

Syntra is powered by [Lycan](https://github.com/ashhart/Lycan), an AI-native machine execution language built on a Rust graph runtime.

Lycan is the substrate. Syntra is the product.

```text
Your application
  -> Syntra HTTP API
  -> compiled Lycan capsule
  -> contextual decision
  -> real-world outcome
  -> feedback
  -> visible learned memory
```

You do not need to learn Lycan to understand the Syntra workflow. Install a compiled `.lyc` capsule, call the HTTP API, send feedback, and inspect the weights. Use the Lycan language repo when you want to author custom capsules.

## Why Syntra?

Syntra is not an LLM wrapper, hosted personalization API, Python notebook, or black-box model.

It is a small self-hosted adaptive decision service:

- **No LLM in the hot path** — the runtime executes compiled capsules directly.
- **No external SaaS dependency** — run it as a Docker container in your own infrastructure.
- **No Python ML stack required** — the appliance is a Rust binary with a filesystem-backed store.
- **Shadow-mode first** — observe and learn before influencing production behaviour.
- **Inspectable memory** — learned weights, decisions, feedback, audits, and snapshots stay visible.
- **Policy-bounded execution** — capsules run with explicit file/network capability boundaries.

## Quickstart

```bash
# Set your admin key
echo "LYCAN_ADMIN_KEY=$(openssl rand -hex 32)" > .env

# Start
docker compose up --build -d

# Open admin console
open http://localhost:8787/admin
```

Run the focused adaptive API proof:

```bash
./examples/demo-static-policy-vs-syntra.sh
```

Run the LLM model-routing proof:

```bash
./examples/demo-llm-model-routing.sh
```

Run the same proof through a disposable Docker container and persistent volume:

```bash
./examples/docker-quickstart/demo-docker-quickstart.sh
```

## Hero Demo: LLM Model Routing

The most direct use case is model selection for AI applications.

```text
Context:
  task_type, customer_tier, urgency, token size

Options:
  cheap_fast, balanced, expensive_accurate

Feedback:
  quality, latency, cost, accepted/rejected outcome
```

The demo starts with neutral weights, then sends delayed feedback for two contexts:

```text
support-low-cost       -> cheap_fast
legal-high-accuracy    -> expensive_accurate
```

Syntra learns separate winners for each context and persists them after restart:

```bash
./examples/demo-llm-model-routing.sh
```

## API

```bash
# Health (public)
curl http://localhost:8787/health

# Install a capsule
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/install \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  --data-binary @router.lyc

# Get a decision
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/decide \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"contextKey":"rush_hour","input":{"latencies":[42,50,88]}}'

# Send feedback
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/feedback \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"strategyId":70,"option":1,"reward":1.0,"contextKey":"rush_hour"}'

# Check what it learned
curl http://localhost:8787/tenants/acme/jobs/routing/capsules/router/report \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY"

# View contexts
curl http://localhost:8787/tenants/acme/jobs/routing/capsules/router/contexts \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY"
```

## Data model

```
tenant / job / capsule

tenant   = organization or environment
job      = independent learning context (same capsule, different memory)
capsule  = the Lycan program + its learned state
```

## Persistent store

```
syntra-store/
  tenants/
    {tenant}/
      jobs/
        {job}/
          job.json
          capsules/
            {capsule}/
              current.lyc       — the graph binary
              policy.json       — runtime permissions
              memory.json       — learned weights (sidecar)
              learning.json     — algorithm config
              audit.jsonl       — mutation log
              decision.jsonl    — decision log
              feedback.jsonl    — feedback log
              evolution.jsonl   — evolution log
              snapshots/        — pre-mutation backups
```

Container is disposable. The store survives restarts.

## Shadow Mode

Syntra can run beside an existing application without taking control:

1. Your app sends the request context to `/decide`.
2. Syntra returns a suggested option and records a `decisionId`.
3. Your app continues using its current production decision.
4. When the real outcome matures, your app posts `/feedback` with the `decisionId`.
5. Syntra updates memory and exposes the learned weights in `/report`, `/contexts`, and the admin console.

That makes it possible to prove the adaptive layer before letting it influence live behaviour.

## Admin console

The browser-based admin at `/admin` provides:

- Tenant / job / capsule navigation
- Live strategy weight visualization with animated graph
- Decision and audit log inspection
- Policy enforcement status
- Context memory viewer
- Capsule deletion and log purging

## Learning layer

Each capsule supports:

- **Contextual learning** — different weights per context key (e.g., `rush_hour` vs `quiet`)
- **Bandit algorithms** — simpleWeighted, epsilonGreedy, ucb1
- **Reward shaping** — outcome-based reward with configurable policy
- **Safety rails** — freeze, max delta, min exploration
- **Decay** — old outcomes fade over time

Configure via `PUT /tenants/:t/jobs/:j/capsules/:c/learning`.

## Evolution Safety

Syntra has two different adaptive paths:

- **Normal path** — update learned weights from feedback. This is the expected production workflow.
- **Advanced path** — verified capsule evolution. This is for controlled proposal testing and should stay opt-in.

Weight learning does not rewrite the capsule. It updates visible sidecar memory for the tenant/job/capsule/context. Capsule evolution is higher blast-radius and belongs behind explicit review, verification, snapshots, and rollback.

## Authoring Path

Today, Syntra serves compiled Lycan capsules:

```text
write .lycs in Lycan
compile to .lyc
install into Syntra
call /decide and /feedback
```

The pragmatic adoption path is to add a higher-level JSON/YAML authoring layer for common bandit cases:

```yaml
name: llm-router
options:
  - cheap_fast
  - balanced
  - expensive_accurate
contexts:
  - task_type
  - customer_tier
  - urgency
reward:
  quality: 0.6
  latency: -0.2
  cost: -0.2
```

That layer can compile down to Lycan capsules while keeping Lycan available for custom logic.

## Security model

- All routes except `/health` and the `/admin` login shell require `Authorization: Bearer` token
- Capsule policy enforced at runtime (file sandbox, network sandbox, SSRF protection)
- File capabilities scoped to capsule working directory
- HTTP capabilities require explicit `allowed_hosts`
- Private networks denied by default
- Constant-time key comparison
- Failed auth logged
- Server refuses startup without admin key unless `--dev-mode`

**Not yet production-hardened for direct public internet exposure.** Run behind a TLS proxy.

## Docker

```yaml
services:
  syntra:
    build: .
    image: syntra
    container_name: syntra
    ports:
      - "8787:8787"
    volumes:
      - syntra-store:/var/lib/lycan
    environment:
      - LYCAN_ADMIN_KEY=${LYCAN_ADMIN_KEY}
    deploy:
      resources:
        limits:
          memory: 1g
```

## Build Boundary

Syntra depends on the released Lycan runtime source at build time and produces a standalone container image. Runtime deployment does not require Rust, Cargo, or a local Lycan checkout.

Author and compile capsules with Lycan. Serve compiled `.lyc` capsules with Syntra.

## License

Apache-2.0
