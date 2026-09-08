# Lycan Scoping and Execution Semantics

Status: Draft v0.1 — describes implementation as of 2026-09-08 (graph FORMAT_VERSION=5)

Normative description of name resolution, frame/stack discipline, statement-vs-expression
discipline, control-flow unwinding, the learning-node contract (`choice` / `strategy` /
`feedback` / `guard`), and the builtin/capability surface. RFC 2119 keywords; citations
are `file:line` into the working tree as of that date, re-verified against it (debug build
plus behavioural probes on both backends). "CURRENT behavior" marks observed facts that a
future revision may tighten; a conforming producer MUST NOT rely on them.

Syntax is in `grammar.md`; values, arithmetic, equality and arity guards are in
`value-model.md`; byte layout in `graph-binary-format.md`; containers and the
verify/no-verify entrypoint split in `capsule-format.md`; learning algorithms, reward
shaping and the capability ABI proper are in `learning-semantics.md` and
`capability-abi.md`.

## 1. Two backends, declared normative up front (normative)

One syntax, one AST, **two independently implemented semantics**:

| Backend | Executor | Reached by | Cite |
|---|---|---|---|
| **Compiled graph** — **NORMATIVE** | `GraphExecutor` (`graph_executor/exec.rs`) | `lycan <f.lyc>` (`LYCN` magic, after `verifier::verify`), `lycan decide`, capsules, server `/decide` | `bin/lycan.rs:186-221`, `capsule-format.md` §7 |
| **Tree-walking interpreter** — **LEGACY** | `Interpreter` (`src/interpreter.rs`) | `lycan <f>` for any non-`.lyc` extension, the REPL, legacy v1 `.lyc` | `bin/lycan.rs:167-184`, `:220-228`, `:1374-1408` |

**Normative declaration.** The compiled graph executor is the definition of Lycan
semantics. The tree-walking interpreter is a legacy development convenience retained for
`.lycs` authoring and the v1 binary path; where the two disagree, the compiled behaviour
is correct and the interpreter MUST be moved toward it (or the divergence MUST be kept
pinned here until it is). Rationale: shipped artifacts are `.lyc`; capsules, `decide`,
and the server execute only the graph path; the verifier only constrains the graph path;
and the learning nodes (`strategy`, `feedback`, weights, bias, journal) exist **only** in
the graph path — a program's adaptive behaviour is unobservable in the interpreter.

Consequences, normative:

1. A conforming **program** MUST be written so that its observable behaviour is identical
   under both backends **except** where this file records a divergence — which means it
   MUST avoid the constructs of §1.1 (string ordering, `!type`, reliance on immutability or
   on `undefined` errors, `!lambert` non-numeric arguments).
2. A conforming **test vector set** MUST carry one expectation column per backend for
   every divergent case (§10). A single-column vector set is non-conforming: it hides the
   split.
3. A conforming **toolchain** MUST run `verifier::verify` before executing any graph it did
   not just compile from source (`capsule-format.md` §7), because the executor's own
   guards are the second line of defence, not the first.

### 1.1 Master divergence table

| Behaviour | Tree-walker (legacy) | Compiled graph (**normative**) | Cite |
|---|---|---|---|
| Binding immutability (`$` vs `= $!`) | enforced: `'x' is immutable` | **erased**: `(= x …)` succeeds on immutable and on never-bound names | `interpreter.rs:63-73`, `environment.rs:57-70` vs `graph_compiler.rs:76-94`, `exec.rs:65-70` |
| Undefined variable | error `undefined 'n'` | evaluates to `Null` | `environment.rs:44-53` vs `exec.rs:60-64`, `:1073` |
| Block scope | `(B …)` pushes a frame; names die | `(B …)` → `Sequence`, no scope; names leak | `interpreter.rs:175-188` vs `graph_compiler.rs:217-222` |
| `each`/`W`/`#` scope | `each` pushes a frame; `W`/`#` push none | no scoping at all | `interpreter.rs:124-149`, `:109-122`, `:151-168` vs `exec.rs:1001-1022` |
| Nested loop variable | shadowed correctly | **clobbered** (loop vars are global slots, unsaved across calls) | `interpreter.rs:133-134` vs `exec.rs:1001-1022`, `:1024-1051` |
| Name resolution | **dynamic** (caller's frames), no capture | flat global slot-per-name | `value.rs:16-23` vs `graph_compiler.rs:375-384` |
| String ordering `(< "a" "b")` | `true` | error `cannot compare str and str` | `interpreter.rs:480` vs `exec.rs:1236-1245` |
| `!type` | `"int"`, `"fn"`, … | value stringified (`1`, `(fn)`) — mis-compile | `interpreter.rs:691-694` vs `graph_compiler.rs:364` |
| Unknown `!builtin` | error `unknown builtin '!x'` | `Noop` → `Null`, silently | `interpreter.rs:780-782` vs `graph_compiler.rs:365` |
| Function printing | `(F name)` | `(fn)` | `value.rs:65` vs `graph_executor/value.rs:53` |
| `!abs` on `±inf` | `inf` | error `abs requires finite float` | `interpreter.rs:589` vs `exec.rs:1158-1159` |
| `(not 1 2)` | arity error | silently drops operand 2 | `interpreter.rs:364-381` vs `exec.rs:153-156` |
| `(not)` | arity error | process panic | `interpreter.rs:364-381` vs `exec.rs:154` |
| `feedback` | no-op, `Null` | real weight update + journal | `interpreter.rs:324-327` vs `exec.rs:851-906` |
| `choice`/`strategy` selection | always option 0, weights ignored | weighted/greedy/ε-greedy selection, contracts, learning | `interpreter.rs:297-322` vs `exec.rs:180-582` |
| `~>` (`adapt`) target | must already exist | need not exist | `interpreter.rs:286` vs `exec.rs:814-827` |
| `!lambert` non-numeric args | silently `0.0` | capability type error | `interpreter.rs:730-736` vs `capability.rs:19-33`, `kernels.rs` arity/type checks |
| `!lambert` too-few args | `!lambert needs 8 args: r1x r1y r1z r2x r2y r2z tof mu` | `astro.lambertSolve expects 8 arguments, got 3` | `interpreter.rs:726-728` vs `kernels.rs:444-447` |
| Recursion limit | none (64 MiB stack) | `max_depth = 65536`, but stack still wins in practice (§3.7) | `bin/lycan.rs:3-7` vs `mod.rs:52`, `exec.rs:13-22` |
| Error diagnostics | `[runtime] msg`, no position | `[runtime] msg`, no node id | `error.rs:19-21` |

Verified pairs (source → compiled): `undefined 'y'` → `1` (block leak);
`undefined 'x'` → `cannot add null and int` (closure attempt);
`2 / 1` → `2 / 2` (shadowing); `1,1,2,2` → `1,9,2,9` (loop-var clobber);
`(F f)` → `(fn)`; `int` → `1` (`!type`); `inf` → `abs requires finite float`;
`'x' is immutable` → success; `undefined 'q'` → assignment succeeds.

## 2. Source scoping (tree-walker, legacy)

`Env` is a stack of frames, each a `HashMap<String, Slot { value, mutable }>`
(`environment.rs:7-20`). `new` starts with exactly one frame (`:22-27`).

| Operation | Rule | Cite |
|---|---|---|
| `push_scope` / `pop_scope` | push a fresh frame; pop **never** empties the stack (`if len > 1`) | `:29-37` |
| `define(name, v, mutable)` | insert into the **top** frame; an existing name in that frame is **overwritten regardless of mutability** | `:39-42` |
| `get(name)` | innermost frame outward; miss → `undefined '{name}'` | `:44-53` |
| `set(name, v)` | innermost frame outward; requires `mutable` else `'{name}' is immutable`; **not found anywhere → `undefined '{name}'`** (assignment never creates) | `:55-70` |
| `redefine(name, v)` | force-write wherever found; if nowhere, define **mutable in frame 0** | `:72-82` |

### 2.1 Bind and assignment rules (normative for this backend)

- `($ x v)` / `($! x v)` define in the **current** frame and evaluate to `Null`
  (`interpreter.rs:63-67`).
- Re-`$` the same name in the same frame is a **silent overwrite**, even when the existing
  binding is immutable — `define` is a `HashMap` insert. Immutability therefore only
  constrains `(= …)`. A conforming program MUST NOT read `$` as "cannot be rebound".
- `(F name …)` defines `name` **immutable** and evaluates to the function value itself
  (`interpreter.rs:75-86`), so `(F f () 1)` is usable as an expression.
- `(= x v)` requires an existing, mutable binding; it never creates one. Verified:
  `(= q 1)` with `q` unset → `[runtime] undefined 'q'`.

### 2.2 Frame discipline, per form

| Form | Pushes a frame? | Notes | Cite |
|---|---|---|---|
| `(B …)` | **yes** (push/pop around body) | bindings die with the block | `interpreter.rs:176`, `:181`, `:187` |
| function call | **yes** (params defined immutable in it) | `(^)` pops on the way out | `:344`, `:347`, `:354`, `:360` |
| `(each v …)` | **yes**; loop var defined **mutable**, initialised `Null`, then `set` per item | body may `(= v …)` | `:133-136`, `:140`, `:147` |
| `(W …)` | **no** | `($ x …)` inside the body redefines in the **enclosing** frame every iteration | `:109-122` |
| `(# n …)` | **no** | same leak as `W` | `:151-168` |
| `(if …)`, `(A …)`, `(I …)`, `(.. …)`, pipes | no | | `:98-107`, `:191-234`, `:241-281` |

Verified: `(B ($ y 1)) (!p y)` → `undefined 'y'`; `(each i (A 1) ($ z 1)) (!p z)` →
`undefined 'z'`. Under the normative backend both print `1` (§3).

### 2.3 Resolution is dynamic, and closures capture nothing

`LycanFn` stores only `{name, params, body, stateful}` (`value.rs:16-23`) — **no
environment** — despite the "Lexically scoped environment" comment at
`environment.rs:5`. A function body is therefore evaluated in the **caller's** frame
chain. Consequences, normative as stated:

- Plain and mutual recursion work (verified `(fact 5)` → `120`).
- A closure over captured state is **impossible**: `(F adder (x) (\ (y) (+ x y)))`,
  `($ add5 (adder 5))`, `(add5 1)` → `[runtime] undefined 'x'` (verified). `x` is gone
  when `adder` returns.
- Conversely, a function silently sees its **caller's** locals:
  `(F show () (!p hidden))` succeeds if the caller happens to bind `hidden`. A
  conforming program MUST pass every dependency explicitly.

### 2.4 `(~> name …)` (adapt) — redefinition

Builds a function value with `target`'s current params (or none, if the target is not a
function) and the given body, `redefine`s the binding, and evaluates to `Null`
(`interpreter.rs:283-295`). The target **MUST already exist**: `self.env.get(target)?`
(`:286`) errors `undefined 'name'`. Verified in the compiled path's opposite behaviour
(§3). Semantics of what `~>` is *for* (self-modification / deopt) belong to
`learning-semantics.md`; there is no journal entry and no revert.

## 3. Compiled scoping (normative backend)

There are **no frames**. The executor holds one `vars: HashMap<u32, GVal>`
(`mod.rs:18`, `:47-57`) and the compiler allocates **one slot per distinct name for the
whole program** (`get_or_create_var`, `graph_compiler.rs:375-384`; `var_map`, `:14`).
Therefore:

1. **No block scope, no shadowing, no nesting.** `(B …)` compiles to `Sequence`
   (`graph_compiler.rs:217-222`); `each` writes a global slot (`exec.rs:1001-1022`);
   `W`/`#` have no scope either. Verified: `($ v 1) (B ($ v 2) (!p v)) (!p v)` prints
   `2` then `2` (source prints `2` then `1`).
2. **`$` vs `$!` immutability is erased at compile time.** `Node::Bind` is destructured
   without `mutable` (`graph_compiler.rs:76-85`) and both `Bind` and `Assign` emit the
   same `StoreVar` (`:87-94`), which unconditionally overwrites (`exec.rs:65-70`).
   Verified: `(# 1 ($ x 1) (= x 2)) (!p x)` → `2` compiled; the tree-walker aborts with
   `'x' is immutable`. `(= q 1)` on a never-bound `q` also **creates** the slot → `1`.
3. **An undefined variable evaluates to `Null`, not an error.** `LoadVar` returns
   `unwrap_or(GVal::Null)` (`exec.rs:60-64`) and a bare `VarSlot` operand does the same
   (`:1073`). Verified: `(!p zzz)` → `null`; `(!p 1e3)` → `1 null` (the `e3` atom
   contributed by the number-lexer quiet, `grammar.md` §3.1); a failed closure attempt
   becomes `cannot add null and int`. **Normative (MUST):** a conforming program MUST
   bind before use; a conforming linter/compiler SHOULD warn on load-before-store, which
   is the only defence available on this path.
4. **Calls save and restore only the parameter slots**
   (`exec.rs:1024-1051`, `restore_vars` `:1053-1060`). Anything a callee writes that is
   *not* a parameter survives the call. Recursion is safe for parameters (verified
   `(fact 5)` → `120` compiled), and unsafe for everything else.
5. **`each` loop variables are clobbered by nested calls.** The loop writes `var_slot`
   directly (`:1012-1013`) and neither saves the old value nor restores it, and a nested
   `each` over the same name reuses the same slot. Verified: outer loop printing `i`
   before and after a call that itself iterates `i` yields `1,1,2,2` in the tree-walker
   but **`1,9,2,9`** compiled. A conforming program MUST use **distinct, unique loop
   variable names** at every nesting level.
6. **`(~> name …)` needs no prior binding**: the target slot is created if absent and the
   existing param slots are reused only if the slot already holds a `GraphFn`
   (`exec.rs:814-827`); otherwise the new function has **zero parameters**. This is a
   real divergence from §2.4 — an `~>` on a typo'd name silently defines a 0-param
   function on this path and errors on the legacy one.
7. **Recursion is capped at `max_depth = 65536` `exec_node` activations**, erroring
   `max recursion depth exceeded` (`mod.rs:52`, `exec.rs:13-22`). CURRENT caveat: each
   graph node costs several native frames, so a source-shaped recursion exhausts the
   64 MiB stack **first** and dies with `fatal runtime error: stack overflow` on **both**
   backends (verified exit `-6` for `(F f (n) (f (+ n 1)))`). The cap MUST NOT be relied
   on as a stack-safety guarantee; a conforming program MUST NOT recurse unboundedly.

## 4. There is no statement/expression split

Every node evaluates to a value; there are no statements, terminators, or declarations
(`grammar.md` §4, `parser.rs:17-36`). `run` folds the top-level nodes and reports the
**last** node's value (`interpreter.rs:35-44`; compiled: the entry `Sequence`'s value,
`graph_compiler.rs:31-49`, `exec.rs:584-593`).

### 4.1 Forms that evaluate to `Null`

| Form | Why | Cite |
|---|---|---|
| `($ …)`, `($! …)` | bind returns `Null` | `interpreter.rs:63-67`; `exec.rs:65-70` (`StoreVar`) |
| `(= …)` | assign returns `Null` | `interpreter.rs:69-73`; `exec.rs:65-70` |
| `(~> …)` | adapt returns `Null` | `interpreter.rs:294`; `exec.rs:826` |
| `(!p …)` | print returns `Null` | `interpreter.rs:526`; `exec.rs:753` |
| `(feedback …)` | `Null` on both paths (real work, no value) | `interpreter.rs:324-327`; `exec.rs:905` |
| `(choice)` / `(strategy)` with **no options** | `Null` | `interpreter.rs:301-303`, `:319-321`; `exec.rs:181-183`, `:272-274` |
| `(guard …)` with `< 3` operands | **compiled only**, and only on decode-without-verify paths | `exec.rs:257-259` (§6.4) |
| `(B)`, `(W c)`, `(each v xs)`, `(# n)` with **no body forms** | the "last value" accumulator starts at `Null` | `interpreter.rs:110`, `:132`, `:158`, `:177`; `exec.rs:585`, `:987`, `:1011`, `:616` |
| `(if c t)` with a false condition and no else | missing else ⇒ `Null` | `interpreter.rs:104-106`; compiled compiles the missing else to `ConstNull` (`graph_compiler.rs:116-119`) |
| A call whose body has zero forms | same accumulator | `interpreter.rs:350-361`; `exec.rs:1036-1047` |

### 4.2 Sequence forms report the last body form's value

`(B …)`, `(W …)`, `(each …)`, `(# …)` and every function/lambda body evaluate all body
forms and report the value of the **last one executed**
(`interpreter.rs:109-122`, `:124-149`, `:151-168`, `:175-189`, `:350-361`;
`exec.rs:584-593`, `:606-626`, `:981-999`, `:1001-1022`, `:1036-1047`). Verified
identically on both backends: `(!p (B 1 2 3))` → `3`, `(!p (W (< 0 0) 5))` → `null`,
`(!p (each x (A 7) 8))` → `8`, `(!p (# 2 9))` → `9`.

**Decision rule for "what does this program report":** the value of a sequence form is
the value of its last body form; it is `Null` if (a) the body never ran (`W` with a false
condition, `# 0`, `each` over an empty array), or (b) the last body form is itself a
`Null`-valued form from §4.1 (a bind, an assign, a `!p`). Loop forms report a *body*
value, never a count or an iteration index, and there is no loop result accumulator.

### 4.3 `(^ v)` unwinding, per backend

| Path | Mechanism | Where it stops |
|---|---|---|
| Tree-walker | `Control::Return(v)` propagates through `If`, `While`, `ForEach`, `Repeat`, `Block` and `call_fn`; **`run` returns it immediately**, terminating the whole program (`interpreter.rs:35-44`, `:116`, `:139-142`, `:162`, `:180-183`, `:353-356`) | the first `(^ …)`, at any depth, including top level |
| Compiled graph | `Flow::Return(v)` propagates through `Sequence` (`exec.rs:588-591`), `Loop` (`:992-995`), `ForEach` (`:1015-1018`), `Repeat` (`:619-623`) and turns a **call** into that value (`:1039-1042`); `GraphExecutor::run` converts it to a plain value with `into_val()` (`mod.rs:82`, `:36-40`) — the value is *returned*, execution does **not** resume | same observable behaviour: later operands of the entry `Sequence` are never reached |

Verified: `(!p 1) (^ null) (!p 2)` prints `1` only, on both backends. `(^ v)` inside a
function is the function's result; at top level it is an **early program exit** with `v`
discarded by the CLI (which prints nothing) — the idiom `(… ) (^ null)` at end of file is
CURRENT practice for "stop here" (e.g. `examples/lycan/calculator.lycs:8-9`). A
conforming program MUST NOT expect `(^ …)` to unwind only one loop: it unwinds to the
program boundary.

## 5. Functions and calls

| Rule | Tree-walker | Compiled graph | Cite |
|---|---|---|---|
| Callable kinds | `Value::Fn` only; else `cannot call {t}` | `GVal::GraphFn` only; else `cannot call {t}` | `interpreter.rs:336-342`, `:339-341` / `exec.rs:1024-1050`, `:1049` |
| Too few args | missing params bound to `Null` | param slot written `Null` | `interpreter.rs:345-347` / `exec.rs:1031-1033` |
| Too many args | **silently discarded** | **silently discarded** | `interpreter.rs:345` (`enumerate` over params) / `exec.rs:1031` |
| Param bindings | immutable, in a fresh frame | global slots, restored after the call | `interpreter.rs:347` / `exec.rs:1030-1034`, `:1053-1060` |
| Body value / empty body | last body form; `Null` if none | same | `interpreter.rs:350-361` / `exec.rs:1036-1047` |
| Recursion | unbounded (64 MiB stack) | depth cap + stack limit (§3.7) | `bin/lycan.rs:3-7` / `mod.rs:52` |
| `F!` | `stateful` stored, never read | `stateful` dropped entirely | `value.rs:21-22` / `graph_compiler.rs:172` |

Verified: `(F f (a) a)` with `(f 1 2)` → `1`; with `(f)` → `null` (both backends).
`F!` is **inert**: there is no per-name persistent state, no state slot, and no observable
difference from `F` on either path — see Open normative decisions.

Pipes are calls in disguise: `(|> d f)` → `call_fn(f, [d])`, `(|? d p)` → truthiness
filter, `(|* d f)` → map, `(|+ d f init)` → left fold with `init` (`Null` if omitted)
(`interpreter.rs:241-281`; `exec.rs:913-955`). `|?`, `|*`, `|+` require an `Array` and
error `expected array, got {t}` (source: `cannot …`-free path uses the same
`expect_array`, `interpreter.rs:499-506`). Verified: `(|> (A 1 2) (\ (a) (!len a)))` → `2`;
`(|* (A 1 2) (\ (x) (+ x 1)))` → `(A 2 3)`; `(|? (A 1 2) (\ (x) (> x 1)))` → `(A 2)`;
`(|+ (A 1 2) (\ (a b) (+ a b)) 0)` → `3`.

## 6. `choice`, `strategy`, `feedback`: the value-and-bias contract

### 6.1 `choice` — returns the **chosen value**, never an index

- **Tree-walker:** `Node::Choice` always evaluates **option 0**, ignoring weights
  entirely; empty ⇒ `Null` (`interpreter.rs:297-304`). `Strategy` identical
  (`:315-322`). So an adaptive program's output in the interpreter is its
  highest-index-0 option, always.
- **Compiled (normative), `AdaptiveChoice` (`exec.rs:180-250`):**
  1. `Null` immediately if `weights` or `operands` is empty (`:181-183`).
  2. `n_options = weights.len()`, minus the trailing tolerance slot **iff** contract is
     `WithinTolerance` and `weights.len() > 1`; then clamped to `operands.len()`; `0` ⇒
     `Null` (`:186-196`).
  3. Selection mode and epsilon come from the `ExecutionContext`; with **no context** the
     default is `Greedy` with `epsilon = 0.0` (`:198-201`, `context.rs`).
     `Greedy` = first maximum weight scan; `Weighted` = cumulative pick over
     `weights[..n_options]` scaled by `learning::rand_f64()` (`sum <= 0.0` ⇒ index 0);
     `EpsilonGreedy` = random index with probability `epsilon`, else greedy
     (`:203-241`).
  4. **`node.bias = chosen_index` (`:244`) — this is the whole feedback interface.**
  5. The chosen option's node is executed and **its value is the value of the `choice`
     form** (`:245-249`). There is no index-visible form, no `chosen` accessor, and no
     way for a program to read `bias`.
  `lycan compile` emits equal weights `1/n` for `choice` (`graph_compiler.rs:278-289`),
  so a freshly compiled greedy choice picks option 0 — verified `(!p (choice 10 20))` →
  `10` on both backends.

**Normative contract (MUST):** `choice` evaluates to the selected **option's value**; the
selected **index** is written to `bias` and is readable only by the executor
(`feedback`, `evolve`, `decide`). A program that needs the index MUST re-derive it (the
in-repo idiom is a dispatch function comparing the chosen value against candidates), and
MUST NOT assume option 0.

### 6.2 `strategy` — compiled only, and it runs every option

`Strategy` (`exec.rs:270-582`) guards `Null` on empty weights/operands (`:272-274`), sets
`n_options = min(weights.len(), operands.len())` (`:275`), and then branches on
`Contract`:

| Contract | Behaviour (normative) | Cite |
|---|---|---|
| `SameOutput` | runs **all** options, majority vote over their **printed** (`Display`) strings, learns only if `majority_count > n_options/2`, returns the highest-weight *majority-agreeing* option's value; `bias` = that index | `:288-391` |
| `WithinTolerance` | runs **all** options, projects each to `f64` (`Str` → sum of comma-separated numbers, anything non-numeric → `0.0`), compares against the **median** within `tol = weights.last()`, consensus `> n_options/2`, returns the highest-weight in-tolerance option's value | `:393-495` |
| `None` | single option executed: deterministic pseudo-exploration `(activation_count*7+13) % 100 < epsilon`, epsilon `= max(0.3/(1+tries/5), 0.02)`, then timing-based weight learning | `:497-581` |

Weight learning in every path: `learning_rate = 0.08`, disagreement punished by `-0.2`,
timing score `1 - 2·(t - t_min_correct)/range`, `clamp(0.01, 0.99)`, renormalisation of the
selection slots only (the epsilon slot is excluded, `:450`, `:470`), plus a
`JournalEntry { mutation: WeightUpdate, reason: u32::MAX }` (`:370-375`, `:475-480`,
`:571-576`). Algorithm semantics belong to `learning-semantics.md`.

**Two normative consequences that MUST be pinned:**

1. `lycan compile` always emits `strategy` with contract `WithinTolerance`
   (`graph_compiler.rs:302-315`), so **a compiled `strategy` executes every option on
   every pass**. Options with side effects (`!p`, `!r`, capability file/network effects)
   therefore **duplicate their effects**, and the returned value may come from a
   different option than the first one that printed. A conforming program's `strategy`
   options MUST be pure; the verifier enforces purity only for `SameOutput`/
   `WithinTolerance` `NodeRef` subtrees containing `Print`/`ReadLine`
   (`verifier.rs:181-196`), which is not full purity.
2. The `None`-contract path is unreachable from `lycan compile` and reachable only from
   hand-authored bytes; a conformance encoder MUST NOT emit it (§11.6).

### 6.3 `feedback`

- **Tree-walker:** unconditional no-op evaluating to `Null` (`interpreter.rs:324-327`).
- **Compiled (`exec.rs:851-906`, normative):** with `< 2` operands it does nothing and
  yields `Null` (`:856`, `:905`). Otherwise: reward is `Float`/`Int` as-is,
  `Bool(true) → 1.0`, `Bool(false) → -1.0`, **anything else → `0.0`** (`:859-865`); the
  target (operand 0) must resolve to an `AdaptiveChoice`/`Strategy` node with non-empty
  weights (`:868-871`); the winner index is **`target.bias as usize`** (`:873`);
  `delta = reward × 0.05`; winner `+delta`, every other `-delta/(n-1)`, each
  `clamp(0.01, 0.99)`; renormalise all weights; journal `WeightUpdate` with
  `reason = u32::MAX`; result `Null` (`:876-905`). Operand 0 is **not evaluated**
  (`get_node_ref`, `:857`), so feedback does not re-run the choice — but a
  non-`NodeRef` operand silently resolves to **node #0** (`exec.rs:1077-1082`), a
  CURRENT hazard.
- **Name → node aliasing.** `(feedback sel 1)` with `sel` a *variable* works only because
  the compiler remembers which graph node produced that variable
  (`var_node`, `graph_compiler.rs:79-80`, `:317-332`) and rewires the feedback target.
  If the name was never bound by a `($ name (choice …))` in the same compile, the target
  silently becomes the compiled form of the identifier — a `LoadVar` — and the feedback is
  a no-op. This aliasing-based resolution is CURRENT behavior and MUST be replaced by an
  explicit handle/label operand (Open decisions).
- **Degenerate shapes.** A one-option `choice`/`strategy`, or a `bias` outside
  `0..weights.len()`, makes every weight the "other" and produces a
  `-delta/(n-1)`-with-`n = 1` update; `clamp` keeps the result finite, but the
  normalisation then has no meaningful signal. A zero-operand `Strategy` is rejected by
  the verifier's weights rule (`verifier.rs:129-147`) and returns `Null` on decode-only
  paths (`exec.rs:272-274`, `graph_guards_panic_holes.rs:68-78`); a conforming encoder
  MUST NOT emit it.

### 6.4 `guard`

Semantics agree across backends: evaluate the assumption; truthy ⇒ `fast_path`, else
⇒ `fallback`; exactly **one** branch is executed, so `guard` is not a speculation barrier
and performs no deopt bookkeeping (`interpreter.rs:306-313`; `exec.rs:252-268`).

**Under-supply divergence (CURRENT).** The compiled handler returns `Null` when the node
has `< 3` operands (`exec.rs:257-259`). That shape is unreachable from `lycs` source (the
parser demands exactly three children, `parser.rs:244-255`) and is rejected by the
verifier (`verifier.rs:103-109`), so it is observable only through a decode-without-verify
path (`capsule-format.md` §7.2). **Normative (MUST):** a malformed `Guard` MUST produce an
error, not `Null` — silently turning a guard into `Null` corrupts downstream decisions
instead of failing them. Tracked as an open decision.

## 7. Collections and iteration (execution rules)

| Form | Rule | Error text (source / compiled) | Cite |
|---|---|---|---|
| `(A e…)` | evaluate all elements left→right into an `Array` | — | `interpreter.rs:191-197` / `exec.rs:679-685` |
| `(I arr i)` | **only** `(Array, Int)`; index cast `as usize`, so a negative index becomes a huge unsigned value | `index {i} out of bounds (len {n})` (identical text; verified `index 18446744073709551615 …` both) / `cannot index {t} with {t}` both | `interpreter.rs:199-217` / `exec.rs:687-701` |
| `(.. s e)` | `Int`,`Int` only, **half-open** (`(.. 1 5)` → `(A 1 2 3 4)`), `s >= e` → empty array | `range start/end must be int, got {t}` / `range requires integers` | `interpreter.rs:219-234` / `exec.rs:703-713` |
| `(W c …)` | `is_truthy(cond)` per iteration — `(W 1 …)` never terminates | — | `interpreter.rs:109-122` / `exec.rs:981-999` |
| `(each v xs)` | `Array` only | `cannot iterate over {t}` / `expected array, got {t}` | `interpreter.rs:124-131` / `exec.rs:1001-1010`, `:1247-1252` |
| `(# n …)` | count must be `Int`; negative ⇒ body never runs | `repeat count must be int, got {t}` (both) | `interpreter.rs:151-157` / `exec.rs:606-612` |

## 8. Builtin table (19 `!` forms) and the capability registry

Names are recognised with the leading `!` stripped (`grammar.md` §7). Compiled lowering is
`graph_compiler.rs:334-371`. "Arity checked" = the builtin validates `args.len()` itself;
otherwise the arity hole of `value-model.md` §9.4 applies. Error texts are given without
the `[runtime] ` prefix; the compiled column shows where the text differs (the graph
backend generally drops the `!`).

| Builtin | Args | Result | Arity checked | Source-only behaviour / error | Compiled | Divergence |
|---|---|---|---|---|---|---|
| `!p` | 0+ any | `Null`; one stdout line, operands joined by a space | variadic | policy `allow_stdout` gate (`interpreter.rs:511-519`) | `Print` (`exec.rs:738-753`) + `stdout_buffer` | none |
| `!r` | 0 | `Str`, line with trailing newline trimmed | 0 | policy `allow_stdin` gate; `read error: {e}` | `ReadLine` (`exec.rs:756-767`) | none |
| `!len` | 1: `Array`\|`Str` | `Int` (elements / **bytes**) | **no** → `(!len)` panics both (`interpreter.rs:545`, `exec.rs:729`) | `cannot get length of {t}` | `Length` | none |
| `!str` | 1 any | `Str` = `Display` | no (0 args → panics) | — | `ToString` | `Fn` renders `(F n)` vs `(fn)` |
| `!num` | 1: `Int`\|`Float`\|`Str` | `Int` or `Float` | no | `cannot parse '{s}' as number` (after `trim`, `i64` then `f64`); `cannot convert {t} to number` | `ParseNum` | none |
| `!split` | 1–2 `Str` | `Array[Str]`, **empty pieces dropped**; non-`Str` delimiter silently `" "` | no (0 args) | `cannot split {t}` | `Split` | none |
| `!chars` | 1 `Str` | `Array[Str]` of single chars (code points, not bytes) | no (0 args) | `cannot get chars of {t}` | `Chars` | disagrees with `!len` on bytes (`value-model.md` §10) |
| `!type` | 1 any | `Str` of `type_name` | no (0 args) | — | **`ToString`** — returns the **stringified value** (`graph_compiler.rs:364`) | **yes: `int` vs `1`; `fn` vs `(fn)`** |
| `!abs` | **exactly 1** `Int`\|`Float` | same type | yes: `!abs expects 1 argument` | `integer overflow in !abs` (checked) | `Abs` (`exec.rs:1153-1162`); `integer overflow in abs`; **requires finite float** | yes for `±inf` |
| `!sin`, `!cos` | **exactly 1** number | `Float` | yes: `!{name} expects 1 argument` | finite-input **and** finite-output guards; `!{name} requires number, got {t}` / `requires finite input` / `produced non-finite output` | `Sin`/`Cos` via `unary_float` (`exec.rs:1164-1178`), same guards, texts without `!` | text only |
| `!sqrt` | **exactly 1** number | `Float` | yes | `!sqrt requires finite input` / `requires non-negative input` | `Sqrt` (`exec.rs:1210-1223`) | text only |
| `!round` | **exactly 1** | **`Int`** | yes | `!round requires finite float`, `!round result out of i64 range` | `Round` (`exec.rs:1180-1195`) | text only |
| `!floor` | **exactly 1** | same type (`Int`→`Int`, `Float`→`Float`) | yes | `!floor requires finite float` | `Floor` (`exec.rs:1197-1208`) | text only |
| `!ln` | 1 (unchecked) | `Float` | **no** | `ln requires positive number` (no `!` in source either) | `Ln` (`exec.rs:118-125`) | none |
| `!exp` | 1 (unchecked) | `Float`, **no finiteness guard** → `inf` | **no** | `exp requires number` | `Exp` (`exec.rs:126-133`) | none |
| `!atan2` | 2 (unchecked) | `Float`; **non-numeric operands silently `0.0`** | **no** → `(!atan2 1)` panics both (`interpreter.rs:757`, `exec.rs:136`) | — | `Atan2` (`exec.rs:134-140`) | none |
| `!lambert` | ≥8: `r1x r1y r1z r2x r2y r2z tof mu` | `Array[7]` `Float`: `v1 v2 status` (`1.0` converged / `0.0`) | yes, `>= 8` | non-numeric args silently coerced to `0.0` (`interpreter.rs:730-736`); `!lambert needs 8 args: r1x r1y r1z r2x r2y r2z tof mu` | **rewritten to a `Capability` call of `astro.lambertSolve`** (`graph_compiler.rs:335-343`) | **yes:** strict capability typing (`astro.lambertSolve expects 8 arguments, got 3`; type errors instead of silent `0.0`) and the returned array comes from the kernel |
| `!cap` | 1 name + n args | per capability | name presence checked | `!cap expects capability name`; `!cap name must be str, got {t}` | `Capability`; `capability node expects a name`; `capability name must be str, got {t}` (`exec.rs:958-968`) | text only |
| *(unknown)* | any | — | — | `unknown builtin '!{name}'` | **`Noop` → `Null`, silently, exit 0** (`graph_compiler.rs:365`; verified `(!nope 1)` compiled is a no-op) | **yes — the most dangerous one** |

**Normative (MUST), builtins:** (a) every builtin MUST validate its argument count and
produce a named error — six do today; `!p` is the only legitimately variadic form; (b) an
unknown builtin MUST fail on every path, so the `Noop` mapping MUST be replaced by a
compile-time or verify-time rejection; (c) `!type` MUST stop compiling to `ToString`
(`value-model.md` §10 proposes the fix).

### 8.1 Capability registry

`!cap "name" args…` (and compiled `Capability` nodes) dispatch through
`capabilities::execute` (`interpreter.rs:786-793`, `graph_executor/capability.rs:9-16`) and
the names are **exact, case-sensitive, camelCase** strings matched by
`REGISTRY.iter().find(|s| s.name == name)` (`registry.rs:566-568`); `names()` (`:570-572`)
backs `runtime.capabilities`. An unrecognised name — including every legacy `snake_case`
spelling — is `unknown capability '{name}'` (`kernels.rs:399`), pinned by
`tests/integration.rs:385-397`. **There are 35 registered capabilities**
(`registry.rs:69-564`):

| Package | Names |
|---|---|
| `runtime` (4) | `runtime.capabilities`, `runtime.input`, `runtime.inputGet`, `runtime.publish` |
| `io`/`net` (5) | `file.exists`, `file.readText`, `file.writeText`, `http.get`, `http.post` |
| `data` (4) | `json.get`, `json.has`, `json.len`, `sql.sqliteQuery` |
| `math`/`ops` (7) | `stats.mean`, `stats.stdDev`, `stats.min`, `stats.max`, `stats.percentile`, `series.ewmaForecast`, `ops.autoScaleRecommend` |
| `comb` (8) | `comb.apTuples`, `comb.isGoodColoring`, `comb.badAp`, `comb.goodColoringWitness`, `comb.hasThreeDistinct4ApColoring`, `comb.badThreeDistinct4Ap`, `comb.threeDistinct4ApWitness`, `comb.threeDistinct4ApSatWitness` |
| `nav` (6) | `nav.norm3`, `nav.distance3`, `nav.dot3`, `nav.radialVelocity`, `nav.ephemerisState`, `nav.horizonsVectors` |
| `astro` (1) | `astro.lambertSolve` |

Each kernel enforces its own arity with `{name} expects {n} arguments, got {k}`
(`kernels.rs:444-447`) plus per-capability range checks (e.g.
`stats.percentile expects percentile in 0..100`, `series.ewmaForecast expects alpha in
0..1`). Arguments are bridged structurally; **functions cannot be passed to a capability**
(`capability.rs:31`: `capability arguments cannot include functions`;
`interpreter.rs:796-812` has the mirror rule). Effects are policy-gated centrally in
`kernels.rs:12-37` and, for `file.*`/`sql.*`/`http.*`, via sandbox and URL checks.
**CURRENT behavior:** with **no** `ExecutionContext` there is **no** policy at all
(`interpreter.rs:511-519` and `exec.rs:739-745` only gate when `ctx.policy` is present),
so a plain `lycan <f.lyc>` run is unrestricted; sandbox/policy semantics are normed in
`execution-policy.md`, per-capability ABI in `capability-abi.md`.

### 8.2 Opcodes with no source syntax

`Spawn`, `Prune`, `Weight`, `Predict`, `Merge`, `Halt`, `Noop`, `Abs`-via-`!abs` only, and
`Operand::StateRef` are reachable only from hand-authored/evolved graphs or internal
machinery: `Weight`/`Predict`/`Merge`/`Noop`/`Halt` all evaluate to `Null`
(`exec.rs:908-910`, `:976-977`); `Spawn` appends a node from a numeric opcode
(`:829-838`, mapping `opcode_from_u8` at `:1254-1265` covers only 9 opcodes and defaults
to `Noop`); `Prune` rewrites a node to `Noop` (`:840-849`); `StateRef` reads the state
vector as `Float`, defaulting to `0.0` out of range (`:1069-1071`). A conforming program
MUST NOT depend on them; they are format/executor internals, not language surface.

## 9. Error model (shared, and its limits)

`LycanError` has exactly three variants; runtime errors carry a bare message with **no
position, no node id, and no call stack** (`error.rs:3-24`). Lexer/parser positions render
as `[lex L:C]` / `[parse L:C]` (`grammar.md` §8). Policy denials are ordinary runtime
errors (`capability=print effect=stdout denied by policy`, `exec.rs:742`), not a distinct
error class. The CLI prints and exits 1 (`bin/lycan.rs:179-184`); the REPL prints the
error and continues (`bin/lycan.rs:1394-1407`); `verify` accumulates **all** structural
errors before rejecting (`verifier.rs:25-232`, `capsule-format.md` §7). On the graph path,
runtime errors surface through `rt_err` (`graph_executor/mod.rs:99-101`) with the same
loss of position. This is a shared, non-normative-quality diagnosability gap; the minimum
required position information is an open decision.

## 10. Conformance requirements

A conforming vector set MUST:

1. **Carry per-backend expectation columns** for every case below, and label the compiled
   column normative (§1). A set with one column fails conformance review.
2. **Scoping pairs** (exact observed outputs): block leak (`undefined 'y'` / `1`);
   `each` body leak (`undefined 'z'` / `1`); shadowing (`2,1` / `2,2`); loop-var clobber
   (`1,1,2,2` / `1,9,2,9`); closure attempt (`undefined 'x'` / `cannot add null and int`);
   undefined read (`undefined 'zzz'` / `null`); undefined assign
   (`undefined 'q'` / succeeds → `1`); immutable assign
   (`'x' is immutable` / succeeds → `2`).
3. **`(^)` unwinding.** `(!p 1) (^ null) (!p 2)` prints `1` only, both backends; and a
   `(^ v)` inside a function is that function's value even when nested in `W`/`each`/`#`.
4. **Call-arity tolerance.** `(f 1 2)` → first param only; `(f)` → `null`; both backends;
   plus `cannot call int` / `cannot call null` for non-callable heads.
5. **`choice` value-and-bias contract.** `choice` yields the selected option's **value**;
   a vector MUST additionally assert (via `lycan inspect`/`dump`, not source) that after a
   greedy selection `bias` equals the chosen index; and that with no `ExecutionContext` the
   mode is `Greedy, epsilon 0.0`; and that a 0-option `choice` yields `Null`.
6. **`strategy` contract paths.** `WithinTolerance` (what `lycan compile` emits) MUST be
   pinned including: all options executed (assert duplicated `!p` output for a
   side-effecting option), median/tolerance comparison, epsilon slot excluded from
   renormalisation, and `bias` set. A `None`-contract `Strategy` vector is required only
   as a decoder/executor vector; a conforming **encoder** MUST NOT emit it.
7. **`feedback` update.** A vector MUST pin: `Bool true → +1.0`, `false → -1.0`, `Str →
   0.0`; winner index read from `bias`; `learning_rate 0.05`, clamp `[0.01, 0.99]`,
   renormalisation; a `WeightUpdate` journal entry with `reason = u32::MAX`; feedback to a
   non-choice target is a silent no-op; and that the source backend performs **no** update
   at all (legacy column) — so adaptive behaviour is only testable on the graph path.
8. **`guard`.** Truthy ⇒ fast path, falsy ⇒ fallback, exactly one branch evaluated
   (assert via `!p` side effects). Plus a hand-authored graph vector for `< 3` operands
   pinning the CURRENT `Null` result and the verifier rejection, so the fix is visible.
9. **Per-form builtin table (§8)** — every row, per backend, including arity-checked
   messages (`!abs expects 1 argument` vs compiled none for the same shape), the
   `!sin`/`!cos`/`!sqrt`/`!round`/`!floor` finiteness texts with and without `!`,
   `!exp` → `inf`, `!atan2` silent `0.0` for non-numeric operands, `!len` bytes vs
   `!chars` code points.
10. **Builtin divergence pins.** `!type` (`int` vs `1`, `fn` vs `(fn)`); unknown builtin
    (`unknown builtin '!nope'` vs silent `Null`, exit 1 vs 0); `!abs inf` (`inf` vs
    `abs requires finite float`); `Fn` printing (`(F f)` vs `(fn)`); `!lambert` (source
    silent `0.0` coercion + `needs 8 args` text vs compiled
    `astro.lambertSolve expects 8 arguments, got 3`).
11. **Ordering divergence pin.** `(< "a" "b")` → `true` vs
    `cannot compare str and str` (`value-model.md` §11 item 5).
12. **Capability registry exactness.** One vector per package asserting a working call, a
    `snake_case` rejection (`unknown capability 'file.read_text'`,
    `tests/integration.rs:385-397`), and a count assertion matching `REGISTRY.len()` so
    that registry drift is caught (the stale count of 33 vs the tree's 35 in §11).
13. **`F!` inertness.** `F!` and `F` MUST produce identical observable behaviour on both
    backends until `F!` acquires semantics.

## 11. Divergences from the upstream fact pass (tree wins)

| Claim received | Tree truth | Cite |
|---|---|---|
| "`(+ 1)`/`(+ 1 2 3)` panic / silently truncate; arity validated nowhere (parser, compiler, verifier)" | Both backends reject wrong-arity arithmetic; the verifier rejects `Add/Sub/Mul/Div/Mod` with ≠ 2 operands and `Neg` with ≠ 1, and the executor has a defensive error for decode-only graphs | `interpreter.rs:364-381`, `verifier.rs:148-177`, `exec.rs:1091-1104` |
| "`(^ v)` graph path propagates … then SWALLOWS at entry … execution continues" | Correct that `into_val()` converts it, and the observable behaviour matches the source path: **later top-level forms do not run** (`1` printed, `2` not) | `mod.rs:82`, `exec.rs:588-591` |
| "`feedback` … `last chosen` from node…" (incomplete) | Fully specified here: winner index = `target.bias`, `lr = 0.05`, others `-delta/(n-1)`, clamp `[0.01,0.99]`, renormalise, journal `WeightUpdate`/`reason u32::MAX`; target operand is **not** evaluated; a non-`NodeRef` target silently becomes node #0 | `exec.rs:851-906`, `:1077-1082` |
| "`AdaptiveChoice` … selects from `ExecutionContext` (`context 43-48`)" | Confirmed, plus the default with no context is `Greedy`/`epsilon 0.0`, `n_options` excludes the tolerance slot, and `bias` is written at `exec.rs:244` | `exec.rs:186-244` |
| "guard compiled under-supply → `Null`" | Confirmed in the executor, but **unreachable from source and rejected by the verifier**; observable only on decode-without-verify paths | `exec.rs:257-259`, `verifier.rs:103-109`, `parser.rs:244-255` |
| "recursion cap 65536 (`exec 13-21`)" | Confirmed as a constant, but the 64 MiB stack is exhausted first for source-shaped recursion — **both** backends die with `fatal runtime error: stack overflow` (exit `-6`), so the cap is not the practical limit | `mod.rs:52`, `exec.rs:13-22`, probe |
| "registry = 33 capabilities" | **35** | `registry.rs:69-564` |
| "`!lambert` returns `Array[7]` … compiled → capability" | Confirmed; also the exact per-backend error texts and the silent-`0.0` vs strict-typing divergence | `interpreter.rs:723-754`, `graph_compiler.rs:335-343`, `kernels.rs:444-447` |
| "unreachable opcodes: `Spawn, Prune, Weight, Predict, Merge, Halt, StateRef`" | Confirmed; also `Noop`, and `opcode_from_u8` (`exec.rs:1254-1265`) maps only 9 bytes and defaults to `Noop`, so `Spawn` is largely inert | `exec.rs:908-910`, `:976-977`, `:1254-1265` |

## Open normative decisions

1. **`F!` is inert** — give it real per-name persistent state or delete it
   (`value.rs:21-22`, `graph_compiler.rs:172`). Recorded in `grammar.md` too; MUST be
   decided once.
2. **`guard` under-supply** — a `< 3`-operand `Guard` currently yields `Null` on
   decode-only paths (`exec.rs:257-259`). Decide whether it MUST error, and whether
   `guard` should grow real speculation/deopt semantics (journal + revert) or be renamed
   to what it is: a three-way conditional.
3. **`!type` fix proposal** — add a `TypeOf` opcode with its own byte and verifier arity
   rule (format-version bump), or delete `!type`. Today's compiled mapping to `ToString`
   is a mis-compile (`graph_compiler.rs:364`, `value-model.md` §10).
4. **Closures** — `LycanFn` captures nothing (§2.3), so higher-order code relies on
   dynamic scoping and caller-visible leakage. Decide between real lexical closures
   (capture on `Value::Fn`/`GraphFn`) and an explicit "no closures" prohibition with a
   compiler check for free variables.
5. **n-ary `+`** (and `*`, `&&`, `\|\|`) — strictly binary today with both backends
   rejecting 3+ operands; `!p` alone is variadic. Recorded identically in `grammar.md`;
   MUST be resolved once, then the verifier rule and the tree-walker guard move together.
6. **Scoping model on the normative path** — global slot-per-name means no shadowing, no
   block scope, silent `Null` for undefined reads, and loop-variable clobbering (§3).
   Decide whether the compiled path MUST grow real frames (or SSA-style slot allocation
   per call) before the language is called lexical.
7. **`feedback` target by name** — replace `var_node` aliasing
   (`graph_compiler.rs:317-332`) and `bias`-as-implicit-index with an explicit target
   handle, and define the error for a target that is not a decision node
   (`exec.rs:868-871`).
8. **Unknown builtin must fail on both paths** — remove the `Noop` fallback
   (`graph_compiler.rs:365`) and add a verifier rule for builtin opcode arities
   (`value-model.md` §9.4).
9. **Runtime diagnostics** — position, node id, and call-stack capture on runtime errors
   (`error.rs:19-21`, §9).
