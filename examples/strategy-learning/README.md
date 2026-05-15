# Strategy Learning Demo

This is the best first demo to run.

It shows the core Lycan primitive in the smallest useful form: one strategy node, two valid implementations, one output contract, and weights that move after execution.

## What It Proves

`demo_learning.lycs` computes the same result in two ways:

- option 0: a slow loop
- option 1: a fast formula

Both options return the same value. The runtime measures them, keeps the output correct, and shifts the compiled graph binary toward the faster option.

## Run It Without Dirtying The Repo

```bash
cargo build --release

WORK=$(mktemp -d)
cp examples/strategy-learning/demo_learning.lycs "$WORK/learning.lycs"

./target/release/lycan compile "$WORK/learning.lycs"

for i in $(seq 1 20); do
  ./target/release/lycan "$WORK/learning.lyc" >/dev/null
done

./target/release/lycan learn-report "$WORK/learning.lyc"
```

Expected shape:

```text
weights: [0.0100, 0.9900, 0.0000]
winner: option 1
reason: fastest fully-correct option
```

The exact timings will vary by machine. The important behaviour is that the output stays correct while the winning weight moves toward the faster strategy.

## Files

| File | Purpose |
|---|---|
| `demo_learning.lycs` | First demo: slow loop vs fast formula |
| `demo_feedback_decision.lycs` | Delayed feedback demo for external reward signals |
| `demo_evolve_target.lycs` | Small target for autonomous evolution proposals |
| `demo_impossible.lycs` | Larger three-way strategy competition |

## What To Look For

Strategy learning is not hidden in a model trace. It is visible runtime state:

- `learn-report` shows tries, average timing, correctness, weights, and winner
- the `.lyc` binary carries the updated weights
- output correctness remains part of the contract
- bad or disagreeing options are punished instead of silently becoming winners
