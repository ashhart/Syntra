# Lycan Guide

Lycan is an AI-native machine execution language for adaptive decision logic. Source programs compile into graph binaries that can be inspected, sandboxed, executed, fed back into, and evolved under verification.

This guide covers the language surface and the runtime workflow: write `.lycs`, compile `.lyc`, inspect the graph, run strategy nodes, apply feedback, package capsules, and verify proposals.

## Quick start

```bash
# Build
cargo build --release

# Run a program
./target/release/lycan examples/hello.lycs

# Compile to graph binary
./target/release/lycan compile examples/hello.lycs

# Run the binary (learns on each run)
./target/release/lycan examples/hello.lyc

# Interactive REPL
./target/release/lycan
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

## Strategy nodes — where programs learn

The core invention. Multiple implementations compete. The program discovers which is best.

```
;; Two strategies for computing sum(1..N)
(F sum_loop (n)
  ($! total 0) ($! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)

(F sum_formula (n)
  (/ (* n (+ n 1)) 2))

;; Strategy competition — Lycan learns which is faster
($ result (strategy (sum_loop 5000) (sum_formula 5000)))
(!p result)
```

After multiple runs, the weights shift toward the faster strategy.

### Contracts

Strategy nodes enforce correctness contracts:

- **WithinTolerance** (default): all options must agree within epsilon. Incorrect options are punished.
- **SameOutput**: all options must produce identical output.
- Both require pure computation — no side effects inside strategy options.

### Viewing what the program learned

```bash
# Show strategy weights and stats
./target/release/lycan learn-report program.lyc

# Show detailed evolution statistics
./target/release/lycan stats program.lyc
```

## Adaptive nodes

### choice — weights decide

```
($ action (choice "scale_up" "hold" "scale_down"))
```

Weights are semantic — the program chooses based on learned preference.

### guard — fast path with fallback

```
($ result (guard (> cache_valid true) cached_value (compute_fresh)))
```

Check assumption first. If true, fast path. If false, fallback.

### feedback — reward signal

```
(feedback solver_node 1.0)      ;; positive reward
(feedback solver_node -0.5)     ;; negative reward
```

Updates weights on the target strategy/choice node.

## Delayed feedback — learning from the real world

External systems can report outcomes after execution:

```bash
# Report success for option 1
./target/release/lycan feedback app.lyc 42 --option 1 --reward 1.0

# Report failure for option 0
./target/release/lycan feedback app.lyc 42 --option 0 --success false
```

This is how Lycan learns from real-world outcomes — not just execution speed.

## Decision runtime

```bash
# Get a structured decision
./target/release/lycan decide app.lyc

# With injected JSON input
./target/release/lycan decide app.lyc --input request.json

# Output:
# {
#   "node_id": 42,
#   "chosen_option": 2,
#   "confidence": 0.82,
#   "objective": "reliability",
#   "weights": [0.08, 0.10, 0.82]
# }
```

Applications call `lycan decide` when they need a choice, then report outcomes via `lycan feedback`.

### Injected input

Programs access injected JSON via capabilities:

```
;; Get full input
($ data (!cap "runtime.input"))

;; Get nested field by dot-path
($ symbol (!cap "runtime.inputGet" "request.body.symbol"))

;; Array index
($ first (!cap "runtime.inputGet" "items.0"))

;; Missing paths return null
($ missing (!cap "runtime.inputGet" "does.not.exist"))  ;; null
```

### Runtime policy enforcement

Capsules enforce security policies at execution time. When a capsule runs, its `policy.json` constrains what capabilities the program can call:

```bash
# Create capsule (auto-detects required effects)
./target/release/lycan capsule create app.lyc my-app "route requests"

# Capsule run enforces policy — denied effects are blocked
./target/release/lycan capsule run my-app.lycap

# If the graph calls file.readText but policy has allow_file_read: false:
# capability=file.readText effect=file_read denied by policy
```

Direct `lycan program.lyc` runs are unrestricted — policy only applies to capsule execution.

## Native capabilities

Lycan provides Rust-native functions for operations that need performance or system access:

```bash
# List all available capabilities
./target/release/lycan capabilities
```

### File I/O

```
(!cap "file.exists" "/tmp/data.json")
(!cap "file.readText" "/tmp/data.json")
(!cap "file.writeText" "/tmp/out.txt" "hello")
```

### HTTP

```
(!cap "http.get" "https://api.example.com/data")
(!cap "http.post" "https://api.example.com/submit" body)
```

### JSON

```
($ val (!cap "json.get" json_str "key"))
(!cap "json.has" json_str "key")
```

### SQLite

```
(!cap "sql.sqliteQuery" "/path/to/db.sqlite" "SELECT * FROM events LIMIT 5")
```

### Statistics

```
(!cap "stats.mean" data)
(!cap "stats.stdDev" data)
(!cap "stats.min" data)
(!cap "stats.max" data)
(!cap "stats.percentile" data 95.0)
```

### Lambert solver (orbital mechanics)

```
($ result (!lambert r1x r1y r1z r2x r2y r2z tof_days mu))
;; Returns: (A v1x v1y v1z v2x v2y v2z status)
```

## Compilation and binary format

```bash
# Compile source to graph binary
./target/release/lycan compile program.lycs

# The .lyc binary IS the program — it contains:
# - Computation graph (nodes, edges, operands)
# - Learned weights
# - Activation counts
# - Strategy statistics
# - Evolution journal

# Every run updates the binary — the program evolves
./target/release/lycan program.lyc
./target/release/lycan program.lyc   # weights shifted
./target/release/lycan program.lyc   # converging...
```

### Inspecting binaries

```bash
# AI-readable JSON view of the graph
./target/release/lycan inspect program.lyc

# Evolution statistics
./target/release/lycan stats program.lyc

# Strategy learning report (read-only)
./target/release/lycan learn-report program.lyc

# Weakness detection
./target/release/lycan improve-report program.lyc

# Raw hex dump
./target/release/lycan dump program.lyc
```

## Capsule format

A capsule packages a program for agent-to-agent exchange:

```bash
# Create a capsule
./target/release/lycan capsule create program.lyc my-app "Route API requests"

# Result:
# my-app.lycap/
#   manifest.json    — intent, SHA256 hashes, capabilities
#   program.lyc      — compiled graph binary
#   inspect.json     — AI-readable graph structure
#   journal.json     — evolution history
#   policy.json      — what the program is allowed to do

# Verify integrity
./target/release/lycan capsule verify my-app.lycap

# Run from capsule (verifies first)
./target/release/lycan capsule run my-app.lycap
```

## Graph evolution — AI-assisted improvement

```bash
# 1. Detect weaknesses
./target/release/lycan improve-report program.lyc

# 2. Get improvement brief for an AI agent
./target/release/lycan capsule improve program.lyc

# 3. Apply a proposed improvement directly
./target/release/lycan capsule apply-proposal program.lyc proposal.json

# Or run the candidate-first autonomous evolution loop
./target/release/lycan evolve program.lyc --proposal proposal.json --min-improvement 0.05

# Proposal format:
# {
#   "name": "BetterStrategy",
#   "source": "(F better (x) (* x 3))\n(better 100)",
#   "expected_output": "300",
#   "insert_into_strategy": 42
# }
```

The proposal is verified (pure, correct, not slower) before being grafted into the running graph. Rejected proposals leave the binary unchanged.

## Weight transfer

```bash
# Transfer learned weights from one program to another
./target/release/lycan transfer-weights source.lyc target.lyc
```

## Examples

| Demo | What it shows |
|---|---|
| `hello.lycs` | Basic output |
| `fibonacci.lycs` | Recursion |
| `fizzbuzz.lycs` | Control flow |
| `analytics.lycs` | Data processing |
| `calculator.lycs` | Interactive I/O |
| `demo_learning.lycs` | Reverse learning — starts wrong, finds right |
| `demo_impossible.lycs` | Three paradigms compete |
| `demo_feedback_decision.lycs` | Delayed feedback |
| `demo_autoscaler.lycs` | Decision runtime |
| `demo_edge_of_chaos.lycs` | Feigenbaum constant, derived from first principles |
| `demo_kepler.lycs` | Orbital mechanics |
| `demo_lorenz.lycs` | Chaos theory — 3 ODE solvers compete |
| `demo_blackhole.lycs` | Schwarzschild geodesic |
| `demo_nbody.lycs` | N-body gravitational simulation |
| `demo_adaptive_routing.lycs` | Adaptive timeout from real signals + feedback |
| `demo_mars_real.lycs` | Earth-to-Mars mission designer (JPL + Lambert) |

## Tests

```bash
cargo test -- --test-threads=1
cargo test --quiet -- --test-threads=1
```

## Architecture

```
Lycan source (.lycs)
  ↓ compile
Compiled graph binary (.lyc)
  ↓ execute
Graph executor (Rust)
  ├── Weighted strategy nodes
  ├── Exploration + auto-reward
  ├── Contract validation
  ├── Native capability calls (Rust kernels)
  └── Persistent weights + journal
  ↓ save
Evolved binary (.lyc)
  ↓ package
Capsule (.lycap)
  ├── manifest.json (intent, hashes)
  ├── program.lyc (graph)
  ├── inspect.json (AI-readable)
  ├── journal.json (history)
  └── policy.json (permissions)
```

## License

Licensed under the Apache License, Version 2.0. See `LICENSE`.
