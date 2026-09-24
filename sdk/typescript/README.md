# @syntra/client

TypeScript client for the [Syntra](../../README.md) v2 HTTP API. It has no
runtime dependencies, ships as ESM and uses the global `fetch`, so it runs
on Node 20 and later, Deno, Bun and edge workers. Node's `fetch` keeps
connections alive between requests.

There are two ways to decide. `SyntraClient.decide` is one HTTP call, and
the server makes the decision. `LocalDecider` decides in-process in a few
microseconds, on the server's own Rust decision core compiled to
WebAssembly, and uploads its decisions in batches for the server to verify
and learn from. See [In-process decisions](#in-process-decisions).

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
| `modelText({ ifNoneMatch })` | `GET .../model?snapshot=true` | The response text, unparsed, or null as above |
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

## In-process decisions

`LocalDecider` keeps a copy of a capsule's published model and decides with
the Rust decision core compiled to WebAssembly. That is the code the server
runs, not a TypeScript port. A decision needs no network, and the decider
keeps working through a server outage with the last model it synced.
Decisions and rewards queue in memory and upload in batches. The server
replays every uploaded decision against the same model, input and seed,
stores it only if the eligible set, the chosen action and every
probability match, and learns from its rewards as if it had made the
decision itself.

```ts
import { LocalDecider } from "@syntra/client";

const router = await LocalDecider.connect({
  url: "http://127.0.0.1:8787",
  token: process.env.SYNTRA_TOKEN!, // a `read` token for the capsule is enough
  tenant: "acme",
  job: "prod",
  capsule: "router",
});

const d = router.decide({ task: "code", promptTokens: 812 }); // synchronous
const answer = await callModel(d.action); // your code
router.reward(d.decisionId, score(answer));

// At shutdown: stop the background timer and upload what is queued.
await router.close();
```

`connect` loads the WebAssembly module once per process, fetches the
published model and checks its tag. It takes every `SyntraClient` option
plus these:

| Option | Default | |
|---|---|---|
| `syncIntervalMs` | 1000 | How often a background timer uploads queued events and looks for a newer model. 0 turns it off; then call `flush()` and `sync()` yourself. In Node and Bun the timer does not keep the process alive. |
| `maxQueue` | 100000 | Most decisions plus rewards held for upload. At the bound `decide` and `reward` throw rather than drop events. |
| `onError` | none | Receives the errors of background uploads and syncs, which are dropped otherwise. |
| `wasm` | the package's `.wasm` | The compiled module, for runtimes that cannot load it themselves. See [Runtimes](#runtimes). |

| Member | |
|---|---|
| `decide(context?, { actions, exclude, baseline })` | Returns a `LocalDecision`: `decisionId`, `action`, `actionIndex`, `probability`, `ranking` (eligible actions, most probable first) and `modelVersion`. |
| `reward(decisionId, value, { idempotencyKey, detail })` | Queues a reward for a decision made here or on the server. |
| `flush()` | Uploads queued decisions, then queued rewards. Returns a `FlushReport`. |
| `sync()` | Fetches the published model if it changed. Returns true when it did. |
| `close()` | Stops the timer, then flushes. Afterwards `decide`, `reward` and `sync` throw. |
| `modelVersion`, `modelTag`, `pending` | The model in use, and the events not yet uploaded. |

What the decider does, in the order it happens:

- `decide` refuses what the server would refuse, with the server's message
  in a `SyntraError`: an unknown action id in `exclude`, a context that is
  not an object, a number that does not fit a 32-bit float. Nothing is
  queued for a refused call.
- The input is serialized once, at `decide`. Changing the context object
  afterwards does not change the upload, and the server parses the same
  text the decision was made from.
- Seeds come from `crypto.getRandomValues`. A capsule whose spec fixes a
  `seed` gets the same sequence of seeds in every decider, the one the
  Rust client and the server draw, so its decisions repeat exactly.
- Decision ids are `loc_`, the time in milliseconds as 11 hex digits, then
  64 random bits as 16 hex digits.
- Under `rewards: "sum"`, a reward without an `idempotencyKey` gets one when
  it is queued, so a retried upload counts it once.
- `flush` sends at most 1000 items and 4 MiB per request, the server's body
  limit. An event larger than that on its own cannot be uploaded; it is
  dropped and reported in `FlushReport.errors`. Rewards wait until every
  decision of the flush is stored, because the server refuses a reward
  for a decision it does not have.
- An item the server could not take right now goes back to the front of
  the queue and counts in `requeued`. For a decision that is a rejection
  marked `retryable`; for a reward, a result with status 503. Other
  refusals are final and count as rejected or failed. A transport or HTTP
  error puts everything back and makes `flush` throw.
- `sync` polls with `If-None-Match`, so an unchanged model costs a 304.
  Decisions already queued keep the tag of the model they were made with;
  the server keeps its 32 newest publications to replay them.

Limits:

- A capsule with a feature program cannot decide locally. `connect` and
  `sync` refuse its model, since the server refuses its uploads.
- Upload promptly. A decision whose model the server has retired, after a
  restart or 32 newer publications, is stored as `unverified` and left out
  of off-policy evaluation.
- The WebAssembly and native builds compute every probability the same
  way, with one exception: libm's `pow`, which sets SquareCB's exploration
  rate, can differ in the last bit between platforms. The server accepts
  probabilities within 1e-9, so those decisions still verify.
- The model takes 12 bytes per hash slot: 3 MiB at the default
  `learner.bits` of 18, and twice that while `sync` swaps models.
  WebAssembly memory grows to its peak and does not shrink.

### Latency

Measured with `npm run bench`, which reproduces the scenario of the Rust
benchmark `examples/bench_local.rs`: three actions, a model trained on
2000 decisions, 100,000 timed `decide` calls after 20,000 warm-up calls,
each including queueing the upload item. On an Apple M5 Max with macOS and
Node 26.8.1, over eight runs:

| | p50 | p90 | p99 | p99.9 | Decisions/s |
|---|---|---|---|---|---|
| `LocalDecider` in Node, WebAssembly | 3.4-3.9 µs | 3.7-4.1 µs | 5.6-7.0 µs | 15-29 µs | 243,000-266,000 |
| Rust `LocalDecider`, 4 runs | 1.3-1.8 µs | 1.5-2.0 µs | 2.6-4.2 µs | 5-20 µs | 388,000-637,000 |

The slowest Node call of each run took 2 to 12 ms, most likely garbage
collection with 100,000 decisions held in the queue. Numbers depend on the
machine, the runtime and the model. Measure on your own hardware before
relying on them.

### Runtimes

The package loads its WebAssembly module, `dist/wasm/syntra_wasm_bg.wasm`,
290 KB or 105 KB gzipped. Where the module's URL is a `file:` URL, as in
Node, Deno and Bun, the package reads the file with `node:fs`. Anywhere
else it fetches the URL, which bundlers for the browser rewrite because
the code names it with `new URL(..., import.meta.url)`.

| Runtime | Status |
|---|---|
| Node | CI runs the tests on Node 22, and they were written on Node 26. Node 20 is not tested. |
| Bun | A smoke test on Bun 1.3.14 uploaded 200 decisions, all accepted. |
| Browsers | A smoke test in Chromium 152 made 20,000 decisions, and the server accepted every upload. The server sends no CORS headers, so a page has to reach it through a proxy on the page's own origin. |
| Deno | Not tested. |
| Edge runtimes | Not tested. Cloudflare Workers and similar runtimes refuse to compile WebAssembly from bytes at run time. Import the module through the bundler and pass it in: `import wasm from "@syntra/client/wasm"`, then `LocalDecider.connect({ ...options, wasm })`. |

`wasm` also takes the module's bytes, a URL or a `fetch` response. Only the
first `connect` in a process uses it.

### The upload protocol

For implementations in other languages, here is what `LocalDecider` sends.
`SyntraClient` has each call.

1. `modelText()` fetches `GET .../model?snapshot=true` as text. The
   answer holds the `decide` section, the learner state as base64
   `snapshot`, `modelVersion` and `modelTag`. The `decide` section lists
   the actions, exploration, mode, baseline epsilon, hash bits, reward
   aggregation, an optional fixed `seed` and a `version`. Refuse a version
   newer than you understand. The seed is a u64, so parse the text with a
   parser that keeps such numbers exact. The tag is the first 16 hex digits
   of SHA-256 over three things: the decide section as serde_json prints it
   (keys sorted, no spaces), a zero byte, and the snapshot's last 32 bytes.
   `src/decision/tag.rs` computes it. Poll with `If-None-Match:
   "<modelTag>"`; the server answers 304 until it publishes a new model. A
   spec change publishes at once, learning at most once a second.
2. Each decision draws a random u64 seed. Queue an `UploadedDecision`:
   `decisionId` (1-128 characters from `A-Z a-z 0-9 _ . : -`), `tsMs`,
   `modelTag`, `modelVersion`, `seed` as a decimal string, `input` (the
   `context` and any `actions`, `excludedActions` and `baselineAction`),
   `chosenIndex`, `chosenId`, `probability`, and `pmf` aligned with
   `eligible`.
3. `uploadDecisions` sends them, at most 4096 items (`MAX_UPLOAD_ITEMS`)
   and 4 MiB per request. `accepted` counts stored items, re-uploads
   included; `duplicates` counts the re-uploads, and `unverified` the
   decisions whose model the server had retired. `rejected` lists the
   rest with their `index` and `error`. Upload a `retryable` one again
   later; the others are final, and the server audits them as
   `upload_rejected`. An id already stored with different content is
   rejected.
4. `uploadRewards` sends rewards after their decisions. Each result is a
   `RewardResponse` or `{ ok: false, status, error }`; status 503 means
   try again later, and 404 means the server has no such decision.
5. `tsMs` must be at most 7 days old and at most 5 minutes ahead of the
   server's clock.

## Development

The WebAssembly module is built from `wasm/`, a Rust crate that compiles
the repository's `src/decision/` into itself, so the decision code exists
once. Building it needs the `wasm32-unknown-unknown` target and the
wasm-bindgen CLI at exactly the version of the `wasm-bindgen` crate in
`wasm/Cargo.lock`:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128 --locked
```

```bash
cargo build --bin syntra   # the end-to-end tests start this server
cd sdk/typescript
npm ci
npm run build:wasm         # cargo build + wasm-bindgen into src/wasm/ (git-ignored)
npm run typecheck          # tsc over src/
npm test                   # builds the module, then node --test
npm run build              # dist/ with declarations and dist/wasm/
npm run bench              # decide latency, see Latency above
```

`typecheck`, `test`, `build` and `prepack` build the module first. The
generated files are not committed. The tests run the TypeScript sources
directly, which needs Node 22.18 or later. `test/client.test.ts` and
`test/local-e2e.test.ts` start `target/debug/syntra` on a free port with a
temporary store; set `SYNTRA_BIN` to use another binary.
`test/retry.test.ts` checks retries, and `test/local.test.ts` checks the
decider, against scripted `node:http` servers; `test/model.ts` builds
published models for them. The package's only dev dependency is
`typescript`, without `@types/node`, so `npm run typecheck` covers `src/`
and Node runs the tests unchecked.
