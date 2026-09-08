# syntra-client

Official Python SDK for [Syntra](../../README.md), the single-binary
decision runtime. Standard library only — `urllib`, `json`,
`dataclasses` — so adding it to a serving path costs no dependencies.

Talks to the canonical `/v1` surface (`docs/openapi.yaml`); the
unversioned paths the server still answers are deprecated aliases and the
SDK never uses them.

## Install

From a checkout of this repository, at the repo root:

```bash
python3 -m pip install ./sdk/python
```

Or run against the source tree with no install at all
(the SDK is a plain package directory):

```bash
PYTHONPATH=sdk/python python3 your_service.py
```

Python 3.9+.

## Quickstart

Mirrors `docs/quickstart-model-routing.md` step for step: appliance at
`http://127.0.0.1:8787`, admin key from `$KEY`, capsule at
`acme / llm-routing / model-router`.

```python
from syntra_client import SyntraClient, scope_read

admin = SyntraClient("http://127.0.0.1:8787", token=KEY)
admin.install_capsule("acme", "llm-routing", "model-router",
                      open("examples/demo_llm_model_router.lyc", "rb").read())
read = admin.create_token(
    scope_read("acme", "llm-routing", "model-router"), "gateway-read", 86400)

client = SyntraClient("http://127.0.0.1:8787", token=read.token)
d = client.decide("acme", "llm-routing", "model-router",
                  {"contextKey": "support-low-cost"})          # shadow: read-only
print(d.chosen_option, d.decision_id, d.refused)               # 0=cheap_fast 1=balanced 2=expensive_accurate
client.feedback("acme", "llm-routing", "model-router", d.decision_id, 0.85)
print(admin.report("acme", "llm-routing", "model-router")["strategies"][0]["graphWeights"])
```

`d.refused` true means `chosen_option` is `None` — call your fallback,
not the suggestion. `d.learned` is false unless you asked for
`learn=True` *and* the token may mutate state.

## Scope the tokens

`POST /v1/admin/tokens` is admin-only, and the runtime enforces scopes
per route:

| Token | `/decide` | `/report` `/contexts` `/decisions` | `/feedback` | `learn=true` |
|---|---|---|---|---|
| `scope_read(tenant, job, capsule)` | yes | yes | **403** (`AuthError`) | silently read-only |
| `scope_tenant_admin(tenant)` | yes | yes | yes | yes (same tenant) |
| legacy admin key | yes | yes | yes | yes |

So hand the serving gateway a `scope_read` token and keep the admin key
in a vault. `Token.__repr__` masks the raw value; so does
`SyntraClient.__repr__`.

## Errors

Every failure is a `SyntraError` subclass, and the message always carries
method, path, status, and the server's own text:

| Status | Class | Notes |
|---|---|---|
| 400 | `BadRequestError` | e.g. body is not a `.lyc` binary |
| 401, 403 | `AuthError` | bad token, or scope too narrow |
| 404 | `NotFoundError` | unknown capsule, or unknown `decisionId` |
| 409 | `ConflictError` | duplicate job id |
| 413 | `PayloadTooLargeError` | over the 4 MB body cap |
| 429 | `RateLimitedError` | `err.retry_after` seconds |
| 5xx | `ServerError` | runtime fault |
| — | `TransportError` | refused connection, DNS, timeout |

```python
from syntra_client import AuthError, RateLimitedError, SyntraClient

client = SyntraClient("http://127.0.0.1:8787", token=tok)
try:
    client.report("acme", "llm-routing", "model-router")
except RateLimitedError as err:
    back_off(err.retry_after)
except AuthError:
    raise  # token is wrong or too narrow — not worth retrying
```

## Retries

Reads (`health`, `metrics`, `report`, `contexts`, `memory`, `decisions`,
`whoami`, `list_tokens`) and capsule installs retry on `5xx` and
transport failures with exponential backoff (`retries=2` → three attempts
max, 0.2 s doubling, capped at 2 s).

`decide` and `feedback` are **never** retried, even on a timeout: both
append to the decision log, and `feedback` mutates learned weights, so a
blind retry would double-count a reward. If a write times out, reconcile
with `client.decisions(...)` before resending — an unconfirmed `feedback`
is exactly-once only if you make it so.

## API

`SyntraClient(base_url, token=None, timeout=10.0, retries=2)`

| Method | Route |
|---|---|
| `health()` / `ready()` / `metrics()` | `/health`, `/ready`, `/metrics` (infra; no `/v1` form exists) |
| `whoami()` | `GET /v1/auth/whoami` |
| `list_tenants()` / `create_job(tenant, job, ...)` | `GET`/`POST /v1/tenants...` |
| `capsule_path(tenant, job, capsule)` | path builder, e.g. for a raw call |
| `install_capsule(tenant, job, capsule, bytes)` → hash | `POST .../install` (raw `.lyc`, not base64) |
| `decide(tenant, job, capsule, context, learn=False)` → `Decision` | `POST .../decide` |
| `feedback(tenant, job, capsule, decision_id, reward=None, components=None, decision_index=None)` → bool | `POST .../feedback` |
| `report(...)` / `contexts(...)` / `memory(...)` → dict | `GET .../report`, `/contexts`, `/memory` |
| `decisions(...)` → list[dict] | `GET .../decisions` (NDJSON, parsed) |
| `delete_capsule(tenant, job, capsule)` | `DELETE .../capsules/{capsule}` |
| `create_token(scope, label, ttl_seconds=None)` → `Token` | `POST /v1/admin/tokens` |
| `list_tokens()` / `revoke_token(hash)` | `GET`/`DELETE /v1/admin/tokens` |

`decide` takes the body shape the capsule expects —
`{"contextKey": "rush_hour"}` for a discrete-context capsule,
`{"features": {...}}` for a feature-context capsule, with an optional
`"input"` object alongside either.

`feedback` takes a scalar `reward` **or** a `components` map (reduced by
the capsule's installed `reward_spec.json`), never both: the server
prefers `reward` and ignores `components` when both arrive, so the SDK
rejects that combination locally instead of shipping a payload whose
components are silently dropped.

## Tests

Both are plain scripts — no test runner, exit 0 on success.

**End-to-end smoke** boots a real appliance against a temp store and
drives install → decide → feedback → report/contexts/memory/decisions,
plus scoped-token semantics (`learn=true` downgrade, feedback `403`),
typed errors (`400`/`401`/`403`/`404`), and the metrics endpoint:

```bash
python3 sdk/python/tests/smoke.py
```

Builds `target/release/syntra` via `cargo build --release --quiet` if it
is missing.

**Retry-policy test** drives the client against a scripted server that
fails selected routes with `5xx` and counts attempts, since which calls
get replayed is invisible to a run against a healthy server:

```bash
python3 sdk/python/tests/retry_policy.py
```
