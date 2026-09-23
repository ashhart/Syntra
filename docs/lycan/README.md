# Lycan

> **Backends (2026-09-23).** This repository ships one execution backend:
> the graph compiler, verifier and graph executor. `lycan <file.lycs>`
> compiles, verifies and runs through it. References below to
> `interpreter.rs`, the tree-walking or source backend, and `!lambert`,
> `nav.*`, `comb.*` or `astro.*` describe code that moved to the Lycan Lab
> repository at split commit `15f5441`. Where the two backends differed,
> Syntra's behavior is the compiled backend's.

An AI-native machine execution language built on a Rust graph runtime.

Lycan is a new language for adaptive software that needs to be generated, inspected, improved by AI, and then executed directly by machines.

Lycan source compiles into a compact computational graph. That graph carries the decision structure, strategy weights, capability calls, policy boundaries, audit trail, and feedback memory. AI can help author or improve the program. The Rust runtime executes it directly.

Lycan is early, but it is not just a design note. The parser, compiler, graph runtime, strategy learning, capsule format, policy checks, inspection tools, and proposal verification loop all exist today.

Syntra, the decision service in this repository, uses Lycan for optional feature programs: a capsule can run a sandboxed Lycan program before each decision to compute derived features or exclude actions (see the [repository README](../../README.md)).

## Why I Created Lycan

I created Lycan because AI-generated software is becoming normal, but adaptive logic is still usually written as human-shaped source files or natural language prompts. That means an AI system has to spend effort interpreting names, framework conventions, comments, side effects, and application structure before it can understand what the program is trying to do.

Lycan starts from a narrower premise: if a piece of software is mostly decision logic, policy, feedback, and repeated execution, the program should preserve that structure directly.

The goal is not to replace every language. The goal is to make adaptive machine logic inspectable, portable, sandboxed, and cheap to run without sending every request back through a model.

```text
JSON input
  -> compiled graph execution
  -> policy-bounded capability calls
  -> weighted strategy or choice selection
  -> decision output
  -> in-graph feedback
  -> weight update
```

No LLM required in the hot path. No token budget per decision. No prompt drift. No GPU. No opaque model reasoning required at execution time; the behaviour is in the graph, weights, policy, and journal.

## What Works Today

The current runtime can:

- parse `.lycs` source and compile it into `.lyc` graph binaries
- verify graph binaries and execute them on a Rust-native runtime
- accept structured JSON input (`lycan <file> --input request.json`)
- run adaptive strategy and choice nodes that learn within a run, with
  in-graph `(feedback ...)`
- inspect and explain compiled graph binaries
- call Rust-native capabilities through explicit `!cap` nodes, bounded by
  an execution policy
- package programs as capsules (`.lycap`) with policy, manifest, and
  journal data, and verify and run them

The strongest primitive is the **strategy node**: multiple valid paths, one output contract, learned weights from outcomes.

The AI-assisted evolution loop (improvement briefs and verified proposals) and the interactive REPL moved to the Lycan Lab repository.

## The Core Primitive: Strategy Nodes

A strategy node lets a program carry several valid implementations or policies for the same task. The runtime chooses between them, records what happened, and updates weights when feedback arrives.

That means the program can learn without changing its public contract.

For an application, the shape might be:

```lisp
($ request (!cap "runtime.input"))

(F low_timeout (req) "low_timeout")
(F medium_timeout (req) "medium_timeout")
(F high_timeout (req) "high_timeout")

($ policy
  (strategy
    (low_timeout request)
    (medium_timeout request)
    (high_timeout request)))
```

All three options produce the same kind of answer. The runtime can learn which policy wins for the actual workload and context after delayed feedback arrives.

The same primitive can also compare implementations:

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

This is the piece to test first. Lycan is not asking you to trust a vague claim that "the program learns." It exposes a concrete runtime object: competing strategies, stable output, observable weights, delayed feedback, and an audit trail.

## Learning

Strategy, choice and feedback nodes update the graph's weights while it runs, with fixed rules: strategy nodes reward fast options that agree with the majority, choice nodes learn only from `(feedback ...)`. The `lycan` CLI does not write learned weights back to the `.lyc` file. See [`language/strategy-nodes.md`](language/strategy-nodes.md) and the normative [`spec/learning-semantics.md`](spec/learning-semantics.md).

## Machine-Native Does Not Mean Unreadable

Lycan is designed for machines to execute, but it still needs to be inspected by people.

The project keeps several layers visible:

- `.lycs` is the readable source form
- `.lyc` is the compact executable graph binary
- `lycan inspect` emits an AI-readable JSON graph view
- `lycan explain` turns binaries back into a textual view, with each
  node's weights
- capsules carry policy, manifest, and journal data beside the program

The aim is not to hide logic inside a black box. The aim is to make adaptive logic explicit enough that both machines and humans can audit what is being executed.

## File Formats

| Format | Purpose |
|---|---|
| `.lycs` | Readable source language using S-expression syntax |
| `.lyc` | Compiled executable graph binary |
| `.lycap` | Capsule exchange format: program, policy, manifest, and journal |

The first target is adaptive decision logic: small hot-path programs that need stable outputs, visible weights, policy boundaries, and feedback.

## Learn the Language

Start here if you want to write or generate Lycan programs:

| Document | Purpose |
|---|---|
| [`GUIDE.md`](GUIDE.md) | Practical guide to the language and runtime |
| [`language/syntax.md`](language/syntax.md) | Source syntax |
| [`language/values-and-types.md`](language/values-and-types.md) | Runtime values and types |
| [`language/strategy-nodes.md`](language/strategy-nodes.md) | Adaptive strategy nodes |
| [`language/capabilities.md`](language/capabilities.md) | Native capability calls |
| [`spec/grammar.md`](spec/grammar.md) | Normative grammar (parser-verified, both backends) |
| [`spec/value-model.md`](spec/value-model.md) | Normative value semantics, arity rules, backend divergences |
| [`spec/scoping-and-execution.md`](spec/scoping-and-execution.md) | Scoping, builtin/opcode execution semantics |
| [`spec/lyc-binary-format.md`](spec/lyc-binary-format.md) | Compiled graph binary format (+ conformance vectors in `tests/conformance_vectors.rs`) |
| [`spec/graph-binary-format.md`](spec/graph-binary-format.md) | NeuralGraph wire format, guards, lenient-decode corners |
| [`spec/capsule-format.md`](spec/capsule-format.md) | Capsule exchange format (.lycap) |
| [`spec/execution-policy.md`](spec/execution-policy.md) | Policy model and enforcement layers |
| [`spec/learning-semantics.md`](spec/learning-semantics.md) | In-run learning of strategy, choice and feedback nodes |
| [`spec/capability-abi.md`](spec/capability-abi.md) | Capability registry and sandbox ABI |

## Runtime Properties

Lycan is built on a Rust-native graph runtime. The important property is not just speed; it is that adaptive behaviour becomes visible runtime state instead of disappearing inside a prompt or scattered application code.

Lycan programs can be:

- inspected as source, graph JSON, or explained binary
- executed deterministically by the runtime
- sandboxed through explicit execution policy
- extended through Rust-native capabilities
- updated through outcome feedback

Efficiency is a consequence of that shape. For the workloads Lycan targets, the runtime does not need to rediscover intent from naming, comments, framework conventions, or natural language prompts on every request. The model can still help write, inspect, and improve Lycan programs. It just does not need to be called every time the program runs.

## Closest Neighbours

Lycan overlaps with a few familiar ideas, but it is aimed at a specific layer.

An embedded DSL can model business decisions inside a host application. Lycan makes the graph, weights, execution policy, capability calls, feedback memory, and journal first-class portable artifacts.

A bandit or reinforcement-learning library can learn action preferences. Lycan wraps that style of learning inside an executable program format with source, binary graph, policy, inspection, feedback, and capsule packaging.

Durable workflow systems are excellent for orchestration. Lycan is lower-level: it decides what to do inside a hot path, records the outcome, and updates the adaptive decision layer.

Use Lycan when the adaptive decision itself is the thing you need to inspect, ship, sandbox, and feed back into.

## Benchmarks

The benchmark story is intentionally narrow: repeated, structured decision-runtime workloads.

See [`benchmarks/README.md`](../../benchmarks/README.md) for the current microbenchmark set and the rules for publishing numbers. Treat early benchmark results as evidence for a specific runtime shape, not as a claim that Lycan is universally faster than every general-purpose runtime.

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

# Verify and run a binary
./target/release/lycan program.lyc

# Run with JSON input for runtime.input / runtime.inputGet
./target/release/lycan program.lyc --input request.json

# Inspect
./target/release/lycan explain program.lyc
./target/release/lycan inspect program.lyc
./target/release/lycan capabilities

# Capsule lifecycle
./target/release/lycan capsule create program.lyc name "intent"
./target/release/lycan capsule verify name.lycap
./target/release/lycan capsule run name.lycap
```

## Native capabilities

20 Rust-native kernels callable via `!cap` (`lycan capabilities` prints the registry with each one's inputs, effects and cost):

| Package | Capabilities |
|---------|-------------|
| runtime | `runtime.capabilities`, `runtime.input`, `runtime.inputGet`, `runtime.publish` |
| io | `file.exists`, `file.readText`, `file.writeText` |
| net | `http.get`, `http.post` |
| data | `json.get`, `json.has`, `json.len`, `sql.sqliteQuery` |
| math | `stats.mean`, `stats.stdDev`, `stats.min`, `stats.max`, `stats.percentile`, `series.ewmaForecast` |
| ops | `ops.autoScaleRecommend` |

`runtime.publish` is how a Syntra feature program hands derived features (`features.<name>`), exclusions (`exclude.<id>`, `only.<id>`) and a `reason` to the decision.

## Tests

```
cargo test -- --test-threads=1
```

## Examples

| Example | What it shows |
|---|---|
| `examples/lycan/hello.lycs` | Smallest runnable program |
| `examples/lycan/fibonacci.lycs` | Recursion |
| `examples/lycan/pipeline.lycs` | Filter, map and reduce with lambdas |
| `examples/lycan/json-input.lycs` | `runtime.inputGet` with structured input |
| `examples/lycan/demo_adaptive_routing.lycs` | Statistics capabilities feeding a strategy node |
| `examples/lycan/capability-policy/` | File, JSON, statistics, forecast and autoscaling capabilities |

The science demos (Mars transfers, Feigenbaum, Lorenz, N-body) and the evolution examples are in the Lycan Lab repository. [`examples/lycan/README.md`](../../examples/lycan/README.md) lists every example here.

## Related Project

```text
Syntra
  self-hosted contextual-bandit decision service, in this repository
```

Lycan is the language. Syntra is the decision service: it chooses among a capsule's actions with its own learner, logs every decision with its probability, and evaluates changes before they ship. A capsule may carry a Lycan feature program, run under the capsule's execution policy, to compute features or exclude actions.

If your hot path makes the same kind of decision repeatedly and learns from delayed feedback, that is the workload Lycan is built for.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE).
