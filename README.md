# Lycan

An AI-native machine execution language built on a Rust graph runtime.

Lycan is a new language for adaptive software that needs to be generated, inspected, improved by AI, and then executed directly by machines.

Lycan source compiles into a compact computational graph. That graph carries the decision structure, strategy weights, capability calls, policy boundaries, audit trail, and feedback memory. AI can help author or improve the program. The Rust runtime executes it directly.

## Why I Created Lycan

I created Lycan because AI-generated software is becoming normal, but adaptive logic is still usually written as human-shaped source files or natural language prompts. That means an AI system has to spend effort interpreting names, framework conventions, comments, side effects, and application structure before it can understand what the program is trying to do.

Lycan starts from a narrower premise: if a piece of software is mostly decision logic, policy, feedback, and repeated execution, the program should preserve that structure directly.

The goal is not to replace every language. The goal is to make adaptive machine logic inspectable, portable, sandboxed, and cheap to run without sending every request back through a model.

```text
JSON input
  -> compiled graph execution
  -> weighted strategy selection
  -> policy-bounded capability calls
  -> decision output
  -> feedback
  -> memory update
```

No LLM required in the hot path. No token budget per decision. No prompt drift. No GPU. No opaque model reasoning required at execution time; the behaviour is in the graph, weights, policy, and journal.

## What Works Today

Lycan is early, but it is not just a design note. The current runtime can:

- parse and run `.lycs` source
- compile `.lycs` into `.lyc` graph binaries
- execute compiled graph binaries on a Rust-native runtime
- run adaptive strategy nodes with persisted weights
- accept structured JSON input through `lycan decide --input`
- accept feedback through `lycan feedback`
- inspect and explain compiled graph binaries
- call Rust-native capabilities through explicit `!cap` nodes
- verify capsule effects against execution policy
- package programs as capsules with policy, manifest, and journal data
- emit improvement briefs for AI-assisted proposal generation
- apply, verify, benchmark, and accept/reject evolution proposals

The strongest primitive today is the **strategy node**: multiple valid paths, one output contract, learned weights from outcomes.

An improvement brief is a structured JSON handoff generated from a compiled graph. It includes the target strategy, contract, current winner, per-option tries, average latency, correctness rate, weights, goal, constraints, and expected proposal format. A proposal is a candidate strategy option with source code, target strategy, and optional expected output; the runtime verifies and benchmarks it before accepting it.

## The Core Primitive: Strategy Nodes

A strategy node lets a program carry several valid implementations or policies for the same task. The runtime chooses between them, records what happened, and updates weights when feedback arrives.

That means the program can learn without changing its public contract.

```lisp
(F sum_loop (n)
  ($! total 0) ($! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)

(F sum_formula (n)
  (/ (* n (+ n 1)) 2))

($ result
  (strategy
    (sum_loop 5000)
    (sum_formula 5000)))
```

Both paths preserve the same output contract. Over time, the runtime can learn which path works best for the actual workload.

The same primitive applies to application decisions:

```text
conservative policy
balanced policy
aggressive policy
  -> one decision
  -> delayed outcome feedback
  -> updated weights
```

This is the piece to test first. Lycan is not asking you to trust a vague claim that "the program learns." It exposes a concrete runtime object: competing strategies, stable output, observable weights, delayed feedback, and an audit trail.

## Machine-Native Does Not Mean Unreadable

Lycan is designed for machines to execute, but it still needs to be inspected by people.

The project keeps several layers visible:

- `.lycs` is the readable source form
- `.lyc` is the compact executable graph binary
- `lycan inspect` emits an AI-readable JSON graph view
- `lycan explain` turns binaries back into a textual view
- `lycan learn-report` shows strategy weights and learning state
- capsules carry policy, manifest, and journal data beside the program

The aim is not to hide logic inside a black box. The aim is to make adaptive logic explicit enough that both machines and humans can audit what is being executed.

## File Formats

| Format | Purpose |
|---|---|
| `.lycs` | Readable source language using S-expression syntax |
| `.lyc` | Compiled executable graph binary |
| `.lycap` | Capsule exchange format: program, policy, manifest, and journal |

The first target is adaptive decision logic: small hot-path programs that need stable outputs, visible weights, policy boundaries, feedback, and evolution under verification.

## Learn the Language

Start here if you want to write or generate Lycan programs:

| Document | Purpose |
|---|---|
| [`docs/GUIDE.md`](docs/GUIDE.md) | Practical guide to the language and runtime |
| [`docs/language/syntax.md`](docs/language/syntax.md) | Source syntax |
| [`docs/language/values-and-types.md`](docs/language/values-and-types.md) | Runtime values and types |
| [`docs/language/strategy-nodes.md`](docs/language/strategy-nodes.md) | Adaptive strategy nodes |
| [`docs/language/capabilities.md`](docs/language/capabilities.md) | Native capability calls |
| [`docs/spec/lyc-binary-format.md`](docs/spec/lyc-binary-format.md) | Compiled graph binary format |
| [`docs/spec/capsule-format.md`](docs/spec/capsule-format.md) | Capsule exchange format |

## Runtime Properties

Lycan is built on a Rust-native graph runtime. The important property is not just speed; it is that adaptive behaviour becomes visible runtime state instead of disappearing inside a prompt or scattered application code.

Lycan programs can be:

- inspected as source, graph JSON, or explained binary
- executed deterministically by the runtime
- sandboxed through explicit execution policy
- extended through Rust-native capabilities
- updated through outcome feedback
- evolved through verified proposals

Efficiency is a consequence of that shape. For the workloads Lycan targets, the runtime does not need to rediscover intent from naming, comments, framework conventions, or natural language prompts on every request. The model can still help write, inspect, and improve Lycan programs. It just does not need to be called every time the program runs.

## Closest Neighbours

Lycan overlaps with a few familiar ideas, but it is aimed at a specific layer.

An embedded DSL can model business decisions inside a host application. Lycan makes the graph, weights, execution policy, capability calls, feedback memory, and journal first-class portable artifacts.

A bandit or reinforcement-learning library can learn action preferences. Lycan wraps that style of learning inside an executable program format with source, binary graph, policy, inspection, feedback, and capsule packaging.

Durable workflow systems are excellent for orchestration. Lycan is lower-level: it decides what to do inside a hot path, records the outcome, and updates the adaptive decision layer.

Use Lycan when the adaptive decision itself is the thing you need to inspect, ship, sandbox, feed back into, and evolve.

## Benchmarks

The benchmark story is intentionally narrow: repeated, structured decision-runtime workloads.

See [`benchmarks/README.md`](benchmarks/README.md) for the current microbenchmark set and the rules for publishing numbers. Treat early benchmark results as evidence for a specific runtime shape, not as a claim that Lycan is universally faster than every general-purpose runtime.

## A Fun One: Mars Transfers

For a bit of fun, Lycan includes astrodynamics examples that work through Mars transfer-style problems using real ephemeris data, orbital calculations, and the native Lambert solver capability.

The point is not that Lycan is a spaceflight toolkit. The point is that a compact graph runtime can take structured data, run numerical logic, call bounded native capabilities, and produce an inspectable result without an LLM in the execution path.

See `examples/mars-horizons/` for the JPL/Horizons-style Mars transfer demos.

## Syntax primer

```lisp
;; Values
42              ;; integer
3.14            ;; float
"hello"         ;; string
true / false    ;; boolean
null            ;; null
(A 1 2 3)       ;; array

;; Bindings
($ x 42)        ;; immutable
($! x 0)        ;; mutable
(= x 10)        ;; reassign

;; Functions
(F add (a b) (+ a b))
(!p (add 3 4))         ;; prints 7

;; Control flow
(? (> x 10) "big" "small")     ;; if/else
(W (< i 10) body...)           ;; while
(each x collection body...)    ;; for-each
(B expr...)                    ;; block

;; Collections
(A 10 20 30)           ;; array literal
(I arr 1)              ;; index access
(.. 1 5)               ;; range

;; Strategy nodes (where programs learn)
($ result (strategy
  (fast_method args)
  (accurate_method args)
  (experimental_method args)))

;; Capabilities (Rust-native kernels)
(!cap "stats.mean" data)
(!cap "http.get" "https://api.example.com/data")
(!cap "file.readText" "config.json")
(!cap "runtime.inputGet" "request.body.symbol")

;; Output
(!p "hello from lycan")
```

## Build and run

```bash
cargo build --release

# Run source
./target/release/lycan program.lycs

# Compile to binary
./target/release/lycan compile program.lycs

# Execute binary (learns on each run)
./target/release/lycan program.lyc

# Decision with JSON input
./target/release/lycan decide program.lyc --input request.json

# Feedback
./target/release/lycan feedback program.lyc <node> --option <n> --reward <f>

# Autonomous evolution
./target/release/lycan evolve program.lyc --proposal proposal.json

# Capsule lifecycle
./target/release/lycan capsule create program.lyc name "intent"
./target/release/lycan capsule verify name.lycap
./target/release/lycan capsule run name.lycap
```

## Native capabilities

26 Rust-native kernels callable via `!cap`:

| Package | Capabilities |
|---------|-------------|
| runtime | `runtime.capabilities`, `runtime.input`, `runtime.inputGet` |
| io | `file.exists`, `file.readText`, `file.writeText` |
| net | `http.get`, `http.post` |
| data | `json.get`, `json.has`, `json.len`, `sql.sqliteQuery` |
| math | `stats.mean`, `stats.stdDev`, `stats.min`, `stats.max`, `stats.percentile` |
| ops | `series.ewmaForecast`, `ops.autoScaleRecommend` |
| astro | `nav.*`, `astro.lambertSolve` |

## Tests

```
cargo test -- --test-threads=1
```

## Examples

| Example | What it shows |
|---|---|
| `examples/hello.lycs` | Smallest runnable program |
| `examples/fibonacci.lycs` | Recursion |
| `examples/json-input.lycs` | `runtime.inputGet` with structured input |
| `examples/strategy-learning/` | Best first demo: strategy weights move while output stays correct |
| `examples/capability-policy/` | Native capabilities with policy enforcement |
| `examples/mars-horizons/` | JPL ephemeris + Lambert solver for real astrodynamics demos |
| `examples/science/` | Feigenbaum, Lorenz, black holes, N-body demos |
| `examples/evolution/` | Autonomous capsule evolution with proposals |

## Related Project

```text
Syntra
  self-hosted Docker/API/admin appliance built on Lycan
```

Lycan is the language. Syntra is the deployable runtime appliance for serving Lycan capsules in applications.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
