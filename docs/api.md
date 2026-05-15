# Syntra — API Reference

Base URL: `http://localhost:8787`

All routes except `/health` and `/admin` require `Authorization: Bearer <admin-key>`.

## Health

```
GET /health
```

Response: `{"ok":true,"service":"Syntra"}`

No auth required.

## Admin Console

```
GET /admin
```

Serves the browser-based admin console. No auth required for the page shell; all data calls within the console use the Bearer token.

## Tenants

```
GET /tenants
```

Response:
```json
{"tenants":["acme","demo"]}
```

## Jobs

```
POST /tenants/:tenant/jobs
```

Body:
```json
{"id":"routing","name":"Request Routing","description":"Learns timeouts","metadata":{}}
```

Response:
```json
{"ok":true,"tenant":"acme","job":{"id":"routing","name":"Request Routing","createdAt":1778830000}}
```

```
GET /tenants/:tenant/jobs
```

Response:
```json
{"tenant":"acme","jobs":[{"id":"routing","name":"Request Routing","capsules":1}]}
```

```
GET /tenants/:tenant/jobs/:job
```

## Capsules

### Install

```
POST /tenants/:tenant/jobs/:job/capsules/:capsule/install
Content-Type: application/octet-stream
Body: raw .lyc binary
```

Response:
```json
{"ok":true,"tenant":"acme","job":"routing","capsule":"router","hash":"abc123..."}
```

### Decide

```
POST /tenants/:tenant/jobs/:job/capsules/:capsule/decide
```

Body:
```json
{
  "contextKey": "rush_hour",
  "input": {
    "latencies": [42, 50, 88],
    "region": "eu-west-1"
  }
}
```

Response:
```json
{
  "ok": true,
  "tenant": "acme",
  "capsule": "router",
  "decisionId": "dec_abc123",
  "contextKey": "rush_hour",
  "algorithm": "simpleWeighted",
  "learned": false,
  "decisions": [
    {
      "node_id": 70,
      "chosen_option": 1,
      "confidence": 0.8345,
      "objective": "general",
      "weights": [0.1557, 0.8345, 0.0098],
      "activations": 42
    }
  ],
  "result": "...",
  "stdout": ["line 1", "line 2"]
}
```

Query parameter `?learn=true` enables weight mutation on decide (default: read-only).

### Feedback

```
POST /tenants/:tenant/jobs/:job/capsules/:capsule/feedback
```

By decisionId:
```json
{"decisionId":"dec_abc123","reward":1.0,"contextKey":"rush_hour"}
```

By explicit node:
```json
{"strategyId":70,"option":1,"reward":1.0,"contextKey":"rush_hour"}
```

By outcome (with reward policy):
```json
{"strategyId":70,"option":1,"contextKey":"rush_hour","outcome":{"success":true,"latencyMs":123,"cost":0.02}}
```

Response:
```json
{"ok":true,"nodeId":70,"option":1,"reward":1.0,"before":[0.33,0.33,0.33],"after":[0.31,0.38,0.31],"contextKey":"rush_hour"}
```

### Report

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/report
```

Returns strategy weights, option stats, graph hash.

### Inspect

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/inspect
```

Returns node count, edge count, journal entries, state size.

### Contexts

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/contexts
```

Returns all known context keys with weights and stats.

### Memory

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/memory
```

Returns the full memory sidecar (per-context weights and stats).

### Learning Config

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/learning
PUT /tenants/:tenant/jobs/:job/capsules/:capsule/learning
```

Body (PUT):
```json
{
  "algorithm": "epsilonGreedy",
  "epsilon": 0.15,
  "decay": {"enabled": true, "halfLifeSeconds": 604800},
  "safety": {"maxWeightDeltaPerFeedback": 0.15, "minExploration": 0.02, "freezeLearning": false},
  "rewardPolicy": {"success": 1.0, "latencyMs": -0.002, "cost": -0.5}
}
```

Supported algorithms: `simpleWeighted`, `epsilonGreedy`, `ucb1`.

### Policy

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/policy
PUT /tenants/:tenant/jobs/:job/capsules/:capsule/policy
```

### Logs

```
GET /tenants/:tenant/jobs/:job/capsules/:capsule/decisions
GET /tenants/:tenant/jobs/:job/capsules/:capsule/audits
GET /tenants/:tenant/jobs/:job/capsules/:capsule/evolution
GET /tenants/:tenant/jobs/:job/capsules/:capsule/snapshots
```

### Evolution (proposal mode only)

```
POST /tenants/:tenant/jobs/:job/capsules/:capsule/evolve
```

Body:
```json
{"proposal":{"name":"FastSolver","source":"...","insert_into_strategy":42,"expected_output":"55"},"minImprovement":0.05,"dryRun":false}
```

Agent-command mode is not available over HTTP.

### Delete (data erasure)

```
DELETE /tenants/:tenant/jobs/:job/capsules/:capsule
DELETE /tenants/:tenant/jobs/:job/capsules/:capsule/logs
DELETE /tenants/:tenant/jobs/:job
DELETE /tenants/:tenant
```

## Compatibility routes

Old routes without `/jobs/:job` map to `job="default"`:

```
POST /tenants/:tenant/capsules/:capsule/decide
POST /tenants/:tenant/capsules/:capsule/feedback
GET  /tenants/:tenant/capsules/:capsule/report
```

## Authentication

All routes except `/health` and `/admin` require:
```
Authorization: Bearer <LYCAN_ADMIN_KEY>
```

Failed auth returns `401` and is logged with remote address.

## Body limits

Maximum request body: 4 MB. Oversized requests return `413`.
