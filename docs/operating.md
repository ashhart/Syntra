# Operating Syntra

A running Syntra is one process, `syntra serve`, and one store directory.
The process holds the models in memory and you can replace it at any time;
the store holds everything that matters. This page covers what is in the store
and how it is written, restarts, backups, metrics, tracing and logs, access
and rate limits, specs kept in files, and what to check when something goes
wrong.
[deployment.md](deployment.md) covers Docker, Kubernetes and TLS.

## The store

```text
<store>/
  store.json                  format marker (2); a v1 store is refused
  syntra.db                   SQLite: decisions, rewards, model snapshots, audit trail
  syntra.db-wal, syntra.db-shm  SQLite's write-ahead log, part of the database while it runs
  tokens.json                 scoped tokens, stored as SHA-256 hashes
  server.pid                  pid of the running server
  tenants/<tenant>/jobs/<job>/
    job.json
    capsules/<capsule>/
      spec.json               the decision spec
      policy.json             execution policy for the feature program (deny-all when created)
      current.lyc             feature program, if one is installed
      manifest.json           its install record
      data/                   the program's file sandbox, created when file access is allowed
      deferred.json           Personalizer events awaiting activation, saved at a graceful
                              shutdown and removed when the capsule loads again
```

Point `--store` at a directory on a local disk (the default is
`./syntra-store`). `syntra.db` is SQLite in WAL mode with one writer, so a
store belongs to one server process; do not share it between processes or
put it on a network file system.

## Writes, durability and restarts

`/decide` and `/reward` never wait on disk. Each decision and reward goes
onto one in-memory queue, and a writer thread commits the queue to
`syntra.db` in transactions of at most 512 records or 2 ms, decisions
before rewards. Consequences:

- A crash (kill -9, power loss) can lose the records acknowledged in the
  last few milliseconds. Pass `"durable": true` on a decide or reward to get
  the answer only after its record is committed (or a 503 if the commit is
  not confirmed within 5 s).
- Reads (`GET .../decisions`, evaluation) wait for the queue first, so they
  include everything acknowledged before them.
- If the queue fills (65,536 records waiting, which means the disk is not
  keeping up), decides and rewards answer 503 with `Retry-After: 1` rather
  than serve a decision whose record would be dropped. Retrying a reward
  with the same idempotency key is safe.

Models are event-sourced. A background thread snapshots a capsule's model
after every `snapshotEvery` updates (default 1000) and the server keeps the
three newest snapshots per capsule. After a restart, the server rebuilds a
capsule's model on first use from its latest snapshot plus the rewards
logged after it, which reproduces the model exactly. It takes a snapshot
only after committing the rewards the snapshot covers, so a snapshot never
runs ahead of the log.

On SIGTERM or Ctrl-C (`syntra stop` sends SIGTERM to the syntra process on
the port), the server stops accepting connections, waits up to 30 s for
requests in flight, commits the queue, snapshots every model with unsaved
updates, saves Personalizer events awaiting activation and removes
`server.pid`. Restarting is safe at any time; clients
see refused connections while the process is down, and SDK deciders keep
deciding on their last model.

SDKs that decide in-process upload their decisions, and the server
replays each one against the model it names before storing it. It refuses
one that does not replay (`upload_rejected` in the audit trail, counted in
`syntra_uploaded_decisions_rejected_total`). The server keeps the models it
published in memory (the 32 newest), so a restart, or an outage longer than
that window, retires them. A decision naming a model the server no longer
has cannot be replayed; if it is self-consistent, the server stores it with
mode `unverified` and audits `upload_unverified`. Its rewards still train
the model, so nothing learned during an outage is lost, but off-policy
evaluation leaves unverified decisions out and reports how many
(`rowsUnverified`). Deciders fetch the current model on their next sync.

## Backups, restores and checks

```bash
syntra backup --store ./syntra-store --out ./backups/2026-09-23
syntra restore --from ./backups/2026-09-23 --into ./syntra-store
syntra doctor --store ./syntra-store
```

- `backup` writes a new directory: `syntra.db` copied with SQLite's
  `VACUUM INTO` (a consistent copy, safe while the server runs), the other
  store files under `files/`, and `manifest.json` with every file's
  SHA-256.
- `restore` checks every checksum before touching the target, stages the
  copy next to it and swaps it in with a rename. It moves an existing store
  aside to `<store>.pre-restore-<ms>` rather than deleting it. It refuses a store
  whose `server.pid` names a running process unless you pass `--force`;
  stop the server first.
- `doctor` checks a store without writing to it (not even SQLite's WAL
  files): the files, specs, policies, programs and the event store. It
  prints one JSON line per finding and a summary, and exits 0 when there
  are none, 1 when there are findings and 2 when the directory is not a
  readable store.

Restoring rolls the learned models back to the backup. SDK deciders pick
up the restored model on their next sync; their queued decisions made on
newer models are stored unverified.

## Metrics, tracing and logs

`GET /metrics` serves Prometheus text. It names every tenant, job and
capsule, so it needs an admin credential (the admin key or an `admin`
token) unless the server runs with `--metrics-public` or
`SYNTRA_METRICS_PUBLIC=1`. A Prometheus scrape job:

```yaml
scrape_configs:
  - job_name: syntra
    static_configs:
      - targets: ["syntra.internal:8787"]
    authorization:
      credentials_file: /etc/prometheus/syntra-admin-key
```

| Metric | Type | What it tells you |
|---|---|---|
| `syntra_requests_total{route, status}` | counter | Requests by route label (`capsule.decide`, `capsule.reward`, `admin.tokens.issue`, ...) and status. |
| `syntra_decide_seconds` | histogram | Server-side time to choose and queue one decision (5 µs to 100 ms buckets). |
| `syntra_decisions_committed_total`, `syntra_rewards_committed_total` | counter | Records committed to `syntra.db`. |
| `syntra_decision_batches_total` | counter | Write transactions. |
| `syntra_decision_log_backlog` | gauge | Records queued, not yet committed. |
| `syntra_decisions_rejected_backlog_total` | counter | Requests refused with 503 because the queue was full. |
| `syntra_events_lost_total` | counter | Records dropped after repeated commit failures. Should stay 0. |
| `syntra_rewards_refused_after_apply_total` | counter | Rewards a model learned from but the store refused. Should stay 0. |
| `syntra_uploaded_decisions_accepted_total`, `syntra_uploaded_decisions_rejected_total` | counter | SDK uploads that replayed, and those refused. |
| `syntra_default_rewards_total` | counter | Default rewards applied after `reward.waitSeconds`. |
| `syntra_model_version{tenant, job, capsule}` | gauge | Updates applied to each loaded capsule's model. |
| `syntra_otel_spans_exported_total`, `syntra_otel_spans_dropped_total` | counter | Trace spans the collector accepted, and those dropped (queue full, export failed, rejected). |
| `syntra_uptime_seconds` | gauge | Seconds since start. |

Alert on `syntra_events_lost_total` or
`syntra_rewards_refused_after_apply_total` above 0, on a growing
`syntra_decision_log_backlog` or `syntra_decisions_rejected_backlog_total`
(the disk is too slow), and on a rising rejected-upload rate after a
deploy (clients deciding on retired models).

### Tracing

The server can send one OpenTelemetry span per request to a collector, as
OTLP over HTTP with JSON. It is off until you set an endpoint (the admin
key comes from `SYNTRA_ADMIN_KEY` as usual):

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318 OTEL_SERVICE_NAME=syntra \
  syntra serve --store ./syntra-store
```

`OTEL_EXPORTER_OTLP_ENDPOINT` is a base URL (the server appends
`/v1/traces`); `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is the full URL.
`OTEL_EXPORTER_OTLP_HEADERS` (`key=value,...`) carries the collector's
credentials, and the other standard variables work too: `..._TIMEOUT`,
`..._COMPRESSION` (`gzip`), `OTEL_RESOURCE_ATTRIBUTES`,
`OTEL_TRACES_SAMPLER` (default `parentbased_always_on`) with
`OTEL_TRACES_SAMPLER_ARG`, and `OTEL_BSP_*` for batching. gRPC is not
supported. `OTEL_SDK_DISABLED=true` turns tracing off.

A W3C `traceparent` header makes the server's span a child of the
caller's. Spans carry the route template (never a raw path), the capsule
and the request id, and on decide and reward calls the decision id,
action, probability, model version and reward, so a trace leads to the
logged decision. `/health`, `/ready` and `/metrics` are not traced.
Exporting happens on a background thread; when its queue is full the
server drops spans rather than slow a request.

### Probes and logs

`/health` answers while the process serves requests; `/ready` also writes
and removes a probe file in the store and answers 503 when it cannot.
Neither needs a credential. `syntra health` asks a running server
(default `127.0.0.1:8787`, or `--addr host:port`) and exits 1 if it does
not answer.

Logs are JSON lines on stderr, at `info` by default; set `RUST_LOG`
(`warn`, `debug`, `syntra=debug`) to change the level. Every response
carries an `x-request-id` header (yours, if you send `X-Request-Id`), and
the server logs authentication failures with the remote address, path and
request id.

## The audit trail

Every change to a capsule is an audit event in `syntra.db`, read with
`GET .../audits?limit=N` (newest N, oldest first): `capsule_created`,
`spec_updated`, `spec_replaced`, `spec_applied`, `mode_changed`,
`program_installed`, `program_removed`, `policy_updated`, `logs_purged`,
`spec_promoted`, `promotion_refused`, `upload_rejected`,
`upload_unverified`, `execution_denied` (a feature program failed or was
denied; the detail has the error and the request id) and
`capsule_deleted`. Deleting a capsule keeps its audit trail readable and
ends it with `capsule_deleted`. Deleting a job or a tenant also keeps the
trails of its capsules, but records no deletion event in them.

There is no automatic retention. `DELETE .../logs` erases a capsule's
decisions and rewards (the spec, program, policy and model stay, and
off-policy evaluation can no longer use that traffic); deleting a capsule,
job or tenant erases their files, decisions, rewards and model snapshots.

## Access

- **The admin key** (`--admin-key`, or `SYNTRA_ADMIN_KEY`; `LYCAN_ADMIN_KEY`
  is still read) is the operator credential. Without one the server does
  not start, unless `--dev-mode`, which serves every route
  unauthenticated and binds only loopback addresses
  (`--dev-mode-allow-remote` overrides that, for an isolated container
  behind another authentication layer). Prefer the environment variable to
  `--admin-key` outside a laptop, since other users of the machine can read
  command lines. To rotate the key, restart with the new one; tokens keep
  working.
- **Tokens** come from the operator, through `POST /v1/admin/tokens`:

  | Scope | Reaches |
  |---|---|
  | `{"kind": "admin"}` | everything, like the admin key |
  | `{"kind": "tenant_admin", "tenant": "acme"}` | every route of one tenant: capsules, specs, programs, policies, logs, evaluate, promote |
  | `{"kind": "read", "tenant": "acme", "job": "prod", "capsule": "router"}` | one capsule's data plane: decide, reward, SDK uploads, the model and the read routes; not evaluate |

  `ttlSeconds` sets an expiry. The response shows the raw token once;
  `tokens.json` keeps its SHA-256. `GET /v1/admin/tokens` lists tokens
  with `createdAt`, `expiresAt` and `lastUsedAt`; `DELETE
  /v1/admin/tokens/{hash}` revokes one at once. Only an admin credential
  (the admin key or an `admin` token) may set a policy's
  `deny_private_networks` to false; a `tenant_admin` token gets 403.
- **Rate limits** apply per credential, as a token bucket of 50,000
  requests a second with a burst of 100,000 by default
  (`SYNTRA_RATE_LIMIT_RPS` and `SYNTRA_RATE_LIMIT_BURST` change them). Over the limit a
  request gets 429 with `Retry-After` and `retryAfterSeconds`. Dev mode is
  not limited. Separately, at most two evaluations (`POST .../evaluate`
  or `.../promote`) run at once, server-wide; a third gets 429.

The admin console at `/admin` is a static page that calls the API with
the key you enter (kept in the tab's session storage). It browses
capsules, decisions, specs and audit trails, runs evaluations and
promotions, and manages tokens with an admin key.

## Specs from files

`syntra serve --specs <dir>` (or `SYNTRA_SPECS_DIR`) applies capsule specs
kept in files, for configuration in version control. Every `.yaml`, `.yml`
or `.json` file in the directory holds one or more documents:

```yaml
tenant: acme
job: prod
capsule: router
spec:
  actions:
    - {id: small, features: {cost: 0.1}}
    - {id: large, features: {cost: 1.0}}
  reward: {default: 0, waitSeconds: 600}
```

At startup each document replaces the capsule's stored spec (fields it
leaves out take their defaults, as with `PUT .../spec?replace=true`), but
only when it differs, so a restart with unchanged files changes nothing.
The audit trail records a new capsule as `capsule_created` and a changed
one as `spec_applied`, both with the file name. An invalid file stops the
server before it applies anything. The file wins. A change made to that
capsule through the API or by `promote` goes away at the next restart
unless you copy it into the file.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `ERROR: no admin key set` at startup | Set `SYNTRA_ADMIN_KEY` or pass `--admin-key`. |
| `cannot bind to ...: port already in use` | Another process holds the port; `syntra status` or `lsof -i :8787` finds it. |
| `... is a v1 store ... which this version cannot read` | v1 logs have no propensities. Serve a new store and recreate each capsule with `PUT .../spec`. |
| `error: <file>: ...` with `--specs` | An invalid spec file; the message names the file, the capsule and the field. |
| 401 `unauthorized` | Missing, unknown, expired or revoked credential. Send `Authorization: Bearer <key>` or `Ocp-Apim-Subscription-Key: <key>`. |
| 403 `forbidden: scope does not allow this action` | The token's scope does not cover the route or the capsule. |
| 400 on decide | An unknown field, no eligible action, an unknown action id, or `baselineAction` missing in `baselineExplore` mode. The message says which. |
| 409 on decide | The `eventId` was used before with a different body. |
| 500 `feature program failed: ...` | The program errored, hit its time budget or was denied by its policy; see the `execution_denied` audit event. `GET` on the capsule shows `policyError` if the stored policy is invalid (the program then runs deny-all). |
| 503 with `Retry-After` | The write queue is full, or a `durable` commit was not confirmed in 5 s. Check disk latency and `syntra_decision_log_backlog`. |
| 429 | The credential's rate limit, or two evaluations already running. |
| Rejected SDK uploads | A decision that does not replay against the model it names, a reused `decisionId`, or a timestamp more than 7 days old or 5 minutes ahead. The `upload_rejected` audit event has the reason. Decisions naming a model the server no longer has (after a restart, a restore, or more than 32 publications) are stored unverified instead. |

## Limits

One server per store: SQLite, one writer, no replication (a Postgres
backend and multiple decide nodes are planned). Feature programs run in
the server process under a policy-enforced sandbox, not behind an OS
boundary, and `max_memory_bytes` in a policy is not enforced. See
[SECURITY.md](../SECURITY.md) for the security model and its known gaps.
