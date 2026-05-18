# What Changed in Phases G and H

This document is for operators who ran a Phase F deployment and are now
upgrading to Phase H. It covers what changed, what you must do before going
to production, and what is in the source but not yet wired into the server.

If you are new to Syntra, start with [`../README.md`](../README.md) and
[`operating.md`](operating.md) first.

Note on changelog status: the `CHANGELOG.md` in this repository describes
Phases A through F under a single `[Unreleased]` header. A sibling agent is
adding a `Phase G + H` section. If that section has not yet landed at the
time you read this, the source of truth is the `Lang/src/` files and this
document — both were written against the same codebase.

---

## 1. Upgrade Checklist

Complete these in order on your staging environment before touching
production.

**1. Memory schema is still v7 — no migration needed.**

The `memory.json` schema did not bump in Phases G or H. The backward-compat
readers cover v2 through v7 and remain unchanged. A capsule restored from any
Phase A–F backup will load correctly into a Phase H binary.

Verify after upgrade:

```bash
curl -s -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/tenants/acme/jobs/routing/capsules/router/memory \
  | jq '.schema_version'
# expected: 7
```

**2. Understand the new rate-limit default before deploying if all production
traffic flows through one bearer token.**

The default is 1000 req/sec sustained, 2000 burst, per principal. A
"principal" is either the legacy `LYCAN_ADMIN_KEY` (keyed as the string
`legacy-admin`) or a scoped token (keyed by its SHA-256 hash). If your Phase
F deployment has every service — analytics, automation, production decide
traffic — sharing one admin key, they all share one rate-limit bucket.

Under normal load this is fine: the default ceiling is high enough that
typical workloads (a few hundred req/sec) never approach it. It becomes a
problem if one service in the shared-key group generates a burst that
exhausts the bucket. The recommended remediation is covered in the next
checklist item. See also [Section 3](#3-new-auth--rate-limit-surface).

**3. Issue scoped tokens and stop using the legacy admin key for
production traffic.**

`POST /admin/tokens` (requires Admin scope) issues a scoped bearer token.
Each token has its own rate-limit bucket, independent of the admin key and
of other tokens. The recommended topology is:

- One `Read`-scoped token per consumer service (analytics dashboard, decision
  client). `Read` tokens can call `POST /decide` and all `GET` endpoints for
  a specific `(tenant, job, capsule)` triple; they cannot post feedback, change
  config, or install capsules.
- One `TenantAdmin`-scoped token per operator or team automation (CI
  pipeline, deployment script). `TenantAdmin` tokens can do everything
  including install and feedback for one tenant.
- Keep `LYCAN_ADMIN_KEY` in the environment but stop using it in normal
  operation. Treat it as a break-glass credential — use it only to issue
  new tokens or recover from a corrupted token store.

Token issuance, listing, and revocation are documented in
[Section 3](#3-new-auth--rate-limit-surface).

**4. If you have a structured-log pipeline parsing Syntra's output, switch
it to JSON.**

Tracing now writes JSON to stderr. A typical log line looks like:

```json
{"timestamp":"2026-05-17T09:12:04.001Z","level":"INFO","fields":{"addr":"0.0.0.0:8787","store":"/var/lib/syntra","workers":8,"service":"Syntra","message":"syntra server listening"}}
```

Drop any line-based regexes you were using. Point your log shipper (Fluent
Bit, Filebeat, Vector) at stderr and configure JSON parsing. The default log
level is `info`, which surfaces startup, auth failures, drift events, and
initialization warnings without per-request noise. See
[Section 2](#2-new-observability-surface) for the level guide.

**5. Set up the backup endpoint and rotate it daily.**

`POST /admin/backup` returns the full store bundle as a JSON body. The
bundle contains all capsule state: `memory.json`, `warmup.json`,
`audit.jsonl`, `decision.jsonl`, `feedback.jsonl`, `learning.json`,
`policy.json`, and the compiled `.lyc` graph binaries. It does not contain
the `snapshots/` subdirectory (those are ephemeral pre-mutation copies) or
the `LYCAN_ADMIN_KEY`.

Recommended cron pattern (daily, 30-day retention):

```bash
# /etc/cron.d/syntra-backup
0 3 * * * syntra-svc curl -sf -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/admin/backup \
  -o /var/backups/syntra/syntra-$(date +\%Y\%m\%d).json \
  && find /var/backups/syntra -name 'syntra-*.json' -mtime +30 -delete
```

Verify a backup is non-empty before setting up retention:

```bash
wc -c /var/backups/syntra/syntra-$(date +%Y%m%d).json
# should be non-trivial even for an empty appliance (>1 KB)
```

`POST /admin/restore` accepts the backup JSON and restores it to a running
(empty-store) server. See [Scenario 7 in the runbook](runbook.md) for the
full migration procedure.

**6. Update your k8s readiness probe from `/health` to `/ready`.**

`GET /ready` performs a live write-test against the store root (writes and
deletes a zero-byte probe file). It returns `200` when the store is writable
and `503` with a structured reason when it is not. This is more informative
for a load balancer drain: a `503` from `/ready` means the process is alive
but the data layer is degraded.

`GET /health` remains the liveness probe and is unchanged.

If you are using the Helm chart in `deploy/helm/syntra/`, the chart currently
sets both probes to `/health`. Update `values.yaml` to point `readinessProbe`
at `/ready`:

```yaml
readinessProbe:
  httpGet:
    path: /ready
    port: http
  initialDelaySeconds: 5
  periodSeconds: 10
  timeoutSeconds: 3
  failureThreshold: 3
```

**7. LinTs is now auto-enrolled in the meta-bandit candidate set for
feature-context capsules. No config change is required.**

The meta-bandit candidate portfolio grew from six to seven candidates. For
feature-context capsules (those with `contextSpec.type = "features"` in
`learning.json`), the portfolio is now: Thompson, UCB, Weighted,
EpsilonGreedy, Greedy, LinUCB, and LinTs. For discrete-context capsules the
portfolio remains the same five (Thompson, UCB, Weighted, EpsilonGreedy,
Greedy) — LinTs and LinUCB require a feature vector and are not enrolled.

LinTs is auto-enrolled; there is nothing to configure. If you inspect
`/memory` on a feature-context capsule after upgrading, you will see a
seventh entry in `strategies[nodeId].metaBandit.candidates` with
`"id": "LinTs"` and a trial count that grows from zero as traffic flows.

What this means for existing capsules: the meta-bandit will allocate some
exploration budget to LinTs trials. During that exploration period you may
see `"candidateId": "LinTs"` appear in `/decide` responses. The trial
distribution stabilizes within a few hundred feedbacks. This is expected
behavior, not a regression. See [Section 4](#4-new-algorithmic-capability)
for a description of when LinTs outperforms LinUCB.

**8. Continuous action space is opt-in per capsule. Existing capsules are
unaffected.**

`ActionSpace` is a new field in `learning.json`. It defaults to
`{"type": "discrete"}`, which is the pre-existing behavior: the K options
in the capsule YAML are treated as discrete choices. No existing capsule
changes behavior unless you explicitly set `actionSpace` to the continuous
form.

To opt a capsule into continuous action space, PUT `learning.json` with the
new field:

```bash
curl -s -X PUT \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/tenants/acme/jobs/pricing/capsules/threshold/learning \
  -d '{"actionSpace": {"type": "continuous", "range": [0, 100], "buckets": 10}}'
```

See [Section 4](#4-new-algorithmic-capability) for the use-case detail.

---

## 2. New Observability Surface

### Prometheus metrics at `/metrics`

`GET /metrics` emits Prometheus text format (no auth is required by the
route, but operators typically control access via the reverse proxy or
network policy on the listener, the same posture as `/health`). The full
set of metric families emitted by `render_metrics` in `Lang/src/server.rs`:

```
# HELP syntra_requests_total Total Syntra HTTP requests, by kind/status.
# TYPE syntra_requests_total counter
syntra_requests_total{kind="decide",tenant="acme",job="routing",capsule="router",status="ok"} 84231

# HELP syntra_decide_latency_seconds /decide latency histogram.
# TYPE syntra_decide_latency_seconds histogram
syntra_decide_latency_seconds_bucket{le="0.005"} 71020
syntra_decide_latency_seconds_bucket{le="0.01"} 83118
syntra_decide_latency_seconds_bucket{le="0.025"} 84180
syntra_decide_latency_seconds_bucket{le="0.05"} 84230
syntra_decide_latency_seconds_bucket{le="0.1"} 84231
syntra_decide_latency_seconds_bucket{le="0.25"} 84231
syntra_decide_latency_seconds_bucket{le="0.5"} 84231
syntra_decide_latency_seconds_bucket{le="1.0"} 84231
syntra_decide_latency_seconds_bucket{le="2.5"} 84231
syntra_decide_latency_seconds_bucket{le="5.0"} 84231
syntra_decide_latency_seconds_bucket{le="10.0"} 84231
syntra_decide_latency_seconds_bucket{le="+Inf"} 84231
syntra_decide_latency_seconds_sum 429.14
syntra_decide_latency_seconds_count 84231

# HELP syntra_refusals_total Total refused /decide responses, by reason.
# TYPE syntra_refusals_total counter
syntra_refusals_total{tenant="acme",job="routing",capsule="router",reason="ood"} 12

# HELP syntra_warmup_state Capsule lifecycle (0=warmup,1=active,2=frozen).
# TYPE syntra_warmup_state gauge
syntra_warmup_state{tenant="acme",job="routing",capsule="router"} 1

# HELP syntra_meta_bandit_trials Meta-bandit trial count per candidate.
# TYPE syntra_meta_bandit_trials gauge
syntra_meta_bandit_trials{tenant="acme",job="routing",capsule="router",candidate="Thompson"} 1203
syntra_meta_bandit_trials{tenant="acme",job="routing",capsule="router",candidate="LinUcb"} 1198
syntra_meta_bandit_trials{tenant="acme",job="routing",capsule="router",candidate="LinTs"} 1194
```

Sample curl:

```bash
curl -s -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/metrics | grep syntra_
```

The five metric families are:

- `syntra_requests_total` — counter, labeled by `kind`, `tenant`, `job`,
  `capsule`, `status`. `kind` is `decide`, `feedback`, or `feedback_batch`.
  `status` is `ok`, `err`, or (for batches) `partial`.
- `syntra_decide_latency_seconds` — histogram with 11 finite buckets plus
  `+Inf`. Buckets at 5 ms, 10 ms, 25 ms, 50 ms, 100 ms, 250 ms, 500 ms,
  1 s, 2.5 s, 5 s, 10 s.
- `syntra_refusals_total` — counter, labeled by `tenant`, `job`, `capsule`,
  `reason`. `reason` is one of `ood`, `interval_too_wide`, or
  `insufficient_calibration_data`.
- `syntra_warmup_state` — gauge, labeled by `tenant`, `job`, `capsule`.
  Values: 0 = Warmup, 1 = Active, 2 = Frozen.
- `syntra_meta_bandit_trials` — gauge, labeled by `tenant`, `job`,
  `capsule`, `candidate`. Candidate names: `Thompson`, `Ucb`, `Weighted`,
  `EpsilonGreedy`, `Greedy`, `LinUcb`, `LinTs`.

`syntra_warmup_state` and `syntra_meta_bandit_trials` are derived by walking
the store on every scrape. At development-scale deployments this is
negligible. At large installations with many capsules, scrape at 60-second
intervals rather than the Prometheus default of 15 s to avoid amplifying
store reads.

### Readiness probe at `/ready`

```bash
curl -s http://localhost:8787/ready
# 200 OK when store is writable:
{"ok":true,"service":"Syntra","store":"/var/lib/syntra"}

# 503 Service Unavailable when store is degraded:
{"ok":false,"service":"Syntra","store":"/var/lib/syntra","reason":"store unwritable: No space left on device"}
```

No auth is required. The probe writes a zero-byte file `.readiness_probe` to
the store root and immediately removes it. If the write fails (permissions,
full disk, mount disappeared) it returns 503 with the OS error reason.

### JSON tracing to stderr

Tracing is initialized via `tracing_subscriber` with a JSON formatter and
stderr writer. A startup line looks like:

```json
{"timestamp":"2026-05-17T09:12:04.001Z","level":"INFO","fields":{"addr":"0.0.0.0:8787","store":"/var/lib/syntra","workers":8,"service":"Syntra","message":"syntra server listening"}}
```

An auth failure looks like:

```json
{"timestamp":"2026-05-17T09:14:22.331Z","level":"WARN","fields":{"remote":"10.0.1.42:51234","method":"POST","url":"/tenants/acme/jobs/routing/capsules/router/decide","reason":"unknown_token","message":"auth failure"}}
```

A drift event looks like:

```json
{"timestamp":"2026-05-17T09:15:01.007Z","level":"INFO","fields":{"message":"change detected","tenant":"acme","job":"routing","capsule":"router"}}
```

Log level is controlled by the `RUST_LOG` environment variable. The default
is `info`.

- `RUST_LOG=info` (default) — startup, auth failures, drift events, store
  errors. Quiet on per-request traffic. This is what you want in production.
- `RUST_LOG=debug` — adds per-request routing decisions, auth outcome
  (granted scope), rate-limit check outcomes, per-capsule lock acquire/
  release, and details of backup and restore operations. Expect 5–20 x the
  log volume of `info` under moderate traffic.
- `RUST_LOG=warn` — only warnings and errors. Use this in very
  high-throughput environments where even `info` volume is costly. You will
  not see startup confirmation lines.

### Wiring Grafana

The dashboard JSON is at
`deploy/grafana/dashboards/syntra-overview.json`. Import it via Grafana's
dashboard import UI or push it via the provisioning mechanism. The dashboard
assumes a Prometheus data source. If your data source name differs from
`Prometheus`, update the `datasource` field in the JSON before import.

The alert rules are at `deploy/grafana/alerts/syntra-alerts.yaml`. The four
rules cover:

- `SyntraHighDecideLatency` — `/decide` p99 > 100 ms for 5 minutes (warning)
- `SyntraHighRefusalRate` — refusing > 50% of `/decide` calls for 5 minutes
  (critical)
- `SyntraCapsuleStuckInWarmup` — `syntra_warmup_state == 0` for 60 minutes
  (warning)
- `SyntraDown` — Prometheus scrape target down for 2 minutes (critical)

The YAML is formatted for Grafana's managed alert provisioning API. If you
use a standalone Alertmanager, convert to a Prometheus rule group.

---

## 3. New Auth and Rate-Limit Surface

### Scope data model

Three scopes are defined in `Lang/src/auth_tokens.rs`:

| Scope | What it allows |
|---|---|
| `Admin` | All routes, all tenants. Equivalent to the legacy `LYCAN_ADMIN_KEY`. |
| `TenantAdmin` | All routes (install, feedback, config, read) for one tenant, any job, any capsule. |
| `Read` | `POST /decide` and all `GET` endpoints for one specific `(tenant, job, capsule)` triple. Cannot post feedback, change config, or install. |

The `Read` scope is intentionally narrow. It allows a downstream service to
call `/decide` (and read inspection endpoints) for one capsule path, and
nothing else. It cannot mutate weights via `/feedback` — that is a
`CapsuleMutate` action reserved for `TenantAdmin` and `Admin`.

### Issuing tokens

```bash
# Issue a read-only token for one capsule — for an analytics service
# that only needs to call /decide and /report
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/admin/tokens \
  -d '{
    "scope": {
      "kind": "read",
      "tenant": "acme",
      "job": "routing",
      "capsule": "router"
    },
    "ttlSeconds": 7776000,
    "label": "analytics-svc-prod"
  }'
```

Response (the `token` value is returned only once — store it securely):

```json
{
  "token": "a3f7c2e1b9d04f8e6a2c1b0d9e3f7a4b2c8d5e6f0a1b3c4d2e9f8a7b6c5d4e3",
  "hash": "8b2c4f6a1e3d5b7c9a0f2e4d6b8c0a2e4f6b8d0a2c4f6e8a0b2d4f6c8e0a2c4",
  "scope": {"kind": "read", "tenant": "acme", "job": "routing", "capsule": "router"},
  "expiresAt": 1766246400
}
```

```bash
# Issue a TenantAdmin token for team automation (CI deploy pipeline)
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/admin/tokens \
  -d '{
    "scope": {"kind": "tenant_admin", "tenant": "acme"},
    "ttlSeconds": 7776000,
    "label": "acme-deploy-automation"
  }'
```

```bash
# List all issued tokens (returns hash + metadata, never the raw token)
curl -s -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/admin/tokens | jq '.tokens[] | {hash, scope, label, expiresAt}'
```

```bash
# Revoke a token by its hash
curl -s -X DELETE \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/admin/tokens/8b2c4f6a1e3d5b7c9a0f2e4d6b8c0a2e4f6b8d0a2c4f6e8a0b2d4f6c8e0a2c4
# → {"ok":true,"revoked":true}
```

Tokens are stored SHA-256 hashed in `$LYCAN_STORE_ROOT/tokens.json`. The
raw value is returned exactly once at issuance. If you lose a raw token, you
cannot recover it — revoke the hash and issue a new token.

### Rate-limit math

The rate limiter uses a token-bucket algorithm, per principal:

- **Sustained rate:** 1000 req/sec per principal
- **Burst capacity:** 2000 tokens per principal
- **Refill:** 1000 tokens/sec, starting from the current bucket level

"Principal" means: the legacy admin key (single bucket, keyed `legacy-admin`)
or a scoped token (bucket keyed by the token's SHA-256 hash). Two scoped
tokens are two independent buckets. If your analytics service and your
production decide traffic each have their own token, a burst from analytics
does not consume production's budget.

The rate limiter applies to `/decide` and `/feedback` (and `feedback/batch`).
Admin routes (`/admin/tokens`, `/admin/backup`, etc.) are not rate-limited.

### How a 429 looks on the wire

```
HTTP/1.1 429 Too Many Requests
Content-Type: application/json
Retry-After: 1

{"error":"rate limit exceeded","retryAfterSeconds":1}
```

`Retry-After` is a whole-second ceiling of the computed wait time. The JSON
body carries `retryAfterSeconds` as a number (same value). Your integration
should honour `Retry-After` and not retry immediately — a retry storm from
multiple clients sharing one token will immediately re-exhaust the bucket.

If you see 429 from production traffic:

1. Check which principal is hitting the limit. Tail stderr for log lines with
   `"reason":"rate_limit"` (these appear at `debug` level — switch to
   `RUST_LOG=debug` temporarily).
2. If legitimate traffic from one service exceeds 1000 req/sec sustained,
   issue that service a dedicated token. Its bucket is independent.
3. If the global default itself is too low for your workload, the current
   release does not expose a per-token rate-limit override via API. The
   workaround is to issue separate tokens per high-volume caller. A
   configurable per-token limit is tracked as known debt; see
   [Section 7](#7-known-not-yet-wired).

---

## 4. New Algorithmic Capability

### LinTs — Linear Thompson Sampling

LinTs (Linear Thompson Sampling) is the seventh candidate in the meta-bandit
portfolio for feature-context capsules. It uses the same per-option `LinUcbState`
(the `A_inv` and `b` matrices in `Lang/src/linucb.rs`) as LinUCB, but at
decision time it samples a parameter vector from the posterior distribution
`N(theta, v^2 * A_inv)` using a Cholesky factorization of `A_inv`, then
scores each option as `x · theta_sample`. LinUCB, by contrast, uses the
deterministic optimism bonus `x · theta + alpha * sqrt(x^T A_inv x)`.

The practical difference: LinTs tends to be more aggressive at exploration
early on, because each decision draws a fresh sample from the posterior and
the sampling noise is itself a form of exploration. LinUCB's optimism bonus
decays monotonically as more observations arrive, which can cause it to
under-explore on non-stationary rewards. LinTs is generally favored when:

- The reward distribution shifts over time and you need exploration to stay
  active throughout the capsule's life.
- The feature vectors are moderately high-dimensional (say, 10–30 features)
  and LinUCB's UCB bonus has high variance due to matrix conditioning.

LinTs is less favored when the reward distribution is stable and low-noise —
in that setting LinUCB's deterministic optimism converges faster to the
correct arm. The meta-bandit resolves this automatically: both candidates run
in parallel and the meta-bandit converges to whichever accumulates higher
rolling reward on your traffic. You do not pick.

LinTs is auto-enrolled in Phase G for all feature-context capsules. No
configuration is required or available. If the Cholesky factorization fails
due to numerical drift, `lin_ts_score` falls back to the posterior-mean
estimate `x · theta` — still valid, temporarily non-Thompson.

### Continuous action space

The `actionSpace` field in `learning.json` (defined in
`Lang/src/learning.rs::ActionSpace`) allows a capsule's K options to be
treated as evenly-spaced buckets over a continuous range rather than as
distinct discrete choices.

Use case: you are tuning a pricing threshold, fraud score cutoff, timeout
value, or retry delay — something with a natural numeric range where the
bandit should learn which part of the range to favor, and where the
"chosen option" is more naturally expressed as a value (e.g., `23.5` rather
than "option index 4 of 10").

When continuous action space is enabled, the `/decide` response carries a
`chosenAction` field alongside the usual `chosen_option` index. `chosenAction`
is the midpoint of the chosen bucket. Your service applies the numeric value
directly without a secondary lookup.

Sample `learning.json` enabling 10-bucket continuous action over [0, 100]:

```json
{
  "actionSpace": {
    "type": "continuous",
    "range": [0, 100],
    "buckets": 10
  }
}
```

The ten buckets are [0,10), [10,20), ..., [90,100]. The midpoints are 5, 15,
25, ..., 95. If the bandit picks bucket index 3, `chosenAction` is `35.0`.
Your capsule YAML must declare exactly 10 options (matching `buckets`):

```yaml
options:
  - bucket_0
  - bucket_1
  - bucket_2
  - bucket_3
  - bucket_4
  - bucket_5
  - bucket_6
  - bucket_7
  - bucket_8
  - bucket_9
```

The bandit learns over these options exactly as in the discrete case; the
continuous framing is a presentation layer. Feedback is sent against the
`decisionId` as usual, with a scalar `reward` reflecting the outcome
(conversion, latency improvement, revenue delta, etc.).

### Multi-objective rewards

Multi-objective reward feedback uses the `components` form of `POST
/feedback`. This requires a `reward_spec.json` installed at
`PUT /tenants/{t}/jobs/{j}/capsules/{c}/reward_spec` that names the
components, their weights, and normalization ranges.

```bash
# Send feedback with named components
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/tenants/acme/jobs/routing/capsules/router/feedback \
  -d '{
    "decisionId": "dec_e1f2a3b4c5d60718",
    "components": {"quality": 0.9, "latency_ms": 1200}
  }'
```

The server reduces components to a scalar via the installed `reward_spec`.
To read per-component Q estimates, call `GET /memory` and inspect the per-
context bucket structure. The Pareto config (`pareto.enabled`, `pareto
.objectives`) tracks which component names constitute the multi-objective
front; when enabled, `/memory` carries a `paretoFront` sub-structure per
strategy node alongside the usual per-option weights.

Note: the `pareto` block in `learning.json` is parsed (see `from_json` in
`learning.rs`) but the Pareto front computation is still being integrated
into the decide path. Reading per-component reward accumulation via `/memory`
works today; the full Pareto-ranked option selection is foundation-only.
See [Section 7](#7-known-not-yet-wired).

### Hierarchical bandits (foundation only)

The data model for hierarchical decision trees lives in
`Lang/src/hierarchical.rs`. The shape allows a capsule's option set to be
declared as a recursive tree of sub-bandits, where each non-leaf node is
itself a bandit and each leaf is a terminal action. The module exposes:
`HierarchicalSpec::from_json`, `validate`, `enumerate_paths`,
`resolve_path`, `state_keys_for_path`, and `propagate_reward`.

The runtime wire-up in `server.rs` — parsing a hierarchical spec at install
time, running multi-level decisions at `/decide`, and routing feedback to
each level via `propagate_reward` — is the next milestone. Operators can
read the schema docs in `hierarchical.rs` now. Do not author capsules using
the hierarchical YAML form yet: `syntra author` will reject them until the
runtime catches up. See [Section 7](#7-known-not-yet-wired).

### Time-series features (foundation only)

`Lang/src/feature_schema.rs` gained a fourth `FeatureType` variant:
`TimeSeries { window_size, aggregations }`. A time-series feature maintains
a rolling window of observations per capsule and collapses the window into
one float per declared aggregation (`mean`, `max`, `min`, `p50`, `p95`,
`slope`) at encode time.

The schema is fully parsed and validated. The runtime wire-up — maintaining
the per-capsule `HashMap<String, TimeSeriesWindow>`, pushing observations at
the `/decide` boundary, and persisting window state in `memory.json` — is
the next milestone. Until then, capsules that declare a `time_series` feature
in their `contextSpec` will have the feature encoded as zeros at every
request (the graceful degradation path). The validation constraints are
enforced: `p95` requires `window_size >= 5`, `slope` requires
`window_size >= 2`, at least one aggregation must be declared. See
[Section 7](#7-known-not-yet-wired).

### Action embeddings (foundation only)

`Lang/src/linucb.rs` contains `LinUcbSharedState` — a shared-parameter
variant of LinUCB where the `A` matrix is shared across options and the
feature vector for each option is the concatenation of the request context
and the action embedding. This enables generalization across actions
(e.g., an LLM model not seen during training can be scored via its embedding
rather than starting from a cold prior).

The runtime integration in `server.rs` — registering action embeddings per
capsule, using `LinUcbSharedState` in the meta-bandit candidate's decide
path — is the next milestone. See [Section 7](#7-known-not-yet-wired).

---

## 5. Multi-Decision Capsules and Batched Feedback

### 5C Multi-AdaptiveChoice: per-node meta-bandit decisions

Capsules with more than one `AdaptiveChoice` node now get independent
meta-bandit selection per node. Previously (the Phase F known limitation),
only `decisions[0]` had a `candidateId` field; trailing `AdaptiveChoice`
nodes ran on uniform weights regardless of feedback.

After the 5C fix, each entry in `decisions[]` carries its own `candidateId`
and `weights`, reflecting the meta-bandit's selection for that specific node.
The `decisionId` returned by `/decide` is still a single ID that covers the
full graph execution.

When posting feedback for a multi-decision capsule, target a specific node's
decision via `decisionIndex`:

```bash
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/tenants/acme/jobs/routing/capsules/multi-router/feedback \
  -d '{
    "decisionId": "dec_e1f2a3b4c5d60718",
    "decisionIndex": 1,
    "reward": 0.72
  }'
```

`decisionIndex` is 0-based and indexes into the `decisions[]` array from the
corresponding `/decide` response. Feedback without `decisionIndex` (or with
`decisionIndex: 0`) targets the first decision node, which preserves
backward compatibility.

Each node has its own meta-bandit state in `/memory` under
`strategies[nodeId]`. Inspect them independently:

```bash
curl -s -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/tenants/acme/jobs/routing/capsules/multi-router/memory \
  | jq '.strategies | to_entries[] | {nodeId: .key, candidates: (.value.metaBandit.candidates // []) | map({id, trials})}'
```

### 2B Batched feedback

`POST /tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback/batch` accepts
up to 10,000 feedback events in a single request under one rate-limit hit and
one per-capsule lock acquisition.

Sample request with three events:

```bash
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/tenants/acme/jobs/routing/capsules/router/feedback/batch \
  -d '{
    "events": [
      {"decisionId": "dec_a1b2c3d4e5f60718", "reward": 0.90},
      {"decisionId": "dec_b2c3d4e5f6071819", "reward": 0.45},
      {"decisionId": "dec_c3d4e5f607181920", "reward": -0.10}
    ]
  }'
```

Response:

```json
{
  "ok": true,
  "total": 3,
  "okCount": 3,
  "errCount": 0,
  "results": [
    {"ok": true, "status": 200, "decisionId": "dec_a1b2c3d4e5f60718"},
    {"ok": true, "status": 200, "decisionId": "dec_b2c3d4e5f6071819"},
    {"ok": true, "status": 200, "decisionId": "dec_c3d4e5f607181920"}
  ]
}
```

Per-event failure does not abort the batch. `ok` at the top level is `true`
only when `errCount == 0`. A partial result (`errCount > 0`) returns HTTP
200 with `"ok": false`. To diagnose a failed event, re-post it individually
via `POST /feedback` (single) — the batch response does not include per-event
error bodies due to the request/response model.

Practical sizing: 10,000 events per batch is the server-side limit. Batches
of 100–1,000 events are typical for delayed-outcome workloads (e.g., sending
24 hours of chargeback outcomes in a morning cron job). Larger batches hold
the per-capsule mutex for longer; at very high feedback rates (> 5,000 events
per minute sustained), prefer smaller batches of 500–1,000 to keep the mutex
available for concurrent `/decide` calls.

---

## 6. Tooling

Four tools shipped alongside the Phase G and H algorithmic work. Each lives
in its own `examples/` subdirectory and is installable as a standalone Python
package.

**`syntra-export` (`examples/export-tool/`).**
Snapshots a capsule's learned state to a portable JSON file. The output
contains `memory.json`, `learning.json`, `warmup.json`, and the graph hash —
enough to reconstruct the bandit's current beliefs offline or transfer state
to another appliance manually. Usage:

```bash
pip install -e examples/export-tool/
syntra-export \
  --url http://localhost:8787 \
  --token $LYCAN_ADMIN_KEY \
  --tenant acme --job routing --capsule router \
  --out router-snapshot-$(date +%Y%m%d).json
```

**`syntra-ope` (`examples/offline-eval/`).**
Offline policy evaluation against logged decision data. Given a
`decision.jsonl` log (exported from the store volume) and a target policy
(a `learning.json` configuration), `syntra-ope` estimates what the target
policy's reward would have been via inverse propensity scoring. Use this
before switching a capsule's `contextSpec` or enabling a new feature — verify
that the new policy is better on historical data before committing.

```bash
pip install -e examples/offline-eval/
syntra-ope \
  --decisions /var/lib/syntra/tenants/acme/jobs/routing/capsules/router/decision.jsonl \
  --feedback  /var/lib/syntra/tenants/acme/jobs/routing/capsules/router/feedback.jsonl \
  --policy new-learning.json
```

**`syntra-ab` (`examples/ab-harness/`).**
A/B comparison harness. Runs two capsule configurations in parallel against
the same traffic replay and reports per-arm reward statistics, p-values, and
the trial count at which one arm achieved statistical dominance. Useful for
comparing `contextSpec.type = "discrete"` vs `"features"`, or comparing
`refusal` enabled vs disabled, before making the change in production.

**`bench/` (`examples/bench/`).**
Latency and throughput benchmarking for the `/decide` and `/feedback`
endpoints. Reports p50, p99, and p999 latency distributions alongside
sustained throughput in req/sec. Run against a local appliance with a
representative capsule before capacity planning:

```bash
pip install -e examples/bench/    # (if a setup.py is present)
python examples/bench/bench.py \
  --url http://localhost:8787 \
  --token $LYCAN_ADMIN_KEY \
  --tenant acme --job routing --capsule router \
  --duration 60 --concurrency 8
```

**Domain packs.**
Three new domain packs ship as opinionated Python integrations:

- `examples/fraud-tuning/` (`syntra-fraud`) — threshold tuning for fraud or
  risk scoring. Pre-baked capsule YAML, reward spec, and integration client
  for workloads with delayed chargeback outcomes.
- `examples/queue-selection/` (`syntra-queue`) — queue or routing selection
  with per-context latency features and multi-component rewards (latency_ms,
  queue_depth).
- `examples/llm-routing/` (`syntra-llm`) — LLM model routing with quality,
  latency, and cost components. Designed for three-arm decisions
  (cheap/fast, balanced, accurate) with a feature-context spec.

Each pack ships with example scripts, a setup capsule, and a test suite.

---

## 7. Known Not-Yet-Wired

The following features are schema-complete in the source but lack runtime
wire-up in `server.rs`. They appear in `learning.rs`, `feature_schema.rs`,
`hierarchical.rs`, and `linucb.rs` respectively but the server does not yet
call the relevant code paths at request time.

**Important for authoring:** capsules that use the hierarchical YAML syntax
(nested `sub_capsule` blocks) will be rejected by `syntra author` with a
validation error until the server-side integration lands. Do not author
production capsules against these features yet. Capsules using only the
existing discrete or feature-context spec are unaffected.

### Hierarchical bandits

- **Status:** schema and credit-assignment logic exist in
  `Lang/src/hierarchical.rs`. The public integration surface
  (`HierarchicalSpec::from_json`, `validate`, `enumerate_paths`,
  `resolve_path`, `state_keys_for_path`, `propagate_reward`) is documented
  and tested in the module.
- **What is missing:** `server.rs` does not yet parse a hierarchical spec at
  capsule install time, does not call `resolve_path` during `/decide`, and
  does not call `propagate_reward` during `/feedback`.
- **Next milestone:** server integration. Until then, any capsule YAML that
  includes `sub_capsule` keys will be compiled by `syntra author` but the
  resulting `.lyc` will be executed as if the capsule were flat.

### Time-series features

- **Status:** `FeatureType::TimeSeries` is parsed and validated in
  `Lang/src/feature_schema.rs`. The `TimeSeriesWindow` type (push, aggregate,
  serialize, deserialize) is complete and tested. `ContextSpec::encode_with_windows`
  correctly consumes window state when it is provided.
- **What is missing:** the server does not yet maintain per-capsule
  `HashMap<String, TimeSeriesWindow>` state, does not push observations onto
  windows at the `/decide` boundary, and does not persist window state in
  `memory.json`.
- **Graceful degradation in the interim:** a capsule that declares a
  `time_series` feature will encode that feature as zeros on every request
  (the `encode_with_windows` fallback path when no window is provided). This
  will not cause errors, but it means the declared feature contributes nothing
  to the LinUCB weight vector until the runtime wire-up lands.
- **Operator action:** defer adoption of `time_series` features in
  `contextSpec` until the server integration ships.

### Action embeddings / shared-state LinUCB

- **Status:** `LinUcbSharedState` exists in `Lang/src/linucb.rs`. The shared
  design matrix approach (one `A` matrix across all options, option scored by
  context-action concatenated feature vector) is implemented and tested.
- **What is missing:** the server does not yet register action embeddings per
  capsule, does not use `LinUcbSharedState` in the meta-bandit candidate's
  decide path, and does not provide an API endpoint to POST action embeddings.
- **Next milestone:** server integration, including a new endpoint to
  register per-action embedding vectors.

### Pareto-ranked multi-objective option selection

- **Status:** the `ParetoConfig` is parsed from `learning.json` (`pareto
  .enabled`, `pareto.objectives`). The schema is wired in `from_json` and
  `to_json`.
- **What is missing:** the Pareto front computation at decide time (ranking
  options by non-dominated reward component estimates rather than by scalar
  reward) is not yet called from `do_decide`.
- **Operator action:** you can set `pareto.enabled = true` and
  `pareto.objectives` in `learning.json` without error, but the capsule will
  continue selecting options by scalar reward until the integration lands.

Operators who want to track the delivery schedule for these items should
watch the `Lang/src/` source tree and the `CHANGELOG.md`. When the server
integration for a feature ships, the version header will note it explicitly.

---

*Apache-2.0. For the full API surface see [`api.md`](api.md). For the
operator playbook see [`operating.md`](operating.md). For incident response
see [`runbook.md`](runbook.md).*
