# Operating Syntra

Syntra is designed to be inspectable. If a capsule appears to learn the wrong thing, start with the logs and reports before changing code.

## First Checks

1. Confirm the request is using the expected tenant, job, capsule, and `contextKey`.
2. Call `/report` to see strategy weights and node IDs.
3. Call `/contexts` to see each known context bucket.
4. Check whether the app is sending feedback by `decisionId` or by explicit `strategyId` / `option`.
5. Confirm reward signs: positive rewards should mean "do more of this"; negative rewards should mean "do less of this".

## Store Files

For a capsule stored at:

```text
syntra-store/tenants/{tenant}/jobs/{job}/capsules/{capsule}/
```

the main diagnostic files are:

- `current.lyc` — installed compiled capsule.
- `policy.json` — runtime capability policy.
- `memory.json` — learned sidecar memory.
- `learning.json` — learning algorithm and safety settings.
- `decision.jsonl` — each decision returned by Syntra.
- `feedback.jsonl` — each feedback event accepted by Syntra.
- `audit.jsonl` — installs, policy changes, deletes, and other mutations.
- `evolution.jsonl` — accepted and rejected capsule evolution attempts.
- `snapshots/` — pre-mutation backups.

## Common Problems

### Weights Are Not Moving

- Check `feedback.jsonl` exists and is growing.
- Confirm `freezeLearning` is not enabled in `/learning`.
- Confirm feedback targets the right `decisionId` or `strategyId`.
- Confirm rewards are non-zero.

### The Wrong Context Is Learning

- Inspect `/contexts`.
- Confirm your app sends a stable `contextKey`.
- Avoid high-cardinality keys such as raw user IDs unless that is intentional.

### The Wrong Option Is Winning

- Inspect `decision.jsonl` and `feedback.jsonl` together.
- Check whether feedback is attached to the option that actually ran in production.
- Check delayed-outcome joins carefully. False rewards teach the wrong behaviour faster than no rewards.

### The Admin Console Shows Unauthorized

- Open `/admin`, enter the admin key, then let the console make authenticated API calls.
- `/health` and the login shell are public; all data endpoints require `Authorization: Bearer <key>`.

## Shadow-Mode Checklist

Before letting Syntra influence production behaviour:

- Run it in shadow mode for enough traffic to see stable context weights.
- Verify feedback coverage and reward signs.
- Compare Syntra's suggested option against the existing production choice.
- Keep the existing production path authoritative until the weights and logs make sense.
- Back up the store volume before enabling any mutation-heavy workflow.
