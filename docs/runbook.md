# Syntra Operator Runbook — "Page Me at 3 AM"

This is the debugging companion to [`operating.md`](operating.md). That document
explains how Syntra works. This document assumes something is already broken and
you need to find it in under thirty seconds.

Structure: [Triage tree](#1-triage-tree) → [Worked failure scenarios](#2-worked-failure-scenarios)
→ [Monitoring playbook](#3-monitoring-playbook) → [Disaster recovery](#4-disaster-recovery)
→ [Known limitations and workarounds](#5-known-limitations-and-workarounds)

Conventions used throughout:

- `$CAPSULE` is shorthand for the path prefix
  `/tenants/{tenant}/jobs/{job}/capsules/{capsule}`.
- `$ADMIN` is `Authorization: Bearer $LYCAN_ADMIN_KEY`.
- All `curl` examples assume the server is at `http://localhost:8787`.

---

## 1. Triage tree

Start here. Find the symptom that matches, follow the arrows.

### `/decide` is slow or timing out

Is the server reachable at all?

```
GET /health     # no auth required
```

- No response → [Server won't start](#17-server-wont-start) or host is down → [Disaster recovery §4.3](#43-total-host-loss).
- Returns 200 but `/decide` is slow → check `GET /metrics`. Look at
  `syntra_decide_latency_seconds` histogram. Is p99 > 100 ms?
  - Yes, and it's a feature-context capsule → the `contextSpec` may have
    categorical features with very large `values` arrays; shrink them.
    See [Scenario 6](#scenario-6-performance-regressed-after-deploy).
  - Yes, and it just started after a config change → revert the learning config
    via `PUT $CAPSULE/learning` with the previous body.
  - No latency spike visible in metrics → the timeout is client-side;
    check the caller's network path and timeout settings.

### `/decide` is returning `refused: true` unexpectedly

Is the capsule in Warmup? (`warmup.state` in the response body)

- `"state": "warmup"` → capsule is in warmup; refusal is disabled during
  warmup by design. Something else is wrong — the capsule should not be
  refusing. Examine the response body carefully for other fields.
- `"state": "active"` → refusal is engaged. Check `refusalReason` in the
  `confidence` block:
  - `"ood"` → input is out-of-distribution for this capsule. See
    [Scenario 1](#scenario-1-decide-returns-refused-for-every-request-after-deploy).
  - `"interval_too_wide"` → calibrator is not confident yet; wait for more
    feedback or raise `maxIntervalWidth`.
  - `"insufficient_calibration_data"` → transient right after Warmup
    completes; wait for ~50 feedback samples to flow through.

### The capsule isn't learning (weights stay uniform)

```bash
curl -s $ADMIN http://localhost:8787/$CAPSULE/report | jq '.strategies[].weights'
```

Are all weights equal across multiple requests?

- Is `safety.freezeLearning` set? `GET $CAPSULE/learning | jq '.safety.freezeLearning'`
  - `true` → the capsule is frozen intentionally. Unfreeze:
    `PUT $CAPSULE/learning` with `{"safety":{"freezeLearning":false}}`.
- Is feedback flowing? Count lines in `/audits` or watch `feedback.jsonl` grow.
  - No feedback → caller is not posting to `POST $CAPSULE/feedback`. Fix
    the caller. See [Scenario 2](#scenario-2-capsule-stuck-in-warmup).
  - Feedback is flowing but weights don't move → rewards may all be zero or
    `decisionId` values are mismatched. See "Weights are not moving" in
    [`operating.md`](operating.md).

### Storage volume filling up

```bash
du -sh $LYCAN_STORE_ROOT/tenants/*/jobs/*/capsules/*/
```

- `decision.jsonl` or `feedback.jsonl` dominating? See
  [Scenario 5](#scenario-5-storage-volume-at-95-capacity).
- `snapshots/` dominating? Each feedback with `snapshotOnFeedback: true`
  writes a pre-mutation copy. Disable with
  `PUT $CAPSULE/learning` `{"safety":{"snapshotOnFeedback":false}}`.

### 401 / 403 errors from production traffic

- 401 → token is missing, malformed, or expired.
- 403 → token is valid but the scope does not cover this action.

See [Scenario 8](#scenario-8-authorization-failing-for-a-known-good-token).

### 429 rate-limit errors

The default rate limit is 1000 req/s per token with a burst of 2000. If a
caller is hitting 429, the token is generating traffic above that sustained
rate. See [Scenario 6](#scenario-6-performance-regressed-after-deploy) and
the monitoring alert for refusal rate.

### Server won't start {#17-server-wont-start}

Check the process log (stdout/stderr). Common messages:

| Log line | Cause | Fix |
|---|---|---|
| `cannot open store: ...` | Store path is missing or unreadable | Create the directory; check `$LYCAN_STORE_ROOT` |
| `cannot bind ...: Address already in use` | Port 8787 taken | `syntra status` to find the holder; `syntra stop` to send SIGTERM. Or set a different `--addr`. |
| `cannot bind to 127.0.0.1:8787 — port already in use` | Stale syntra from a previous run still holds the port | Same as above — `syntra stop` is the one-liner. The error message also prints the `lsof -i :<port>` / `kill $(lsof -ti :<port>)` commands. |
| `WARNING: no admin key set` | `LYCAN_ADMIN_KEY` is unset | Set the env var or pass `--admin-key`; or add `--dev-mode` for localhost-only dev use |

The server exits non-zero on store and bind failures; it continues with a
warning on missing admin key (dev mode).

#### Inspecting / stopping a running Syntra

Two convenience subcommands ship with the `syntra` binary:

```bash
# Is anything listening?
syntra status                       # checks default :8787
syntra status --addr 127.0.0.1:9090 # custom port
syntra status --port 9090           # same, via --port

# Stop whatever is listening on that port
syntra stop                         # SIGTERM to the holder of :8787
syntra stop --addr 127.0.0.1:9090
```

Both emit JSON on stdout. `syntra stop` does **not** verify that the
process it is killing is actually a Syntra instance — it kills whatever
process holds the configured port. If you have something unrelated bound
to `:8787`, use `lsof -i :8787` first to confirm.

---

## 2. Worked failure scenarios

### Scenario 1: Decide returns refused for every request after deploy

**Symptom.** Every `POST $CAPSULE/decide` returns `"refused": true` with
`"refusalReason": "ood"`. This started immediately after a deploy or after the
capsule's first Warmup completed.

**Diagnostic queries.**

```bash
# 1. Confirm the capsule is Active (refusal only fires in Active)
curl -s $ADMIN http://localhost:8787/$CAPSULE/decide \
  -H "Content-Type: application/json" \
  -d '{"contextKey":"any"}' | jq '{warmup,refused,confidence}'

# 2. Inspect the OOD detector state
curl -s $ADMIN http://localhost:8787/$CAPSULE/memory | \
  jq '.strategies | to_entries[] | {nodeId: .key, discreteOod: .value.discreteOod, featureOod: .value.featureOod}'

# 3. Check how many calibration residuals the conformity calibrator has seen
curl -s $ADMIN http://localhost:8787/$CAPSULE/memory | \
  jq '.strategies | to_entries[] | {nodeId: .key, calibratorSize: (.value.contexts | to_entries[0].value.conformityCalibrator.residuals | length // 0)}'

# 4. Check current thresholds
curl -s $ADMIN http://localhost:8787/$CAPSULE/learning | jq '.refusal'
```

**Likely cause.** One of two things:

1. The OOD threshold is too tight. During Warmup the OOD detector saw a
   limited set of context keys or feature vectors. When production traffic
   arrived post-Warmup with a slightly different distribution, every input
   scored above `oodThreshold` (default 0.8). This is most common when the
   Warmup data was synthetic or came from a narrow context subset.

2. The conformal calibrator hasn't seen enough residuals for the declared
   `coverage`. With fewer than about 50 calibration samples the interval is
   very wide, and `"interval_too_wide"` is the immediate trigger, with
   `"insufficient_calibration_data"` appearing as the reason on the very
   first requests.

**Remediation.**

For the OOD trigger: raise `oodThreshold` temporarily to 0.95 or higher to
stop refusing while the detector accumulates a broader sample base:

```bash
curl -s -X PUT $ADMIN http://localhost:8787/$CAPSULE/learning \
  -H "Content-Type: application/json" \
  -d '{"refusal":{"oodThreshold":0.95}}'
```

Monitor refusal rate in `/metrics`. Once the OOD detector has seen ~200 real
requests (`discreteOod.seen` count in `/memory`), tighten `oodThreshold` back
toward 0.8 in steps of 0.05.

For the calibration trigger: there is no shortcut. Feed back real outcomes
through `POST $CAPSULE/feedback` and wait for ~50 residuals to accumulate.
In the interim, raise `maxIntervalWidth` to 1.0 to permit wide-interval
decisions:

```bash
curl -s -X PUT $ADMIN http://localhost:8787/$CAPSULE/learning \
  -H "Content-Type: application/json" \
  -d '{"refusal":{"maxIntervalWidth":1.0}}'
```

**Do not disable refusal entirely** unless you are testing. The right
approach is to widen the thresholds while real data flows in, then tighten
them back on a schedule.

---

### Scenario 2: Capsule stuck in warmup

**Symptom.** `syntra_warmup_state` gauge is 0 (Warmup) for more than an hour.
`warmup.state` in `/decide` responses stays `"warmup"`. Weights remain uniform.

**Diagnostic queries.**

```bash
# 1. How many feedback records have arrived?
curl -s $ADMIN http://localhost:8787/$CAPSULE/audits | \
  grep '"event":"warmup_transition"' | wc -l
# Expect 0 if stuck; expect 1 if already transitioned (would contradict the symptom)

# 2. Count feedback log entries via the decisions log as a proxy
curl -s $ADMIN http://localhost:8787/$CAPSULE/decisions | wc -l
curl -s $ADMIN http://localhost:8787/$CAPSULE/audits | grep '"event":"feedback_received"' | wc -l

# 3. Read warmup.json directly from the store volume
cat $LYCAN_STORE_ROOT/tenants/{tenant}/jobs/{job}/capsules/{capsule}/warmup.json | jq .
```

The lifecycle transitions from Warmup to Active once 30 feedback rounds
(not decisions — feedback) have arrived. The `warmup.json` field
`feedbackCount` shows how many have landed.

**Likely causes.**

1. The caller is posting `/decide` but not posting `/feedback`. This is the
   most common root cause. The feedback path may be swallowing errors, or the
   outcome resolution window is so long that feedback hasn't arrived yet.

2. The caller is posting feedback with `reward: 0.0` on every record. Zero
   rewards count as samples for warmup but contribute nothing to reward
   characterization; the capsule may transition eventually but characterize the
   problem incorrectly.

3. Feedback is being sent to the wrong `decisionId` — for example, because an
   older code path is reusing stale IDs. Those records are accepted silently
   but do not count toward the lifecycle counter.

**Remediation.**

Confirm the caller is actually posting to `POST $CAPSULE/feedback`:

```bash
# Watch the audit log for new feedback events in real time
curl -s $ADMIN http://localhost:8787/$CAPSULE/audits | tail -20
```

If feedback is not flowing, fix the calling side first. If the caller cannot
post feedback (for example, the outcome resolution window is measured in days
and you need the capsule to enter Active now for A/B purposes), there is no
first-class API to force the lifecycle transition. The workaround is to post
synthetic feedback records with non-zero rewards until `feedbackCount` reaches
30. Use only if you understand the implication — those synthetic rewards will
influence the initial weight characterization.

```bash
# Synthetic feedback loop — run until the capsule transitions
for i in $(seq 1 35); do
  DID=$(curl -s -X POST $ADMIN http://localhost:8787/$CAPSULE/decide \
    -H "Content-Type: application/json" \
    -d '{"contextKey":"warmup-seed"}' | jq -r '.decisionId')
  curl -s -X POST $ADMIN http://localhost:8787/$CAPSULE/feedback \
    -H "Content-Type: application/json" \
    -d "{\"decisionId\":\"$DID\",\"reward\":0.5}"
done
```

There is no `PUT /policy` workaround to force the transition directly — that
is a known gap. It is tracked in the Syntra issue tracker as a lifecycle
override endpoint.

---

### Scenario 3: Meta-bandit picked an algorithm we didn't expect

**Symptom.** The `candidateId` field in `/decide` responses consistently shows,
say, `"EpsilonGreedy"` when you expected `"Thompson"` or `"LinUcb"`. Or the
meta-bandit has committed to a candidate that performs badly in your manual
evaluation.

**Diagnostic queries.**

```bash
# See all candidates with their trial counts and rolling rewards
curl -s $ADMIN http://localhost:8787/$CAPSULE/memory | \
  jq '.strategies | to_entries[] | {
    nodeId: .key,
    candidates: (.value.metaBandit.candidates // [] | map({
      id, trials, rollingReward: .reward
    }))
  }'
```

Look for two things: which candidate has the most trials, and whether that
candidate has a meaningfully higher rolling reward than the others.

**Likely cause.** Early warmup data was unrepresentative of steady-state
traffic. The meta-bandit committed to a candidate based on its performance
during the warmup window. If that window was short, synthetic, or dominated
by a single context, the winning candidate's advantage may not generalize.
A secondary cause is that one candidate has far more trials than the others
(the meta-bandit's UCB exploration bonus is computed relative to total
trials), making it look artificially dominant during exploration balance.

**Remediation.** Continue running. The rate-adaptive meta-bandit with per-
candidate geometric forgetting (default `optionStateForgetting: 0.999`) will
rebalance as real traffic accumulates. Expect meaningful rebalancing after
roughly 500–1000 feedback samples under realistic conditions.

If you have strong independent evidence that the winning candidate is wrong
and cannot wait for rebalancing, the only reset path is to delete and
reinstall the capsule. There is no direct API to reset the meta-bandit state
without losing all learned weights. After reinstall, the capsule re-enters
Warmup and the meta-bandit starts fresh.

```bash
# Nuclear option: delete capsule and reinstall
curl -s -X DELETE $ADMIN http://localhost:8787/$CAPSULE
curl -s -X POST $ADMIN http://localhost:8787/$CAPSULE/install \
  -H "Content-Type: application/octet-stream" \
  --data-binary @program.lyc
```

Do not override `algorithm` in `learning.json` to force a specific candidate
unless you have a proven reason — that disables meta-bandit selection
entirely and you lose the adaptive layer.

---

### Scenario 4: Drift detection fires constantly

**Symptom.** The audit log has many `"event":"change_detected"` entries per
hour. Each one reverts the capsule to Warmup, causing the `syntra_warmup_state`
gauge to oscillate between 0 and 1.

**Diagnostic queries.**

```bash
# Count drift events per hour in the audit log
curl -s $ADMIN http://localhost:8787/$CAPSULE/audits | \
  jq -r 'select(.event=="change_detected") | .ts' | \
  cut -c1-13 | sort | uniq -c | sort -rn | head -10
```

**Likely cause.** The capsule-level ADWIN detector is too sensitive for your
reward variance. ADWIN's `delta` parameter controls false-positive rate: lower
delta means more sensitive, higher delta means less sensitive. The default is
`0.002`. If your rewards are naturally volatile (for example, a binary 0/1
outcome with a true win rate near 0.5), ADWIN at the default sensitivity will
fire frequently on random fluctuations.

A secondary cause is a per-context ADWIN firing. Per-context firings reset only
the affected context bucket and do not produce `change_detected` in the top-
level audit. If the capsule-level audit is clean but a specific context's
weights keep resetting, check `/memory` for the per-context ADWIN detector
state under `strategies[nodeId].contextDetectors`.

**Remediation.**

The ADWIN `delta` threshold is currently hard-coded at `0.002` and is not
configurable via `learning.json` (see [Known limitations §5](#5-known-limitations-and-workarounds)). There is no API knob to change it at
runtime. The immediate operational workaround is to freeze learning during
an ongoing incident and investigate whether the drift events correspond to
real distribution shifts:

```bash
# Freeze learning to stop re-warmup churn
curl -s -X PUT $ADMIN http://localhost:8787/$CAPSULE/learning \
  -H "Content-Type: application/json" \
  -d '{"safety":{"freezeLearning":true}}'
```

Inspect the `oldMean` and `newMean` in the `change_detected` audit entries.
If the difference is small (< 0.05) and the events are frequent, this is
ADWIN over-sensitivity on noisy rewards. If the difference is large and
events correspond to actual events in your system (deploys, traffic shifts),
the detector is doing its job correctly.

For the persistent over-sensitivity case: rebuild Syntra with a higher default
delta in `Lycan/src/change_detection.rs`. Per-capsule ADWIN configurability
is in the debt backlog.

---

### Scenario 5: Storage volume at 95% capacity

**Symptom.** Disk alert fires. `du` shows `decision.jsonl` and/or
`feedback.jsonl` are large.

**Diagnostic queries.**

```bash
# Find the largest log files across all capsules
du -sh $LYCAN_STORE_ROOT/tenants/*/jobs/*/capsules/*/*.jsonl | sort -rh | head -20

# Check snapshots directory separately — snapshotOnFeedback generates many small files
du -sh $LYCAN_STORE_ROOT/tenants/*/jobs/*/capsules/*/snapshots/ | sort -rh | head -10
```

**Likely cause.** `decision.jsonl` and `feedback.jsonl` are append-only and
grow unbounded. There is no built-in log rotation. A high-traffic capsule
processing thousands of requests per day will fill the volume within weeks
or months depending on disk size. `snapshots/` grows if `snapshotOnFeedback`
is enabled (the default is `true`).

**Remediation — safe procedure.**

Step 1: Take a backup before touching anything.

```bash
curl -s -X POST $ADMIN http://localhost:8787/admin/backup \
  -o syntra-backup-$(date +%Y%m%d-%H%M%S).json
```

Verify the backup file is non-empty and parses as JSON before proceeding.

Step 2: Disable snapshot-on-feedback if it is the driver.

```bash
curl -s -X PUT $ADMIN http://localhost:8787/$CAPSULE/learning \
  -H "Content-Type: application/json" \
  -d '{"safety":{"snapshotOnFeedback":false}}'
# Then remove old snapshot files
rm -rf $LYCAN_STORE_ROOT/tenants/{tenant}/jobs/{job}/capsules/{capsule}/snapshots/*
```

Step 3: For the decision and feedback logs, use the logs-purge endpoint to
clear interaction history without destroying the learned state:

```bash
curl -s -X DELETE $ADMIN http://localhost:8787/$CAPSULE/logs
```

This truncates `decision.jsonl` and `feedback.jsonl` in place but preserves
`memory.json`, `warmup.json`, `audit.jsonl`, and the installed graph. Learned
weights are not affected. The audit log records the deletion.

Step 4: Set up OS-level log rotation for future control. Example `logrotate`
stanza:

```
$LYCAN_STORE_ROOT/tenants/*/jobs/*/capsules/*/decision.jsonl {
    daily
    rotate 7
    compress
    missingok
    nocreate
    copytruncate
}
```

`copytruncate` is safe here because Syntra only appends to these files.
Do not rotate `memory.json`, `warmup.json`, `audit.jsonl`, or any `.lyc`
binary — those are structural, not logs.

**Warning.** Truncating or deleting `audit.jsonl` is irreversible. The audit
log is the only record of installs, policy changes, drift events, and refused
decisions. Keep at least 30 days of audit history; archive older entries to
cold storage rather than deleting them.

---

### Scenario 6: Performance regressed after deploy

**Symptom.** `syntra_decide_latency_seconds` p99 increased noticeably after a
configuration change or capsule reinstall. `/decide` calls that previously
completed in a few milliseconds now take 50–200 ms or more.

**Diagnostic queries.**

```bash
# Current latency histogram
curl -s $ADMIN http://localhost:8787/metrics | grep syntra_decide_latency_seconds

# Compare refusal rate — high refusal rate inflates apparent latency
# (refused decisions still go through the full OOD/conformal stack)
curl -s $ADMIN http://localhost:8787/metrics | grep syntra_refusals_total

# Inspect the contextSpec — large categorical sets expand the feature vector
curl -s $ADMIN http://localhost:8787/$CAPSULE/learning | jq '.contextSpec'
```

**Likely causes.**

1. A `contextSpec` change introduced a categorical feature with a large
   `values` array. One-hot encoding a categorical with N values produces N-1
   columns in the feature vector. LinUCB matrix operations are O(d^2) per
   decision where d is the feature dimension. A categorical with 200 values
   adds ~200 columns and can increase LinUCB latency by 10-40x.

2. The rate limit is returning 429 responses that the caller's retry loop is
   hiding as apparent latency. Check `syntra_requests_total` with
   `status="429"`.

3. A learning config change re-enabled `snapshotOnFeedback` with high feedback
   volume, causing disk I/O contention. The decide path itself does not write
   snapshots, but if feedback is being posted synchronously by the same client,
   the mutex contention from snapshot writes will block concurrent decides.

**Remediation.**

Revert the config change that triggered the regression:

```bash
curl -s -X PUT $ADMIN http://localhost:8787/$CAPSULE/learning \
  -H "Content-Type: application/json" \
  -d '{ ... previous learning.json body ... }'
```

For the feature-vector size issue, shrink the offending categorical's
`values` array to the most common groupings (ideally under 20 values).
Rarer values can be collapsed into an `"other"` bucket at the caller side.

For the rate-limit issue, the global default is 1000 req/s per token with
burst 2000. Per-token rate-limit configuration is not yet exposed via API
(tracked as known debt). If a legitimate caller is being throttled, consider
issuing it a separate token so its bucket is isolated.

---

### Scenario 7: Need to migrate Syntra to a new host

**Symptom.** Planned host migration, hardware replacement, or cloud region
move.

**Migration procedure.**

Step 1: On the old host, take a full backup via the HTTP endpoint.

```bash
curl -s -X POST $ADMIN http://localhost:8787/admin/backup \
  -o syntra-backup-$(date +%Y%m%d-%H%M%S).json
echo "Backup size: $(wc -c < syntra-backup-*.json) bytes"
```

The backup bundle contains all tenant/job/capsule data: `memory.json`,
`warmup.json`, `audit.jsonl`, `decision.jsonl`, `feedback.jsonl`,
`learning.json`, `policy.json`, and the compiled `.lyc` graph binaries.

It does **not** contain the `LYCAN_ADMIN_KEY`. The admin key is
environment-supplied separately and must be set on the new host before the
server starts.

Step 2: Provision the new host with the same `LYCAN_ADMIN_KEY` value, or
generate a new key and record it. Start Syntra on the new host with an empty
store.

```bash
LYCAN_ADMIN_KEY=<your-key> syntra serve --store /var/lib/syntra
```

Confirm it starts:

```bash
curl http://new-host:8787/health
# → {"ok":true,"service":"Syntra"}
```

Step 3: Restore the backup bundle.

```bash
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://new-host:8787/admin/restore \
  --data-binary @syntra-backup-20240516-120000.json | jq .
# → {"ok":true,"filesRestored":N}
```

Step 4: Smoke-test the restored state.

```bash
curl $ADMIN http://new-host:8787/health
curl $ADMIN http://new-host:8787/metrics | grep syntra_warmup_state
curl $ADMIN http://new-host:8787/$CAPSULE/report | jq '.strategies[].weights'
curl $ADMIN http://new-host:8787/$CAPSULE/memory | jq '.schema_version'
```

Confirm `schema_version` is 7, warmup state is what you expect, and weights
look reasonable.

Step 5: Point DNS or load-balancer at the new host. Let a small fraction of
traffic flow through and verify `syntra_requests_total` is climbing normally
before cutting over fully.

**Gotchas.**

- Token store moves with the backup bundle. Previously issued scoped tokens
  (listed via `GET /admin/tokens`) will work on the new host without re-issue.
- If traffic patterns differ significantly on the new host (different region,
  different load profile), the restored warmup and meta-bandit state may be
  stale. The ADWIN detector will catch genuine drift and trigger re-warmup.
  Expect one re-warmup cycle if the host move coincides with a meaningful
  traffic shift.
- The backup bundle does not include `snapshots/` subdirectory content (those
  are pre-mutation ephemeral copies). This is expected; the current live
  `memory.json` is authoritative.

---

### Scenario 8: Authorization failing for a known-good token

**Symptom.** A caller that worked last week is now receiving 401 or 403
responses. The admin key itself still works.

**Diagnostic queries.**

```bash
# List all issued scoped tokens
curl -s $ADMIN http://localhost:8787/admin/tokens | jq '.tokens[] | {hash, scope, expiresAt, label}'

# Cross-reference against the failing token's hash (sha256 of raw token)
echo -n "THE_RAW_TOKEN_VALUE" | sha256sum
```

**Likely causes.**

1. The token expired. The `expiresAt` field is a Unix timestamp. Compare it
   to `date +%s`. A token with a 90-day TTL issued at deploy time will expire
   without ceremony.

2. The token was revoked explicitly (via `DELETE /admin/tokens/{hash}`) or
   the token store was wiped during a backup/restore operation where the
   source backup predated the token's creation.

3. Scope mismatch. The token has `Scope::Read` (decide + read only) but the
   caller is attempting a mutating operation (install, feedback, learning PUT).
   A `Read`-scoped token is sufficient for `POST /decide` and `GET /report`
   but not for `POST /feedback`. Check the token's `scope` value against
   the required action. Scope definitions:
   - `Admin` — all routes, all tenants
   - `TenantAdmin` — all routes for one tenant
   - `Read` — `POST /decide` and all GET endpoints for one specific
     `(tenant, job, capsule)` triple

**Remediation.**

Re-issue a token with the appropriate scope:

```bash
# Issue a new TenantAdmin token for tenant "acme", valid for 90 days
curl -s -X POST $ADMIN http://localhost:8787/admin/tokens \
  -H "Content-Type: application/json" \
  -d '{
    "scope": {"TenantAdmin": {"tenant": "acme"}},
    "ttlSeconds": 7776000,
    "label": "acme-prod-2024"
  }' | jq '{token, hash, expiresAt}'
```

Store the raw `token` value securely — it is returned only once at issuance.
Revoke the old token by hash to prevent confusion:

```bash
curl -s -X DELETE $ADMIN http://localhost:8787/admin/tokens/{old-hash}
```

---

## 3. Monitoring playbook

### Prometheus metrics surface

The full metrics exposition is at `GET /metrics` in Prometheus text format.
No auth is required for the metrics endpoint by convention, but Syntra
does require the Bearer token — scrape it with the admin key set in your
Prometheus job config.

The four metric families emitted by `render_metrics` in `server.rs`:

```
syntra_requests_total{kind, tenant, job, capsule, status}     # counter
syntra_decide_latency_seconds{le}                              # histogram
syntra_refusals_total{tenant, job, capsule, reason}            # counter
syntra_warmup_state{tenant, job, capsule}                      # gauge: 0=warmup, 1=active, 2=frozen
syntra_meta_bandit_trials{tenant, job, capsule, candidate}     # gauge
```

`syntra_warmup_state` and `syntra_meta_bandit_trials` are derived by
walking the store on every scrape. At development-scale deployments this
is negligible; at large installations with many capsules, consider scraping
`/metrics` at 60-second intervals rather than the default 15s to avoid
read amplification.

### Alert rules

```yaml
groups:
  - name: syntra
    rules:

      # Latency: /decide p99 exceeds 100 ms for 5 minutes
      - alert: SyntraHighDecideLatency
        expr: |
          histogram_quantile(0.99,
            rate(syntra_decide_latency_seconds_bucket[5m])
          ) > 0.1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Syntra /decide p99 latency > 100 ms"
          description: "Check /metrics for latency histogram; check contextSpec feature-vector size and learning config changes."

      # Refusal rate: more than half of /decide calls are refusing
      - alert: SyntraHighRefusalRate
        expr: |
          rate(syntra_refusals_total[5m])
          > 0.5 * rate(syntra_requests_total{kind="decide"}[5m])
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Syntra refusing >50% of /decide requests"
          description: "Check OOD and conformal thresholds. See Scenario 1 in the runbook."

      # Capsule stuck in Warmup for more than 1 hour
      - alert: SyntraCapsuleStuckInWarmup
        expr: syntra_warmup_state == 0
        for: 60m
        labels:
          severity: warning
        annotations:
          summary: "Syntra capsule has been in Warmup for >1 hour"
          description: "Feedback may not be reaching /feedback. See Scenario 2 in the runbook."

      # Server unreachable
      - alert: SyntraDown
        expr: up{job="syntra"} == 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "Syntra server is unreachable"
          description: "Check process status and /health endpoint."
```

### Storage alert

Syntra itself does not expose a disk-usage metric. Alert on the host volume
from your infrastructure layer:

```yaml
# Node Exporter example
- alert: SyntraStorageHigh
  expr: |
    (node_filesystem_size_bytes{mountpoint="/var/lib/syntra"}
     - node_filesystem_free_bytes{mountpoint="/var/lib/syntra"})
    / node_filesystem_size_bytes{mountpoint="/var/lib/syntra"} > 0.80
  for: 15m
  labels:
    severity: warning
  annotations:
    summary: "Syntra store volume >80% full"
    description: "See Scenario 5 in the runbook for safe log rotation."
```

### Dashboard queries (Grafana / Prometheus)

Decide rate by status:

```promql
sum by (status) (rate(syntra_requests_total{kind="decide"}[5m]))
```

Refusal rate as a fraction of decide traffic:

```promql
sum(rate(syntra_refusals_total[5m]))
/ sum(rate(syntra_requests_total{kind="decide"}[5m]))
```

Meta-bandit candidate trial distribution (which algorithm is dominant):

```promql
syntra_meta_bandit_trials{capsule="router"}
```

---

## 4. Disaster recovery

### 4.1 Lost admin key

The `LYCAN_ADMIN_KEY` is environment-supplied and not stored by Syntra. If
the key is lost you cannot authenticate to any protected route — including
token issuance or backup.

**Recovery procedure.**

Step 1: On the host (not over HTTP), restart Syntra in dev mode. Dev mode
binds to `127.0.0.1` only and grants Admin scope to unauthenticated requests
from localhost. The learned state is unaffected.

```bash
systemctl stop syntra
LYCAN_STORE_ROOT=/var/lib/syntra syntra serve --dev-mode &
```

Step 2: Issue a new admin-scoped token via the unauthenticated local endpoint.

```bash
curl -s -X POST http://127.0.0.1:8787/admin/tokens \
  -H "Content-Type: application/json" \
  -d '{"scope":"Admin","label":"recovered-admin","ttlSeconds":31536000}' | jq .
```

Save the returned `token` value securely.

Step 3: Stop the dev-mode instance. Restart Syntra normally with the new
`LYCAN_ADMIN_KEY` set to the newly issued raw token value, or generate a fresh
random key and set it in the environment, then authenticate with the issued
token to revoke any other tokens you no longer trust.

```bash
kill %1   # or systemctl stop syntra
export LYCAN_ADMIN_KEY=<new-random-key>
systemctl start syntra
```

Step 4: Verify normal operation:

```bash
curl -s -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/health
```

### 4.2 Corrupted store

Signs of a corrupted store: the server starts but `/report` or `/memory`
returns 500 errors; `memory.json` fails to parse; `warmup.json` is truncated
(partial write during an OS crash mid-feedback).

**Recovery procedure.**

Step 1: Stop Syntra immediately. Do not let it write further on a possibly
corrupt store.

```bash
systemctl stop syntra
```

Step 2: Identify the last known good backup. The recommended pattern is a
daily `POST /admin/backup` run via cron:

```bash
# Example cron entry — runs at 03:00 daily
0 3 * * * curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  http://localhost:8787/admin/backup \
  -o /var/backups/syntra/syntra-$(date +\%Y\%m\%d).json
```

Step 3: Wipe or move the corrupted store directory.

```bash
mv /var/lib/syntra /var/lib/syntra.corrupted.$(date +%Y%m%d-%H%M%S)
mkdir /var/lib/syntra
```

Step 4: Start Syntra fresh (empty store) and restore from the last good backup.

```bash
systemctl start syntra
curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://localhost:8787/admin/restore \
  --data-binary @/var/backups/syntra/syntra-YYYYMMDD.json | jq .
```

Step 5: Verify the restore.

```bash
curl -s $ADMIN http://localhost:8787/metrics | grep syntra_warmup_state
curl -s $ADMIN http://localhost:8787/$CAPSULE/report | jq '.strategies[].weights'
```

Step 6: Send a test `/decide` request and a test `/feedback` to confirm the
learning pipeline is functional.

Learning state between the backup date and the crash is lost. The capsule will
resume from the weights it had at backup time. The ADWIN detector will
recalibrate from its restored window state; expect one possible re-warmup
cycle if the data gap is large enough to look like drift.

### 4.3 Total host loss

**Recovery procedure.**

Step 1: Provision a new host. Install the same version of Syntra. Set
`LYCAN_ADMIN_KEY` (same key as before if you have it; if not, any value
works — see §4.1 if the old key is unavailable).

Step 2: Mount or copy the backup file to the new host. The backup is a
self-contained JSON bundle; it does not require any particular directory
structure before restore.

Step 3: Start Syntra with an empty store, then restore:

```bash
LYCAN_ADMIN_KEY=<key> LYCAN_STORE_ROOT=/var/lib/syntra \
  syntra serve --bind 0.0.0.0:8787 &

sleep 2  # wait for bind

curl -s -X POST \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  http://new-host:8787/admin/restore \
  --data-binary @syntra-backup-YYYYMMDD.json
```

Step 4: Smoke-test the restored appliance:

```bash
curl http://new-host:8787/health
curl $ADMIN http://new-host:8787/metrics | grep 'syntra_warmup_state'
curl $ADMIN http://new-host:8787/$CAPSULE/report
```

Step 5: Update DNS / load-balancer records to point at the new host. Do a
staged cut-over: route 10% of traffic, verify metrics, route 100%.

Step 6: Schedule daily backups on the new host before calling the recovery
complete.

**Important.** The `memory.json` schema reader is backward-compatible from
v2 through v7. A backup taken from any Syntra release in the Phases A–F range
will restore correctly.

---

## 5. Known limitations and workarounds

These are tracked in the CHANGELOG "Known debt" section and are not yet
addressable via API. Each entry includes the operational workaround.

### ADWIN drift threshold is not per-capsule configurable

**Limitation.** The ADWIN `delta` parameter (default `0.002`) is compiled in
at `Lycan/src/change_detection.rs`. It applies to every capsule on the
appliance. There is no `learning.json` field to change it per-capsule.

**Workaround.** If the default sensitivity is wrong for your reward
distribution, the only current option is to rebuild Syntra from source with
a different default delta. Set the constant in `change_detection.rs` before
`cargo build`. For incident response, freeze learning via
`PUT $CAPSULE/learning` with `{"safety":{"freezeLearning":true}}` to stop
re-warmup churn while you investigate. Per-capsule ADWIN configuration is
planned for the next debt-reduction sprint.

### `/inspect` returns graph shape only

**Limitation.** `GET $CAPSULE/inspect` returns only the graph structure
(node count, edge count, journal entries). It does not return live weights,
lifecycle state, OOD scores, or meta-bandit state.

**Workaround.** Use the other endpoints for live state:
- `GET $CAPSULE/report` — weights, per-option tries, graph hash.
- `GET $CAPSULE/memory` — meta-bandit candidates, calibrators, OOD detectors.
- `GET $CAPSULE/contexts` — per-context bucket detail.
- `warmup.json` is surfaced via the `warmup` field in every `/decide` response.

The admin console at `GET /admin` assembles all of these into a unified view.
When scripting, query the individual endpoints directly.

### Multi-AdaptiveChoice graphs only attach meta-bandit decisions to `decisions[0]`

**Limitation.** Capsules with more than one `AdaptiveChoice` node (a graph
where two independent bandits run in sequence, for example) only have the
meta-bandit selection recorded against `decisions[0]` in the `/decide`
response. The trailing `AdaptiveChoice` nodes use uniform weights regardless
of feedback.

**Workaround.** Until this limitation is resolved (tracked as a 5C milestone),
design capsules with one `AdaptiveChoice` node per request. If you need two
independent decisions per request, install two separate capsules and call
each independently. They can share a tenant and job but must have separate
capsule paths:

```
POST /tenants/acme/jobs/routing/capsules/primary-router/decide
POST /tenants/acme/jobs/routing/capsules/secondary-router/decide
```

Each capsule maintains its own memory and learns independently. Feedback goes
to each independently as well, keyed to the `decisionId` returned by that
capsule's `/decide` call.

### HTTP backup endpoint does not include snapshot files

**Limitation.** `POST /admin/backup` serializes all structural files —
`memory.json`, `warmup.json`, all `.jsonl` logs, `learning.json`, `policy.json`,
the `.lyc` binary — but does not include the `snapshots/` subdirectory. Those
pre-mutation snapshot files are ephemeral debugging aids, not structural state,
so they are intentionally excluded.

**Workaround.** If you need to preserve snapshot history (for example, to
reconstruct the exact weight vector at a specific feedback round), take a
filesystem-level copy of the store directory in addition to the HTTP backup:

```bash
# Full filesystem copy (stop Syntra first for consistency, or use LVM/ZFS snapshot)
systemctl stop syntra
cp -r /var/lib/syntra /var/backups/syntra-full-$(date +%F)
systemctl start syntra
```

### Per-token rate-limit configuration not yet exposed via API

**Limitation.** The global rate limit defaults to 1000 req/s per token with
burst 2000, as compiled into `rate_limit.rs`. There is no API to change this
per-token or per-capsule at runtime.

**Workaround.** If a legitimate high-throughput caller is being throttled:
the limit is high enough that only runaway or adversarial callers should hit
it under normal conditions. If a genuinely high-volume production caller
approaches the limit, issue it a dedicated token so its per-token bucket is
isolated from shared traffic. If the global default itself needs changing,
rebuild from source with different `RateLimitConfig::default()` values.
