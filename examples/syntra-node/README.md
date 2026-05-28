# @ashhart/syntra-client

Node.js/TypeScript client for [Syntra](../../README.md) — a self-hosted adaptive
decision appliance. Provides a drop-in `RetryClient` that asks Syntra which HTTP
retry policy to use for each request, learns from outcomes, and falls back safely
when Syntra is unavailable.

## Install

```bash
npm install @ashhart/syntra-client
```

Requires Node 18+ (uses the built-in `fetch` API). No runtime dependencies.

## Quickstart

```typescript
import { RetryClient } from "@ashhart/syntra-client";

const client = new RetryClient({
  baseUrl: "http://localhost:8787",
  adminKey: process.env.SYNTRA_ADMIN_KEY!,
  capsulePath: "/tenants/myteam/jobs/retry/capsules/router",
  timeoutMs: 2000,
});

// Drop-in replacement for fetch
const response = await client.request("GET", "https://api.example.com/users");
```

For every request the client:

1. Calls `/decide` with the endpoint's rolling feature vector (failure rate,
   p99 latency, hour-of-day) to select a retry policy.
2. Executes the request with that policy — up to `maxRetries` attempts with
   configurable backoff.
3. Calls `/feedback` with a reward derived from success and total latency.

## Fail-safe semantics

If Syntra is unreachable, returns a refusal, or produces a malformed response,
the client falls back to `fallbackPolicy` (default: `"single"` — one retry,
no backoff). The fallback is transparent to the caller; `request()` never
rejects due to a Syntra-side failure.

Feedback failures are also silently swallowed by default. Pass an
`onFeedbackError` callback to observe them:

```typescript
const client = new RetryClient({
  // ...
  onFeedbackError: (err) => console.warn("feedback failed:", err),
});
```

## Policies

| Name               | Max retries | Initial backoff | Multiplier |
|--------------------|-------------|-----------------|------------|
| `none`             | 0           | —               | —          |
| `single`           | 1           | —               | —          |
| `triple`           | 3           | —               | —          |
| `exponential_fast` | 3           | 100 ms          | ×2         |
| `exponential_slow` | 3           | 500 ms          | ×2         |

5xx responses and transport errors are retried. Responses below 500 are not.

## Customization

```typescript
import { DEFAULT_POLICIES, RetryClient } from "@ashhart/syntra-client";

const client = new RetryClient({
  baseUrl: "http://localhost:8787",
  adminKey: "...",
  capsulePath: "...",
  fallbackPolicy: DEFAULT_POLICIES[2], // "triple"
});
```

You can also use `SyntraClient` directly for lower-level access to `/decide`
and `/feedback`:

```typescript
import { SyntraClient } from "@ashhart/syntra-client";

const syntra = new SyntraClient({ baseUrl, adminKey, capsulePath });
const decision = await syntra.decide({ contextKey: "support-low-cost" });
await syntra.feedback({ decisionId: decision.decisionId!, reward: 0.9 });
```

## Tests

```bash
npm install
npm test
```

Seven test suites cover: decide+feedback round-trip, refusal fallback, Syntra
unreachable, feedback failure isolation, per-host tracker math (failure rate and
p99), retry attempt count, and backoff timing with `jest.useFakeTimers()`.

## Build

```bash
npm run build   # emits to dist/
npm run lint    # tsc --noEmit
```

## License

Apache-2.0
