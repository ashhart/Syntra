# Syntra

Syntra is a self-hosted decision service. Your application asks it which of
several actions to take (which model answers a request, which backend
serves it, how strict a check should be), reports the outcome later, and
Syntra learns from those outcomes. Decisions, outcomes and every policy
change are logged so you can audit and replay them.

> **Status:** the v2 decision core is under construction on the `v2`
> branch: one contextual learner, the probability of every chosen action
> in the decision log, and off-policy evaluation that gates promotion.
> The sections below still describe v1 behavior. See
> [CONTEXT.md](CONTEXT.md) for the known limitations of v1.

The science demos, proof lab, self-evolving capsules and the interpreter
moved to a separate Lycan Lab repository.

## Why a decision layer

A fixed rule keeps making the same tradeoff when costs, latency, load, or
outcomes change.
Syntra lets you evaluate those tradeoffs per context and update the policy from
observed results, while keeping explicit limits around the choices.

- **Compute before choosing.** Capsules can derive signals from recent data,
  including latency statistics and load forecasts, before selecting an action.
- **Learn from delayed outcomes.** Report model quality, request success, job
  completion, or cost when it becomes available, using the original decision ID.
- **Keep state across restarts.** Learned policies live in the persistent store
  alongside decision, feedback, and audit logs.
- **Prove a change before promoting it.** Run in shadow mode, replay the results,
  compare against the incumbent, and require configured promotion gates to pass.

## Put it to work

Use it for repeated decisions where the right choice depends on changing
conditions and you can measure what happened afterward.

| Use case | What Syntra chooses | What you feed back | Starting point |
| --- | --- | --- | --- |
| Which LLM to call | A configured model route for the request, balancing task complexity, prompt size, quality needs, and budget | Answer quality or task success, response time, and actual cost | [LLM routing](examples/llm-routing/) |
| Which API or backend to use | The primary provider, a fallback, a cached response, or a circuit-breaker action based on recent service behavior | Success, errors, latency, and cost | [Anomaly-aware routing](examples/anomaly-routing/) |
| How to retry an API call | A permitted retry, backoff, and timeout policy for the endpoint's current conditions | Eventual success and total request latency | [Retry tuning](examples/retry-tuning/) |
| How much infrastructure to run | A capacity recommendation or a hold decision from recent load and current capacity | Processing latency, missed service targets, utilization, and infrastructure cost | [Predictive autoscaling](examples/predictive-autoscaling/) |
| Where to send a job | A configured queue or worker pool based on job requirements and queue conditions | Completion time, failures, and execution cost | [Queue selection](examples/queue-selection/) |
| Whether an agent should act | Execute, limit, request approval, or block, with explicit rules around the learned choice | Task outcomes and policy violations | [Agent governor demo](scripts/demo-agent-governor.py) |
| How strict fraud controls should be | A preset threshold policy for your existing fraud scorer, using time-of-week and transaction-volume context | Confirmed fraud caught and the cost of false positives, linked to the original decision | [Seasonal fraud thresholds](examples/seasonal-fraud-threshold/) |

### LLM routing

Map routes such as `cheap_fast`, `balanced`, and `expensive_accurate` to the
model endpoints your application supports, including hosted or self-hosted
models.
For example, the router could favor a lower-cost model for routine extraction
and a more capable model for a difficult coding request when measured results
justify the extra cost.
Your application makes the model call and supplies the quality signal, such as
a passed task check or reviewer score, together with latency and cost.
Syntra learns from that feedback; the route names alone do not establish which
model is best for your workload.

### Fraud detection and risk decisions

Try the [LLM-free email demo](examples/email-fraud/): a local statistical text
scorer feeds Syntra's learned safe/phishing decision, with optional review
deferral for low-margin cases.
It reports held-out accuracy, false positives, missed phishing, review coverage,
and separate text-scoring and decision timings, with no model API calls.
The [phrase-aware follow-up](examples/email-fraud/EVALUATION.md) publishes
separate-corpus results and a failing promotion check when fewer total errors
come with more missed risky emails.

Fraud controls have a tradeoff: a stricter threshold may catch more fraud
while blocking more legitimate customers.
Syntra can learn which of your approved threshold policies works best for a
given context, using the outcomes your application reports.

The [fraud demo](examples/seasonal-fraud-threshold/) selects `loose`,
`baseline`, `tight`, or `very_tight` using hour, weekend, and transaction-volume
features.
It also computes and records the mean, 95th percentile, and an EWMA forecast
of recent fraud rates for inspection; those statistics are not automatically
inputs to the demo's learned choice.
Your application maps the selected policy to a numeric threshold on its own
fraud scorer, then sends delayed feedback about fraud caught and false-positive
cost using the original decision ID.

You supply the transaction-scoring model, confirmed outcomes, and enforcement
rules; the included demo demonstrates adaptive threshold selection, with no
claim of measured fraud-detection accuracy on real transactions.
Start in shadow mode against your current policy and use
[replay and promotion gates](#governed-promotion) before applying learned choices
to live transactions, since the learner explores alternatives during warmup
and active learning.

### AWS infrastructure decisions

A possible AWS integration is a controller that sends recent load, queue
backlog, current capacity, and permitted capacity limits to Syntra, then maps
the chosen scaling policy to an
[ECS service task count](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/service-auto-scaling.html)
or an
[EC2 Auto Scaling group's desired capacity](https://docs.aws.amazon.com/autoscaling/ec2/userguide/asg-capacity-limits.html).
After the change takes effect, the controller reports processing performance
and cost so subsequent decisions can account for the outcome.

The repository includes a scaling capsule and generic HTTP clients; collecting
AWS metrics and applying AWS changes requires an integration you build with
your AWS SDK or infrastructure controller.
That controller owns IAM permissions, capacity bounds, cooldowns, and coordination
with any existing scaling policies.
Start by recording recommendations alongside your current policy and use the
[promotion workflow](#governed-promotion) before allowing it to change capacity.

## Try it

With Rust, Python 3, `curl`, and `jq` installed, run the governed model-routing
example from the repository root:

```bash
cargo build --release --locked
bash examples/demo-governed-llm-routing.sh
```

The demo starts a local runtime, installs a capsule, learns from synthetic
feedback, compares a candidate policy with the existing route, and produces a
promotion report with reward, cost, latency, and coverage checks.
It requires no model-provider credentials.
To connect real traffic, follow the [model-routing integration guide](docs/quickstart-model-routing.md).

## One repository, two layers

**Syntra** is the deployable runtime: HTTP API, admin console, tenants, persistent
learning, audit logs, replay, and simulation tooling.
**Lycan** is the language and execution core: source programs, compiled graphs,
capability kernels, verification, and constrained choices.
Both ship in one Rust crate; one build produces the `syntra` and `lycan` binaries.

Use the [architecture and reviewer notes](CONTEXT.md) for a deeper source tour.
The sections below cover rollout, integration, configuration, and operations.

## Governed promotion

Syntra includes a native replay gate so adaptive policy promotion can be
measured before production control changes hands. The golden commercial demo is
the governed LLM-routing flow:

```bash
examples/demo-governed-llm-routing.sh
```

It starts Syntra, installs the LLM model-router capsule, teaches two contexts
from delayed feedback, runs Syntra in shadow mode beside the incumbent
`balanced` route, writes a JSONL shadow log from the dominant learned policy,
replays that log, and emits a Markdown promotion report. The gate must pass
reward, cost, latency, coverage, and segment-regression thresholds before the
script succeeds.

The lower-level replay command is:

```bash
syntra replay \
  --events examples/replay/decisions.jsonl \
  --policy-json examples/replay/candidate-policy.json \
  --gates examples/replay/promotion.yaml \
  --format markdown \
  --out promotion-report.md \
  --fail-on-gate
```

The report compares a candidate policy against the logged baseline across
reward, cost, latency, oracle-match rate, candidate coverage, and per-segment
regressions. This is the intended trust loop:

```text
shadow log -> replay -> promotion report -> gate pass -> active rollout
```

Use `--fail-on-gate` in CI to prevent a capsule or policy from being promoted
unless it clears the configured thresholds.

## Lycan capability surface

Capsules are authored as `.lycs` (Lycan source) and compiled to `.lyc`.
The runtime exposes Rust-native capability kernels; the ones that
matter for the operational use cases above:

| Package | Kernels |
|---------|---------|
| math    | `stats.mean`, `stats.stdDev`, `stats.min`, `stats.max`, `stats.percentile` |
| math    | `series.ewmaForecast` (one-step EWMA forecast) |
| ops     | `ops.autoScaleRecommend` |
| net     | `http.get`, `http.post` (allow-listed hosts, private networks denied) |
| data    | `sql.sqliteQuery` (read-only SELECT/WITH/PRAGMA), `json.get/has/len` |
| io      | `file.readText`, `file.writeText`, `file.exists` (sandboxed) |
| runtime | `runtime.input`, `runtime.inputGet` |

Every call is policy-enforced at the runtime layer. Full registry and
sandbox semantics in [`src/capabilities/mod.rs`](src/capabilities/mod.rs); the
[Lycan README](docs/lycan/README.md) groups them in a table.

## Demo container

```bash
docker build -f docker/Dockerfile.demo -t syntra:demo .
docker run -d --name syntra-demo -p 8080:8080 -p 8787:8787 syntra:demo
```

Open the admin console at `http://localhost:8080`; the API listens on port `8787`.

Five demo capsules are pre-installed, and a traffic generator drives one of
them. Open the admin console to inspect decisions, feedback, and the transition
from warmup to active learning.

- **Predictive autoscaling** — EWMA forecast + adaptive scaling policy
- **Anomaly-aware API routing** — latency z-score + adaptive fallback
- **Seasonal fraud threshold** — EWMA on fraud rate + threshold policy
- **Shared-state action embeddings** — LinUCB generalization across actions
- **Hierarchical region routing** — nested decisions with per-level learning

The [demo-image workflow](.github/workflows/publish-demo-image.yml) can publish
`ghcr.io/ashhart/syntra:demo`; pulling it requires the image to have been published
and your account to have access.

For production deployment, see the [Helm chart](deploy/helm/syntra/). For local development, see
[Local Development](docs/site/docs/contributing/local-development.md).

## Integrate into your service

The [retry-tuning](examples/retry-tuning/) example is the canonical Python
integration. Drop-in for `requests`:

```python
import os
from syntra_retry import RetryClient

client = RetryClient(
    syntra_url="http://localhost:8787",
    capsule_path="/tenants/myteam/jobs/retry/capsules/router",
    admin_key=os.environ["SYNTRA_ADMIN_KEY"],
)

response = client.request("GET", "https://api.example.com/users")
```

Every request goes through `/decide` to pick a retry policy, then `/feedback`
with success and latency. The client falls back to a configured default when
Syntra is unreachable, refuses, or returns a malformed response — a Syntra
outage degrades adaptive retry to "always fall back" without breaking the
request flow.

See [`examples/retry-tuning/README.md`](examples/retry-tuning/) for setup,
customization, and tests.

## Deployment scope

Syntra is self-hosted and currently runs as a single process with a local
filesystem store.
Applications supply their own provider calls, infrastructure adapters, and
outcome measurements.
Define permitted actions and fallback behavior before sending live traffic;
start with the [shadow and promotion workflow](#governed-promotion).
For storage and security boundaries, see [Persistent store](#persistent-store)
and [Security](#security).

## How the learning layer works

Each capsule moves through a lifecycle: **Warmup → Active → Frozen**.

1. **Warmup** — Syntra runs uniform random selection for the first ~30
   feedback rounds, watches reward shape, and characterizes the problem
   (binary / continuous / sparse). It picks an initial algorithm
   automatically.
2. **Active** — the meta-bandit selects among Thompson, UCB1,
   EpsilonGreedy, Weighted, and Greedy candidates, with LinUCB and LinTS
   also available for feature-context capsules. Outcome feedback updates
   the candidate that made the decision and informs future selection.
3. **Frozen** — operator-triggered; the bandit stops learning but continues
   serving decisions from the current weights.

Drift detection runs at two scopes: a capsule-level ADWIN detector triggers
re-warmup when reward distribution shifts globally, and per-context ADWIN
detectors reset just the affected context bucket on narrower shifts.

Refusal (Phase E, opt-in) wraps reward predictions in split-conformal
intervals and tracks out-of-distribution scores per context. When the
interval is too wide or the input is OOD, `/decide` returns
`{"refused": true, "confidence": {…}}` and your service falls back. See
[refusal config](#configuration) below.

## Configuration

A `learning.json` per capsule controls the learner. Most fields default to
sensible values; the ones you usually touch:

```json
{
  "contextSpec": {
    "type": "features",
    "features": [
      {"name": "recent_failure_rate", "type": {"kind": "continuous", "range": [0, 1]}},
      {"name": "p99_latency_ms",      "type": {"kind": "continuous", "range": [0, 5000]}},
      {"name": "hour",                "type": {"kind": "cyclic", "period": 24.0}}
    ]
  },
  "refusal": {
    "enabled": true,
    "coverage": 0.95,
    "maxIntervalWidth": 0.5,
    "oodThreshold": 0.8
  }
}
```

- `contextSpec` — `discrete` (string `contextKey`, the default) or `features`
  (typed vector; enables the LinUCB candidate in the meta-bandit).
- `refusal` — enabled-off by default. When on, the response carries a
  `confidence` block with `oodScore`, `intervalWidth`, and `refused: bool`.

PUT it at any time: `PUT /tenants/{t}/jobs/{j}/capsules/{c}/learning`.

## API

```bash
# Install a capsule
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/install \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  --data-binary @router-capsule/program.lyc

# Optional: install the reward spec so feedback can use the components form
curl -X PUT http://localhost:8787/tenants/acme/jobs/routing/capsules/router/reward_spec \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  --data-binary @router-capsule/reward_spec.json

# Get a decision (discrete-context capsule)
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/decide \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"contextKey":"support-low-cost"}'
# → response carries decisionId, decisions[], oodScore, refused, confidence

# Get a decision (feature-context capsule)
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/decide \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"features":{"recent_failure_rate":0.15,"p99_latency_ms":1200,"hour":3.0}}'

# Send feedback
curl -X POST http://localhost:8787/tenants/acme/jobs/routing/capsules/router/feedback \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"decisionId":"dec_abc123","reward":0.85}'

# Inspect learned state
curl http://localhost:8787/tenants/acme/jobs/routing/capsules/router/report   -H "Authorization: Bearer $LYCAN_ADMIN_KEY"
curl http://localhost:8787/tenants/acme/jobs/routing/capsules/router/memory   -H "Authorization: Bearer $LYCAN_ADMIN_KEY"
curl http://localhost:8787/tenants/acme/jobs/routing/capsules/router/contexts -H "Authorization: Bearer $LYCAN_ADMIN_KEY"
```

See [docs/api.md](docs/api.md) for the full surface including evolution,
chaos, evaluate, and audit endpoints.

**API versioning.** `/v1` is the canonical prefix for every API route (e.g.
`POST /v1/tenants/acme/jobs/routing/capsules/router/decide`); the unversioned
paths shown above are the same routes, kept as deprecated aliases until 1.0 —
they behave identically and additionally return `Deprecation: true` and
`Link: </v1/...>; rel="successor-version"` response headers. `/health`,
`/ready`, and `/metrics` are infra endpoints and stay unversioned.

## Authoring capsules

Capsules are authored as YAML and compiled to a deployable `.lyc` by the
`syntra author` command:

```yaml
name: llm-router
options:
  - cheap_fast
  - balanced
  - expensive_accurate
reward:
  type: continuous
  range: [-1.0, 1.0]
```

```bash
syntra author my-capsule.yaml --out-dir ./my-capsule/
# emits my-capsule/program.lyc + sidecar JSON
```

Then POST `program.lyc` to `/install` and PUT a `learning.json` to attach a
feature-context spec or enable refusal.

Smoke-test a spec locally before deploying:

```bash
syntra simulate my-capsule.yaml --rounds 5000 --true-arm-rewards "0.2,0.5,0.7" --seed 7
```

## Data model

```
tenant / job / capsule

tenant   = organization or environment
job      = independent learning context (same capsule, different memory)
capsule  = the compiled program + its learned state
```

## Persistent store

```
syntra-store/
  tenants/{tenant}/jobs/{job}/capsules/{capsule}/
    current.lyc       — the graph binary
    policy.json       — runtime permissions
    memory.json       — learned weights, meta-bandit, calibrators, OOD detectors
    learning.json     — algorithm config (contextSpec, refusal, …)
    warmup.json       — lifecycle state
    audit.jsonl       — mutation log
    decision.jsonl    — decision log (carries refused flag and confidence)
    feedback.jsonl    — feedback log
    snapshots/        — pre-mutation backups
```

Container is disposable. The store survives restarts. The `memory.json`
schema is at version 7, with backward-compat readers for v2 through v6.

### Log retention

The JSONL logs (`decision`, `feedback`, `audit`, `evolution`) rotate by
size: when a log would exceed `maxLogBytes`, the current file moves to
`<name>.jsonl.1` (one rotated generation) and new entries start a fresh
file. API readers still see one continuous oldest-first stream —
rotation does not change the wire format — and backup/restore includes
rotated generations automatically.

Retention is configured in `syntra-store/retention.json`, read at
startup, fail-closed:

```json
{ "maxLogBytes": 67108864, "rotateKeep": 1 }
```

`maxLogBytes` defaults to 64 MiB per log file (`0` disables rotation).
An invalid or unknown-field config refuses to start the server rather
than silently falling back. Entries rotated away are no longer
resolvable by `/feedback` — they honestly return 404 — so size the cap
above your feedback-latency horizon. Design rationale:
`docs/store-retention.md`.

## Shadow mode

Syntra can run beside an existing application without taking control:

1. Your app sends request context to `/decide`.
2. Syntra returns a suggested option and a `decisionId`.
3. Your app continues with its current production decision.
4. When the real outcome resolves, your app posts `/feedback` with the
   `decisionId` and the observed reward.
5. Syntra updates memory and exposes the learned state in `/report`,
   `/contexts`, the admin console.

That makes it possible to validate the adaptive layer before letting it
influence live behaviour.

## Admin console

Browser UI at `/admin`:

- Tenant / job / capsule navigation
- Live strategy weight visualization
- Decision and audit log inspection
- Policy enforcement status
- Context memory viewer
- Capsule deletion and log purging

## Security

- API routes require an `Authorization: Bearer` token; `/health`, `/ready`,
  and `/metrics` are public infrastructure endpoints.
- Capsule policy enforced at runtime (file sandbox, network sandbox,
  SSRF protection).
- File capabilities scoped to capsule working directory.
- HTTP capabilities require explicit `allowed_hosts`. Private networks
  denied by default.
- Constant-time key comparison. Failed auth logged.
- Server refuses startup without an admin key unless `--dev-mode`
  (binds localhost only).

**Not yet production-hardened for direct public-internet exposure.** Run
behind a TLS proxy. The path to production hardening is tracked in
[ROADMAP.md](ROADMAP.md) and starts with the
threat model in [SECURITY.md](SECURITY.md).

## Operating

When weights look wrong, inspect the data trail before changing the capsule:

1. `/report` for current strategy weights.
2. `/contexts` to confirm the request landed in the expected `contextKey`.
3. `decision.jsonl` for what Syntra suggested.
4. `feedback.jsonl` for which option was rewarded and whether the reward
   sign is correct.
5. `audit.jsonl` for installs, policy changes, deletes, refusals, and
   change-detection events.

When something looks broken at the storage layer, run
`syntra doctor --store <root>` — a read-only validator (JSONL findings,
exit 0/1/2, cleans nothing); backup/restore are `syntra backup` /
`syntra restore`, and the crash-durability contract is documented in
[docs/store-retention.md](docs/store-retention.md).

See [docs/operating.md](docs/operating.md) for the full operator checklist
and [docs/deployment.md](docs/deployment.md) for production deployment.

Throughput and latency of the `/decide` hot path are benchmarked and
recorded in [docs/benchmarks.md](docs/benchmarks.md)
(`./scripts/bench-decide.sh`). The per-token rate limiter defaults to
1000 req/s and can be raised — never silently disabled — via
`SYNTRA_RATE_LIMIT_RPS` / `SYNTRA_RATE_LIMIT_BURST`.

## Architecture

Syntra is built on Lycan, a graph-execution runtime that ships in this
repo as part of the single syntra crate. Capsules are authored as YAML
and compiled to Lycan's binary format automatically — most Syntra users
never interact with Lycan directly. If you want to dig into the
substrate, the source lives at [`src/`](src/) and the language reference
at [`docs/lycan/README.md`](docs/lycan/README.md).

## Examples

The curated demo gallery is [DEMOS.md](DEMOS.md). Start there if you are
evaluating whether Syntra is more than a narrow API service.

Operational-kernel demos — `series.ewmaForecast`, `stats.percentile`,
`stats.mean / stdDev`, `ops.autoScaleRecommend` feeding into an adaptive
choice. Each ships a `capsule.yaml`, a `program.lycs`, a `learning.json`,
and a README walking through install / decide / feedback.

- [`examples/predictive-autoscaling/`](examples/predictive-autoscaling/) —
  EWMA forecast + autoscale-recommend driving a four-policy scaling choice.
- [`examples/anomaly-routing/`](examples/anomaly-routing/) —
  mean / stddev / z-score driving a four-policy routing choice.
- [`examples/seasonal-fraud-threshold/`](examples/seasonal-fraud-threshold/) —
  EWMA forecast on a fraud-rate series driving a four-policy threshold choice.

Integration packs — Python and language-client examples consuming Syntra
over HTTP:

- [`examples/retry-tuning/`](examples/retry-tuning/) — canonical Python
  integration library and tests.
- [`examples/fraud-tuning/`](examples/fraud-tuning/),
  [`examples/queue-selection/`](examples/queue-selection/),
  [`examples/llm-routing/`](examples/llm-routing/) — sister domain packs.

Bash demos and tooling:

- [`examples/demo-llm-model-routing.sh`](examples/) — three model routes,
  two contexts, persistence across restart.
- [`examples/demo-static-policy-vs-syntra.sh`](examples/) — focused
  static-vs-adaptive demonstration.
- [`examples/offline-eval/`](examples/offline-eval/) — IPS and
  doubly-robust off-policy estimators.
- [`examples/ab-harness/`](examples/ab-harness/) — A/B simulation harness.

## Roadmap & license

- [docs/why-syntra.md](docs/why-syntra.md) — honest positioning: the governed
  LLM-routing wedge, the auditable-decisions case, "why not just X", and the
  limits.
- [docs/quickstart-model-routing.md](docs/quickstart-model-routing.md) — the
  one buyer journey end to end: install, decide, feedback, metrics, shadow →
  gated promotion, rollback.
- [docs/concepts.md](docs/concepts.md) — contextual-bandit concept doc.
- [docs/concepts/operational-intelligence.md](docs/concepts/operational-intelligence.md) — the
  kernel-feature-derivation-to-strategy-node pattern this README leads with.
- [docs/evaluations/2026-09-08-adaptive-policy-baseline.md](docs/evaluations/2026-09-08-adaptive-policy-baseline.md)
  — dated simulated-traffic regret benchmark against built-in baselines; regenerate with `bash scripts/eval-report.sh`.
- [ROADMAP.md](ROADMAP.md) — short version of upcoming work.
- [CHANGELOG.md](CHANGELOG.md) — what shipped in each phase.
- Apache-2.0.
