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
| Local evaluation (SDK) | same as in-process | the SDK holds a synced published model and evaluates locally; decisions and rewards upload in batches |

Measured on an Apple M5 Max, release build, one machine (numbers depend on
hardware; `examples/bench_decide.rs` and `examples/bench_local.rs` reproduce
them):

| Path | Load | p50 | p99 | Throughput |
|---|---|---|---|---|
| HTTP `/decide` | 1 connection | 55 µs | 112 µs | |
| HTTP `/decide` | 8 connections | | ~470 µs | 39–47k/s |
| HTTP decide + reward | 8 connections | | | 30k req/s |
| `LocalDecider::decide` | 1 thread | 1.2 µs | 2.3–2.5 µs | 660k/s |
| `LocalDecider::decide` | 8 threads, one decider | 2.4–3.0 µs | 4.7–6.2 µs | 2.2–2.6M/s |
| Verified upload (`flush`) | one decider | | | ~60k decisions/s |

### Write-behind decision log

Decisions and rewards are appended to one bounded in-memory queue drained by
one writer thread that commits in batches (at most 2 ms or 512 records per
transaction, SQLite WAL with `synchronous = NORMAL`); decisions are inserted
before rewards, so a reward is never written ahead of its decision. A reward
or read that arrives for a decision still in the queue is resolved from the
queue, and log reads (`GET .../decisions`) wait for the queue first, so
ordering is never visible to callers. A process crash can lose at most the
uncommitted tail (bounded by the batch window); the model is snapshotted only
after a flush, so after a restart it equals the replay of the committed log.
`"durable": true` on a decide or reward request makes that request wait for
its commit, for callers that prefer latency to that window. When the queue is
full, requests answer 503 rather than serve a decision whose log record
would be dropped.

### Default rewards

Feedback is often implicit: a click is reported, a miss is not. With
`reward.default` set, a decision that has no reward `reward.waitSeconds`
(default 600) after it was made receives the default, as Azure Personalizer
does. A sweeper thread checks every loaded capsule once a second, after
waiting for the write-behind queue, and applies the default through the
normal reward path (audited in the reward's `detail` as `"default": true`,
counted in `syntra_default_rewards_total`). Under `rewards: "first"` its
idempotency key is the decision id, so a real reward that arrives later is
a duplicate; under `"sum"` a late reward still adds. Sweeps are idempotent,
and after a restart the sweep looks back 24 hours.

### Local evaluation (the paradigm shift)

Feature-flag SDKs evaluate flags locally against a synced ruleset; Syntra
does the same for a learned policy. The server is the control plane (spec,
learning, logs, OPE, promotion); SDKs are the data plane.

1. The SDK fetches `GET .../model?snapshot=true`: the published `decide`
   section, snapshot bytes (base64), model version and `modelTag`. The
   decide section holds only what an SDK needs (actions, exploration, mode,
   baseline epsilon, hash bits, seed, reward aggregation) plus a `version`;
   server-only settings stay out, so adding one never breaks a deployed
   SDK, and an SDK refuses a newer section version rather than make
   decisions the server could not replay. The tag is the first 16 hex
   digits of SHA-256 over the decide section (as served) and the snapshot
   checksum, and it is the ETag, so polling with `If-None-Match` costs a
   304. The model version alone cannot name what a client decided with: a
   spec change (mode, exploration, actions) keeps the version, so it
   publishes a new tag at once. Learning publishes a new tag at most once a
   second.
2. `decide()` runs the same `Engine::decide` code locally (microseconds, no
   network hop) and keeps working through a server outage with the last
   model it synced. A shared decider takes no shared lock on the decide
   path: each thread caches the current model (revalidated with one atomic
   load), keeps its own seed stream, and appends to one of 16 queue shards.
3. The decision record (context, per-request actions, exclusions, baseline,
   PMF, eligible set, chosen index, probability, seed, model tag) is queued
   and uploaded in batches to `POST .../decisions:batch`; rewards follow to
   `POST .../rewards:batch`, always after their decisions.
4. The server verifies each uploaded decision by replaying it on the
   published model named by its tag, with the same input and seed: the
   eligible set, the chosen action and every probability (to 1e-9) must
   match. It then stores the replayed record and learns from the decision's
   rewards exactly as if it had served it. A decision that does not replay,
   names a model the server did not publish (or has retired), or is dated
   outside [now − 7 days, now + 5 minutes] is refused and audited
   (`upload_rejected`), so a client cannot log a propensity the model would
   not have produced. A client that picks seeds to steer the draw is not
   detectable this way; data-plane tokens are trusted to sample honestly, as
   they are for server-side decides.
5. Uploads are idempotent. A decision re-uploaded with the same content is
   accepted as a duplicate; the same id with different content is refused.
   Under `rewards: "first"` a decision's reward is keyed by its id (a
   caller-supplied key cannot add a second reward); under `"sum"` the SDK
   gives each reward its own key when it is queued, so a retried flush
   counts it once. Refusals caused by backpressure are marked `retryable`
   and the SDK queues them again; transport errors keep the whole batch
   queued.

The server keeps the 32 newest publications (the sparse snapshot bytes; the
two newest also keep a restored engine warm), so a decision must be
uploaded within about 32 seconds of its model being superseded under
continuous learning, and much longer when the model changes less often.
Publications are held in memory: a restart retires them, and decisions made
on a pre-restart model are refused unless the restored model is identical.
Capsules with a feature program are not supported by local evaluation yet
(the program would have to run in the SDK).

The Rust crate is the first SDK (`syntra::client::LocalDecider`); Python
(PyO3) and TypeScript/edge (WASM) bindings wrap the same core.

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

Rows `(context, actions, eligible, pmf, chosen, probability, reward)` come
from the event store or a JSONL file (either the row format or decisions as
`GET .../decisions/{id}` serves them, with their `rewards`). Candidate
policies: `logged`, `constant:<id>` (falls back to the logged PMF where the
action is ineligible), `greedy` (cross-fitted on K folds), `spec:<file>`
(greedy under a candidate spec's learner settings and declared action
features) and a per-row target PMF. Estimators: DM, IPS, SNIPS and
cross-fitted DR, with weight clipping, effective sample size, coverage and
bootstrap confidence intervals, plus each estimator's lift over the logged
policy, paired on the same rows and resampled together.

- `syntra evaluate --store <root> --capsule t/j/c --policy ...` reads the
  event store read-only (safe while the server runs, on a backup copy or a
  read-only mount); `--input rows.jsonl` reads a file. `--gates` with
  `--fail-on-gate` makes a failed gate exit 1.
- `POST .../evaluate` runs the same report on the capsule's log (`policy`
  of `logged`, `greedy` or `constant:<id>`, or `spec`: a merge patch
  evaluated as a candidate), with optional `gates`, `since`/`until` and
  estimator settings. Read scope.
- `POST .../promote` evaluates a candidate spec patch and applies it only
  when every gate passes (at least one is required; `lift.dr.lower >= 0` is
  the recommended one): 200 with the new spec and the report, or 409 with
  the report. It refuses to apply if the spec changed while it evaluated.
  Both outcomes are audited (`spec_promoted`, `promotion_refused`).
  Mutate scope.

A candidate is scored as the greedy policy of its learner trained on the
logs; exploration and mode are not part of the estimate, so a gate answers
whether the candidate's choices beat what was logged, not what exploring
will cost. At most 2,000,000 logged decisions are read per evaluation
(`since`/`until` narrow larger logs).

## API (v2)

- `POST .../decide`, `POST .../reward` (`POST .../feedback` is an alias).
- `PUT .../spec` (merge patch, unknown fields rejected), `GET .../spec`.
- `POST .../mode`.
- `GET .../decisions`, `GET .../decisions/{id}`.
- `GET .../model` (spec, version; `?snapshot=true` for the published model,
  its tag and snapshot).
- `POST .../decisions:batch`, `POST .../rewards:batch` (SDK uploads).
- `POST .../evaluate` (OPE report), `POST .../promote` (gated spec change).
- Personalizer-compatible `rank`, `events/{id}/reward`, `events/{id}/activate`.
