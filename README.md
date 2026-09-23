# Syntra

**Decisions that learn, made in microseconds, with the evidence to trust them.**

Syntra is a self-hosted contextual-bandit decision service. Your application
asks which of several actions to take (which model answers a request, which
backend serves it, which offer to show), acts, and reports how it went.
Syntra learns from those outcomes, and it logs every decision with the
probability it was made with. That log is the evidence: from it Syntra
estimates, before you ship a change, how a new policy would have done
(off-policy evaluation), and it refuses to promote a change that cannot
prove itself.

- **In-process decisions in about a microsecond.** SDKs hold a copy of the
  published model and decide locally: no network hop, no outage when the
  server is down. The server verifies every uploaded decision by replaying
  it, then learns from its rewards.
- **Or one HTTP call:** `/decide` answers in tens of microseconds on the
  server, and nothing on the hot path waits on disk.
- **Provable changes.** Every decision's propensity is logged, so IPS, SNIPS
  and doubly robust estimates with paired confidence intervals come from
  your own traffic. `POST .../promote` applies a spec change only if its
  gates pass (for example `lift.dr.lower >= 0`).
- **A drop-in for Azure Personalizer**, which retires on 1 October 2026:
  point a Personalizer client at Syntra and change the key.
- **One binary, one SQLite file.** Decisions, rewards, model snapshots and
  the audit trail live in `syntra.db`; the process is disposable, the store
  is not.

## Quickstart

To watch it learn first: `cargo run --release -- demo` starts a server with
simulated LLM-routing traffic and prints the admin console's address and
key.

```bash
cargo build --release
export KEY=$(openssl rand -hex 24)
./target/release/syntra serve --store ./syntra-store --admin-key "$KEY" &
B=http://127.0.0.1:8787/v1/tenants/acme/jobs/prod/capsules/router
```

Create a capsule (one decision point) with its actions, then decide and
reward:

```bash
curl -X PUT $B/spec -H "Authorization: Bearer $KEY" \
  -d '{"actions": [{"id": "small", "features": {"cost": 0.1}}, {"id": "large", "features": {"cost": 1.0}}]}'

curl -X POST $B/decide -H "Authorization: Bearer $KEY" \
  -d '{"context": {"task": "code", "promptTokens": 812}}'
# {"decisionId":"dec_1a0cff5777e748dd133cbd4","action":"large","probability":0.5,
#  "ranking":[{"id":"small","probability":0.5},{"id":"large","probability":0.5}],
#  "mode":"learner","modelVersion":0,...}

curl -X POST $B/reward -H "Authorization: Bearer $KEY" \
  -d '{"decisionId": "dec_1a0cff5777e748dd133cbd4", "reward": 0.8}'
# {"ok":true,"applied":true,"learned":true,"modelVersion":1}
```

`GET $B/decisions/{id}` shows the stored decision: the context, the actions,
the full probability distribution it was drawn from, the seed, and its
rewards.

With Docker: `docker build -t syntra .` then
`docker run -p 8787:8787 -e SYNTRA_ADMIN_KEY=$KEY -v syntra-data:/var/lib/syntra syntra`.

## Decide in-process

```python
from syntra import LocalDecider   # sdk/python (Rust core via PyO3)

with LocalDecider("http://127.0.0.1:8787", token=KEY,
                  tenant="acme", job="prod", capsule="router") as router:
    d = router.decide({"task": "code", "promptTokens": 812})   # ~1-3 us, no network
    answer = call_model(d.action)
    router.reward(d.decision_id, score(answer),
                  detail={"latencyMs": 840, "costUsd": 0.0031})
```

The decider syncs the published model (an ETag poll), decides with the same
code the server runs, and uploads decisions and rewards in the background.
The server replays each uploaded decision against the exact model and seed
it names; anything that does not replay is refused and audited, so the log
only ever holds propensities the model produced. Retries are idempotent.
The Rust SDK is `syntra::client::LocalDecider`.

For LLM routing, `syntra.llm.ModelRouter` wraps `litellm.completion` (or
any completion function): it picks the model per request, measures latency
and cost, and learns from `quality - cost_weight * cost - latency_weight *
latency`, with quality reported at call time, by a judge, or later (see
[sdk/python/README.md](sdk/python/README.md)).

## Measured

Apple M5 Max, release build, client and server on one machine over
loopback. Your hardware will differ; `examples/bench_decide.rs` and
`examples/bench_local.rs` reproduce these.

| Path | Load | p50 | p99 | Throughput |
|---|---|---|---|---|
| `LocalDecider.decide` (Rust) | 1 thread | 0.92 µs | 1.8 µs | 870k/s |
| `LocalDecider.decide` (Rust) | 8 threads, one decider | 1.7 µs | 4.3 µs | 3.2M/s |
| `LocalDecider.decide` (Python) | 1 thread | 1.3-2.8 µs per call | | |
| HTTP `/decide` | 1 connection | 36 µs | 68 µs | 27k/s |
| HTTP `/decide` | 8 connections | 92 µs | 267 µs | 80k/s |
| HTTP decide + reward | 8 connections | 106 µs / 182 µs | 243 µs / 524 µs | 51k pairs/s |
| Verified upload | one decider | | | 67-80k decisions/s |

Learning quality on simulated contextual bandits with a known optimum
(`cargo run --release --example learning_bench`: 20,000 rounds, 5 seeds;
the share of the oracle's expected reward over the final 10% of rounds):

| Environment | SquareCB (default) | Epsilon-greedy 0.1 | Uniform |
|---|---|---|---|
| 4 segments x 3 actions | 0.986 | 0.965 | 0.755 |
| Same, best actions change halfway | 0.985 | 0.963 | 0.761 |
| 20 of 200 items per request, reward nonlinear in features | 0.792 | 0.782 | 0.596 |

The last row shows a limit: the learner is linear in its (quadratic)
features, so rewards that depend nonlinearly on feature matches are only
partly captured. Simulations are not your traffic; evaluate on your own
logs before trusting a policy.

## Evaluate before you change anything

Every decision is logged with the probability of the chosen action and the
full distribution, so a candidate policy can be scored on real traffic
before it serves any:

```bash
# From the store, read-only (safe while the server runs):
syntra evaluate --store ./syntra-store --capsule acme/prod/router \
  --policy constant:small --gates gates.yaml --fail-on-gate
```

The report gives DM, IPS, SNIPS and cross-fitted doubly robust estimates
with bootstrap intervals, each estimator's lift over the logged policy
paired on the same rows, effective sample size, weight diagnostics and a
plain-language verdict. Over HTTP:

```bash
# What would the learned policy have earned?
curl -X POST $B/evaluate -H "Authorization: Bearer $KEY" -d '{"policy": "greedy"}'

# Apply a spec change only if it beats what was logged:
curl -X POST $B/promote -H "Authorization: Bearer $KEY" \
  -d '{"spec": {"learner": {"learningRate": 0.25}}, "gates": ["lift.dr.lower >= 0"]}'
# 200 with the new spec and the report, or 409 with the report
```

Promotions and refusals are audited. A candidate spec is scored as it
would serve: on each logged decision, the probabilities its exploration
would put on each action, from its learner trained on the other rows
(cross-fitting). So a gate also sees what a change to exploration costs.

## Moving off Azure Personalizer

Personalizer v1.0 clients that rank, reward and activate work unchanged
against Syntra: create a capsule, issue a key scoped to it, and use that as
`Ocp-Apim-Subscription-Key` with the endpoint pointed at your Syntra server.

```bash
# Actions arrive with each rank call, so the spec declares none.
curl -X PUT http://127.0.0.1:8787/v1/tenants/acme/jobs/prod/capsules/news/spec \
  -H "Authorization: Bearer $KEY" -d '{"actions": [], "exploration": {"kind": "epsilonGreedy", "epsilon": 0.2}}'

TOKEN=$(curl -s -X POST http://127.0.0.1:8787/v1/admin/tokens -H "Authorization: Bearer $KEY" \
  -d '{"scope": {"kind": "read", "tenant": "acme", "job": "prod", "capsule": "news"}, "label": "personalizer"}' \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["token"])')

curl -X POST http://127.0.0.1:8787/personalizer/v1.0/rank -H "Ocp-Apim-Subscription-Key: $TOKEN" \
  -d '{"contextFeatures": [{"user": {"tier": "pro"}}],
       "actions": [{"id": "sports", "features": [{"topic": "sports"}]}, {"id": "news", "features": [{"topic": "news"}]}],
       "eventId": "75269AD0-BFEE-4598-8196-C57383D38E10"}'
```

`rank`, `events/{eventId}/reward`, `events/{eventId}/activate` (deferred
activation holds the event and its early rewards until activated) and
`configurations/service` (reward wait time, default reward, reward
aggregation, exploration percentage, Online or Apprentice learning) are
supported, with Personalizer's error shape. Multi-slot ranking is not.
Apprentice mode maps to `baselineExplore`, with the first action as the
baseline.

Bring the history with you: `syntra import dsjson --store ./syntra-store
--capsule acme/prod/news exported-logs.json` loads Personalizer (or Vowpal
Wabbit) DSJSON logs with their propensities and rewards, so
`syntra evaluate` can score policies on them before any traffic moves.
Imported rewards are not learned unless `--learn` (a warm start).

## Concepts

- **Capsule**: one decision point, addressed as
  `tenant/job/capsule`. Its **spec** lists the actions (ids and features),
  exploration (SquareCB by default, or epsilon-greedy with a floor), the
  learner (hashed features, normalized online least squares), the reward
  range and aggregation (`first` or `sum`), and the mode. Specs change by
  JSON merge patch; unknown fields are rejected.
- **Modes**: `learner` serves the learned policy with exploration and keeps
  learning; `baselineExplore` serves your incumbent action most of the time
  and explores the rest (a safe way to start logging); `frozen` serves
  without learning.
- **Rewards** can arrive late, several per decision (`sum`) or one
  (`first`), with idempotency keys for retries. `reward.default` with
  `reward.waitSeconds` applies a default to decisions that get none, for
  feedback that only reports successes.
- **Per-request actions**: pass `actions` in `/decide` when the candidates
  change per request (articles, offers); `excludedActions` removes some.
- **Feature programs** (optional) compute derived features or exclude
  actions in [Lycan](docs/lycan/README.md), sandboxed by a per-capsule
  policy: file access confined to the capsule's `data/`, HTTP only to
  allow-listed hosts, private networks denied.

The full API is in [docs/openapi.yaml](docs/openapi.yaml), kept in step
with the router by `tests/openapi_drift.rs`. The design, including the
durability model and the local-evaluation protocol, is in
[docs/design/v2-decision-core.md](docs/design/v2-decision-core.md).

## Operating it

- **Storage.** `syntra.db` (SQLite, WAL) holds decisions, rewards, model
  snapshots and the audit trail; specs, policies and programs are files
  beside it. Decisions and rewards are written behind the request in
  batches (at most 2 ms); `"durable": true` on a request waits for the
  commit. After a restart, a capsule's model is rebuilt on first use from
  its latest snapshot plus the rewards logged after it, which reproduces
  it exactly.
- **Backups.** `syntra backup --store <root> --out <dir>` takes a
  consistent copy while the server runs; `syntra restore` refuses a live
  store. `syntra doctor --store <root>` checks a store read-only.
- **Observability.** `/metrics` (Prometheus; admin credential unless
  `--metrics-public`) has decide latency, event-log commits and backlog,
  upload and default-reward counters and per-capsule model versions.
  `/health` and `/ready` are open. The admin console is at `/admin`.
- **Access.** The operator key, or scoped tokens: `tenant_admin` for one
  tenant, `read` for one capsule's data plane (decide, reward, uploads,
  reads). Tokens expire and can be revoked; per-token rate limits apply.
- **Deployment.** A `Dockerfile` at the root and a Helm chart in
  `deploy/helm/syntra`. Put the server behind a TLS-terminating proxy; see
  [SECURITY.md](SECURITY.md) for the security model and its known gaps.

## Status and limits

The v2 decision core, server, local evaluation, off-policy evaluation and
the Personalizer-compatible API are complete and tested (450+ tests,
including crash recovery under load, fuzzing of specs and model snapshots,
and a drift test that keeps the OpenAPI document honest). Not yet:

- One node: SQLite, one writer. A Postgres backend and multiple decide
  nodes are planned.
- SDKs: Rust and Python decide in-process. TypeScript (`sdk/typescript`)
  decides over HTTP; its in-process decider waits on a WebAssembly build
  of the Rust core.
- Several docs and examples under `docs/` and `examples/` still describe
  v1 and are being ported; [DEMOS.md](DEMOS.md) lists what runs against
  v2 today.
- The capability sandbox runs in-process, not behind an OS boundary.

The Lycan language (`.lycs` source, the graph binary format, verifier and
`lycan` CLI) ships in this repository as part of the same crate; the
science demos and research tooling live in the separate Lycan Lab
repository.

## License

Apache-2.0. See [LICENSE](LICENSE).
