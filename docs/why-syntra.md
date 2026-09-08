# Why Syntra

An honest positioning doc: what Syntra is for, where it earns its keep,
where it doesn't, and when the boring alternative is the right call.
Every claim below links to a file in this repo you can open. Per the repo
rule ([`AGENTS.md`](../AGENTS.md), rule 10): no universal-superiority
claims; measured numbers carry their hardware and workload caveats.

## The wedge: governed LLM model routing

The first commercial problem Syntra solves is model selection. One service,
many model routes (`cheap_fast` / `balanced` / `expensive_accurate`), and a
per-request choice that depends on live context and resolves only later:

- **Hot-path decision from live context.** A request's prompt-token count,
  task complexity, customer tier, and hour-of-day go into
  `POST …/decide`; the chosen route comes back before you call the model
  ([`examples/llm-routing/README.md`](../examples/llm-routing/README.md),
  hot path in [`src/server/decide.rs`](../src/server/decide.rs)).
- **Delayed feedback.** Quality, latency, and cost resolve after the
  response. You `POST …/feedback` with the `decisionId` and the observed
  reward whenever they arrive; the learned per-context weights persist in
  the store ([`src/server/feedback.rs`](../src/server/feedback.rs),
  [`docs/operating.md`](operating.md)).
- **Shadow mode first.** `/decide` is read-only by default — it suggests,
  logs, and learns only from explicit `/feedback`; your app keeps using its
  incumbent route until you decide otherwise
  ([`docs/api.md`](api.md) "Decide",
  [`docs/operating.md`](operating.md) "Shadow-mode checklist").
- **Promotion gates, not vibes.** Shadow logs replay into a pass/fail
  promotion report on reward, cost, latency, coverage, and per-segment
  regression, with `--fail-on-gate` for CI
  ([`examples/replay/README.md`](../examples/replay/README.md),
  [`examples/demo-governed-llm-routing.sh`](../examples/demo-governed-llm-routing.sh)).

The full buyer-grade loop — train, shadow beside the incumbent `balanced`
route, replay the evidence, gate promotion — is one script:
[`examples/demo-governed-llm-routing.sh`](../examples/demo-governed-llm-routing.sh).
The end-to-end command sequence is in
[`docs/quickstart-model-routing.md`](quickstart-model-routing.md).

## The deeper position: auditable adaptive decisions

Model routing is the wedge. The broader claim is for **any repeated,
consequential operational decision** where someone will later ask *"why did
the system choose that, and who approved the policy that chose it?"* —
retry budgets, fraud bands, scaling policies, routing under anomaly
([`README.md`](../README.md) "What Syntra is for").

For regulated or otherwise consequential systems, four properties of the
runtime map onto audit, oversight, and robustness duties. These are
**alignment** observations — Syntra gives you the artifacts the duties
need — **not compliance claims**. Syntra is v0 software
(`version = "0.2.0"` in [`Cargo.toml`](../Cargo.toml)); nothing here has
been through external certification.

| Duty family (NIST AI RMF / EU AI Act high-risk, as categories) | Mechanism in Syntra | Where to verify |
|---|---|---|
| Record-keeping / traceability | Every decision, feedback, and mutation is an append-only JSONL record; decisions carry a `decisionId`, input hash, and the installed graph's SHA-256 | [`docs/store-retention.md`](store-retention.md), [`docs/operating.md`](operating.md) "Decision-log forensics" |
| Verifiable, version-pinned artifacts | Capsules are compiled `.lyc` binaries; `/install` validates the format, hashes the bytes, and records the hash in `audit.jsonl`, so any decision can be pinned to the exact graph that produced it | [`src/server/routes.rs`](../src/server/routes.rs) install handler, [`docs/api.md`](api.md) "Capsule install" |
| Human oversight / change control | Promotion is human-gated by default: shadow mode, `syntra replay` pass/fail report, `--fail-on-gate` in CI, and an operator-triggered Freeze state | [`src/lib.rs`](../src/lib.rs) `cli_replay`, [`examples/replay/promotion.yaml`](../examples/replay/promotion.yaml), [`docs/operating.md`](operating.md) lifecycle states |
| Measured evaluation before deployment | Offline policy evaluation (IPS / doubly robust) on historical logs, and paired-seed A/B with statistical testing | [`examples/offline-eval/README.md`](../examples/offline-eval/README.md), [`examples/ab-harness/README.md`](../examples/ab-harness/README.md) |
| Robustness / fall back under uncertainty | Opt-in confidence-based refusal (conformal intervals + OOD scores) returns `refused: true` so your service uses its default policy; ADWIN drift detection re-warms on regime shifts | [`docs/api.md`](api.md) refusal response, [`docs/operating.md`](operating.md) "Drift detection" |
| Least-privilege operations | Scoped bearer tokens (`Admin`, `TenantAdmin`, `Read`); a `Read` token can decide and inspect but cannot mutate learned state or issue tokens | [`src/auth_tokens.rs`](../src/auth_tokens.rs), [`src/server/routes.rs`](../src/server/routes.rs) |

If your regime requires signed artifact distribution, tamper-evident
storage, or certified processes, those are **your** controls layered on
top — the signed-capsule distribution layer ("Lycan Marketplace") is a
future idea, not shipping software ([`AGENTS.md`](../AGENTS.md) product
boundary). What Syntra contributes is that the evidence trail exists by
default and is inspectable: the store directory *is* the audit
([`docs/deployment.md`](deployment.md) "What you're standing up").

## "Why not just X?"

Steelmanned. Where X is the better tool, it says so.

### …a feature-flag system (LaunchDarkly / Unleash) plus a Python bandit library?

Feature flags answer "should this user get variant B?" They do not learn
from delayed outcomes, compute nothing in the hot path, and hold no policy
state. A `contextual_bandit` pip package gives you the learner — then you
own the serving service, the decision/audit logs, delayed-feedback
correlation, retention, the replay gate, refusal, drift handling, and the
promotion report. That glue is most of what this repo already ships and
tests.

Also, this repo's own README states Syntra is *adjacent to*, not a
replacement for, flag/experiment platforms: flags decide whether to ship
X; Syntra picks which option to use once X is shipped
([`README.md`](../README.md) "What Syntra is not").

**Right call for the flag + library stack:** one Python service, one
discrete choice, no audit appetite, and a team happy to own the glue.
Then genuinely use the library — this doc is not an argument for buying
infrastructure you don't need.

### …a cloud LLM router (OpenRouter / Portkey)?

If you want managed multi-provider access with rules-based routing,
fallbacks, and per-key budgets, a cloud router is simpler and probably
correct. The difference is where the decision lives. Cloud routers route
on caller-declared rules; the policy, the logs, and the learning all sit
in a vendor's cloud, and none of them learns from *your* delayed
outcome-per-context (this customer tier's sessions get throttled by the
judge model, so demote this route — learned, not declared). Syntra runs in
your network, learns from a reward you define
([`examples/llm-routing/README.md`](../examples/llm-routing/README.md)
reward formula), and its evidence never leaves your store.

**Right call for the cloud router:** you need provider failover and budget
enforcement, not an adaptive policy you can audit and gate.

### …LangSmith / Langfuse?

These are the right tools for LLM application observability: tracing,
prompt iteration, eval scoreboards, cost dashboards. They are not in your
request path — they record what happened; they don't pick what happens
next, hold learned state between requests, or refuse. If you adopt Syntra
for routing decisions you will still want a tracer, and Syntra's decision
logs (`GET …/decisions`) are exportable JSONL your existing stack can
ingest ([`docs/operating.md`](operating.md)).

**Right call for LangSmith/Langfuse alone:** your problem is visibility,
not an adaptive control loop.

### …doing nothing (one model, one static route)?

The honest default. A single well-chosen model with a static fallback is
correct until you can point at measurable waste: cheap-context traffic
billed at premium rates, or quality incidents from under-powered routes.
Syntra only has material to learn from if (a) context changes which option
is best and (b) outcomes resolve late enough that you can post them back
as feedback ([`docs/concepts.md`](concepts.md)). It is also v0 software
with a single-node posture — see the limits below before putting it in
front of anything.

**Right call for doing nothing:** if you can't name the delayed outcome
you'd post to `/feedback`, you don't have a Syntra problem yet.

## Honest limitations

- **Single node, no HA.** The store is a local filesystem and the learner
  is single-writer; `replicaCount = 1` is the only supported shape
  ([`deploy/README.md`](../deploy/README.md) caveats,
  [`docs/deployment.md`](deployment.md) "Kubernetes via Helm"). The
  documented resilience pattern is an active capsule with shadow-mode
  peers promoted on failure — not concurrent writers.
- **Throughput numbers are machine-specific.** The measured peak — ~11.6k
  sustained **shadow-mode** decisions/s at p99 1.45 ms — was taken on an
  Apple M5 Max with a co-limiting Python client, limiter raised, and is
  a floor on server capability, not a portable guarantee
  ([`docs/benchmarks.md`](benchmarks.md)). The default rate limiter is
  1000 req/s per principal and is deliberately not silently disabled.
- **Learning traffic costs more.** That baseline is `learn=false` shadow;
  `learn=true` decides take the per-capsule lock and fsync graph + memory
  and are far more expensive per request
  ([`docs/benchmarks.md`](benchmarks.md) "Known limits").
- **A crash can lose the log tail.** Append-only decision/audit logs are
  buffered writes without per-append fsync; the accepted tradeoff and the
  SQLite mitigation plan are documented honestly
  ([`docs/store-retention.md`](store-retention.md),
  [`docs/benchmarks.md`](benchmarks.md) "Known limits").
- **Retention rotates, it doesn't archive.** Logs rotate at a size cap
  (default 64 MiB + one rotated generation); rotated-away decisions stop
  resolving on `/feedback` with an honest 404 — size the cap above your
  feedback-latency horizon ([`README.md`](../README.md) "Log retention").
- **It is v0 software.** Pre-1.0 known gaps are listed in
  [`SECURITY.md`](../SECURITY.md) (among them: the admin-console posture
  needs a dedicated review, decision logs can carry application input
  fields, no field-level store encryption). Run it inside your trusted
  network behind a TLS-terminating proxy — never exposed directly to the
  public internet. There is no documented binary downgrade path
  ([`docs/deployment.md`](deployment.md) "Upgrades").
- **Scope honesty.** No GPU/training/fine-tuning, one shipped one-step
  EWMA forecaster, discrete action spaces only, nothing for one-shot
  decisions ([`README.md`](../README.md) "What Syntra is not",
  [`POSITIONING.md`](../POSITIONING.md)).

## Naming boundaries

Per [`AGENTS.md`](../AGENTS.md):

- **Lycan** is the language: `.lycs` syntax, compiler, graph binary format,
  capability ABI, the `lycan` CLI. It ships inside this repo as part of the
  single crate — you never need a separate checkout to run Syntra.
- **Syntra** is the runtime: the HTTP API, tenant/job/capsule store,
  persistent learning memory, audit logs, replay/simulation tooling, and
  the browser UI — the **admin console** (never "admin studio").
- **Lycan Marketplace** — signed-capsule distribution — is a *future*
  idea, not a product you can install today.
