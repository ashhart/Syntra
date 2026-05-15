# Lycan Benchmarks

This folder should contain reproducible benchmark code for Lycan and comparable
general-purpose language runtimes.

The benchmark goal is narrow:

> measure small, repeated, structured decision-runtime workloads.

It is not a claim that Lycan beats every general-purpose runtime at every task.

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

Before publishing numbers, every benchmark run should include:

- hardware model
- OS version
- Lycan version
- comparable runtime versions
- warmup runs
- median/min/max/p95 over many runs
- source `.lycs` timing separated from compiled `.lyc` timing

## Why This Matters

Lycan is not just about raw speed. The stronger runtime claim is:

```text
compiled graph execution
+ persistent adaptive weights
+ policy-bounded capabilities
+ no LLM call per request
+ inspectable audit trail
```
