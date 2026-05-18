# Lycan Benchmarks

The benchmark goal is narrow:

> measure small, repeated, structured decision-runtime workloads.

Benchmarks are not the main Lycan claim. The main claim is inspectable adaptive behaviour: compiled graph execution, visible weights, policy-bounded capabilities, feedback memory, and no model call in the hot path.

Runtime efficiency matters because it is what makes that layer cheap enough to sit under ordinary applications. It is not a claim that Lycan beats every general-purpose runtime at every task.

## Status

The numbers below are local microbenchmark results from the current project history. Treat them as directional until the full benchmark harness, environment metadata, and repeated-run summaries are checked in beside them.

## Current Microbenchmark Set

The current benchmark suite from the original project compares:

- recursive functions
- string processing
- small data pipelines
- bubble sort

Measured locally:

| Benchmark | `.lyc` Binary | Runtime A | Runtime B |
|-----------|---------------|--------|---------|
| recursive | 26ms | 29ms | 59ms |
| strings | 21ms | 32ms | 61ms |
| pipeline | 17ms | 29ms | 59ms |
| sort | 19ms | 28ms | 59ms |

Average:

```text
.lyc binary:  20.75ms
Runtime A:    29.50ms
Runtime B:    59.50ms
```

On this microbenchmark set, compiled `.lyc` graph execution was roughly 30%
lower wall time than Runtime A and 65% lower wall time than Runtime B.

## Benchmark Rules

Before citing numbers publicly, every benchmark run should include:

- hardware model
- OS version
- Lycan version
- comparable runtime versions
- warmup runs
- median/min/max/p95 over many runs
- source `.lycs` timing separated from compiled `.lyc` timing
- the benchmark source for every compared runtime
- exact commands used to run each benchmark

## What A Good Benchmark Should Measure

The useful comparison is not "language A versus language B" in the abstract. It is whether Lycan is a better fit for a hot adaptive decision layer.

A good benchmark should include:

- repeated decision calls
- structured JSON input
- at least one strategy node
- feedback that changes weights
- an inspectable report of what changed
- no model call during the measured hot path

## Why This Matters

Lycan is not just about raw speed. The stronger runtime claim is:

```text
compiled graph execution
+ persistent adaptive weights
+ policy-bounded capabilities
+ no LLM call per request
+ inspectable audit trail
```
