# Syntra v2 decision core

Status: design, 2026-09-23. Replaces the v1 learning stack (meta-bandit,
candidate portfolio, per-context buckets, warmup, in-graph weight learning on
the server path, ADWIN resets, weight-based "conformal" refusal, hash-keyed
LinUCB state).

## Goal

Learned decisions at the speed of a lookup, with the evidence to trust them:

- **Instant.** A decision is a pure function of (spec, model snapshot,
  context, seed). It takes microseconds in process and stays under a
  millisecond over HTTP. Nothing on the decide path waits for a disk.
- **Correct.** Every decision records the probability it was sampled with,
  so logged traffic supports unbiased off-policy evaluation (OPE).
- **Provable.** A candidate policy is scored on real logged traffic with
  confidence intervals before it serves anyone. Promotion gates on the lower
  bound.
- **Replayable.** Seed and model version are logged, so any decision can be
  reproduced exactly, on the server or offline.

## Guarantees

1. Every eligible action has probability at least `floor / K` (floor > 0 is
   enforced), so logs always support OPE.
2. One learner per capsule. It generalizes across contexts and supports
   per-request action sets with action features (Personalizer / VW ADF style).
3. Rewards join decisions by ID in O(1) and count once (idempotency key).
4. OPE (IPS, SNIPS, cross-fitted DR) runs over Syntra's own logs with
   bootstrap confidence intervals.
5. The decide path does no fsync and no whole-file rewrite; model state lives
   in memory, rebuilt from a durable event log plus snapshots.

## Latency architecture

| Path | Budget | How |
|---|---|---|
| In-process `Engine::decide` | ~2 µs (3 actions, 10 features) | hashed sparse features, dense weight array, SquareCB PMF, SplitMix64 |
| Server `/decide` | p99 < 1 ms on a laptop at thousands of rps | engine cached in memory behind an `RwLock`; the decision record goes to a bounded write-behind queue; no request waits on disk |
| Local evaluation (SDK) | same as in-process | the SDK holds a synced model snapshot and evaluates locally; decisions and rewards upload in batches |

### Write-behind decision log

Decisions are appended to an in-memory queue drained by one writer thread
that commits in batches (at most 2 ms or 512 records per transaction, SQLite
WAL with `synchronous = NORMAL`). A reward that arrives for a decision still
in the queue is resolved from the queue, so ordering is never visible to
callers. A process crash can lose at most the unflushed tail (bounded by the
batch window); rewards for those decisions answer 404, and the loss is
counted in `/metrics`. `durability: "sync"` in the spec makes a capsule's
decide wait for its commit instead, for callers that prefer latency to that
window.

### Local evaluation (the paradigm shift)

Feature-flag SDKs evaluate flags locally against a synced ruleset; Syntra
does the same for a learned policy. The server is the control plane (spec,
learning, logs, OPE, promotion); SDKs are the data plane.

1. The SDK fetches `GET .../model` (spec, snapshot bytes, version) and polls
   or long-polls for newer versions.
2. `decide()` runs the same `Engine::decide` code locally: microseconds, no
   network hop, works during a server outage with the last snapshot.
3. The decision record (context, actions, PMF, chosen, probability, seed,
   model version) is queued and uploaded in batches to
   `POST .../decisions:batch`; rewards to `POST .../rewards:batch`.
4. The server verifies each uploaded decision by replaying it with the same
   seed and model version, stores it, and learns from its reward exactly as
   if it had served it. A decision that does not replay is rejected and
   audited, so a client cannot poison the log with fabricated propensities.

The Rust crate is the first SDK; Python (PyO3) and TypeScript/edge (WASM)
bindings wrap the same core.

## Concepts

- Tenant / job / capsule addressing is unchanged.
- **Capsule**: one decision point. Spec (actions, exploration, reward range,
  optional feature program), learner, logs.
- **Action**: `id` plus optional `features` (JSON object), declared in the
  spec or supplied per request.
- **Context**: arbitrary JSON object.
- **Mode**: `learner` (serve the learned policy), `baselineExplore` (serve the
  caller's incumbent action with probability `1 - epsilon`, uniform
  otherwise), `frozen` (serve the learned policy, apply no updates).

## Decide request and response

```json
POST /v1/tenants/{t}/jobs/{j}/capsules/{c}/decide
{
  "context": {"task": "code", "prompt_tokens": 812, "user": {"tier": "pro"}},
  "actions": [{"id": "small", "features": {"cost": 0.2}}, {"id": "large", "features": {"cost": 1.0}}],
  "excludedActions": ["large"],
  "baselineAction": "large",
  "eventId": "client-supplied-id-optional"
}
```

`actions` is optional (spec actions otherwise). `baselineAction` is required
only in `baselineExplore` mode. `eventId`, when given, becomes the decision ID
and must be unique per capsule.

```json
{
  "decisionId": "dec_01J...",
  "action": "small",
  "actionIndex": 0,
  "probability": 0.93,
  "ranking": [{"id": "small", "probability": 0.93}, {"id": "large", "probability": 0.07}],
  "mode": "learner",
  "modelVersion": 1234
}
```

## Features

JSON flattens into sparse `(namespace, name, value)` features: nested objects
give dotted names; numbers are numeric features; bools and strings are
indicators `name=value`; string arrays give one indicator per element;
number arrays give indexed numeric features; null is skipped. Namespaces:
`c` context, `a` action (`id=<id>` for spec-declared actions plus flattened
action features), `d` derived (published by the feature program).

Names hash with FNV-1a 64 over `namespace \0 name`; slots are
`fmix64(hash) & (2^bits - 1)` (default `bits = 18`). Stable across Rust
releases (golden tests). `phi(x, a) = [bias, a, c x a, d x a]`; context-only
terms cannot change a ranking and are added only for the DR reward model.

## Learner

Online least squares with NAG (Ross, Mineiro and Langford 2013,
Algorithm 2) on hashed sparse features: per-weight scale and AdaGrad
accumulator, global normalizer. Trains on observed `(x, a, r)` without
importance weighting (as SquareCB assumes); `importance: "mtr"` weights by
`min(1/p, w_max)`. Rewards normalize to [0, 1] by the spec's `reward.range`.

## Exploration

- `squarecb` (default): inverse gap weighting, `gamma = 10 * n^0.5`.
- `epsilonGreedy`.
- Then the floor: `p = (1 - f) * p + f / K`, `f > 0` (default 0.05).
- `baselineExplore`: `(1 - eps) * delta(baseline) + eps / K`.

Sampling uses SplitMix64 seeded per decision; the seed is logged.

## Feature program (optional Lycan)

A capsule may carry a compiled Lycan program that runs before sampling with
the request as `runtime.input`. It can publish `features.<name>` (derived
features), `eligible` (action ids) and `reason`. Its return value is ignored.
Programs containing `choice`/`strategy`/`feedback` nodes are rejected at
install (those remain language features). The execution policy (file and
network sandbox, time budget) applies unchanged.

## Storage

Capsule artifacts stay in the store directory (`spec.json`, `policy.json`,
`current.lyc`). Events live in `<root>/syntra.db` (SQLite, WAL,
`synchronous = NORMAL`): `decisions`, `rewards` (unique idempotency key per
capsule), `models` (versioned snapshots with the reward watermark), `audit`.
The model is event-sourced: snapshot every `snapshotEvery` updates and at
shutdown; at startup load the latest snapshot and replay later rewards in
sequence order. A `Store` trait abstracts the backend; Postgres follows for
multi-node deployments.

## Evaluation (OPE)

Rows `(context, actions, eligible, pmf, chosen, probability, reward)` from
`syntra.db` or a generic JSONL file. Candidate policies: `logged`,
`constant:<id>`, `greedy` (cross-fitted on K folds), `spec:<file>`, and a
per-row target PMF. Estimators: DM, IPS, SNIPS, cross-fitted DR, with weight
clipping, effective sample size, coverage, and bootstrap confidence
intervals. Gates such as `dr.lower >= logged.mean + 0.01` make
`syntra evaluate` exit non-zero; the same report is served over HTTP.

## API (v2)

- `POST .../decide`, `POST .../reward` (`POST .../feedback` is an alias).
- `PUT .../spec` (merge patch, unknown fields rejected), `GET .../spec`.
- `POST .../mode`.
- `GET .../decisions`, `GET .../decisions/{id}`.
- `GET .../model` (spec, version, snapshot for local evaluation).
- `POST .../decisions:batch`, `POST .../rewards:batch` (SDK uploads).
- `POST .../evaluate` (OPE report).
- Personalizer-compatible `rank`, `events/{id}/reward`, `events/{id}/activate`.
