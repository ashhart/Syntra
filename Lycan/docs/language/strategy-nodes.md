# Strategy Nodes

Lycan has **two** adaptive-decision forms. They look superficially
similar — both list N options and the runtime picks one — but they
compile to different runtime opcodes with different semantics. Picking
the wrong form is a real authoring bug and is easy to do.

## When to use which

| Form | Use when |
|------|----------|
| `(strategy ...)` | The program is **self-contained** and converges during repeated execution. There is no external feedback loop. The runtime auto-updates weights from execution-time differences across options. Suitable for Lycan-standalone programs where the only learning signal is "which option ran fastest while producing the consensus answer". The strategy-learning demos under `examples/strategy-learning/` are the canonical use case. |
| `(choice ...)` | An **external runtime owns the feedback loop**. Typical case: a capsule installed in [Syntra](https://github.com/ashhart/Syntra), whose `/decide` returns an option and whose `/feedback` carries the reward back. The Lycan executor does *no* auto-updates; weight movement comes entirely from the runtime's feedback path. The Syntra contextual-bandit + meta-bandit stack drives selection. |

**Rule of thumb:** if you can name an external system that posts
`/feedback` to your program, use `(choice ...)`. If your program is
expected to learn purely by running over and over, use `(strategy ...)`.

The two forms are not interchangeable. `syntra author` emits a
stderr warning if it encounters `(strategy ...)` in a capsule being
installed, because that is almost always a bug — a Syntra capsule
should be using `(choice ...)`. See
[Syntra/docs/investigations/greedy-lock-2026-05.md](https://github.com/ashhart/Syntra/blob/main/docs/investigations/greedy-lock-2026-05.md)
for the bug write-up that prompted this distinction.

## `(choice ...)` — externally-driven adaptive choice

```lisp
($ chosen (choice
  (option_a args...)
  (option_b args...)
  (option_c args...)))
```

Compiles to `OpCode::AdaptiveChoice`. The compiler initialises the
node's weights uniformly at `1/N` per option. The executor reads
`selection_mode` and `selection_epsilon` from the runtime's
`ExecutionContext` (Greedy, Weighted, or EpsilonGreedy modes are
supported). Weights are updated only by the runtime's feedback path,
not by the executor.

When running inside Syntra, the meta-bandit picks the active
selection algorithm from the seven-candidate portfolio (Thompson,
UCB1, EpsilonGreedy, Weighted, Greedy, LinUCB, LinTS), the capsule's
learning config drives `selection_mode` / `selection_epsilon` /
`min_exploration`, and `/feedback` records reward against the
chosen option's bucket.

## `(strategy ...)` — self-converging strategy node

Strategy nodes are the original Lycan invention. Multiple
implementations compete and the runtime learns which is best across
repeated runs of the same compiled `.lyc`.

```lisp
($ result (strategy
  (option_a args...)
  (option_b args...)
  (option_c args...)))
```

Compiles to `OpCode::Strategy`. Each option is a function call that
returns a value. The runtime:

1. **Explores** options through epsilon-greedy selection. Epsilon
   decays as `0.3 / (1 + tries/5)`, floored at `0.02`. "Random" is a
   deterministic pseudo-random keyed off the node's activation count.
2. **Measures** wall-clock execution time per option.
3. **Validates** correctness via contract (SameOutput or
   WithinTolerance, see below). If neither contract is set, the
   default path runs only the selected option.
4. **Rewards** fast correct options, punishes incorrect ones. Weight
   updates are derived from execution-time deltas, not from any
   external signal.
5. **Persists** learned weights in the `.lyc` binary across runs.

This form is **not** the right choice for capsules running inside
Syntra. The executor's auto-updates from execution-time deltas will
fight the contextual-bandit's reward-driven updates, and the
meta-bandit's candidate selection will not reach the strategy node.
Use `(choice ...)` instead.

## Contracts

Strategy nodes enforce correctness:

- **WithinTolerance** (default): all options must agree within epsilon
- **SameOutput**: all options must produce identical output

Options that disagree with the majority get punished. Effectful code inside strategy options is rejected.

## When learning actually fires

Strategy nodes learn *conditionally*. Knowing when they don't is important.

**WithinTolerance** updates weights only when a majority of options produce numerically similar results. Specifically: each option's value is compared to the median across options; the option is marked correct if `|value − median| ≤ tol`. Weights update only if more than half the options are marked correct (`has_consensus`). If methods produce numerically different answers, no learning fires — stats are tracked but weights stay at their initialization.

Tolerance is the *last element of the weights vector*, in the same units as the option results. The compiler does not currently allocate a separate tolerance slot, so if your strategy has N options, the Nth option's selection weight is reinterpreted as the tolerance. With default initialization (`1/N` per option) that gives a tolerance of `1/N` — typically far too tight for any computation whose result is in the hundreds, thousands, or beyond.

**Practical implication:** numerical algorithms that legitimately differ — different integration methods on a peaky integrand, different solvers on a stiff ODE, different optimizers on a non-convex landscape — will produce results that disagree by more than `1/N`. Consensus will silently fail, and the strategy will explore-but-never-converge.

**Workarounds:**

- For algorithm-comparison strategies where results are values rather than identities, use a relative-tolerance reward function and report outcomes via `/feedback` rather than relying on the WithinTolerance contract to derive them automatically.
- **SameOutput** uses string equality, not numeric distance — useful when options produce structured outputs (parsed JSON, canonical strings) that should match exactly.
- **No contract** (default if neither is specified) runs only the selected option per call and learns from timing alone. Use this when correctness is asserted externally rather than via cross-option comparison.

**SameOutput** has a related but distinct constraint: it stringifies each option's result and uses a majority vote on string equality. Stable for symbolic outputs; fragile for floating-point results because two methods that agree to 10 decimal places will still produce different string representations.

**Both contracts gate learning on consensus.** If your options are correct but produce numerically divergent representations, the contract's safety default prevents weight updates that might encode the wrong winner. This is intentional. It also means strategies whose options *should* disagree (e.g., comparing approximation methods of differing accuracy) need their reward signal supplied externally.

## Example

```lisp
(F sum_loop (n)
  ($! total 0) ($! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)

(F sum_formula (n)
  (/ (* n (+ n 1)) 2))

($ result (strategy (sum_loop 5000) (sum_formula 5000)))
```

After multiple runs:

```
Fresh:    weights [0.500, 0.500]    — no preference
Run 10:   weights [0.010, 0.990]   — formula wins
Output:   12502500                 — correct every run
```

## AdaptiveChoice

For semantic decisions (not algorithm competition):

```lisp
($ action (choice "scale_up" "hold" "scale_down"))
```

Weights represent learned preference, updated via feedback.

## Feedback

External systems can report outcomes:

```bash
lycan feedback program.lyc 42 --option 1 --reward 1.0
```

Or via API:

```bash
curl -X POST .../feedback -d '{"strategyId":42,"option":1,"reward":1.0}'
```

## Viewing what the program learned

```bash
lycan learn-report program.lyc
lycan improve-report program.lyc
```
