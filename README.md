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

## Run With Docker

Pull and run the published image:

```bash
docker run -p 8787:8787 \
  -e LYCAN_ADMIN_KEY=change-me \
  -v syntra-store:/var/lib/syntra \
  ghcr.io/ashhart/syntra:latest
```

Then open:

```text
http://localhost:8787/admin
```

Use the same `LYCAN_ADMIN_KEY` value as the admin key in the browser.

Published tags:

```bash
docker pull ghcr.io/ashhart/syntra:latest
docker pull ghcr.io/ashhart/syntra:0.2.0
```

## Powered By Lycan

Syntra capsules are compiled [Lycan](https://github.com/ashhart/Lycan) programs. You do not need to read or write Lycan to use Syntra: install the capsule, call the API, send feedback, and inspect the weights.

If you want to author custom capsules, use the Lycan language repo. Lycan is the substrate. Syntra is the deployable product.

## Why Syntra?

Syntra is not an LLM wrapper, hosted personalization API, Python notebook, or black-box model.

It is a small self-hosted adaptive decision service:

- **No LLM in the hot path** — the runtime executes compiled capsules directly.
- **No external SaaS dependency** — run it as a Docker container in your own infrastructure.
- **No Python ML stack required** — the appliance is a Rust binary with a filesystem-backed store.
- **Shadow-mode first** — observe and learn before influencing production behaviour.
- **Inspectable memory** — learned weights, decisions, feedback, audits, and snapshots stay visible.
- **Policy-bounded execution** — capsules run with explicit file/network capability boundaries.

Syntra is closest in shape to contextual-bandit cores such as Vowpal Wabbit and hosted personalization systems such as Azure Personalizer. It is adjacent to experimentation and feature-flagging platforms such as Statsig, Eppo, GrowthBook, and LaunchDarkly; those help decide which experiment or feature should ship, while Syntra optimizes which option to pick per request once your application is running.

## Quickstart

To build locally from source:

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

Compile a YAML bandit spec into a capsule:

```bash
syntra author examples/authoring/llm-router.yaml \
  --out router.lyc \
  --source-out router.lycs
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

In the checked-in demo configuration, after 60 feedback events Syntra converges to `cheap_fast` for support traffic and `expensive_accurate` for legal/enterprise traffic at about 98% weight in each context. Real workloads will vary with reward sparsity, context cardinality, algorithm choice, and safety settings.

## Field Use

Syntra is currently running in shadow mode against [MoEfolio.ai](https://moefolio.ai/), a public AI trading panel that produces a verdict per cycle and resolves outcomes against the market after a delayed window.

Across recent verdicts, Syntra has learned non-trivial weights against the panel's gate and is expressing structured disagreements, primarily that the gate may be over-cautious on some BUY signals. Whether those disagreements are correct requires more resolved outcomes than are currently available; the experiment is ongoing, and we will publish results regardless of direction.

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

Syntra includes an MVP YAML authoring layer for common bandit cases:

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

Compile it into the `.lyc` binary accepted by the existing `/install` API:

```bash
syntra author examples/authoring/llm-router.yaml \
  --out router.lyc \
  --source-out router.lycs
```

The intended reward shape is a weighted sum of normalized outcome metrics. For example, quality might be normalized to `0..1`, while latency and cost become penalties normalized against a deployment-specific budget.

Under the hood, the YAML layer compiles down to Lycan capsules while keeping Lycan available for custom logic. The current MVP turns options and context keys into executable capsule behaviour. Reward weights are parsed and preserved in the generated source as the declared reward policy; automatic weighted reward computation is the next authoring step tracked in [#1](https://github.com/ashhart/Syntra/issues/1).

## Security model

- All routes except `/health` and the `/admin` login shell require `Authorization: Bearer` token
- Capsule policy enforced at runtime (file sandbox, network sandbox, SSRF protection)
- File capabilities scoped to capsule working directory
- HTTP capabilities require explicit `allowed_hosts`
- Private networks denied by default
- Constant-time key comparison
- Failed auth logged
- Server refuses startup without admin key unless `--dev-mode`

**Not yet production-hardened for direct public internet exposure.** Run behind a TLS proxy. The path to production hardening is tracked in [#2](https://github.com/ashhart/Syntra/issues/2) and starts with the threat model in [SECURITY.md](SECURITY.md).

## Operating Syntra

When weights look wrong, inspect the data trail before changing the capsule:

1. Call `/report` to see current strategy weights.
2. Call `/contexts` to confirm the request is landing in the expected `contextKey`.
3. Check `decision.jsonl` for what Syntra suggested.
4. Check `feedback.jsonl` for which option was rewarded and whether the reward sign is correct.
5. Check `audit.jsonl` for installs, policy changes, deletes, and other mutations.
6. Use the admin console for a quick visual pass over tenants, jobs, capsules, weights, decisions, and policy state.

See [docs/operating.md](docs/operating.md) for the operator checklist.

For system-level issues such as startup failures, `500` responses, missing feedback writes, or lost memory after restart, start with container logs, the store volume mount, and `LYCAN_ADMIN_KEY`; the operating guide has the short checklist.

## Docker

Published image:

```bash
docker pull ghcr.io/ashhart/syntra:latest
```

Tagged releases:

```bash
docker pull ghcr.io/ashhart/syntra:0.2.0
```

```yaml
services:
  syntra:
    image: ghcr.io/ashhart/syntra:latest
    container_name: syntra
    ports:
      - "8787:8787"
    volumes:
      - syntra-store:/var/lib/syntra
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

If your service makes the same decision repeatedly and only learns whether it was right later, Syntra is the layer for that loop.

## Roadmap

See [ROADMAP.md](ROADMAP.md) for the short version: YAML authoring, hero-demo CI, operator hardening, and the 1.0 security track.

## License

Apache-2.0
