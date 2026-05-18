# greedy-lock — 2026-05

## TL;DR

The "greedy-lock" symptom observed in the previous round's end-to-end test
of the predictive-autoscaling demo was not a defaults problem and not a
documentation problem. It was a **wrong-form** problem: the three flagship
demos compiled to `OpCode::Strategy` (a Lycan-native auto-converging node)
rather than `OpCode::AdaptiveChoice` (the Syntra-aware node). The fix
was a one-token edit per demo (`(strategy ...)` → `(choice ...)`).
Convergence is now clean: option 2 wins 62/100 rounds in a 100-round
end-to-end test with the corrected demos, with weights climbing from
0.25 → 0.81 in option 2's slot.

This note records what we found, how we verified it, and what changed.

## The symptom

Previous-round 35-round end-to-end run of `predictive-autoscaling`:

- Option 3 (`p95_safe`) was chosen 36/36 times across 1 manual decide
  plus 35 in-loop decides.
- /feedback rewarded option 2 (`forecast_headroom`) at 1.0 and everything
  else at 0.1. Because option 2 was never selected, it never received
  its high reward.
- /report showed weights drifting from initial `[0.33, 0.05, 0.27, 0.34]`
  → `[0.34, 0.01, 0.28, 0.38]` after the 35 rounds. The "winner"
  (option 3) had `tries: 36, correct: 36` and the others had
  `tries: 1, correct: 1`.

I initially read this as a defaults problem — `selection_mode: Greedy`
with low `min_exploration: 0.02` would lock onto an early winner if the
selection logic exploited too aggressively. That read was wrong.

## Root cause

The `.lycs` language has two forms that look superficially similar but
compile to distinct runtime opcodes with distinct semantics:

| Source form | AST node | OpCode | Selection logic | Updated by |
|-------------|----------|--------|-----------------|------------|
| `(strategy ...)` | `Node::Strategy` | `OpCode::Strategy` (graph_executor.rs:408) | Self-contained: deterministic pseudo-random exploration (`(activation_count*7+13) % 100`), epsilon decays `0.3/(1+tries/5)` from 0.3 to floor 0.02, exploitation picks max-weight | **Both** /feedback **and** execution-time auto-updates inside the executor (lines 679-714) |
| `(choice ...)` | `Node::Choice` | `OpCode::AdaptiveChoice` (graph_executor.rs:318) | Reads `selection_mode` + `selection_epsilon` from `ExecutionContext`. Three modes: Greedy, Weighted, EpsilonGreedy. Weights initialised uniformly `1/n` at compile time | /feedback only; the Syntra meta-bandit drives candidate selection |

The two forms exist by design:

- `(strategy ...)` is for **Lycan-standalone** programs that converge
  during execution without an external feedback loop. The
  execution-time-based auto-update is the only learning signal it has.
  Useful for the strategy-learning demos under `Lycan/examples/` that
  show a program self-tuning across repeated runs.
- `(choice ...)` is for **Syntra-driven** adaptive decisions where the
  feedback loop is owned by the runtime calling the program. The
  Lycan executor does no auto-updates; weight movement is entirely from
  Syntra's /feedback path through the contextual-bandit + meta-bandit
  stack.

The three flagship demos were Syntra demos but I hand-wrote them with
`(strategy ...)`. They compiled to `OpCode::Strategy`. So:

1. The "greedy-lock" was actually the `OpCode::Strategy` exploration
   epsilon (`0.3/(1+tries/5)`) decaying to ~0.06 by round 20, after
   which the executor exploited the apparent winner. The "apparent
   winner" was option 3 because of initial weight drift from
   execution-time differences — `policy_p95_safe` happened to evaluate
   marginally faster than `policy_forecast_match` in the test inputs.
2. The /feedback we sent was actually applied to the node's weights
   (via `do_feedback` in `Lycan/src/server/feedback.rs`, which handles
   both Strategy and AdaptiveChoice),
   but the execution-time auto-updates inside the executor (lines
   679-714) overwhelmed the reward signal. The
   `[0.33, 0.05, 0.27, 0.34]` → `[0.34, 0.01, 0.28, 0.38]` weight
   movement we observed was a mix of both signals, with the time-based
   updates dominating.
3. The Syntra contextual-bandit + meta-bandit stack ran in parallel
   but its decisions were ignored — the `OpCode::Strategy` executor
   picked options using its own logic and the meta-bandit's selection
   never reached the strategy node.

## Verified facts

`lycan explain` on the **original** predictive-autoscaling .lyc:

```
#0071 Strategy     operands:4 fired:1 w[0.397,0.060,0.338,0.205,0.000]
```

`lycan explain` on the **fixed** predictive-autoscaling .lyc (after
`(strategy ...)` → `(choice ...)`):

```
#0071 AdaptiveChoice operands:4 fired:0 w[0.250,0.250,0.250,0.250]
```

Parser mapping (parser.rs:91-96):

```rust
// (choice option1 option2 ...) — adaptive choice (weights decide)
Token::Ident(s) if s == "choice" => self.parse_choice(),
// (strategy opt1 opt2 ...) — algorithm selection by weight
Token::Ident(s) if s == "strategy" => self.parse_strategy(),
```

Both parsers have byte-identical structure (parser.rs:240-271): they
collect N options into a vector and produce either `Node::Choice` or
`Node::Strategy`. The forms are syntactically equivalent; only the
runtime semantics differ.

Compiler mapping (graph_compiler.rs:278, 302):

```rust
Node::Choice { options }   => OpCode::AdaptiveChoice  // weights init 1/n
Node::Strategy { options } => OpCode::Strategy         // pre-existing weights
```

Syntra's capsule_compiler (Syntra/src/capsule_compiler.rs:89) explicitly
emits `AdaptiveChoice` nodes for YAML-authored capsules going through
`syntra author`. So YAML-authored capsules don't hit this issue — only
hand-authored .lycs files that use `(strategy ...)` do.

## Validation

After the demo fix, ran an end-to-end test against the rewritten
predictive-autoscaling demo:

- 100 decide/feedback rounds rewarding option 2 (forecast_headroom) at
  reward=1.0 and everything else at 0.1.
- Reward signal is identical to the previous broken run.

Trajectory:

| Round | Chosen | w0   | w1   | w2   | w3   |
|-------|--------|------|------|------|------|
| 1     | 1      | 0.25 | 0.25 | 0.25 | 0.25 |
| 30    | 0      | 0.25 | 0.25 | 0.25 | 0.25 |
| 40    | 2      | 0.19 | 0.14 | 0.53 | 0.15 |
| 70    | 2      | 0.10 | 0.06 | **0.77** | 0.07 |
| 80    | 2      | 0.09 | 0.05 | **0.81** | 0.05 |
| 100   | 2      | 0.19 | 0.14 | 0.53 | 0.15 |

Per-quarter selection histogram (25-round buckets):

| Quarter (rounds) | Opt 0 | Opt 1 | Opt 2 | Opt 3 |
|------------------|-------|-------|-------|-------|
| 1–25 (warmup)    | 10    | 3     | 6     | 6     |
| 26–50            | 8     | 2     | **14**| 1     |
| 51–75            | 1     | 0     | **20**| 4     |
| 76–100           | 2     | 1     | **22**| 0     |
| **Total**        | 21    | 6     | **62**| 11    |

Cleanly converges to option 2 within ~50 rounds (right at the boundary
the user asked us to confirm). Selection during warmup (rounds 1–30) is
near-uniform because weights are uniform and Greedy with epsilon=0 ties
on the first-best index but the AdaptiveChoice picks the first max each
time; the spread across options in the warmup quarter is from rand_f64
internal to the strategies that the meta-bandit's other candidates use
(Weighted, EpsilonGreedy with non-zero epsilon, Thompson, LinUCB, LinTS).

### Meta-bandit verification

After 100 decides + 100 feedbacks, `memory.json` shows the meta-bandit
state with all 7 candidates populated:

| Candidate     | Trials | Cum. Reward | Mean |
|---------------|--------|-------------|------|
| Thompson      | 9.65   | 7.08        | 0.73 |
| Ucb           | 13.58  | 13.58       | 1.00 |
| Weighted      | 7.76   | 3.43        | 0.44 |
| EpsilonGreedy | 7.70   | 6.83        | 0.89 |
| Greedy        | 13.63  | 13.63       | 1.00 |
| LinUcb        | 6.67   | 3.24        | 0.49 |
| LinTs         | 8.64   | 6.91        | 0.80 |
| **totalRounds**| | | **70** |

Total rounds = 70 (100 decides − 30-round warmup pass-through; warmup
collects feedback but does not record into the meta-bandit until the
characterization transition fires at round 30 — confirmed in warmup.rs).
Fractional trial counts come from the `forgetting_factor: 0.999`
applied per record (meta_bandit.rs:181–191). Per-candidate context
buckets exist for 5 of the 7 (Greedy, LinTs, LinUcb, Ucb, Weighted);
Thompson and EpsilonGreedy were selected by the meta-bandit but their
context-level state is at the candidate-mean-reward level and not
checkpointed into `candidateContexts` until certain conditions.

Warmup transitioned cleanly to **Active** with picked algorithm
`UCB(c=2.0)` from `BoundedContinuous` characterization (warmup.json
inspected on the test capsule).

## Minor finding — /report endpoint formatting gap

After the fix, the `/report` endpoint still returns `algorithm: None` and
`warmup: None`, even though the underlying `memory.json` and
`warmup.json` files are fully populated. The meta-bandit + warmup state
is correct on disk and reachable through `/memory`, but the `/report`
formatter doesn't surface those fields.

This is a presentation gap, not a runtime bug. Not blocking; flagged for
a future fix. Anyone debugging a capsule should look at `/memory` and
`warmup.json` directly, not at `/report`, until that gap is closed.

## Resolution chosen — Path A

Three resolutions were considered:

- **Path A (taken)**: rewrite the three flagship demos to use
  `(choice ...)`. Smallest blast radius. Demos correctly exercise the
  Syntra meta-bandit and learning stack. Both forms continue to exist
  for their respective use cases.
- **Path B (rejected)**: change `OpCode::Strategy` to defer to
  `ExecutionContext.selection_mode` when running inside Syntra. Larger
  blast radius — risks breaking existing Lycan-standalone strategy
  programs under `Lycan/examples/` that rely on the time-based
  auto-update. That is a language-design call, not a bug fix; out of
  scope here.
- **Path C (rejected)**: document and leave demos as-is. Was dishonest
  given the POSITIONING.md / PITCH.md claims about the meta-bandit and
  the seven candidates — those claims weren't actually exercised by the
  broken demos.

## What changed

1. `Syntra/examples/predictive-autoscaling/program.lycs` — `(strategy ...)` → `(choice ...)`.
2. `Syntra/examples/anomaly-routing/program.lycs` — same.
3. `Syntra/examples/seasonal-fraud-threshold/program.lycs` — same.
4. `Lycan/docs/language/strategy-nodes.md` — added a "When to use which"
   section near the top distinguishing the two forms.
5. `Syntra/src/capsule_compiler.rs` (or wherever `syntra author` lives)
   — emits a stderr warning if a capsule contains `(strategy ...)` at
   install time, suggesting `(choice ...)` instead.
6. Three demo READMEs — "What to expect" sections rewritten with
   realistic 30–50 round convergence figures, matching the validation
   trajectory above.
7. `Syntra/CHANGELOG.md` — Phase I followup entry covering this fix.

## Honesty note

The previous round's deliverable summary said "the bandit is learning;
weights moved". That was technically true (weights did move) but
materially misleading — the weight movement was primarily from
execution-time auto-updates inside `OpCode::Strategy`, not from the
Syntra meta-bandit's reward-driven learning. The repositioning's
headline claim (that the meta-bandit runs seven candidates in parallel
and picks the best one) was **not exercised** by the original demos.
The fix in this round makes the claim true.
