# Capabilities

Capabilities are Rust-native kernels callable from Lycan via `!cap`. They provide system access and computational primitives that would be impractical in the Lycan language itself.

## Calling a capability

```lisp
(!cap "capability.name" arg1 arg2 ...)
```

## Registry

| Name | Package | Purity | Effects |
|---|---|---|---|
| `runtime.capabilities` | runtime | Pure | — |
| `runtime.input` | runtime | Pure | — |
| `runtime.inputGet` | runtime | Pure | — |
| `file.exists` | io | ReadOnly | file_read |
| `file.readText` | io | ReadOnly | file_read |
| `file.writeText` | io | Effectful | file_write |
| `http.get` | net | ReadOnly | network |
| `http.post` | net | Effectful | network |
| `json.get` | data | Pure | — |
| `json.has` | data | Pure | — |
| `json.len` | data | Pure | — |
| `sql.sqliteQuery` | data | ReadOnly | file_read |
| `stats.mean` | math | Pure | — |
| `stats.stdDev` | math | Pure | — |
| `stats.min` | math | Pure | — |
| `stats.max` | math | Pure | — |
| `stats.percentile` | math | Pure | — |
| `series.ewmaForecast` | ops | Pure | — |
| `ops.autoScaleRecommend` | ops | Pure | — |
| `nav.*` | astro | Pure/ReadOnly | — / file_read |
| `astro.lambertSolve` | astro | Pure | — |

## Purity levels

- **Pure**: no side effects, deterministic
- **ReadOnly**: reads external state but doesn't modify it
- **Effectful**: can write files, send HTTP requests, etc.

## Policy enforcement

When a capsule runs under policy, capabilities check their effects against the policy:

```
capability=file.readText effect=file_read denied by policy
```

File capabilities are sandboxed to the capsule working directory. Network capabilities require explicit `allowed_hosts`. Private networks are denied by default.

## Input access

```lisp
;; Get full JSON input (from --input flag or API body)
($ data (!cap "runtime.input"))

;; Dot-path accessor with numeric array indexes
($ symbol (!cap "runtime.inputGet" "request.body.items.0.symbol"))

;; Missing paths return null
($ missing (!cap "runtime.inputGet" "does.not.exist"))
```

## Listing capabilities

```bash
lycan capabilities
```

Returns the full registry as JSON with metadata (inputs, outputs, purity, effects, cost, safety).
