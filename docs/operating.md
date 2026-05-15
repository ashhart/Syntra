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

## System-Level Issues

### Syntra Will Not Start

- Check `docker compose logs syntra` or the process stderr.
- Confirm `LYCAN_ADMIN_KEY` is set, unless you intentionally started with `--dev-mode`.
- Confirm port `8787` is free or change the published port.
- Confirm the store volume is writable by the Syntra process.

### The Admin Console or API Returns 500

- Check the container logs first; server-side errors should be visible there.
- Call `/health` to confirm the process is still alive.
- Confirm the tenant, job, and capsule IDs exist in the store.
- Check `policy.json` and `current.lyc` for the capsule you are querying.

### Decisions Work But Feedback Is Not Recorded

- Confirm the feedback request receives a success response.
- If using `decisionId`, confirm that ID exists in `decision.jsonl`.
- If using explicit feedback, confirm `strategyId`, `option`, `reward`, and `contextKey` match the decision being rewarded.
- Confirm `feedback.jsonl` is growing after each request.

### Memory Disappears After Restart

- Confirm Docker is using the same named volume or host path after restart.
- Confirm `memory.json` exists under the expected tenant/job/capsule directory.
- Avoid running demos with temporary stores when you expect state to survive.

## Shadow-Mode Checklist

Before letting Syntra influence production behaviour:

- Run it in shadow mode for enough traffic to see stable context weights.
- Verify feedback coverage and reward signs.
- Compare Syntra's suggested option against the existing production choice.
- Keep the existing production path authoritative until the weights and logs make sense.
- Back up the store volume before enabling any mutation-heavy workflow.
