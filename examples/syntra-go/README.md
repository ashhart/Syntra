# syntra-go

Go integration library for [Syntra](../../README.md), a self-hosted contextual-bandit appliance.
Mirrors the canonical Python `syntra_retry` package with idiomatic Go conventions.
Apache-2.0. No external dependencies — stdlib only.

## Install

```
go get github.com/ashhart/syntra-go
```

## Quickstart

```go
import (
    syntra "github.com/ashhart/syntra-go"
    "github.com/ashhart/syntra-go/retry"
)

client := retry.NewRetryClient(retry.ClientOptions{
    SyntraOptions: syntra.ClientOptions{
        BaseURL:     "http://localhost:8787",
        AdminKey:    os.Getenv("SYNTRA_ADMIN_KEY"),
        CapsulePath: "/tenants/myteam/jobs/retry/capsules/router",
        Timeout:     2 * time.Second,
    },
})

req, _ := http.NewRequestWithContext(ctx, http.MethodGet, "https://api.example.com/users", nil)
resp, err := client.Do(req)
```

Every call to `Do`:

1. Queries Syntra `/decide` with per-host features (recent failure rate, p99 latency, hour of day).
2. Executes the real request with the chosen retry policy.
3. Posts `/feedback` with success bit and latency-penalised reward.

## Fail-safe semantics

Syntra errors (unreachable, refusal, malformed response) never surface to the caller.
The `RetryClient` silently falls back to `FallbackPolicy` (default: `single`).
Feedback errors are passed to `OnFeedbackError` if set, otherwise dropped.
No panics in any production path.

## Retry policies

| Name               | Max retries | Initial backoff | Multiplier |
|--------------------|-------------|-----------------|------------|
| `none`             | 0           | -               | -          |
| `single`           | 1           | -               | -          |
| `triple`           | 3           | -               | -          |
| `exponential_fast` | 3           | 100 ms          | 2.0        |
| `exponential_slow` | 3           | 500 ms          | 2.0        |

Status < 500 is never retried. 5xx and transport errors trigger the backoff sequence.

## Customisation

```go
retry.NewRetryClient(retry.ClientOptions{
    SyntraOptions: syntra.ClientOptions{ /* ... */ },

    // Which policy to use when Syntra cannot be reached.
    FallbackPolicy: retry.PolicyByName(retry.PolicyTriple),

    // Plug in a custom *http.Client (e.g. with TLS config).
    HTTPClient: myHTTPClient,

    // Called with feedback errors instead of silently dropping them.
    OnFeedbackError: func(err error) { slog.Warn("feedback", "err", err) },

    // Rolling window size for per-host stats (default 100).
    TrackerWindow: 200,
})
```

### Using the base client directly

```go
c := syntra.NewClient(syntra.ClientOptions{
    BaseURL:     "http://localhost:8787",
    AdminKey:    "...",
    CapsulePath: "/tenants/t/jobs/j/capsules/c",
})

decision, err := c.Decide(ctx, syntra.DecideBody{
    Features: map[string]float64{"recent_failure_rate": 0.1},
})
// or discrete context:
decision, err = c.Decide(ctx, syntra.DecideBody{ContextKey: "premium"})

err = c.Feedback(ctx, syntra.FeedbackBody{
    DecisionID: decision.DecisionID,
    Reward:     0.85,
})
```

## Tests

```
cd examples/syntra-go
go test ./... -v
```

Seven tests cover:

1. Successful decide + feedback round-trip with a mocked Syntra server.
2. Refusal falls back to the configured default policy.
3. Unreachable Syntra — fallback used, no panic.
4. Feedback failure does not return an error; `OnFeedbackError` hook fires.
5. Per-host tracker accumulates outcomes and computes failure rate correctly.
6. Retry policy executes the correct number of attempts (none / single / triple).
7. Exponential backoff respects the multiplier sequence.

All tests use `httptest.Server` — no network required.
