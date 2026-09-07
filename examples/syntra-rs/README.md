# syntra-client

Rust client for [Syntra](../../README.md), a self-hosted contextual-bandit
appliance. Mirrors the canonical Python `syntra_retry` package and the
[`syntra-go`](../syntra-go) port with idiomatic Rust conventions.

Apache-2.0. Standalone crate — not a member of any workspace.

## Install

```toml
[dependencies]
syntra-client = "0.1"
```

Dependencies are intentionally small:

- `reqwest` (blocking + json features; no default TLS roots — bring your own)
- `serde`, `serde_json`
- `thiserror`

Edition 2021. MSRV 1.70.

## Quickstart

```rust
use syntra_client::{retry::RetryClient, SyntraClient};

let syntra = SyntraClient::new(
    "http://localhost:8787",
    std::env::var("SYNTRA_ADMIN_KEY").unwrap(),
    "/tenants/myteam/jobs/retry/capsules/router",
);
let client = RetryClient::new(syntra);

let resp = client.get("https://api.example.com/users")?;
```

Every `request` call:

1. Asks Syntra `/decide` with per-host features
   (`recent_failure_rate`, `p99_latency_ms`, `hour`).
2. Executes the real HTTP request under the chosen retry policy.
3. Posts `/feedback` with a success bit minus a latency penalty.

## Fail-safe semantics

Syntra errors (transport, refusal, malformed response) never surface to the
caller. `RetryClient` silently falls back to `fallback_policy` (default
`single`). Feedback delivery failures are routed to an optional
`on_feedback_error` hook and otherwise dropped.

## Retry policies

| Index | Name               | Max retries | Initial backoff | Multiplier |
|-------|--------------------|-------------|-----------------|------------|
| 0     | `none`             | 0           | -               | -          |
| 1     | `single`           | 1           | -               | -          |
| 2     | `triple`           | 3           | -               | -          |
| 3     | `exponential_fast` | 3           | 100 ms          | 2.0        |
| 4     | `exponential_slow` | 3           | 500 ms          | 2.0        |

Status `< 500` is not retried. 5xx and transport errors trigger the backoff
sequence.

## Customisation

```rust
use std::sync::Arc;
use syntra_client::retry::{RetryClient, RetryPolicy, DEFAULT_POLICIES};

let client = RetryClient::builder(syntra)
    .fallback_policy(&DEFAULT_POLICIES[2])             // "triple"
    .tracker_window(200)
    .on_feedback_error(|e| eprintln!("feedback: {e}"))
    .build();
```

For backoff that must run in tests without real sleeps, supply a custom
`Sleeper` via `.sleeper(Arc::new(MyRecordingSleeper::default()))`.

## Tests

```
cargo test --release
```

Seven integration tests cover the same surface as the Go port:

1. Successful `/decide` + `/feedback` round-trip.
2. Refusal falls back to the configured default policy.
3. Syntra unreachable — fallback used, no error propagated.
4. Feedback failure does not break the request flow; hook fires.
5. Per-host tracker computes failure rate and respects the window cap.
6. `RetryPolicy::from_option` clamps OOB indices.
7. Exponential backoff respects the multiplier sequence.

### Why a hand-rolled stub server

The tests use a small `std::net::TcpListener`-based HTTP/1.1 stub
(`tests/retry.rs::MockServer`) rather than `httpmock` or `wiremock`. The stub
covers the few endpoints we need (path-keyed canned responses, request
recording, configurable failure for `/feedback`) in ~100 lines and keeps the
dev-dependency set to just `serde_json`. If your tests need a fuller surface
(URL matching, sequenced responses, request matchers), swap in `httpmock 0.7`
or `wiremock`.

## Standalone crate

The `Cargo.toml` declares an empty `[workspace]` so the crate is not pulled
into any parent workspace. Build and test from this directory directly.
