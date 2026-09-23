# Lycan Learning Semantics

Status: Draft v0.2, 2026-09-23. Normative description of how a running Lycan
graph learns: the weight updates of the `Strategy`, `AdaptiveChoice`,
`Feedback`, `Branch` and `Adapt` opcodes, and the randomness they use.

v0.1 (2026-09-08) also specified the learning layer of the v1 Syntra server
(warmup lifecycle, reward characterization, ADWIN change detection, the
`/feedback` pipeline, meta-bandits, hierarchical capsules, OOD refusal and
`memory.json`). Syntra v2 removed that layer. Its decision core is specified
in [`docs/design/v2-decision-core.md`](../../design/v2-decision-core.md), and
it does not run Lycan's learning opcodes: installing a feature program that
contains a `choice`, `strategy` or `feedback` node is refused. The sections
below describe the executor only. Section numbers changed with v0.2: v0.1's
§4 (executor weight updates, cited as §4.1 to §4.3 elsewhere) is §2 here.

The key words MUST, MUST NOT, SHOULD, MAY are used as in RFC 2119. `[verified]`
marks facts checked against the source files named; `[GAP]` a divergence
between intent and implementation; `[CURRENT-BEHAVIOR]` a choice a future
version may change.

## 1. Scope and state

The learning opcodes mutate the weights of the in-memory `NeuralGraph`
while it runs, with fixed constants (`lr 0.08` for `Strategy` contracts,
`lr 0.05` for `Feedback`, clamp `[0.01, 0.99]`, renormalize), and journal
every mutation in the graph's journal [verified `src/graph.rs`,
`src/graph_executor/exec.rs`]. Per-option statistics (`OptionStats`: tries,
correctness, timings) live in the executor for the length of the run.

Weights persist only as far as the graph does. The `lycan` CLI compiles and
runs a program but does not write the graph back to the `.lyc` file, so every
`lycan` run starts from the compiled weights [verified `src/bin/lycan.rs`:
only `compile` writes a file]. A host that keeps a `NeuralGraph` and runs it
repeatedly (or calls `to_bytes()` after a run) carries the learned weights
forward. `[CURRENT-BEHAVIOR]`

## 2. Executor-internal weight updates

Initial weights at compile time: `AdaptiveChoice` = `1/n` equal [verified
`src/graph_compiler.rs`]; `Strategy` = `1/n` plus a **trailing epsilon slot** `1e-6` for
WithinTolerance tolerance [verified `src/graph_compiler.rs`].

All weight mutations converge on: per-weight `clamp(0.01, 0.99)` then, when
`sum > 0.0`, renormalize (`w_i /= sum`) [verified `src/graph_executor/exec.rs`; `src/graph_executor/state.rs`]. Every mutation journals a
`JournalEntry { run_number, node_id, mutation: MutationKind::WeightUpdate, reason: u32::MAX }`
[verified `src/graph.rs`; sites `exec.rs`]. There is no
`WeightUpdate` struct [verified grep; the 2026-09-08 fact-report review].

### 2.1 `Strategy` opcode (contracts) [verified exec.rs]

Common constants: learning rate **0.08** [verified `exec.rs`]; majority/consensus
guard `count > n_options / 2` (integer division) [verified `exec.rs`]; speed score

$$\text{score}_i = 1 - \frac{2\,(t_i - t_{\min})}{\text{range}} \qquad (\text{NEVER clamped to } [0,1]; \text{ only } w_i + 0.08\cdot\text{score}_i \text{ is clamped})$$

[verified `exec.rs`; fact-report claim that the score itself is clamped to [0,1]
REFUTED].

| Contract | Reward derivation | Update |
|---|---|---|
| **SameOutput** | Run ALL options, time each (`as_nanos`); plurality vote over stringified outputs; `correct_i ⇔ result_i == majority` | if `has_majority`: wrong options `w_i = (w_i − 0.2).clamp(0.01, 0.99)` [exec.rs]; correct options `w_i += 0.08 · score_i` with `t_min` = min time among CORRECT options, `max` = max time over ALL options, `range = max − t_min`; **no reward when `range ≤ 0`** [exec.rs]; renormalize [exec.rs]. **No majority → NO weight update** (stats still recorded) [exec.rs] |
| **WithinTolerance** | Run ALL options; numericize (Float/Int direct; `Str` parsed as comma list and summed; else 0.0); reference = median; `tol` = trailing epsilon weight (default 1e-6) [exec.rs]; `correct_i ⇔ |v_i − median| ≤ tol` | identical shape (punish −0.2 / reward `0.08·score` / renormalize), but normalization covers only `weights[..n]` with `n = len−1` — the epsilon slot is excluded from both learning and renorm [exec.rs]. No consensus → no update |
| **Fallback** (no contract) | ONE option per activation; selection = ε-exploration over least-tried, else argmax weight | below |

Fallback exploration [verified `exec.rs`]:

$$\varepsilon = \max\!\left(\frac{0.3}{1 + \text{total\_tries}/5},\; 0.02\right)$$

the explore gate is **deterministic pseudo-random**, `explore ⇔ (activation_count·7 + 13) mod 100
< ε·100` [verified `exec.rs`] (it consumes no RNG); exploring picks the least-tried option set,
ties broken by `activation_count % |candidates|` [verified `exec.rs`]. Once **every** option
has `tries > 0` and the average-time range > 0, ALL options are updated every activation by
`w_i += 0.08 · score(avg_time_i)` (min/max over average times), clamped and renormalized [verified
`exec.rs`]. There is no "+0.1 winner / ×0.9 others" rule [verified grep of `exec.rs`;
the 2026-09-08 fact-report review]. Chosen options always get `correct += 1` in stats (no verdict exists) [verified
`exec.rs`].

### 2.2 `AdaptiveChoice` + `Feedback` opcodes

`AdaptiveChoice` performs selection only [verified `exec.rs`]:

* WithinTolerance reserves the trailing weight as epsilon slot (`n_options = len − 1`)
  [verified `exec.rs`].
* Mode from `ExecutionContext`: `Greedy` (argmax), `Weighted` (roulette, one RNG draw),
  `EpsilonGreedy` (two draws). Without a context the executor uses `Greedy`; the default
  context is `Greedy` with ε `0.10` [verified `src/context.rs`].
* Chosen index is stashed as `node.bias` for the `Feedback` opcode [verified `exec.rs`].

`Feedback` opcode is the ONLY weight-update path for AdaptiveChoice/Strategy targets from user
programs [verified `exec.rs`]:

Compile-time enforcement: the graph compiler refuses a `Feedback` whose target
name is unbound or is bound to a non-choice value (fail-closed; it previously
compiled to a bare `LoadVar` reference and the executor silently dropped the
credit). Non-`Ident` targets (inline `choice`/`strategy` nodes) are unaffected
[verified `src/graph_compiler.rs` `Node::Feedback` arm].

* Reward coercion: Float/Int passthrough; `Bool(true) → +1.0`, `Bool(false) → −1.0`, other →
  `0.0` [verified `exec.rs`].
* `lr = 0.05` (fixed); chosen `w += r·lr`; each other option `w −= r·lr/(n−1)` (**additive** share,
  not multiplicative decay); clamp `[0.01, 0.99]`; renormalize [verified `exec.rs`].

There is no decaying learning rate in the executor; the only decaying quantity `0.3/(1+tries/5)`
capped at floor `0.02` is the Strategy **exploration ε** of §2.1, not an lr [verified
`exec.rs`; the 2026-09-08 fact-report review].

### 2.3 `Branch` and `Adapt`

* `Branch` [verified `exec.rs`]: taken slot `+0.01`, untaken slot `−0.01`, **queued** into
  `weight_deltas` and flushed post-run: `(w + δ).clamp(0.01, 0.99)` then per-touched-node
  renormalization [verified `src/graph_executor/mod.rs`; `state.rs`]. No reward
  term and no EMA.
* `Adapt` [verified `exec.rs`]: rebinds a `GraphFn` variable's body; performs **zero**
  weight arithmetic. (Fact-report "Adapt: w += 0.05·(r−w)" REFUTED — the 0.05 lr lives in
  `Feedback`, §2.2; the 2026-09-08 fact-report review.)

## 3. Randomness

`AdaptiveChoice` in `Weighted` and `EpsilonGreedy` modes draws from a
thread-local SplitMix64 seeded from the operating system's entropy for each
thread [verified `src/graph_executor/mod.rs`, `rand_f64`]. `seed_rng(seed)`
reseeds the calling thread's stream for reproducible runs; the `lycan` CLI
does not call it and reads no seed variable, so CLI runs that make weighted
or epsilon-greedy choices are not reproducible. `[CURRENT-BEHAVIOR]`

The `Strategy` exploration gate uses no randomness: it is keyed off the
node's activation count (§2.1).

## 4. Conformance requirements

A conforming implementation MUST satisfy each item below; boundary values
are exact.

* C-W1 — SameOutput, n=4: 2/4 agreement → NO weight change (majority needs `> n/2`, i.e. ≥3);
  3/4 → the 1 dissenter loses exactly 0.2 pre-clamp; correct options change only if `range > 0`.
* C-W2 — Speed score is unclamped: an option slower than `t_min + range/2` receives a negative
  delta; `w` result clamps to `[0.01, 0.99]`; post-update weights sum to 1 (WithinTolerance: sum of
  `weights[..n_options]` = 1 with the epsilon slot unchanged).
* C-W3 — Fallback: no weight update until every option has `tries > 0`; explore iff
  `(activation_count·7+13) % 100 < ⌊ε·100⌋`; ε floor 0.02 reached at `total_tries ≥ 70`
  (`0.3/(1+14) = 0.02`).
* C-W4 — Feedback opcode: `Feedback(true)` ≡ `r=1.0`, `Feedback(false)` ≡ `r=−1.0`; chosen
  `+0.05`, others `−0.05/(n−1)`, then renormalize; `Adapt` performs zero weight mutations.
* C-W5 — Branch deltas apply once, post-run, clamped, then per-node renormalized.
