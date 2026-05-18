# Operations

Operating Syntra is about three things in roughly the order you will
need them: knowing whether the appliance is healthy, knowing whether
each capsule's learner is healthy, and knowing what to do when one
of them isn't.

## Available runbooks

- [**Debugging refusals**](operations/debugging-refusals.md) — when
  `/decide` returns `refused: true`, the three possible reasons and
  the diagnosis path for each one.

## Planned topics (not yet written)

!!! note "Stubs below"

    The topics listed below are planned but unwritten. Until each is
    filled in, the closest existing material is the
    [`docs/operating.md`](https://github.com/ashhart/Syntra/blob/main/docs/operating.md)
    and [`docs/deployment.md`](https://github.com/ashhart/Syntra/blob/main/docs/deployment.md)
    files in the repository.

### Monitoring

- [ ] **Health probes** — `GET /health` for liveness, `GET /ready`
      for store-writability readiness. What each one returns and
      what triggers each one to fail.
- [ ] **Prometheus `/metrics`** — the full metric surface, with
      dashboards: request rate, decide latency, feedback latency,
      refusal rate, per-capsule activity, meta-bandit candidate
      activity, store fsync rate.
- [ ] **Structured logging via `tracing`** — JSON log shape, the
      fields you'll grep on, default log level and how to change
      it per route.
- [ ] **Per-capsule signals** — lifecycle state, days since last
      feedback, current refusal rate, ADWIN drift event counts.

### Alerting

- [ ] **Pageable alerts** — Syntra down (health failing), store
      not writable (ready failing), 5xx rate above 1%, auth failures
      spiking.
- [ ] **Capsule-level alerts** — feedback arrival rate dropped to
      zero (downstream pipeline broken), refusal rate spiked above
      a threshold (regime shift), drift event fired (informational,
      route to a slow queue).
- [ ] **What NOT to page on** — lifecycle Warmup → Active flip
      (informational), single drift event (likely true positive),
      individual refusals.

### Backup & restore

- [ ] **Snapshots** — what `/snapshots` returns, when pre-mutation
      backups are written, how to inspect one before restoring.
- [ ] **Full backup pattern (pre-1E)** — copy the store volume.
      Atomic? When to fsync? Interaction with capsule writes during
      copy.
- [ ] **First-class backup endpoint (Phase 1E)** — what the planned
      endpoint will return.
- [ ] **Restore workflow** — replacing `memory.json` for one
      capsule, restoring an entire tenant, GDPR Article 17 deletion.

### Debugging

- [ ] **The five-step inspection trail** — `/report` → `/contexts`
      → `decision.jsonl` → `feedback.jsonl` → `audit.jsonl`. What
      each one tells you and how they link by `decisionId`.
- [ ] **Reading the meta-bandit panel** — interpreting candidate
      trial counts and cumulative rewards; what "the meta-bandit
      can't decide" looks like.
- [ ] **Reward function debugging** — the monotonicity check from
      the `writeup_reward_blindness.md` writeup. Run before you
      blame the bandit.
- [ ] **Feedback that never lands** — symptoms, common causes
      (wrong `decisionId` format, expired buffer in the feedback
      pipeline, auth scoped wrong), fixes.

### Capacity & scaling

- [ ] **Single-node throughput envelope** — measured decides /
      second by capsule complexity (discrete vs feature, no kernels
      vs full operational-intelligence chain).
- [ ] **Rate-limit tuning** — default is 1000 req/sec/principal.
      When to raise, when to scope.
- [ ] **Memory growth** — `memory.json` size by capsule traffic and
      `contextKey` cardinality. When to consider per-context
      decay.
- [ ] **Store growth** — JSONL log appendage rate, rotation /
      truncation strategy.

### Security operations

- [ ] **Token rotation** — how to rotate `SYNTRA_ADMIN_KEY` without
      downtime.
- [ ] **Scoped tokens** — `Admin` / `TenantAdmin` / `Read` roles,
      when to use each.
- [ ] **TLS proxy patterns** — nginx, Caddy, Cloudflare Tunnel.
- [ ] **Audit log shape and retention** — `audit.jsonl` event
      types, what each one means, how long to keep them.

## When a capsule isn't behaving

The short version of the debugging runbook:

1. **Read `/report`** for current strategy weights.
2. **Read `/contexts`** to confirm the request landed in the
   expected `contextKey`.
3. **Tail `decision.jsonl`** for what Syntra suggested.
4. **Tail `feedback.jsonl`** for which option was rewarded and
   whether the reward sign is correct.
5. **Tail `audit.jsonl`** for installs, policy changes, deletes,
   refusals, and change-detection events.

This is enough to disambiguate most "the bandit isn't learning what I
expected" reports. The repo-side
[`docs/operating.md`](https://github.com/ashhart/Syntra/blob/main/docs/operating.md)
has the long version.
