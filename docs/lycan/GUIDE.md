# Lycan Guide

> **What moved (2026-09-23).** This repository ships one execution
> backend: the graph compiler, verifier and graph executor. The
> tree-walking interpreter, the REPL, `!lambert` and the `nav.*`,
> `comb.*` and `astro.*` capabilities, the learning and evolution commands
> (`decide`, `feedback`, `learn-report`, `improve-report`, `evolve`,
> `transfer-weights`, `capsule improve`, `capsule apply-proposal`) and the
> science demos moved to the Lycan Lab repository at split commit
> `15f5441`.

Lycan is a small S-expression language whose programs compile into graph
binaries that can be inspected, verified, sandboxed and executed. In
Syntra, a Lycan program is a capsule's optional feature program: it runs
before each decision to compute derived features or restrict the
eligible actions, under the capsule's execution policy.

This guide covers the language and the `lycan` CLI: write `.lycs`, compile
`.lyc`, inspect the graph, use strategy and choice nodes, read injected
input, call native capabilities and package capsules.

## Quick start

```bash
# Build
cargo build --release

# Run a program
./target/release/lycan examples/lycan/hello.lycs

# Compile to a graph binary, then run the binary
./target/release/lycan compile examples/lycan/hello.lycs
./target/release/lycan examples/lycan/hello.lyc
```

## Language basics

Lycan uses S-expression syntax. Every construct is a tagged, parenthesized list.

### Values

```
42              ;; integer
3.14            ;; float
"hello"         ;; string
true / false    ;; boolean
null            ;; null
(A 1 2 3)       ;; array
```

### Variables

```
($ x 42)        ;; immutable binding
($! x 0)        ;; mutable binding
(= x 10)        ;; reassign mutable
```

### Arithmetic and comparison

All operators are prefix:

```
(+ 2 3)         ;; 5
(- 10 4)        ;; 6
(* 7 8)         ;; 56
(/ 7 2)         ;; 3.5
(% 17 5)        ;; 2
(== 5 5)        ;; true
(!= 5 3)        ;; true
(< 3 5)         ;; true
(&& true false)  ;; false
(|| false true)  ;; true
(not true)       ;; false
```

### Functions

```
;; Named function
(F add (a b) (+ a b))
(!p (add 3 4))          ;; prints 7

;; Lambda
($ square (\ (x) (* x x)))
(!p (square 7))         ;; prints 49

;; Recursive
(F fib (n)
  (? (<= n 1) n
    (+ (fib (- n 1)) (fib (- n 2)))))
(!p (fib 10))           ;; prints 55
```

### Control flow

```
;; If/else (expression — returns a value)
(? (> x 10) "big" "small")

;; Chained if
(? (> x 20) "high"
  (? (> x 10) "medium"
    "low"))

;; While loop
($! i 0)
(W (< i 10) (!p i) (= i (+ i 1)))

;; For-each
(each x (A 1 2 3 4 5) (!p x))

;; Repeat N times
(# 5 (!p "hello"))
```

### Collections

```
($ arr (A 10 20 30))     ;; array
(I arr 1)                ;; index: 20
(!len arr)               ;; length: 3
(.. 1 5)                 ;; range: (A 1 2 3 4)
(+ (A 1 2) (A 3 4))     ;; concat: (A 1 2 3 4)
```

### Pipelines

```
;; Filter, map, reduce
(|? (A 1 2 3 4 5) (\ (x) (> x 3)))           ;; (A 4 5)
(|* (A 1 2 3) (\ (x) (* x 2)))               ;; (A 2 4 6)
(|+ (A 1 2 3 4 5) (\ (a b) (+ a b)) 0)       ;; 15

;; Chained: filter evens, double, sum
(|+ (|* (|? data (\ (x) (== (% x 2) 0)))
        (\ (x) (* x 2)))
    (\ (a b) (+ a b)) 0)
```

### Built-in functions

```
(!p expr)           ;; print
(!r)                ;; read line from stdin
(!len x)            ;; length of array or string
(!str x)            ;; convert to string
(!num "42")         ;; parse string to number
(!split "a b c" " ") ;; split string: (A "a" "b" "c")
(!chars "abc")      ;; chars: (A "a" "b" "c")
(!abs -5)           ;; absolute value: 5
(!sin 1.0)          ;; sine
(!cos 1.0)          ;; cosine
(!sqrt 144.0)       ;; square root: 12
(!ln 2.718)         ;; natural log
(!exp 1.0)          ;; e^x
(!atan2 y x)        ;; arc tangent
(!floor 3.7)        ;; floor: 3
(!round 3.5)        ;; round: 4
```


## Strategy nodes

Several implementations of one computation compete inside a node:

```
;; Two strategies for computing sum(1..N)
(F sum_loop (n)
  ($! total 0) ($! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)

(F sum_formula (n)
  (/ (* n (+ n 1)) 2))

($ result (strategy (sum_loop 5000) (sum_formula 5000)))
(!p result)     ;; 12502500
```

Each time a strategy node runs, the executor checks its options against
the node's contract, punishes an option that disagrees, and shifts weight
toward the fastest correct one. The weights live in the graph while it
runs; the `lycan` CLI does not write them back to the `.lyc` file, so each
run starts from the compiled weights.

### Contracts

- **WithinTolerance** (default): all options must agree within epsilon. Incorrect options are punished.
- **SameOutput**: all options must produce identical output.
- Both require pure computation: no side effects inside strategy options.

## Adaptive nodes

### choice: weights decide

```
($ action (choice "scale_up" "hold" "scale_down"))
```

### guard: fast path with fallback

```
($ result (guard (> cache_valid true) cached_value (compute_fresh)))
```

The assumption is checked first; if it holds the fast path runs, otherwise
the fallback.

### feedback: a reward inside the program

```
($ pick (choice "a" "b"))
(feedback pick 1.0)      ;; positive reward
(feedback pick -0.5)     ;; negative reward
```

This updates the target node's weights for the rest of the run. Learning
from real outcomes across requests is Syntra's job: decisions and rewards
go through a capsule's decision spec, and a Lycan program there computes
features.

## Injected input

A program reads JSON injected with `--input` (in Syntra, the decision's
context) through capabilities:

```bash
./target/release/lycan examples/lycan/json-input.lycs --input examples/lycan/request.json
```

```
;; The whole input
($ data (!cap "runtime.input"))

;; A nested field by dot-path
($ symbol (!cap "runtime.inputGet" "request.body.symbol"))

;; An array index
($ first (!cap "runtime.inputGet" "items.0"))

;; Missing paths return null
($ missing (!cap "runtime.inputGet" "does.not.exist"))  ;; null
```

## Runtime policy enforcement

A capsule's `policy.json` limits which capabilities its program may call:

```bash
# Create a capsule (detects the effects the graph needs)
./target/release/lycan capsule create app.lyc my-app "route requests"

# Capsule runs enforce the policy; a denied effect stops the call:
./target/release/lycan capsule run my-app.lycap
# capability=file.readText effect=file_read denied by policy
```

Direct `lycan program.lyc` runs are unrestricted; policy applies to capsule
runs and to feature programs inside Syntra, where it is deny-all until the
capsule's policy allows more.

## Native capabilities

Rust functions for work that needs speed or system access. `lycan
capabilities` prints the full catalog with each capability's inputs,
effects and failure behavior.

```
;; Runtime
(!cap "runtime.input")
(!cap "runtime.inputGet" "request.body.symbol")
(!cap "runtime.publish" "score" 0.82)       ;; into the decision's journal
(!cap "runtime.capabilities")

;; Files (effects file_read / file_write)
(!cap "file.exists" "data.json")
(!cap "file.readText" "data.json")
(!cap "file.writeText" "out.txt" "hello")

;; HTTP (effect network; allow-listed hosts, private networks denied)
(!cap "http.get" "https://api.example.com/data")
(!cap "http.post" "https://api.example.com/submit" body)

;; JSON
($ val (!cap "json.get" json_str "user.tier"))   ;; by path
(!cap "json.has" json_str "user.tier")
(!cap "json.len" json_str "items")              ;; array, object or string

;; SQLite (read-only query)
(!cap "sql.sqliteQuery" "events.sqlite" "SELECT * FROM events LIMIT 5")

;; Statistics and forecasting
(!cap "stats.mean" data)
(!cap "stats.stdDev" data)
(!cap "stats.min" data)
(!cap "stats.max" data)
(!cap "stats.percentile" data 95.0)
(!cap "series.ewmaForecast" data 0.3)
(!cap "ops.autoScaleRecommend" 1200.0 250.0 2 20)  ;; load, per instance, min, max
```

## Compilation and binary format

```bash
./target/release/lycan compile program.lycs     # writes program.lyc
```

A `.lyc` holds the computation graph (nodes, edges, operands) and has
fields for strategy weights and statistics, activation counts and a
journal; [spec/graph-binary-format.md](spec/graph-binary-format.md) is the
normative layout. Running a binary does not modify it.

### Inspecting binaries

```bash
./target/release/lycan inspect program.lyc   # the graph as JSON
./target/release/lycan explain program.lyc   # the graph as text
./target/release/lycan stats program.lyc     # nodes, branches, weights
./target/release/lycan dump program.lyc      # raw hex
```

## Capsule format

A capsule packages a program with its intent, hashes and policy:

```bash
./target/release/lycan capsule create program.lyc my-app "Route API requests"
# my-app.lycap/
#   manifest.json    intent, SHA-256 hashes, capabilities
#   program.lyc      compiled graph binary
#   inspect.json     the graph as JSON
#   journal.json     the capsule's history
#   policy.json      what the program may do

./target/release/lycan capsule verify my-app.lycap
./target/release/lycan capsule inspect my-app.lycap
./target/release/lycan capsule run my-app.lycap      # verifies first
```

## Examples

[examples/lycan](../../examples/lycan/README.md) lists the example
programs: output, recursion, loops, pipelines, stdin, injected input, a
strategy node choosing a timeout policy, and the capability pack.

## Tests

```bash
cargo test -- --test-threads=1
```

`tests/conformance_vectors.rs` holds byte-for-byte vectors for the
specification in [spec/](spec/).

## Architecture

```
Lycan source (.lycs)
  | lycan compile: parser, compiler, verifier
Graph binary (.lyc)
  | lycan <file.lyc>, or a Syntra capsule's feature program
Graph executor (Rust)
  |-- strategy, choice and guard nodes (weights, contracts)
  |-- native capability calls, checked against the policy
  '-- injected input (runtime.input)
  | lycan capsule create
Capsule (.lycap): manifest.json, program.lyc, inspect.json, journal.json, policy.json
```

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
