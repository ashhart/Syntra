# HTTP API

The reference is [`openapi.yaml`](openapi.yaml) (OpenAPI 3.0): every route,
request and response schema, status code and the scopes that may call it.
`tests/openapi_drift.rs` fails the build when the router and the document
disagree. This page is the short version.

- **Base URL.** `http://127.0.0.1:8787` unless `syntra serve --addr` says
  otherwise. Put a TLS-terminating proxy in front of it
  ([deployment.md](deployment.md)).
- **Versions.** `/v1/...` is canonical. The same paths without `/v1` still
  answer, as deprecated aliases with `Deprecation: true` and a
  `Link: </v1/...>; rel="successor-version"` header. `/health`, `/ready`,
  `/metrics` and `/admin` are unversioned.
- **Credentials.** `Authorization: Bearer <key>` or
  `Ocp-Apim-Subscription-Key: <key>`, with the operator admin key or a
  scoped token from `POST /v1/admin/tokens`. Missing or unknown keys get
  401; a scope that does not cover the route gets 403. A server started
  with `--dev-mode` treats every request as the operator.
- **Scopes.** `admin` reaches everything; `tenant_admin` every tenant, job
  and capsule route of one tenant; `read` decide, reward, uploads and the
  read routes of one capsule. The Scopes column below lists who may call
  each route.
- **Errors.** `{"error": "<message>"}`. Bodies over 4 MiB get 413, bodies
  not received within 30 s get 408. 429 (the credential's rate limit, or
  two evaluations already running) and 503 (event log backlogged, or a
  `durable` commit not confirmed within 5 s) carry `Retry-After`. The
  Personalizer routes answer errors in Personalizer's shape,
  `{"error": {"code", "message"}}`.
- **Request ids.** Every response carries `x-request-id`, your
  `X-Request-Id` if you sent one.

`...` below stands for `/v1/tenants/{tenant}/jobs/{job}`. Tenant, job and
capsule names are 1-128 characters from `A-Z a-z 0-9 _ - .`, not starting
with `.`. Path segments are percent-decoded, so a decision id such as
`order:42` can also be requested as `order%3A42`.

| Method | Path | Scopes | Operation |
|---|---|---|---|
| GET | `/health` | none | Liveness probe |
| GET | `/ready` | none | Readiness probe |
| GET | `/metrics` | admin | Prometheus metrics |
| GET | `/admin` | none | Admin console |
| GET | `/v1/auth/whoami` | admin, tenant_admin, read | Describe the presented credential |
| GET | `/v1/capabilities` | admin, tenant_admin, read | Capabilities available to feature programs |
| POST | `/v1/admin/tokens` | admin | Issue a scoped token |
| GET | `/v1/admin/tokens` | admin | List unexpired tokens |
| DELETE | `/v1/admin/tokens/{tokenHash}` | admin | Revoke a token |
| GET | `/v1/admin/capsules` | admin | List every capsule in the store |
| GET | `/v1/tenants` | admin, tenant_admin, read | List the tenants the caller administers |
| DELETE | `/v1/tenants/{tenant}` | admin, tenant_admin | Delete a tenant |
| GET | `/v1/tenants/{tenant}/jobs` | admin, tenant_admin | List jobs |
| POST | `/v1/tenants/{tenant}/jobs` | admin, tenant_admin | Create a job |
| GET | `/v1/tenants/{tenant}/jobs/{job}` | admin, tenant_admin | Get a job |
| DELETE | `/v1/tenants/{tenant}/jobs/{job}` | admin, tenant_admin | Delete a job |
| GET | `/v1/tenants/{tenant}/jobs/{job}/capsules` | admin, tenant_admin | List a job's capsules |
| GET | `.../capsules/{capsule}` | admin, tenant_admin, read | Get a capsule |
| DELETE | `.../capsules/{capsule}` | admin, tenant_admin | Delete a capsule |
| POST | `.../capsules/{capsule}/decide` | admin, tenant_admin, read | Choose an action |
| POST | `.../capsules/{capsule}/reward` | admin, tenant_admin, read | Record a reward |
| POST | `.../capsules/{capsule}/feedback` | admin, tenant_admin, read | Record a reward (alias of `/reward`) (deprecated) |
| GET | `.../capsules/{capsule}/spec` | admin, tenant_admin, read | Get the decision spec |
| PUT | `.../capsules/{capsule}/spec` | admin, tenant_admin | Create a capsule or patch its spec |
| POST | `.../capsules/{capsule}/mode` | admin, tenant_admin | Switch the serving mode |
| POST | `.../capsules/{capsule}/install` | admin, tenant_admin | Install a feature program |
| DELETE | `.../capsules/{capsule}/program` | admin, tenant_admin | Remove the feature program |
| GET | `.../capsules/{capsule}/policy` | admin, tenant_admin, read | Get the execution policy |
| PUT | `.../capsules/{capsule}/policy` | admin, tenant_admin | Replace the execution policy |
| DELETE | `.../capsules/{capsule}/logs` | admin, tenant_admin | Erase decisions and rewards |
| GET | `.../capsules/{capsule}/decisions` | admin, tenant_admin, read | List logged decisions |
| GET | `.../capsules/{capsule}/decisions/{decisionId}` | admin, tenant_admin, read | Get one decision and its rewards |
| GET | `.../capsules/{capsule}/model` | admin, tenant_admin, read | Get the model |
| POST | `.../capsules/{capsule}/decisions:batch` | admin, tenant_admin, read | Upload locally made decisions |
| POST | `.../capsules/{capsule}/rewards:batch` | admin, tenant_admin, read | Upload rewards |
| POST | `.../capsules/{capsule}/evaluate` | admin, tenant_admin | Off-policy evaluation on the capsule's log |
| POST | `.../capsules/{capsule}/promote` | admin, tenant_admin | Apply a spec patch if it passes off-policy evaluation |
| POST | `.../capsules/{capsule}/personalizer/v1.0/rank` | admin, tenant_admin, read | Rank actions (Personalizer) |
| POST | `.../capsules/{capsule}/personalizer/v1.0/events/{eventId}/reward` | admin, tenant_admin, read | Report a reward (Personalizer) |
| POST | `.../capsules/{capsule}/personalizer/v1.0/events/{eventId}/activate` | admin, tenant_admin, read | Activate a deferred event (Personalizer) |
| GET | `.../capsules/{capsule}/personalizer/v1.0/configurations/service` | admin, tenant_admin, read | Service configuration (Personalizer) |
| PUT | `.../capsules/{capsule}/personalizer/v1.0/configurations/service` | admin, tenant_admin | Update the service configuration (Personalizer) |
| GET | `.../capsules/{capsule}/audits` | admin, tenant_admin, read | List audit events |

A key scoped to one capsule (`read`) also reaches that capsule's
Personalizer routes without the capsule path, which is what an unmodified
Personalizer client calls: `POST /personalizer/v1.0/rank`,
`POST /personalizer/v1.0/events/{eventId}/reward`,
`POST /personalizer/v1.0/events/{eventId}/activate` and
`GET /personalizer/v1.0/configurations/service`. Changing the service
configuration needs `tenant_admin` or `admin`, on the capsule path
([migrating/personalizer.md](migrating/personalizer.md)).

Walkthroughs: [quickstart.md](quickstart.md) for decide, reward, tokens,
local evaluation and promotion; [operating.md](operating.md) for metrics,
audits, backups and limits.
