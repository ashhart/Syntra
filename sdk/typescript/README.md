# @syntra/client

Official TypeScript SDK for the [Syntra](../../README.md) decision appliance
(`/v1` HTTP surface). Zero runtime dependencies; Node ≥ 22 (any runtime with a
spec-compliant `fetch` works — Deno, Bun, workers, browsers pointed at your
appliance).

## Install

```bash
npm install @syntra/client
```

## Quickstart

Bring up the appliance (see [`docs/quickstart-model-routing.md`](../../docs/quickstart-model-routing.md)):

```bash
cargo build --release --bin syntra
syntra serve --addr 127.0.0.1:8787 --store /var/lib/syntra --admin-key "$KEY"
```

Then, from your serving code:

```ts
import { SyntraClient, AuthError, RateLimitedError } from "@syntra/client";

const syntra = new SyntraClient({
  baseUrl: "http://127.0.0.1:8787",
  token: process.env.SYNTRA_READ_TOKEN ?? "", // read-scoped token is enough for decide
});

// 1. Ask. Default is shadow mode: never mutates learned state.
const decision = await syntra.decide("acme", "llm-routing", "model-router", {
  contextKey: "support-low-cost",
});

if (decision.refused) {
  // OOD or under-calibrated: use your incumbent fallback, not the suggestion.
  routeToFallback();
} else {
  // 0=cheap_fast, 1=balanced, 2=expensive_accurate for the demo capsule.
  routeTo(options[decision.decisions[0].chosen_option]);

  // 2. Later, when the outcome resolves (judge score, retry, cost report):
  //    needs a write-capable token (tenant_admin or admin key).
  await syntraWithWriteToken.feedback("acme", "llm-routing", "model-router", {
    decisionId: decision.decisionId,
    reward: 0.85,
  });
}
```

Inspect learned state at any time:

```ts
const report = await syntra.report("acme", "llm-routing", "model-router");
console.log(report.hash, report.warmup.state, report.strategies[0].options);

const contexts = await syntra.contexts("acme", "llm-routing", "model-router");
const memory = await syntra.memory("acme", "llm-routing", "model-router");
const log = await syntra.decisions("acme", "llm-routing", "model-router"); // NDJSON parsed
const prometheusText = await syntra.metrics();
```

### Operator paths

```ts
const admin = new SyntraClient({ baseUrl, token: process.env.SYNTRA_ADMIN_KEY ?? "" });

await admin.createJob("acme", { id: "llm-routing", name: "LLM Routing" });

// install: raw `.lyc` bytes (magic header `LYCN`), e.g. from `syntra author`.
import { readFileSync } from "node:fs";
const installed = await admin.installCapsule(
  "acme", "llm-routing", "model-router",
  readFileSync("out/program.lyc"),
);
console.log(installed.hash); // SHA-256 of the graph, also in audit.jsonl

// Scoped tokens (raw value shown exactly once — store it in your vault).
const { token } = await admin.createToken({
  scope: { kind: "read", tenant: "acme", job: "llm-routing", capsule: "model-router" },
  label: "gateway-read",
  ttlSeconds: 86400,
});
```

## Behavior contract

| Call | Retried? | Notes |
|---|---|---|
| `decide`, `feedback` | **never** | Duplicating a decide/feedback would double-count rewards. Transport failures throw `SyntraNetworkError` — decide fallbacks yourself. |
| `report`, `contexts`, `memory`, `decisions`, `health`, `metrics` | 5xx only | Up to 3 extra GET attempts, exponential backoff (100 ms → 2 s). |
| `installCapsule`, `createToken`, `createJob` | never | Non-idempotent POSTs. |

- **Timeouts:** every request aborts after `timeoutMs` (default 10 000).
- **Tokens never leak:** `SyntraClient#toString()` and every error's
  `toString()` carry only a masked fingerprint (`smok…3f2a`).
- **`learn` semantics:** `decide(..., { learn: true })` sends `?learn=true`;
  `read`-scoped tokens are silently downgraded server-side (`learned: false`
  in the response stays your ground truth).
- **`read` tokens cannot post feedback** — you get a `ForbiddenError` (403).

## Errors

All non-2xx responses throw `SyntraApiError` subclasses:

| Status | Class | Extras |
|---|---|---|
| 401 | `AuthError` | missing/invalid bearer token |
| 403 | `ForbiddenError` (extends `AuthError`) | scope not allowed |
| 404 | `NotFoundError` | unknown tenant/job/capsule |
| 429 | `RateLimitedError` | `retryAfterMs` from `Retry-After` |
| other | `SyntraApiError` | `status`, `apiMessage`, parsed `body` |
| transport/timeout | `SyntraNetworkError` | `timedOut`, `cause` |

```ts
import { RateLimitedError, SyntraApiError } from "@syntra/client";

try {
  await syntra.decide("acme", "llm-routing", "model-router", { contextKey: "x" });
} catch (err) {
  if (err instanceof RateLimitedError) await nap(err.retryAfterMs);
  else if (err instanceof SyntraApiError) console.error(err.status, err.apiMessage);
  else throw err;
}
```

## Development

```bash
npm run typecheck   # tsc --noEmit (strict)
npm run smoke       # builds, then runs a real `syntra serve` e2e roundtrip
```

`scripts/smoke.ts` compiles the release binary if missing, spawns the
appliance on a random port with a throwaway store, installs
`examples/demo_llm_model_router.lyc`, and asserts the full
install → decide → feedback → report roundtrip, read-token learn downgrade,
feedback 403, bad-token 401, and unknown-capsule 404.
