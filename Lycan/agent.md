# Agent Guide: Lycan Language

This file is for AI agents, maintainers, and collaborators working inside the Lycan language repo.

## One-line identity

Lycan is an AI-native machine execution language: a compact language and compiled graph runtime designed for AI to generate, machines to execute, and applications to learn from feedback without putting an LLM in the hot path.

## The big idea

Most programming languages are shaped around human reading habits: files, classes, naming, framework conventions, comments, side effects, and business logic spread across layers. That gives people a manageable interface, but it gives AI systems a lot to decode before they can safely change or execute anything.

Lycan takes the opposite route.

A Lycan program is explicit computational structure:

- source code in `.lycs`
- compiled graph binary in `.lyc`
- exchangeable capsule in `.lycap`
- policy boundaries beside the program
- capability calls declared through the runtime
- strategy nodes that learn from outcomes
- audit and evolution trails that explain what changed

The aim is not to make another scripting language for humans. The aim is to create a substrate that AI can write cleanly and machines can execute directly.

## What Lycan is not

Lycan is not just a decision DSL.

Adaptive decision-making is the first killer use case because it shows the value immediately: JSON in, graph execution, weighted strategy selection, capability calls, decision out, feedback back in, memory updated. But the language itself is broader than that. It is a machine-oriented execution format for AI-generated computation.

Lycan is also not an LLM wrapper. The runtime can be inspected, compiled, benchmarked, sandboxed, and executed without calling a model.

## Product split

There are two repos in the current product shape:

| Repo | Role |
|---|---|
| `Lycan` | The Lycan language, compiler, graph format, CLI runtime, capability registry, verifier, examples, and language documentation. |
| `Syntra` | The self-hosted Docker/API/admin appliance that runs Lycan capsules for real applications, stores memory, exposes jobs, serves the dashboard, and handles operational deployment. |

Use this language:

- **Lycan** = the language.
- **Syntra** = the runtime appliance built on Lycan.
- **Lycan Marketplace** = future distribution layer for signed capsules, capability packages, templates, and integrations.

The product name is Syntra. The browser UI is the admin console.

## Core technical model

The language flow is:

```text
.lycs source
  -> parser / compiler
  -> graph IR
  -> .lyc binary
  -> runtime execution
  -> optional capsule packaging
  -> policy-enforced capability calls
  -> feedback / memory / evolution
```

Key files:

| File | Purpose |
|---|---|
| `src/parser.rs` | Parses `.lycs` source. |
| `src/graph.rs` | Graph data model, nodes, strategy metadata, journals. |
| `src/graph_compiler.rs` | Compiles source AST into graph form. |
| `src/graph_executor.rs` | Executes compiled graphs. |
| `src/capabilities.rs` | Native capability registry and central policy enforcement. |
| `src/context.rs` | `ExecutionContext` and `ExecutionPolicy`. |
| `src/verifier.rs` | Purity/effect verification. |
| `src/capsule.rs` | Capsule creation, verification, policy loading. |
| `src/learning.rs` | Strategy learning and contextual memory. |
| `src/evolve.rs` | Proposal application and benchmark gates. |
| `src/evolution_loop.rs` | Autonomous evolution loop. |
| `src/main.rs` | CLI entrypoint. |

Lycan is implemented in Rust and runs on a Rust-native graph runtime. Some shared runtime modules, including server/store support used by Syntra, are currently exported from this crate. Treat those as shared runtime code, not duplicated source.

## Syntax shape

Lycan source is expression-oriented and intentionally compact:

```lisp
($ latency (!cap "runtime.inputGet" "request.latency_ms"))

($ decision
  (strategy
    "fast_path"
    "balanced_path"
    "safe_path"))

(!p decision)
```

Common forms:

| Form | Meaning |
|---|---|
| `($ name value)` | Immutable binding. |
| `($! name value)` | Mutable binding. |
| `(= name value)` | Assignment. |
| `(F name (args...) body...)` | Function. |
| `(B expr...)` | Block. |
| `(? condition then else)` | Conditional. |
| `(W condition body...)` | While loop. |
| `(each item array body...)` | Iteration. |
| `(A item...)` | Array literal. |
| `(I array index)` | Index access. |
| `(strategy option...)` | Adaptive strategy node. |
| `(!cap "name" args...)` | Native capability call. |
| `(!p value...)` | Print/output. |

## Why this matters

For many applications, the expensive part is not raw computation. It is making every request pass through a large model to reinterpret intent, constraints, context, and business logic again and again.

Lycan moves that structure into executable graph form.

```text
JSON input
  -> compiled graph execution
  -> weighted strategy selection
  -> capability calls
  -> decision output
  -> feedback
  -> memory update
```

No LLM required in the hot path. No token budget. No prompt drift. No GPU. No opaque model reasoning required at execution time. Just executable adaptive logic.

## Working rules for agents

When changing this repo:

1. Preserve the language/runtime boundary. If the work is Docker, admin UI, HTTP API, deployment, jobs dashboard, or operational store, it probably belongs in Syntra.
2. Keep Lycan language docs focused on how to write, compile, run, package, verify, and evolve Lycan programs.
3. Do not introduce secrets, `.env` files, local databases, Docker volumes, store data, or generated `target/` artifacts.
4. Do not claim universal benchmark superiority. Use measured benchmark language and include hardware/test caveats.
5. Do not describe Lycan as merely "a decision language". It is an AI-native machine execution language whose first strong application is adaptive decisions.
6. Keep examples small, runnable, and named by what they teach.
7. Prefer explicit policy and capability examples over hidden magic.
8. If adding features that mutate graphs, include rollback, verification, and audit/evolution trail behavior.

## Useful commands

```bash
cargo build
cargo test -- --test-threads=1
cargo build --release

# Run source
cargo run -- examples/hello.lycs

# Compile source
cargo run -- compile examples/hello.lycs

# Decision mode with input
cargo run -- compile examples/json-input.lycs
cargo run -- decide examples/json-input.lyc --input examples/request.json
```

## Current split TODO

- Keep this repo as the canonical language crate plus CLI:
  - `src/lib.rs` for language/runtime APIs
  - `src/main.rs` for the Lycan CLI
  - Syntra depends on this repo as a crate
- Decide whether shared `server.rs` and `store.rs` remain in `lycan`, move into a separate `lycan-runtime` crate, or move into Syntra.
- Add a proper language specification:
  - grammar
  - type/value model
  - graph binary format
  - capability ABI
  - policy model
  - capsule format
  - learning semantics
- Add editor support and syntax highlighting.
- Add benchmark harness with warmups, repeated runs, medians, p95, and environment metadata.

## Preferred positioning

Use this framing in docs, demos, and product copy:

> Most AI systems wrap business logic in natural language prompts. Lycan compiles that logic into a compact executable graph. The model can help author or improve the program, but the runtime executes it directly under policy, with feedback-driven memory and no LLM in the hot path.

Short version:

> Write in Lycan. Run on Syntra. Extend through Lycan Marketplace.
