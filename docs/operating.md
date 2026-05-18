# Operating Syntra

This is the operator playbook for a running Syntra appliance: what each
lifecycle state means, how to interpret learned state, what to do when a
capsule isn't behaving the way you expect, and how to back up and restore.
For the API surface see [`api.md`](api.md); for deployment shapes see
[`deployment.md`](deployment.md); for what shipped in each phase see
[`../CHANGELOG.md`](../CHANGELOG.md).

The operating principle is that Syntra is inspectable. When a capsule appears
to learn the wrong thing, the data trail is in the store — read it before
changing the capsule.

## Lifecycle states

Every capsule moves through three lifecycle states, persisted in
`warmup.json` next to the graph and returned in every `/decide` response
under the `warmup` field.

**Warmup** is the first ~30 feedback rounds. Syntra runs uniform random
selection, watches the rewards arrive, and characterizes the problem as
binary, continuous, or sparse-continuous. The first active algorithm is
picked from this characterization automatically. Refusal is never engaged
during Warmup — refusing every decision in this phase would starve the
calibrator and deadlock the cold start. Treat Warmup as a known-noisy period
and do not draw conclusions from the weights yet.

**Active** is the steady state. The rate-adaptive meta-bandit runs six
candidate algorithms in parallel (Thompson, UCB1, EpsilonGreedy, Weighted,
Greedy, and — for feature-context capsules — LinUCB) and converges on
whichever one is doing best on the actual traffic. Each candidate keeps its
own per-context bucket so the meta-bandit can compare like with like. This is
the state your dashboards and SLOs care about.

**Frozen** is an operator-triggered hold. The bandit stops mutating weights
and serves decisions from whatever it learned last. Use this for change
freezes, incident response, or to A/B a learned policy against a static
baseline. There is currently no HTTP route to freeze a capsule; freeze a
capsule by setting `safety.freezeLearning = true` in `learning.json` via
`PUT /learning`.

A capsule reverts from Active back to Warmup automatically when the
capsule-level ADWIN detector fires (see Drift detection below).

## Inspecting learned state

For a capsule at `tenants/{tenant}/jobs/{job}/capsules/{capsule}`, the
inspection endpoints in order of increasing cost are:

`GET /report` is the cheap path. It returns each strategy node with current
weights, per-option tries / correct counts, average latency, and the SHA-256
hash of the installed graph binary. This is what your dashboards should poll.
The graph hash lets you correlate weights against the install audit entry —
useful when a capsule has been reinstalled and you need to know whether you
are looking at the new graph's learning or the old graph's residual.

`GET /contexts` returns one row per `(nodeId, contextKey)` with weights, total
tries, and last-update timestamp. Use it to confirm requests are landing in
the contexts you expect. High-cardinality keys (raw user IDs, request IDs)
usually indicate a mistake — Syntra learns nothing per-bucket if no bucket
sees more than one decision.

`GET /memory` returns the full `memory.json` sidecar: per-context buckets,
the meta-bandit state for each strategy node, candidate-context buckets, per-
context ADWIN detectors, and the OOD detectors. The most useful sub-structure
is `strategies[nodeId].metaBandit.candidates[]`: one entry per candidate
algorithm with its trials and rolling reward. When the meta-bandit picks
something surprising, this is where you confirm whether the surprise is
"it's actually better on your data" or "it's mis-attributed because one
candidate has far fewer trials than the others".

`GET /decisions` and `GET /audits` return the JSONL logs as text. Decisions
carry the `decisionId`, the chosen option, the context, refusal flags, and
the OOD score; audits carry installs, policy changes, deletes, warmup
transitions, change-detection events, and decision-refused records.

## Common failure modes

### Capsule stuck in Warmup

The lifecycle only transitions out of Warmup once 30 feedback rounds have
arrived. If a capsule looks frozen in Warmup, the answer is almost always
that not enough feedback is being sent. Confirm `feedback.jsonl` is growing
at the rate `decision.jsonl` is growing; if it isn't, fix the calling side.
The most common bug is a feedback path that drops on error rather than
retrying.

A secondary cause is feedback with zero reward — a `reward: 0.0` payload
counts as a sample for warmup but contributes nothing to the
characterization. If every reward is zero, audit your reward computation;
positive rewards should mean "do more of this", negative rewards should mean
"do less of this".

### Too many refusals

When `refusal.enabled = true`, the response carries `refused: true` whenever
the OOD score exceeds `oodThreshold` or the conformal interval width exceeds
`maxIntervalWidth`. If refusal rate climbs above what your integration
expects, check the refusal reason distribution in `decision.jsonl` (filter
on `refused: true` and bucket by `refusalReason`).

`"ood"` means the input is far from anything seen during training; either
raise `oodThreshold` to widen tolerance, or accept the refusal and let your
service fall back. `"interval_too_wide"` means the predictor isn't confident
yet; raise `maxIntervalWidth` for a looser bound, or wait for more data to
flow in. `"insufficient_calibration_data"` means the calibrator hasn't seen
enough residuals for the requested coverage; usually a transient state right
after Warmup completes.

### Unexpected algorithm pick

When the meta-bandit picks an algorithm you didn't expect, do not change
the algorithm — that defeats the whole point of having a meta-bandit. Inspect
`/memory` and look at `strategies[nodeId].metaBandit.candidates[]`. Each
candidate has a `trials` count and a rolling reward; the meta-bandit favors
whichever candidate has the highest rolling reward subject to a UCB
exploration bonus over trials.

Three things to check. First, candidate trial counts: if one candidate has
ten times the trials of another, the picks are dominated by exploration
balance, not by mean reward. Second, candidate-context bucket counts under
`candidateContexts`: if a candidate has not seen the current context yet,
it falls back to its prior, which can look like aggressive exploration.
Third, the candidate's residual width via the conformity calibrator —
candidates with wide residuals are running on noisy data and should not yet
be trusted as winners.

Only override the meta-bandit's pick (by setting `algorithm` in
`learning.json`) when you have reason to believe the meta-bandit is being
fooled — for example, when one candidate has a learnability advantage you
can prove independently. Even then, prefer to let the meta-bandit converge.

### Wrong context is learning

The bandit can only learn per-context weights for the contexts it actually
sees. If a context that should exist isn't appearing in `/contexts`, your
calling side is sending a different `contextKey` than you think. Tail
`decision.jsonl` for a minute and look at the `contextKey` field on recent
entries; compare to what the caller is logging on its side.

Avoid raw user IDs and request IDs as context keys — those produce one
bucket per request and learn nothing. Group into the meaningful axis (tier,
region, time-of-day bucket, recent-failure-rate band) instead.

### Weights are not moving

If feedback is flowing but weights are not changing, check `safety.freezeLearning`
in `/learning`. If frozen, that explains it. Otherwise, confirm rewards are
non-zero and confirm feedback is targeting the right `decisionId` — feedback
against a `decisionId` that doesn't exist in `decision.jsonl` is silently
accepted but does not move weights against any specific node. Tail
`audit.jsonl` for `feedback_on_refused` entries; feedback against refused
decisions is recorded but does not mutate the bandit.

## Drift detection

Drift is detected at two scopes. The capsule-level ADWIN detector watches
the rolling reward distribution across the whole capsule. When it fires, the
capsule reverts from Active back to Warmup, the meta-bandit state is
preserved, and an `audit.jsonl` entry records the trigger:

```json
{"event":"change_detected","previousAlgorithm":"Thompson","oldMean":0.71,"newMean":0.42,...}
```

Re-warmup is the right response to global regime shifts — a deployment, a
new upstream, a holiday traffic pattern — because the meta-bandit's previous
trial counts no longer reflect current dynamics. The previous algorithm
choice is remembered in the audit log so you can verify the post-drift
re-pick.

Per-context ADWIN detectors run alongside, one per `(nodeId, contextKey)`.
When one fires, only the affected context bucket is reset; the rest of the
capsule continues serving without interruption. This is the right response
to narrow shifts — a single tenant moves to a different upstream, one route
suddenly gets slower — and avoids re-warming the entire capsule for changes
that touched a small fraction of traffic.

ADWIN's drift threshold is currently hard-coded to `delta = 0.002` and is
not yet configurable via `learning.json`. This is tracked in known debt; for
now, treat it as a fixed sensitivity.

## Decision-log forensics

To trace a single `decisionId` end to end:

1. Find the decision in `decision.jsonl`. The record carries the contextKey,
   chosen option, the algorithm (or candidate) that picked it, the refusal
   flag, the OOD score, and the conformal interval width.
2. Find any matching record in `feedback.jsonl` keyed by the same
   `decisionId`. The feedback record carries the observed reward and the
   weights `before` and `after` the update.
3. Check `audit.jsonl` for events bracketing the decision's timestamp:
   installs, policy changes, warmup transitions, drift triggers, and refused
   decisions. The audit log is the global truth for "what was Syntra doing
   at this moment".

The decision and feedback logs are append-only JSONL; use `jq` or any
standard JSONL processor. Decision records carry a 16-character prefix of
the input SHA-256 (`inputSha256`) and a 16-character graph hash
(`graphHash`), so you can pin a decision to a specific request and a
specific installed graph version.

## Backup and restore

The entire state of a Syntra appliance is its store directory. Back up the
store, you have backed up everything. The recommended pattern is a
file-system snapshot (LVM, ZFS, EBS volume snapshot) for live backups, or
`cp -r` of the store root with the appliance stopped for a fully consistent
file-level copy:

```bash
systemctl stop syntra
cp -r /var/lib/syntra /var/backups/syntra-$(date +%F)
systemctl start syntra
```

For Docker, use `docker volume` against the `syntra-store` named volume;
for Kubernetes, snapshot the PersistentVolume. Restore is symmetric: stop
the appliance, replace the store directory, start the appliance. There is
no schema-migration step; the `memory.json` reader is backward-compatible
across schema versions 2 through 7.

A first-class HTTP backup endpoint that produces a single restorable
artifact per capsule is planned for Phase 1E. Until that ships, the
volume-copy pattern is the supported backup story.

## Shadow-mode checklist

Before letting Syntra influence production behaviour, run it in shadow mode
for a meaningful slice of traffic. The minimum bar before flipping the
switch:

1. Confirm decision and feedback rates are roughly equal in the logs (every
   decision gets feedback within your delayed-outcome window).
2. Confirm context buckets in `/contexts` look like what you expect — the
   right number of buckets, the right distribution of traffic across them.
3. Confirm the meta-bandit has converged on a candidate in `/memory` — one
   candidate should have meaningfully more trials than the others and a
   higher rolling reward.
4. Compare Syntra's suggested option against your existing production
   choice. The disagreement rate should be non-trivial (otherwise Syntra
   isn't adding value) but the disagreements should be defensible when
   spot-checked against the feature vector.
5. Back up the store volume before enabling any mutation-heavy workflow.
