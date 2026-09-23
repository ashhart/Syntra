# Strategy Nodes

Lycan has two adaptive-decision forms, `(strategy ...)` and `(choice ...)`,
and a `(feedback ...)` form that rewards a choice from inside the program.
Both forms list N options and the runtime picks one, but they compile to
different opcodes with different learning rules. The exact rules are in
[spec/learning-semantics.md](../spec/learning-semantics.md).

These are language features. Syntra's decision service does not use them:
a capsule's feature program may not contain a `choice`, `strategy` or
`feedback` node (installing one is refused with a 400), because the capsule
itself chooses the action and learns from rewards
([docs/design/v2-decision-core.md](../../design/v2-decision-core.md)).

## When to use which

| Form | Use when |
|------|----------|
| `(strategy ...)` | Several implementations of the same computation compete. The runtime compares their results and running times and moves weight toward the fast, correct ones. No outside signal is needed. |
| `(choice ...)` | The program picks one of several answers and learns from a reward you supply with `(feedback ...)`. The executor never changes its weights on its own. |

Weights live in the graph while it runs. The `lycan` CLI does not write
them back to the `.lyc` file, so each `lycan` run starts from the compiled
weights; learning shows up within a run, when a node executes many times
(in a loop, for example), or in a host that keeps the graph between runs.

## `(choice ...)` and `(feedback ...)`

```lisp
($ action (choice "scale_up" "hold" "scale_down"))
(!p "chose" action)
(feedback action 1.0)
```

`choice` compiles to `OpCode::AdaptiveChoice` with weights `1/N` per
option. It selects greedily by weight unless the execution context asks
for weighted (roulette) or epsilon-greedy selection, and it remembers which
option it chose. `(feedback <choice> <reward>)` compiles to
`OpCode::Feedback`: a positive reward raises the chosen option's weight by
`0.05 · reward` and lowers the others by an equal share, a negative reward
does the opposite, and the weights are clamped to `[0.01, 0.99]` and
renormalized. `true` counts as `1.0` and `false` as `-1.0`. The target must
be a variable bound to a `choice` or `strategy` node, or the program does
not compile.

## `(strategy ...)`

```lisp
(F sum_loop (n)
  ($! total 0) ($! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)

(F sum_formula (n)
  (/ (* n (+ n 1)) 2))

($ result (strategy (sum_loop 5000) (sum_formula 5000)))
(!p result)    ;; 12502500
```

`strategy` compiles to `OpCode::Strategy`. Each option is a function call
that returns a value. Depending on the node's contract, the runtime:

1. **Runs every option** (the SameOutput and WithinTolerance contracts),
   times each one and takes a majority vote on the results; or runs **one
   option** per activation (no contract), exploring the least-tried option
   with a probability that decays as `0.3 / (1 + tries/5)` down to `0.02`.
   The exploration gate is deterministic, keyed off the node's activation
   count.
2. **Rewards** fast options that agree with the majority and **punishes**
   those that disagree, with a learning rate of `0.08`; weights are clamped
   and renormalized.

## Contracts

Strategy nodes enforce correctness:

- **WithinTolerance**: all options must agree within a tolerance. The
  compiler gives every `strategy` in source this contract.
- **SameOutput**: all options must produce identical output.
- **No contract**: one option runs per activation.

SameOutput and no contract exist in the graph format, for graphs built
other than from source. Options that disagree with the majority get
punished, and the verifier rejects a contract node whose options contain
effectful code (printing, reading input).

## When learning actually fires

Strategy nodes learn only when their options agree, so it matters when they don't.

Under WithinTolerance, each option's value is compared with the median of all options, and an option counts as correct if it is within the tolerance of the median. Weights change only when more than half the options are correct. When the options produce numerically different answers, the node records statistics but its weights stay where they started.

The tolerance is the last element of the weights vector, in the units of the option results, and the compiler sets it to `1e-6`. That is far too tight for computations whose results legitimately differ in the last digits: different integration methods on a peaky integrand, different solvers on a stiff ODE, different optimizers on a non-convex problem. Such strategies never reach consensus, so they explore and never converge.

What to do instead:

- When the options compute values that should differ, compute a relative-tolerance reward yourself and report it with a `(feedback ...)` node.
- SameOutput compares strings, not numbers. It suits options that produce structured outputs (parsed JSON, canonical strings) that should match exactly. It is fragile for floating-point results: two methods that agree to 10 decimal places still print differently.
- With no contract, only the selected option runs and the node learns from timing alone. That fits cases where something outside the node checks correctness.

Both contracts refuse to learn without consensus, on purpose: an update from options that disagree might reward the wrong one. Strategies whose options should disagree, such as approximation methods of different accuracy, need their reward from `(feedback ...)`.

## Viewing a program's weights

`lycan inspect program.lyc` prints the graph as JSON, including each
node's weights, and `lycan explain program.lyc` prints it as text.
