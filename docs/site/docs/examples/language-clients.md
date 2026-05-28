# Language clients

Syntra is a plain HTTP API — anything that can POST JSON can drive
it. Four official client libraries mirror the Python `syntra_retry`
shape in Go, Node, Java, and Rust. All four ship as worked example
packages with tests, a `setup_capsule` helper, and idiomatic
fallback semantics.

| Language | Repository path | Module |
|----------|-----------------|--------|
| Go       | [`examples/syntra-go/`](https://github.com/ashhart/Syntra/tree/main/examples/syntra-go) | `github.com/ashhart/syntra-go` |
| Node     | [`examples/syntra-node/`](https://github.com/ashhart/Syntra/tree/main/examples/syntra-node) | `@ashhart/syntra-client` (TypeScript) |
| Java     | [`examples/syntra-java/`](https://github.com/ashhart/Syntra/tree/main/examples/syntra-java) | Maven |
| Rust     | [`examples/syntra-rs/`](https://github.com/ashhart/Syntra/tree/main/examples/syntra-rs) | `syntra` |

The Python pack is [`examples/retry-tuning/`](retry-tuning.md), the
canonical one. The other four mirror its public surface and its
fail-safe semantics.

## The pattern, in any language

Every client does the same four things:

1. **Compute features.** Maintain a rolling-window tracker of recent
   destination behaviour (latency, error rate, queue depth — whatever
   makes sense for the domain).
2. **Call `/decide`.** Send the feature vector. Receive a
   `decisionId` and an option index.
3. **Apply the option.** Translate index → policy and run the real
   request.
4. **Call `/feedback`.** Send the observed reward against the
   `decisionId`.

The wire contract is identical across languages — what differs is the
HTTP client, the error handling idioms, and the package conventions.

## Go

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

No external dependencies — stdlib only. See
[`examples/syntra-go/README.md`](https://github.com/ashhart/Syntra/blob/main/examples/syntra-go/README.md)
for the full surface.

## Node (TypeScript)

The Node client ships TypeScript types, runs on Node ≥ 18 (uses
native `fetch`), and mirrors the Python pattern. See
[`examples/syntra-node/README.md`](https://github.com/ashhart/Syntra/blob/main/examples/syntra-node/README.md).

## Java

Maven artifact, mirrors `RetryClient`. See
[`examples/syntra-java/README.md`](https://github.com/ashhart/Syntra/blob/main/examples/syntra-java/README.md).

## Rust

Cargo crate, async + sync APIs. See
[`examples/syntra-rs/README.md`](https://github.com/ashhart/Syntra/blob/main/examples/syntra-rs/README.md).

## Fail-safe semantics across all four

Every client treats Syntra as a *best-effort* augmentation, not a
hard dependency on the request path:

- Syntra unreachable → use the configured fallback policy.
- Syntra returns `refused: true` → use the fallback; still post
  `/feedback` for audit if a `decisionId` was provided.
- Syntra returns a malformed response → use the fallback.
- `/feedback` POST fails → silently drop (or pass to an
  `OnFeedbackError` hook in the Go client).

A Syntra outage degrades the adaptive layer to a fixed fallback. It
does not break the caller's request flow.

## See also

- [HTTP retry tuning](retry-tuning.md) — the canonical Python pack
  these libraries mirror.
- [Refusal](../concepts/refusal.md) — the signal the clients check
  to decide whether to fall back.
- [API reference](../reference/api.md) — the wire contract.
