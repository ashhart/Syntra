# Lycan

An AI-native machine execution language built on a Rust graph runtime.

Lycan is a new language for software that is generated, inspected, improved by AI, and then executed directly by machines.

Most AI systems wrap adaptive business logic in human-shaped source files or natural language prompts. That makes every decision depend on interpretation: names, framework structure, comments, side effects, prompt wording, and whatever the model infers from them.

Lycan takes a different route.

Lycan source compiles into a compact computational graph. That graph contains the decision structure, strategy weights, capability calls, policy boundaries, audit trail, and feedback memory. The AI can help author or improve the program. The Rust runtime executes it directly.

## Why I Created Lycan

I created Lycan because AI-generated software is becoming normal, but most code is still shaped for humans first. That means an LLM has to spend effort interpreting files, names, framework conventions, comments, side effects, and application structure before it can understand what the program is really trying to do.

Lycan starts from a different premise: if software is going to be generated, inspected, improved, and exchanged by AI systems, the program should be closer to the structure machines actually execute.

So Lycan compiles source into a compact graph runtime built in Rust. The graph carries the decision structure, capability calls, policy boundaries, strategy weights, feedback memory, and evolution trail in a form that can be run directly. The AI can help write or improve the capsule, but the hot path stays small, deterministic, inspectable, and cheap to execute.

The goal is not to replace every language. The goal is to remove the overhead around adaptive machine logic, especially where applications need to make decisions, learn from outcomes, and keep doing that without sending every request back through a model.

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

An improvement brief is a structured JSON handoff generated from a compiled graph. It includes the target strategy, contract, current winner, per-option tries, average latency, correctness rate, weights, goal, constraints, and expected proposal format. That gives an AI agent context for offline improvement, while the runtime still verifies and benchmarks any proposal before accepting it.

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

## What Lycan Is

- `.lycs` is the source language (S-expression syntax)
- `.lyc` is the compiled graph binary
- `.lycap` is the capsule exchange format (program + policy + manifest + journal)
- The Lycan runtime executes graphs directly, learns from outcomes, and evolves under verification
- The implementation is written in Rust and runs on a Rust-native graph runtime

The first killer use case is adaptive decisions. But the substrate is broader: Lycan is a general-purpose execution format for AI-generated computation.

## Rust-Native Runtime

Lycan is built on top of a Rust runtime. The source language compiles into a compact graph binary, and that graph is executed by Rust-native runtime code rather than by a large model or prompt interpreter.

That matters because Lycan programs can be:

- compiled and inspected
- executed deterministically
- sandboxed through explicit policy
- extended through Rust-native capabilities
- fed back into via outcome rewards
- evolved through verified proposals

The LLM can help write or improve the program. The Rust runtime runs it.

## Why Lycan can be more efficient

Lycan is not trying to replace other languages for every kind of application. Existing languages are excellent for building products, services, interfaces, data systems, and large human-maintained codebases.

Lycan is built for a narrower layer: compact adaptive machine logic that needs to execute repeatedly, safely, and cheaply.

Many application decisions pass through layers that were designed for human comprehension:

```text
human-shaped source
  -> framework structure
  -> dynamic runtime behavior
  -> business logic spread across files
  -> optional model or prompt interpretation
  -> output
```

Lycan moves that decision structure into an executable graph:

```text
JSON input
  -> compiled graph
  -> strategy weights
  -> policy-checked capability call
  -> output
  -> feedback
  -> memory update
```

That gives machines and AI systems less to decompress. The runtime does not need to rediscover intent from naming, comments, framework conventions, or natural language prompts on every request. The program is already explicit structure.

The advantage is not that Lycan is universally faster than other languages. The advantage is that, for adaptive decision logic, it can remove interpretation overhead:

- no LLM in the hot path
- no token budget per decision
- no prompt drift
- no GPU requirement for execution
- deterministic compiled graph runtime
- policy-bounded capability calls
- feedback-driven weights and memory

The model can still help write, inspect, and improve Lycan programs. It just does not need to be called every time the program runs.

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

30 Rust-native kernels callable via `!cap`:

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
| `examples/strategy-learning/` | Strategy nodes learning from execution |
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
