# @syntra/client

TypeScript client for the [Syntra](../../README.md) v2 HTTP API. It has no
runtime dependencies, ships as ESM and uses the global `fetch`, so it runs
on Node 20 and later, Deno, Bun and edge workers. Node's `fetch` keeps
connections alive between requests.

Each `decide` is one HTTP call, and the server makes the decision. The
Rust and Python SDKs can also decide in-process with a `LocalDecider`; the
TypeScript one will wrap a WebAssembly build of the Rust core. See
[Local evaluation](#local-evaluation).

## Quickstart

Start a server as in the [root README](../../README.md#quickstart), then:

```ts
import { SyntraClient } from "@syntra/client";

const router = new SyntraClient({
  url: "http://127.0.0.1:8787",
  token: process.env.SYNTRA_TOKEN!,
  tenant: "acme",
  job: "prod",
  capsule: "router",
});

// Creates the capsule, or patches its spec.
await router.putSpec({
  actions: [
    { id: "small", features: { cost: 0.1 } },
    { id: "large", features: { cost: 1.0 } },
  ],
});

const d = await router.decide({ task: "code", promptTokens: 812 });
const answer = await callModel(d.action); // your code
await router.reward(d.decisionId, score(answer), { detail: { latencyMs: 840 } });
```

`putSpec` needs the operator key or a `tenant_admin` token. The service
that decides only needs a `read` token for its capsule:

```ts
const { token } = await admin.issueToken(
  { kind: "read", tenant: "acme", job: "prod", capsule: "router" },
  "checkout-api", // label
  86400,          // lifetime in seconds; omit for no expiry
);
```

## API

`new SyntraClient(options)` addresses one capsule.

| Option | Default | |
|---|---|---|
| `url` | | Server URL. A path prefix, for a proxy, is kept. |
| `token` | | Operator key or scoped token, sent as `Authorization: Bearer`. |
| `tenant`, `job`, `capsule` | | The capsule the capsule methods act on. |
| `timeoutMs` | 10000 | Per attempt. |
| `maxRetries` | 3 | Extra attempts for requests that are safe to repeat. 0 turns retries off. |
| `maxRetryDelayMs` | 10000 | Longest wait before a retry. |
| `fetch` | `globalThis.fetch` | Another `fetch` implementation. |

Every method returns the parsed response body, typed after
[docs/openapi.yaml](../../docs/openapi.yaml). `...` stands for
`/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}`.

| Method | Route | Returns |
|---|---|---|
| `decide(context?, { actions, exclude, baseline, eventId, durable })` | `POST .../decide` | `DecideResponse` |
| `reward(decisionId, value, { idempotencyKey, detail, durable })` | `POST .../reward` | `RewardResponse` |
| `putSpec(patch, { replace })` | `PUT .../spec` | `DecisionSpec` |
| `getSpec()` | `GET .../spec` | `DecisionSpec` |
| `model()` | `GET .../model` | `Model` |
| `model({ snapshot: true, ifNoneMatch })` | `GET .../model?snapshot=true` | `PublishedModel`, or null while `ifNoneMatch` is still the published tag |
| `decision(id)` | `GET .../decisions/{id}` | `DecisionWithRewards` |
| `decisions({ limit, after, since, until })` | `GET .../decisions` | `DecisionList` |
| `audits({ limit })` | `GET .../audits` | `AuditList` |
| `evaluate(body)` | `POST .../evaluate` | `OpeReport` |
| `promote(body)` | `POST .../promote` | `PromoteResult` |
| `uploadDecisions(items)` | `POST .../decisions:batch` | `DecisionUploadResponse` |
| `uploadRewards(items)` | `POST .../rewards:batch` | `RewardUploadResponse` |
| `issueToken(scope, label?, ttlSeconds?)` | `POST /v1/admin/tokens` | `IssueTokenResponse` |
| `listTokens()` | `GET /v1/admin/tokens` | `TokenList` |
| `revokeToken(hash)` | `DELETE /v1/admin/tokens/{hash}` | `Revoked` |
| `whoami()` | `GET /v1/auth/whoami` | `WhoAmI` |
| `health()` | `GET /health`, no credential | `Health` |

A few calls need more than the table says.

- `decide` sends `exclude` as `excludedActions` and `baseline` as
  `baselineAction`. `actions` replaces the spec's actions for one request.
- `putSpec` takes a JSON merge patch. `null` restores a field's default
  and arrays replace whole. With `{ replace: true }` the patch applies to
  the default spec instead of the current one.
- `decisions` pages oldest first. Pass a page's `next` as `after` to get
  the next page; `next` is null on the last one.
- `promote` applies a spec patch only if every gate passes. A failed gate
  is a result, `{ promoted: false, error, report }`, not an exception.
  The other 409s, nothing to evaluate or a spec that changed during the
  evaluation, throw `HttpError`.
- The token routes need the operator key.

## Errors

Every error the client throws for a request is a `SyntraError`.

- `HttpError` means the server answered with a status outside 2xx, an
  error or a redirect the client does not follow. It carries `status`,
  `body`, `method`, `path`, `requestId` and, when the answer had
  `Retry-After`, `retryAfterMs`. `body` is the parsed JSON, usually
  `{"error": "..."}`, and `requestId` is the `x-request-id` that the
  server's log lines carry. The message reads
  `PUT /v1/tenants/acme/jobs/prod/capsules/router/spec: HTTP 403: forbidden: scope does not allow this action`.
- `TransportError` means no answer arrived. The connection failed, or the
  attempt timed out and `timedOut` is true. The request may still have
  reached the server.
- Bad arguments, such as a non-finite reward or a decision id of `..`,
  throw a `TypeError` before any request is sent.

The token never appears in an error message. The client keeps it in a
private field, so `console.log(client)` and `JSON.stringify(client)` leave
it out as well.

## Retries

The client retries a request only when sending it twice cannot change the
result.

| Call | Retried |
|---|---|
| Every GET | yes |
| `decide` with `eventId` | yes. The server answers a repeated request with the stored decision and `replayed: true`. |
| `reward` with `idempotencyKey` | yes. The server applies a key once. |
| `uploadDecisions` | yes. The server stores a re-uploaded decision once. |
| `uploadRewards` | when every item has an `idempotencyKey` |
| Anything else | no |

A retry follows a transport error, a 503 or a 429. When the server sends
`Retry-After`, the client waits that long. Otherwise it backs off, starting
at 100 ms and doubling each attempt, with jitter, up to `maxRetryDelayMs`.
When `Retry-After` asks for longer than `maxRetryDelayMs`, the client
throws the `HttpError` at once and leaves the wait to the caller. The
client does not follow redirects, because a redirected POST would turn
into a GET.

The server tells an `eventId` retry from a conflict by comparing request
bytes. The client serializes a body once and resends the same bytes, and
the same arguments always serialize the same way. If you repeat a call
yourself, pass the same arguments, with the context's keys in the same
order. A different request with a used `eventId` gets a 409.

## Local evaluation

`LocalDecider` is exported as an interface only. It mirrors the Python
`LocalDecider`: a synchronous `decide`, then `reward`, `flush`, `sync`,
`close`, `modelVersion`, `modelTag` and `pending`. The implementation will
wrap a WebAssembly build of the Rust decision core. The server accepts an
uploaded decision only if it replays exactly, and a TypeScript copy of the
feature hashing, learner and sampler would drift from the Rust one.
`SyntraClient` already has the calls an implementation needs. The
protocol, from `src/server/upload.rs` and
[docs/design/v2-decision-core.md](../../docs/design/v2-decision-core.md),
runs in five steps.

1. Fetch the published model with `model({ snapshot: true })`. The answer
   holds the `decide` section, the learner state as base64 `snapshot`,
   `modelVersion` and `modelTag`. The `decide` section lists the actions,
   exploration, mode, baseline epsilon, hash bits, reward aggregation, an
   optional fixed seed and a `version`; refuse a version newer than you
   understand. Poll with `model({ snapshot: true, ifNoneMatch: modelTag })`.
   It returns null, from a 304, until the server publishes a new model. A
   spec change publishes at once, and learning at most once a second.
2. Decide locally. Each decision draws a random u64 seed and gets an id of
   1-128 characters from `A-Z a-z 0-9 _ . : -`; the Rust SDK uses `loc_`
   plus the time and random bits. Queue an `UploadedDecision` with the
   `decisionId`, `tsMs`, `modelTag`, `modelVersion`, `chosenIndex`,
   `probability`, `pmf` and `eligible`. Send `seed` as a decimal string,
   since JavaScript numbers lose u64 precision. `input` holds the
   `context` and any `actions`, `excludedActions` or `baselineAction`.
3. Upload decisions with `uploadDecisions`, up to 4096 items per call
   (exported as `MAX_UPLOAD_ITEMS`) and 4 MiB. The server replays each
   item on the model its tag names, with the same input and seed, and
   stores it only if the eligible set, the chosen index and every
   probability match to within 1e-9. `accepted` counts stored items,
   re-uploads included, and `duplicates` counts the re-uploads. `rejected`
   lists the rest with their `index` and `error`. Upload a `retryable`
   one again later. The others are final, and the server audits them as
   `upload_rejected`. An id already stored with different content is
   rejected.
4. Upload rewards with `uploadRewards`, always after their decisions. A
   reward for a decision the server has not stored fails with status 404.
   Under `rewards: "sum"`, give each reward its own `idempotencyKey` when
   you queue it, so a retried upload counts it once. Under `"first"` the
   server keys rewards by decision id. Each result is a `RewardResponse`
   or `{ ok: false, status, error }`, and status 503 means try again later.
5. Upload promptly. The server keeps its 32 newest publications in memory,
   so under continuous learning a decision must be uploaded within about
   32 seconds of its model being replaced, and a server restart retires
   them all. `tsMs` must be at most 7 days old and at most 5 minutes ahead
   of the server's clock. Capsules with a feature program refuse uploads.

## Development

```bash
cargo build --bin syntra   # the end-to-end tests start this server
cd sdk/typescript
npm ci
npm run typecheck          # tsc over src/
npm test                   # node --test, a few seconds
npm run build              # dist/ with declarations
```

The tests run the TypeScript sources directly, which needs Node 22.18 or
later. `test/client.test.ts` starts `target/debug/syntra` on a free port
with a temporary store; set `SYNTRA_BIN` to use another binary.
`test/retry.test.ts` checks retries against a scripted `node:http` server.
The package's only dev dependency is `typescript`, without `@types/node`,
so `npm run typecheck` covers `src/` and Node runs the tests unchecked.
