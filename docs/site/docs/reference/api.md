# API reference

The full endpoint surface for the Syntra HTTP server. Every endpoint
below is verified to exist in the source (`Lycan/src/server/`). For the
platform overview, see the [home page](../index.md); for what shipped
in each phase, see [`CHANGELOG.md`](https://github.com/ashhart/Syntra/blob/main/CHANGELOG.md)
in the repository.

Base URL is `http://localhost:8787` by default. All endpoints except
`GET /health` and the static `GET /admin` page require an admin bearer
token:

```
Authorization: Bearer $LYCAN_ADMIN_KEY
```

Failed auth returns `401` and is logged with the remote address.
Request bodies are capped at 4 MB; oversized requests return `413`.
Capsule-mutating routes (`install`, decide-with-learn, `feedback`,
`evolve`, `policy` PUT, `learning` PUT, `reward_spec` PUT, `DELETE`)
take a per-capsule mutex; read paths do not.

The capsule path prefix throughout is:

```
/tenants/{tenant}/jobs/{job}/capsules/{capsule}
```

Older clients can use the legacy compatibility prefix
`/tenants/{tenant}/capsules/{capsule}` — it is rewritten to
`job = "default"` server-side and is preserved for v0.2-era
integrations.

## Health

```
GET /health
```

Liveness probe. No auth required.

```json
{"ok": true, "service": "Syntra"}
```

## Tenants and jobs

`tenant / job / capsule` is the data model. A tenant is an organization
or environment; a job is an independent learning context (same capsule
binary, separate memory and logs); a capsule is the installed graph
plus its sidecars.

```
GET    /tenants
POST   /tenants/{tenant}/jobs
GET    /tenants/{tenant}/jobs
GET    /tenants/{tenant}/jobs/{job}
GET    /tenants/{tenant}/jobs/{job}/capsules
DELETE /tenants/{tenant}
DELETE /tenants/{tenant}/jobs/{job}
```

`POST /tenants/{tenant}/jobs` body:

```json
{
  "id": "routing",
  "name": "Request Routing",
  "description": "Per-tenant retry policy",
  "metadata": {}
}
```

Only `id` is required. Returns `409` if the job already exists.
`GET /tenants` returns `{"tenants": ["acme", "demo"]}`.
`GET /tenants/{tenant}/jobs` returns each job with a `capsules` count.

`DELETE /tenants/{tenant}` and `DELETE /tenants/{tenant}/jobs/{job}`
wipe all nested state (capsules, memory, logs, snapshots) and exist
for GDPR Article 17 compliance.

## Capsule install

```
POST /tenants/{tenant}/jobs/{job}/capsules/{capsule}/install
Content-Type: application/octet-stream
Body: raw .lyc graph binary (must begin with the magic header LYCN)
```

The `.lyc` file is the output of `syntra author my-capsule.yaml`.
Response:

```json
{
  "ok": true,
  "tenant": "acme",
  "job": "routing",
  "capsule": "router",
  "hash": "a1b2c3..."
}
```

`hash` is the SHA-256 of the uploaded bytes; the install event is
appended to `audit.jsonl` with this hash so you can correlate "which
graph was running between 09:00 and 11:00" against decision-log entries
from that window.

## Decide

```
POST /tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide
```

The body shape depends on the capsule's `contextSpec` (see the
Learning section below). For a discrete-context capsule:

```json
{"contextKey": "rush_hour"}
```

For a feature-context capsule:

```json
{
  "features": {
    "recent_failure_rate": 0.15,
    "p99_latency_ms": 1200,
    "hour": 3.0
  }
}
```

You can also pass an arbitrary `input` object alongside either form;
it is made available to the graph during execution and is logged with
the decision but does not affect option selection directly.

`?learn=true` enables in-band weight mutation on decide (rare; almost
all production callers use the default read-only mode and post weights
via `/feedback`). The default is read-only.

Response (Active capsule, refusal not triggered):

```json
{
  "ok": true,
  "tenant": "acme",
  "job": "routing",
  "capsule": "router",
  "decisionId": "dec_e1f2a3b4c5d60718",
  "contextKey": "rush_hour",
  "algorithm": "simpleWeighted",
  "learned": false,
  "warmup": {"state": "active", "algorithm": "Thompson", "reason": "ready"},
  "decisions": [
    {
      "node_id": 70,
      "chosen_option": 1,
      "confidence": 0.8345,
      "objective": "general",
      "weights": [0.1557, 0.8345, 0.0098],
      "activations": 42,
      "candidateId": "LinUcb"
    }
  ],
  "result": "...",
  "stdout": ["line 1", "line 2"],
  "oodScore": 0.12,
  "refused": false,
  "confidence": {
    "oodScore": 0.12,
    "intervalWidth": 0.18,
    "coverage": 0.95,
    "refused": false,
    "refusalReason": null
  }
}
```

The fields integration libraries care about are `decisionId` (passed
back in `/feedback`), `decisions[0].chosen_option` (the option to act
on), `refused` (if true, fall back to your default behaviour), and
`confidence` (for logging and adaptive throttling). The `candidateId`
field appears when the meta-bandit is the active selector and tells
you which of the seven candidates served this decision.

Response when refusal triggers (only possible in Active state with
`refusal.enabled = true`):

```json
{
  "ok": true,
  "decisionId": "dec_e1f2a3b4c5d60718",
  "contextKey": "rush_hour",
  "warmup": {"state": "active", "algorithm": "Thompson"},
  "decisions": [],
  "refused": true,
  "oodScore": 0.92,
  "confidence": {
    "oodScore": 0.92,
    "intervalWidth": 0.62,
    "coverage": 0.95,
    "refused": true,
    "refusalReason": "ood"
  }
}
```

`refusalReason` is one of `"ood"`, `"interval_too_wide"`, or
`"insufficient_calibration_data"`. During Warmup the capsule never
refuses — the bootstrap path needs unconditional data flow to
characterize the reward.

## Feedback

```
POST /tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback
```

Recommended form (decisionId + scalar reward):

```json
{"decisionId": "dec_e1f2a3b4c5d60718", "reward": 0.85}
```

DecisionId + reward components (the server reduces them to a scalar
using the installed `reward_spec.json`):

```json
{
  "decisionId": "dec_e1f2a3b4c5d60718",
  "components": {"quality": 0.85, "latency_ms": 1240, "cost_usd": 0.018}
}
```

You can also send `outcome` and let the on-disk `rewardPolicy` weight
it, or skip the decisionId entirely and send
`strategyId`/`option`/`contextKey` explicitly (advanced; bypasses the
decision-log lookup so refusal accounting and meta-bandit
context-binding are skipped — only use this if you have a good reason).

Response:

```json
{
  "ok": true,
  "nodeId": 70,
  "option": 1,
  "reward": 0.85,
  "before": [0.33, 0.33, 0.33],
  "after":  [0.31, 0.38, 0.31],
  "contextKey": "rush_hour"
}
```

Feedback against a `refused` decision is recorded but does not mutate
the bandit; an audit event `feedback_on_refused` is appended.

## Reports and learned state

```
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/report
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/memory
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/contexts
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/decisions
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/audits
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/evolution
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/snapshots
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/inspect
```

- `/report` — live graph view: each strategy node with its current
  weights, per-option tries / correct counts, average latency, SHA-256
  hash of the installed graph binary. Cheap; use it for dashboards.
- `/memory` — full `memory.json` sidecar: per-context buckets,
  meta-bandit state for each strategy node, candidate-context buckets
  (per algorithm, per context — this is where you go to see how each
  of the candidates is performing), per-context ADWIN detectors, and
  the discrete and feature OOD detectors. Schema version 7. Can be
  large.
- `/contexts` — one row per `(nodeId, contextKey)` with weights, total
  tries, and last-update timestamp. Useful for confirming requests are
  landing in the contexts you expect.
- `/decisions`, `/audits`, `/evolution` — the corresponding `.jsonl`
  log files as text. Append-only; the response is the full log. Use a
  Range request or tail it via the store volume for long histories.
- `/snapshots` — list of pre-mutation backups.
- `/inspect` — graph shape (node count, edge count, journal entries).

## Learning configuration

```
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/learning
PUT /tenants/{tenant}/jobs/{job}/capsules/{capsule}/learning
```

The body is the persisted `learning.json`. The schema is defined by
`LearningConfig::from_json` / `to_json` in `Lycan/src/learning.rs`; the
camelCase wire field names below match the parser exactly.

```json
{
  "algorithm": "thompsonSampling",
  "learningRate": 0.1,
  "decay": {
    "enabled": true,
    "halfLifeFeedbacks": 200,
    "halfLifeSeconds": 604800
  },
  "safety": {
    "maxWeightDeltaPerFeedback": 0.15,
    "minExploration": 0.02,
    "freezeLearning": false,
    "rewardClip": 2.0,
    "trimmedFraction": 0.0,
    "snapshotOnFeedback": true,
    "journalOnFeedback": true,
    "selectionMode": "greedy",
    "selectionEpsilon": 0.10,
    "optionStateForgetting": 0.999
  },
  "window": {"enabled": false, "size": 100},
  "changeDetection": {
    "enabled": false,
    "threshold": 5.0,
    "minDrift": 0.05,
    "explorationBoost": 0.25,
    "boostDuration": 50,
    "method": "pageHinkley",
    "surpriseKSigma": 2.5,
    "surpriseFractionThreshold": 0.30
  },
  "conformal": {"enabled": false, "coverage": 0.90, "calibrationSize": 100},
  "rewardPolicy": {"success": 1.0, "latencyMs": -0.002, "cost": -0.5},
  "contextSpec": {
    "type": "features",
    "features": [
      {"name": "recent_failure_rate", "type": {"kind": "continuous", "range": [0.0, 1.0]}},
      {"name": "p99_latency_ms",      "type": {"kind": "continuous", "range": [0.0, 5000.0]}},
      {"name": "tier",                "type": {"kind": "categorical", "values": ["free", "pro", "enterprise"]}},
      {"name": "hour",                "type": {"kind": "cyclic", "period": 24.0}}
    ]
  },
  "refusal": {
    "enabled": false,
    "coverage": 0.95,
    "maxIntervalWidth": 0.5,
    "oodThreshold": 0.8
  }
}
```

The two fields most production deployments configure are `contextSpec`
and `refusal`. `contextSpec.type` is either `"discrete"` (the default;
uses an opaque `contextKey` string) or `"features"` (which enables the
LinUCB candidate in the meta-bandit and accepts a `features` map at
`/decide`). Each feature declares a `name` and a `type.kind` of
`continuous`, `categorical`, or `cyclic`, with the kind-specific tail
(`range`, `values`, or `period`).

`refusal` is off by default. When `enabled` is true and the capsule is
Active, `/decide` returns a refused response when the OOD score
exceeds `oodThreshold` or the conformal prediction-interval width
exceeds `maxIntervalWidth` at the declared `coverage`.

Algorithm values are `simpleWeighted`, `epsilonGreedy`, `ucb1`,
`thompsonSampling`, `softmax`. These set the algorithm only when the
capsule is in Warmup or when the meta-bandit is disabled; once the
meta-bandit is running it overrides this and picks per-decision.

`GET` returns the canonicalized form (every default field is filled
in). `PUT` accepts a partial body and merges with defaults.

## Reward spec

```
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/reward_spec
PUT /tenants/{tenant}/jobs/{job}/capsules/{capsule}/reward_spec
```

`PUT` accepts the `reward_spec.json` emitted by `syntra author
--out-dir`. Installing one lets `/feedback` accept `components`
(`{"quality":0.85,...}`) and have the server reduce them to a scalar
via the named components, weights, and normalizers in the spec.
Without an installed spec, `/feedback` callers must send `reward` or
`outcome` directly.

## Policy

```
GET /tenants/{tenant}/jobs/{job}/capsules/{capsule}/policy
PUT /tenants/{tenant}/jobs/{job}/capsules/{capsule}/policy
```

Runtime capability policy for the capsule sandbox. Body is a JSON
object with boolean keys `allow_stdout`, `allow_stdin`,
`allow_file_read`, `allow_file_write`, `allow_network`, plus the
allow-list fields for file paths and HTTP hosts. Non-boolean values
for the boolean keys return `400`.

## Evolution (proposal mode)

```
POST /tenants/{tenant}/jobs/{job}/capsules/{capsule}/evolve
```

Submit a proposed graph mutation. Body:

```json
{
  "proposal": {
    "name": "FastSolver",
    "source": "...",
    "insert_into_strategy": 42,
    "expected_output": "55"
  },
  "minImprovement": 0.05,
  "dryRun": false
}
```

The proposal is evaluated against a small benchmark and accepted only
if the score improvement is above `minImprovement`. Accepted
proposals are appended to `evolution.jsonl`. The agent-command
(subprocess) mode of evolve is CLI-only and is intentionally not
exposed over HTTP.

## Delete (GDPR Article 17)

```
DELETE /tenants/{tenant}/jobs/{job}/capsules/{capsule}
DELETE /tenants/{tenant}/jobs/{job}/capsules/{capsule}/logs
DELETE /tenants/{tenant}/jobs/{job}
DELETE /tenants/{tenant}
```

`/logs` truncates the decision and feedback logs in place while
preserving the installed graph, learned memory, and audit history —
useful when an outside authority compels deletion of a specific
tenant's interaction history but the service must keep running. The
other three forms cascade and remove all data under the given path.

## Capabilities catalogue

```
GET /capabilities
```

Returns the JSON catalogue of host capabilities the runtime exposes to
capsules (file, HTTP, stdout, etc.). Useful when authoring capsule
policy files. No body parameters.

## Admin console

```
GET /admin
```

Serves the static login shell. The page itself is public; all data
fetches from the console use the Bearer token entered at login. No
memory of the key is persisted server-side.

## Memory backup

Snapshots of pre-mutation state are listed at `/snapshots` but Syntra
does not yet expose a single-call full-capsule backup over HTTP. For
now the backup pattern is to copy the store volume directly. A
first-class backup endpoint is tracked for Phase 1E.
