# Syntra

A self-hosted Docker/API appliance for running Lycan capsules under applications.

Syntra turns compiled Lycan capsules into adaptive decision services: JSON in, policy-bounded execution, feedback-driven learning, decision out. It persists learned weights, enforces security policies, exposes an HTTP API, and provides an admin console for inspection.

Lycan is the language/runtime. Syntra is the deployable product.

The Docker image is self-contained at runtime. It does not require a local Lycan checkout to run.

## What Syntra Is For

Use Syntra when you want to strap a lightweight adaptive layer under an existing app:

- shadow-mode decisions beside an existing production path
- per-job learned weights for the same capsule
- contextual memory via `contextKey`
- delayed feedback when outcomes arrive later
- audit, decision, feedback, and evolution logs
- an admin console for seeing what the system learned

The killer demo is simple: one decision problem, several policies, delayed feedback, and visible weights. A static policy stays static. Syntra adapts.

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

Run the same proof through a disposable Docker container and persistent volume:

```bash
./examples/docker-quickstart/demo-docker-quickstart.sh
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
