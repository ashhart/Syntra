# Strategy Nodes

Strategy nodes are the core invention in Lycan. Multiple implementations compete. The runtime learns which is best.

## Declaring a strategy

```lisp
($ result (strategy
  (option_a args...)
  (option_b args...)
  (option_c args...)))
```

Each option is a function call that returns a value. The runtime:

1. **Explores** options through epsilon-greedy selection
2. **Measures** wall-clock execution time
3. **Validates** correctness via contract (SameOutput or WithinTolerance)
4. **Rewards** fast correct options, punishes incorrect ones
5. **Persists** learned weights in the `.lyc` binary across runs

## Contracts

Strategy nodes enforce correctness:

- **WithinTolerance** (default): all options must agree within epsilon
- **SameOutput**: all options must produce identical output

Options that disagree with the majority get punished. Effectful code inside strategy options is rejected.

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
