# Quickstart: governed LLM model routing

Zero to production for the one journey that matters: install a model-router
capsule, decide per request, send delayed feedback, watch metrics, run in
shadow, gate promotion with replay, and know your rollback path.

Every command below was verified against the arg parsing in
[`src/lib.rs`](../src/lib.rs) (`cli_serve`, `cli_author`, `cli_simulate`,
`cli_replay` — there is no `src/bin/syntra.rs`; the `syntra` binary's
entry is `src/main.rs` → `syntra::run()`) and the route table in
[`src/server/routes.rs`](../src/server/routes.rs), and the HTTP sequence
was executed end-to-end against `target/release/syntra`. The Docker and
Helm paths are cited from their manifests, not re-executed here — those
two steps are marked accordingly.

Conventions: appliance at `http://127.0.0.1:8787`, admin key in
`$KEY`, capsule at `/tenants/acme/jobs/llm-routing/capsules/model-router`
(shortened below as `$API`).

## 1. Bring up the appliance

Pick one shape. All three run the identical surface
([`docs/deployment.md`](deployment.md)).

**Docker Compose** *(manifest-cited, not executed in this doc pass)* —
the root [`docker-compose.yml`](../docker-compose.yml) builds from the
repo root, publishes `8787`, and requires `LYCAN_ADMIN_KEY` (compose
refuses to start without it):

```bash
LYCAN_ADMIN_KEY=$(openssl rand -hex 32) docker compose up --build
```

The store lives in the named volume `syntra-store`; lose it and you lose
every learned weight and the audit history.

**Kubernetes (Helm)** *(manifest-cited, not executed in this doc pass)* —
chart at [`deploy/helm/syntra/`](../deploy/helm/syntra/); single replica
by design. Key handling (`adminKey.generate` default, `adminKey.value`,
`adminKey.existingSecret`) and the retrieve command are in
[`deploy/README.md`](../deploy/README.md):

```bash
helm install syntra ./deploy/helm/syntra/ --set adminKey.value=$(openssl rand -hex 32)
```

**Bare metal** *(verified)*:

```bash
cargo build --release --bin syntra
syntra serve --addr 127.0.0.1:8787 --store /var/lib/syntra --admin-key "$KEY"
```

`serve` accepts `--addr`, `--store`, `--admin-key`, `--dev-mode`
([`src/lib.rs`](../src/lib.rs) `cli_serve`). Without a key it refuses to
start; `--dev-mode` runs unauthenticated on loopback only. The default
store is `./lycan-store` — always pass `--store` explicitly.

Sanity check *(verified)*:

```bash
curl -s http://127.0.0.1:8787/health   # {"ok":true,"service":"Syntra"}
curl -s http://127.0.0.1:8787/ready    # store-writability probe (503 if unwritable)
```

## 2. Create the job and install the capsule

Use the shipped demo capsule — the same `.lyc` the golden governed demo
installs ([`examples/demo-governed-llm-routing.sh`](../examples/demo-governed-llm-routing.sh)):

```bash
API=http://127.0.0.1:8787/tenants/acme/jobs/llm-routing/capsules/model-router

curl -s -X POST http://127.0.0.1:8787/tenants/acme/jobs \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"id":"llm-routing","name":"LLM Routing"}'

curl -s -X POST "$API/install" \
  -H "Authorization: Bearer $KEY" \
  --data-binary @examples/demo_llm_model_router.lyc
# → {"ok":true,…,"hash":"e80c50…"}   SHA-256 of the graph, also appended to audit.jsonl
```

The install body must be a raw `.lyc` binary (magic header `LYCN`);
anything else is a 400 ([`src/server/routes.rs`](../src/server/routes.rs)
install handler). In production you author your own spec
(`syntra author capsule.yaml --out-dir ./out/`, verified) and install its
`program.lyc`; the option list here is `cheap_fast`, `balanced`,
`expensive_accurate`.

## 3. Create scoped tokens (do this before wiring traffic)

Keep the admin key in a vault for operators. The serving path and the
outcome pipeline get separate scoped tokens (`POST /admin/tokens` is
admin-only; the raw token is shown exactly once):

```bash
# Serving gateway: decides + reads only
curl -s -X POST http://127.0.0.1:8787/admin/tokens \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"scope":{"kind":"read","tenant":"acme","job":"llm-routing","capsule":"model-router"},"label":"gateway-read","ttlSeconds":86400}'

# Outcome pipeline: may post feedback (mutates learned state)
curl -s -X POST http://127.0.0.1:8787/admin/tokens \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"scope":{"kind":"tenant_admin","tenant":"acme"},"label":"outcome-writer","ttlSeconds":86400}'
```

Scope semantics, verified against [`src/auth_tokens.rs`](../src/auth_tokens.rs)
and executed:

| Token | `/decide` | `/report`, `/contexts`, `/decisions` | `/feedback` | `learn=true` | token admin |
|---|---|---|---|---|---|
| `read` (per tenant+job+capsule) | ✔ | ✔ | **403** | silently forced to read-only | 403 |
| `tenant_admin` (per tenant) | ✔ | ✔ | ✔ | ✔ (same tenant) | 403 |
| legacy admin key | ✔ | ✔ | ✔ | ✔ | ✔ |

The `read`-token feedback 403 is pinned by
[`tests/auth_routes.rs`](../tests/auth_routes.rs); the `learn=true`
downgrade is in [`src/server/routes.rs`](../src/server/routes.rs)
(read-scoped decide). Check any token's identity with
`curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8787/auth/whoami`.
Revoke with `DELETE /admin/tokens/{hash}`; inventory with `GET /admin/tokens`.

## 4. Decide → act → feedback

The serving side. This example uses the discrete-context demo capsule
(`contextKey` per request bucket); feature-context capsules post a
`features` map instead ([`docs/api.md`](api.md) "Decide"). The Python
integration for real LLM traffic is
[`examples/llm-routing/`](../examples/llm-routing/) (`LLMRouter.choose` /
`router.report`).

```bash
# 1. Ask (read token is enough). No ?learn=true — default is read-only shadow.
RESP=$(curl -s -X POST "$API/decide" \
  -H "Authorization: Bearer $READ_TOKEN" -H "Content-Type: application/json" \
  -d '{"contextKey":"support-low-cost"}')

DID=$(echo "$RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["decisionId"])')
# decisions[0].chosen_option: 0=cheap_fast, 1=balanced, 2=expensive_accurate
# If "refused": true → call your fallback model, not the suggestion.

# 2. Later, when the outcome resolves (judge score, user retry, cost report):
curl -s -X POST "$API/feedback" \
  -H "Authorization: Bearer $WRITE_TOKEN" -H "Content-Type: application/json" \
  -d "{\"decisionId\":\"$DID\",\"reward\":0.85}"
# → {"ok":true,"option":0,"before":[…],"after":[…],"warmup":{…}}
```

Feedback may also arrive as components (`{"decisionId":"…","components":
{"quality":0.85,"latency_ms":1240,"cost_usd":0.018}}`) once you `PUT
$API/reward_spec`; the shape and reduction rules are in
[`docs/api.md`](api.md) "Feedback".

Notes from [`docs/operating.md`](operating.md): the first ~30 feedback
rounds are **Warmup** (uniform-random exploration — don't read weights
yet); the response's `warmup.state` tells you where you are. High-cardinality
`contextKey`s (user IDs, request IDs) learn nothing — bucket on the axis
that matters (tier, task type, urgency).

Inspect learned state at any time (all verified):

```bash
curl -s -H "Authorization: Bearer $READ_TOKEN" "$API/report"     # weights, tries, graph hash
curl -s -H "Authorization: Bearer $READ_TOKEN" "$API/contexts"   # per-context buckets
curl -s -H "Authorization: Bearer $READ_TOKEN" "$API/memory"     # meta-bandit + detectors
curl -s -H "Authorization: Bearer $READ_TOKEN" "$API/decisions"  # raw decision log (JSONL)
```

## 5. Metrics

`GET /metrics` is public by design (Prometheus exposition format) — gate
it with your network policy, same posture as `/health`. Exact series,
from [`src/server/metrics.rs`](../src/server/metrics.rs) (verified live;
`syntra_refusals_total` appears only once refusals occur, and
`syntra_meta_bandit_trials` once a capsule reaches Active — both honest
absences, not missing metrics):

| Series | Type | Labels |
|---|---|---|
| `syntra_requests_total` | counter | `kind` (decide/feedback/…), `tenant`, `job`, `capsule`, `status` |
| `syntra_decide_latency_seconds` (+`_bucket`/`_sum`/`_count`) | histogram | `le` |
| `syntra_refusals_total` | counter | `tenant`, `job`, `capsule`, `reason` |
| `syntra_warmup_state` | gauge | `tenant`, `job`, `capsule` (0=warmup, 1=active, 2=frozen) |
| `syntra_meta_bandit_trials` | gauge | `tenant`, `job`, `capsule`, `candidate` |

Counters are in-process and reset on restart; the two gauges are derived
from the store at scrape time. Dashboards/alerts in
[`deploy/grafana/`](../deploy/grafana/).

## 6. Shadow → governed promotion

The default posture **is** shadow: `/decide` never mutates learned state
in-band (read-only unless you pass `?learn=true`, and never for `read`
tokens). Your app answers with its incumbent route, logs Syntra's
suggestion, and posts real outcomes to `/feedback`. The pre-promotion bar
is the checklist in [`docs/operating.md`](operating.md) "Shadow-mode
checklist": decision/feedback rates roughly equal, sensible context
buckets, a converged meta-bandit candidate, defensible disagreements.

Then make the decision an artifact, not a meeting:

1. **Export evidence.** `GET $API/decisions` returns the append-only JSONL
   decision log. For the promotion gate you need shadow events with the
   fields listed in `syntra replay --help` (`contextKey`,
   `baselineAction`, `candidateAction`, `actionRewards`, optional
   `actionCostsUsd`, `actionLatencyMs`, `segment`, `oracleAction`).
   [`examples/demo-governed-llm-routing.sh`](../examples/demo-governed-llm-routing.sh)
   shows exactly this writer, end to end.
2. **Write the gates** — copy
   [`examples/replay/promotion.yaml`](../examples/replay/promotion.yaml)
   (reward uplift + CI floor, max cost increase, max latency p95 increase,
   coverage, per-segment regression) and set numbers your SLO owners will
   sign.
3. **Replay** *(verified flag set)*:

```bash
syntra replay \
  --events shadow-decisions.jsonl \
  --policy-json candidate-policy.json \
  --gates promotion.yaml \
  --format markdown \
  --out promotion-report.md \
  --fail-on-gate
```

Exit code is non-zero when a gate fails — put `--fail-on-gate` in CI so
promotion is a build result ([`examples/replay/README.md`](../examples/replay/README.md)).
Two supporting tools, used the same way:
[`examples/offline-eval/`](../examples/offline-eval/) estimates the
candidate's value on historical logs **before** you shadow (needs logged
propensities — it says so when it can't run), and
[`examples/ab-harness/`](../examples/ab-harness/) compares two capsules on
paired seeded traffic with a paired t-test.

4. **Promote by flipping your side.** Production control lives in your
   caller: after the gate passes, your serving code starts honoring
   `decisions[0].chosen_option` instead of the incumbent route. There is
   no "take control" flag to click — that's deliberate
   ([`docs/why-syntra.md`](why-syntra.md)).

Pre-flight a new capsule before any of this:
`syntra simulate capsule.yaml --rounds 5000 --true-arm-rewards "0.2,0.5,0.7" --seed 7`
(verified flags) exercises the learner offline against a synthetic reward
model ([`src/lib.rs`](../src/lib.rs) `cli_simulate`).

## 7. Rollback story

Ordered by blast radius, all mechanisms cited:

1. **Stop honoring suggestions** — a caller-side change; Syntra keeps
   deciding into the log harmlessly (this is just shadow mode again).
   Client-side fail-safes (unreachable / refused / malformed → fallback
   model) are in [`examples/llm-routing/README.md`](../examples/llm-routing/README.md)
   "Fail-safe behavior".
2. **Stop the learner, keep the behavior** — freeze: `PUT $API/learning`
   with `safety.freezeLearning = true` (route verified in
   [`src/server/routes.rs`](../src/server/routes.rs); semantics in
   [`docs/operating.md`](operating.md) — there is no dedicated freeze
   route).
3. **Roll the artifact** — reinstall the previous `.lyc` bytes via
   `$API/install`; the new install is hashed and audited, and
   `audit.jsonl` + `GET $API/report`'s graph hash tell you exactly which
   graph served which window ([`docs/api.md`](api.md) "Capsule install").
   Pre-mutation snapshots are listed by `GET $API/snapshots`.
4. **Roll the whole store** — `POST /admin/backup` streams a restorable
   JSON bundle, `POST /admin/restore` restores it (routes verified;
   verified end-to-end walkthrough in
   [`examples/walkthroughs/05_backup_and_restore.py`](../examples/walkthroughs/05_backup_and_restore.py));
   the volume-copy pattern is the always-available path
   ([`docs/operating.md`](operating.md) "Backup and restore").
5. **Binary rollback** — there is **no documented downgrade path**:
   restore the store from a pre-upgrade backup and run the older binary
   ([`docs/deployment.md`](deployment.md) "Upgrades").

## Not verifiable in this pass

- `docker compose up --build` and the `helm install` above are cited from
  [`docker-compose.yml`](../docker-compose.yml) and
  [`deploy/README.md`](../deploy/README.md) / `deploy/helm/syntra/` and
  were not executed here (Docker/K8s were out of scope for this
  verification); flags and env requirements match the manifests.
- Everything else on this page — `serve`, `author`, `simulate`, `replay`,
  every curl route, the token scope table, the metric names, and the
  fixture replay pass — was executed against `target/release/syntra`
  (v0.2.0).
