# Lycan Value Model

Status: Draft v0.1 — describes implementation as of 2026-09-08 (graph FORMAT_VERSION=5)

Normative description of Lycan runtime values, truthiness, printing, equality, ordering,
numeric coercion, and the fail-closed arity rules. RFC 2119 keywords; citations are
`file:line` into the working tree as of that date and were re-verified against it
(debug build + behavioural probes). "CURRENT behavior" marks observed facts a future
revision may tighten; a conforming producer MUST NOT rely on them.

Two backends evaluate the same AST with two distinct value representations
(`grammar.md` §1). Per the split declared in `scoping-and-execution.md` §1, the
**compiled graph executor is the normative backend** and the tree-walking interpreter is
legacy; this file states one shared contract where the two agree and uses explicit
per-backend columns where they do not. Where only the normative backend is named, the
other column exists precisely because a conformance vector MUST pin both.

## 1. The two value models

| Variant | Source backend `Value` (`value.rs:5-14`) | Graph backend `GVal` (`graph_executor/value.rs:5-16`) |
|---|---|---|
| integer | `Int(i64)` (`:7`) | `Int(i64)` (`:6`) |
| real | `Float(f64)` (`:8`) | `Float(f64)` (`:7`) |
| text | `Str(String)` (`:9`) | `Str(String)` (`:8`) |
| boolean | `Bool(bool)` (`:10`) | `Bool(bool)` (`:9`) |
| absence | `Null` (`:11`) | `Null` (`:10`) |
| sequence | `Array(Vec<Value>)` (`:12`) | `Array(Vec<GVal>)` (`:11`) |
| callable | `Fn(LycanFn)` (`:13`, struct `:16-23`) | `GraphFn { param_slots, body_nodes }` (`:12-15`) |

The seven-fold taxonomy, `type_name`, `is_truthy`, and `Display` are deliberately
parallel; the differences below are the whole of the divergence.

- `LycanFn { name, params, body, stateful }` carries **no environment**
  (`value.rs:16-23`), which is why closures cannot capture anything
  (`scoping-and-execution.md` §2.3). `stateful` is `#[allow(dead_code)]` (`:21-22`) —
  `F!` is inert.
- `GVal::GraphFn` is a pair of **slot ids and node ids** (`value.rs:12-15` of the graph
  module), i.e. a closure over the *global* slot table, not over a frame.
- `GVal` additionally has `OptionStats { tries, total_ns, correct }`
  (`graph_executor/value.rs:58-64`), a per-option learning record with no source-level
  analogue (consumed by `Strategy`, see `scoping-and-execution.md` §6).
- There is no map, struct, character, byte-buffer, error, exception, or coroutine value.
  Structured data is `Array`, and heterogeneous key/value data is represented as nested
  `Array` or as JSON text plus the `json.*` capability.

## 2. Truthiness

Identical in both backends (`value.rs:26-35`, `graph_executor/value.rs:19-28`):

| Value | Truthy? | Rule source |
|---|---|---|
| `Bool(b)` | `b` | `value.rs:28` |
| `Null` | **no** | `:29` |
| `Int(0)` | **no** | `:30` |
| `Str("")` | **no** | `:31` |
| `Array([])` | **no** | `:32` |
| **`Float(0.0)`** | **YES** | fall-through `:33` |
| `Int(-1)`, any other `Int` | yes | `:33` |
| any non-empty `Str`, any non-empty `Array` | yes | `:33` |
| **every `Fn`/`GraphFn`, including one with an empty body** | **YES** | `:33` |
| `Float(±inf)`, `Float(NaN)` | yes | `:33` |

**`Float(0.0)` is truthy** while `Int(0)` is falsy: the falsy-int arm matches on the
literal pattern `Int(0)` and there is no float arm, so `0.0`, `-0.0` and `NaN` all fall
through to `true`. Verified: `(not 0.0)` → `false` (i.e. `0.0` is truthy), `(not "")` →
`true`, `(not (A))` → `true`, `(not -1)` → `false`, identically on both backends.

Normative consequences: a loop driven by a float counter MUST test it with `<=`/`<`, not
by value (`(W x …)` with `x → 0.0` never terminates); `(? f …)` on a function value is
always the `then` branch. A future revision MUST either add the symmetric float arm or
drop the int arm; it MUST NOT keep an inconsistency that makes `0` and `0.0` disagree.

## 3. `type_name`

`value.rs:37-47` and `graph_executor/value.rs:30-37` agree exactly:
`"int" "float" "str" "bool" "null" "array" "fn"`. These are the strings interpolated into
every runtime error message in this file, and the return value of `!type` — but only in
the source backend; compiled `!type` is broken (§10, `scoping-and-execution.md` §8).

## 4. Printing (`Display`) and the `!p` line

| Value | Printed as | Cite |
|---|---|---|
| `Int(n)` | `n` | `value.rs:53` |
| `Float(f)` | Rust `f64` `Display` | `value.rs:54` |
| `Str(s)` | **`s` verbatim, unquoted, unescaped** | `value.rs:55` |
| `Bool(b)` | `true` / `false` | `value.rs:56` |
| `Null` | `null` | `value.rs:57` |
| `Array(e…)` | `(A e1 e2 …)`, recursive, space-separated | `value.rs:58-64` |
| `Fn(f)` | **`(F {name})`** (source) | `value.rs:65` |
| `GraphFn` | **`(fn)`** (compiled) | `graph_executor/value.rs:53` |

Verified, both backends unless noted:

- `(!p 6.0)` → `6`, `(!p 100.0)` → `100`, `(!p -0.0)` → `-0`, `(!p 1.5)` → `1.5`.
  `Float` printing is Rust's shortest-round-trip `Display`, so **every integral float
  prints without `.0`** and the printed text is not a float literal (`3.1` lexes back as
  `Float`, `6` does not — it lexes as `Int`).
- `(/ 0.0 0.0)` prints **`NaN`** (capitalised, Rust `Display`) and `(/ 1.0 0.0)` prints
  `inf`. Neither string is lexable back into a float literal, and `inf`/`NaN` are not
  atoms in the lexer (`grammar.md` §3).
- `(!p "hi")` → `hi` (no quotes). `(!p (A "a" 1))` → `(A a 1)`.
- `(!p f)` for `(F f () 1)` → `(F f)` in source, `(fn)` compiled.

**`!p` output is not re-parseable and MUST NOT be treated as a serialization format.**
Because strings print raw, `(!p "a b")` is indistinguishable from `(!p "a" "b")`'s
interior, a string containing `(`/`)`/`;`/whitespace breaks the paren structure, integral
floats lose their type, `NaN`/`inf` have no literal syntax, and arrays print as `(A …)`
— a form that re-parses as an *array literal*, re-evaluating elements rather than
yielding data. Verified: `(!p "hi")`/`(!p (A 1 2))` emit `hi` / `(A 1 2)`. In-repo tests
assert on this stdout (§ fixtures in `tests/integration.rs`), which is CURRENT practice,
not a normative contract. A conforming exchange format MUST be a distinct mechanism.

`!p` itself: it evaluates **all** operands, joins their `Display` with a single space,
writes one line to stdout, and evaluates to `Null` (`interpreter.rs:520-526`;
`exec.rs:746-753`, which additionally appends the same line to
`GraphExecutor::stdout_buffer`, `mod.rs:26-27`). Zero operands print an empty line
(verified both backends). Under a policy context, `!p` is gated on `allow_stdout` in both
backends (`interpreter.rs:511-519`, `exec.rs:739-745`).

## 5. Equality: structural for arrays, non-coercing for scalars — DECIDED 2026-09-08

**Normative (MUST):** `==` is **deep structural equality on `Array`** and a
**same-type-only** comparison on every other value. There is no numeric coercion and
no callable arm. `equal` (`interpreter.rs`, `OpKind::Eq`/`Neq`) and `gval_eq`
(`graph_executor/exec.rs`) mirror each other with **six arms plus a `false` fallback**:

| Pair | `==` result | Note |
|---|---|---|
| `Int` vs `Int`, `Float` vs `Float`, `Str` vs `Str`, `Bool` vs `Bool` | component compare | |
| `Null` vs `Null` | `true` | |
| **`Array` vs `Array`** | **deep structural** | same length AND every element `==`: `(== (A 1) (A 1))` → `true`, `(== (A (A "a") 3) (A (A "a") 3))` → `true`. DECIDED 2026-09-08 — before this there was no `Array` arm and both cells below it read `false` |
| **`Int` vs `Float`** | **`false`** | DECIDED: no coercion (`!num` is the explicit bridge); `(== 1 1.0)` → `false`, and the rule holds at depth: `(== (A 1) (A 1.0))` → `false` |
| `Fn`/`GraphFn` vs anything | `false`, always | call-value identity is deferred to the (still open) closure register |
| `Bool` vs `Null`, `Str` vs `Int`, `Array` vs scalar, mixed anything | `false` | |
| `Float(NaN)` vs `Float(NaN)`, incl. inside arrays | **`false`** | the arm is IEEE `x == y` at every depth: `(== (A (/ 0.0 0.0)) (A (/ 0.0 0.0)))` → `false` |

**Depth cap (fail-closed):** recursion past **64 array levels** is the runtime error
`structural equality depth limit (64) exceeded`, identical text on both backends.
Arrays are immutable, but `W`-loop construction nests them arbitrarily deep, and
unbounded equality recursion is a stack overflow — so the guard **errors**, it does
not answer `false`. Boundary pinned: **65 nesting levels compares, 66 raises**
(`tests/semantics_parity.rs::equality_depth_limit_boundary_identical`).

`!=` is exactly the negation, and the depth error short-circuits both forms.

All rows verified byte-identical on both backends by
`tests/semantics_parity.rs::structural_equality_decided_identically_on_both_backends`
— including the two FLIPS: `(== (A 1) (A 1))` and `(== (A) (A))` are now `true`.

**History:** until 2026-09-08 `==` had five arms and no `Array` arm, so two
structurally identical arrays were never equal and `!=` on them was `true`. The open
register asked whether to add structural equality or document the representation
semantics permanently; it was decided to add it (arrays are the value shape contracts
compare, and "never equal" made array-valued decision outputs unverifiable). No
in-repo program relied on the old `false` (verified by grep before the flip).

## 6. Ordering

Source (`compare`, `interpreter.rs:474-486`) vs compiled (`gval_cmp`,
`exec.rs:1236-1245`):

| Pair | Source | Compiled |
|---|---|---|
| `Int`,`Int` | integer compare | same |
| `Float`,`Float` | `partial_cmp().unwrap_or(Equal)` | same |
| `Int`,`Float` (either order) | `Int` widened via `as f64`, `partial_cmp().unwrap_or(Equal)` | same |
| `Str`,`Str` | byte-order lexicographic (`x.cmp(y)`) | **same** — `Str` arm added to `gval_cmp` 2026-09-08; byte order matches `!len`'s byte semantics |
| anything else (`Bool`, `Null`, `Array`, `Fn`, mixed) | `cannot compare {ta} and {tb}` | same |

Normative-critical quirks:

1. **NaN compares `Equal` for ordering.** `partial_cmp` returns `None`, replaced by
   `Ordering::Equal`. Therefore `(< a b)` and `(> a b)` are `false` while
   `(<= a b)` and `(>= a b)` are **`true`** whenever either side is NaN — verified:
   `(<= (/ 0.0 0.0) 1)` → `true`, `(>= (/ 0.0 0.0) 1)` → `true`, `(<= 1 (/ 0.0 0.0))` →
   `true`, `(< nan 1)`/`(> nan 1)` → `false`, on both backends. Note this is *ordering*
   only; `==` stays IEEE (§5), so `(<= nan nan)` and `(== nan nan)` disagree. A loop or
   sort written against `<=` MUST be NaN-free.
2. **String ordering was backend-dependent — RESOLVED 2026-09-08.** The compiled
   `gval_cmp` had no `Str` arm, so `(< "a" "b")` returned `true` in source and died
   `[runtime] cannot compare str and str` once compiled. The arm is now present:
   byte-order lexicographic on both backends (`(< "B" "a")` → `true`, i.e. byte
   order, not collation; `(< "z" "é")` → `true` because `é` starts `0xC3`).
   Pinned by `tests/semantics_parity.rs::string_ordering_parity_closes_str_arm_divergence`.

`Int`→`Float` widening means comparisons above `2^53` are **lossy**: the `Int` side can
round, so `(== big_int big_float)`-adjacent ordering decisions are not exact. This is
CURRENT behavior; a conforming program MUST NOT order integers beyond `2^53` against
floats.

## 7. Arithmetic coercion

All arithmetic is **strictly binary** (§9). `Bool`, `Null`, `Array` (except `+`), and
`Fn` participate in no numeric arm.

### 7.1 `+` (`interpreter.rs:423-441` / `arith_add` `exec.rs:1113-1127`)

Arms are matched **in this order**, which is why the string rule is asymmetric:

| `a` ↓ / `b` → | `Int` | `Float` | `Str` | `Bool` | `Null` | `Array` | `Fn` |
|---|---|---|---|---|---|---|---|
| `Int` | `Int` (wrapping, §8) | `Float` | **`Str`** `(int)`‖`b` | err | err | err | err |
| `Float` | `Float` | `Float` | **`Str`** `(float)`‖`b` | err | err | err | err |
| `Str` | **`Str`** `a`‖`(int)` | **`Str`** `a`‖`(float)` | **`Str`** concat | **`Str`** `a`‖`"true"`/`"false"` | **`Str`** `a`‖`"null"` | **`Str`** `a`‖`"(A …)"` | **`Str`** `a`‖`(fn-render)` |
| `Bool` | err | err | **`Str`** `"true"`‖`b` | err | err | err | err |
| `Null` | err | err | **`Str`** `"null"`‖`b` | err | err | err | err |
| `Array` | err | err | **`Str`** `"(A …)"`‖`b` | err | err | **`Array`** shallow concat | err |
| `Fn` | err | err | **`Str`** `(render)`‖`b` | err | err | err | err |

`err` is `cannot add {ta} and {tb}` (`interpreter.rs:437-439`, `exec.rs:1125`). Verified:
`(+ 1 "a")` → `1a`; `(+ "a" (A 1))` → `a(A 1)`; `(+ true "a")` → `truea`;
`(+ (A 1) 1)` → `[runtime] cannot add array and int`; `(+ true true)` and
`(+ null null)` → error; `(+ f f)` → `cannot add fn and fn`. Both backends identical.

**String-concatenation asymmetry is normative-shaping and MUST be stated:** `+` with a
`Str` on **either** side concatenates using the other operand's `Display`, but a `Str`
left operand wins the earlier arm (`(Str, _)` at `:430`/`:1120` precedes `(_, Str)` at
`:431`/`:1121`), so `(+ "a" 1)` and `(+ 1 "a")` both work while `(+ (A 1) 1)` fails and
`(+ (A 1) "x")` succeeds. Concatenation is **not** typed: `(+ 1 "a")` never errors, so a
numeric pipeline that drifts into string territory keeps running and produces text.
`Array + Array` is a shallow copy-concat (`:432-436`), not in-place.

### 7.2 `-`, `*`, `%` (`arith`, `interpreter.rs:462-472` / `exec.rs:1143-1151`)

Only `(Int,Int)`, `(Float,Float)`, `(Int,Float)`, `(Float,Int)`; the result type is
`Int` only when both sides are `Int`. Everything else is
`cannot do arithmetic on {ta} and {tb}`. There is no string or array form of `-`/`*`/`%`.

### 7.3 `/` (`div`, `interpreter.rs:443-460` / `arith_div` `exec.rs:1129-1141`)

| Expression | Result | Rule |
|---|---|---|
| `(/ 20 4)` | `5` (`Int`) | `Int/Int` and `x % y == 0` → `Int` (`:447-448`) — pinned by `tests/integration.rs:75-78` |
| `(/ 7 2)` | `3.5` (`Float`) | `Int/Int` non-exact → `Float` (`:449-451`) — `tests/integration.rs:80-83` |
| `(/ 4 0)` | error `division by zero` | `Int` divisor zero (`:446`) |
| `(/ 1.0 0.0)` | `inf` | **CURRENT: float division has no zero guard** (`:453`) |
| `(/ 0.0 0.0)` | `NaN` | same (`:453`) |
| `(/ 1 0.0)` | `inf` | mixed arm, no guard (`:454`) |
| `(/ "a" 2)` | `cannot divide str by int` | (`:456-458`) |

`/` is **type-changing on value**, not on operand types: `(/ 6 3)` is `Int(6÷3→2)` while
`(/ 6.0 3)` is `Float(2.0)` — and, because `6.0` prints as `6` (§4), `Int` and `Float`
results of division are *indistinguishable in stdout*. Verified on both backends
(`inf`, `NaN`, `5`, `3.5`).

**Normative (MUST):** a conforming implementation MUST reject integer division by zero
with an error (both do), and a future revision MUST extend the same treatment to a zero
`Float` divisor — CURRENT silent `inf`/`NaN` (`:453`) silently poisons downstream
computations and interacts with the NaN-ordering quirk (§6.1). Until then, producers
MUST guard float divisors explicitly.

### 7.4 `%`

`(Int,Int)` with zero divisor → **error `modulo by zero` in both backends**
(`interpreter.rs:403-406`, `exec.rs:77-82`); verified `(% 7 0)` on both. `(% 17 5)` →
`2`, pinned by `tests/integration.rs:85-87`. Float operands use Rust's `%`, which is
**unguarded**: `(% 1.0 0.0)` → `NaN` silently on both backends (verified). The verifier
additionally rejects a *statically known* zero divisor (§9.2), but see the immediate-vs
-node caveat there — `lycan compile` lowers literals to nodes, so source programs reach
the executor guard, not the verifier rule.

### 7.5 `neg`, and `!abs`

`neg` (`interpreter.rs:387-393`, `exec.rs:83-93`) accepts `Int` and `Float` only, else
`cannot negate {t}`. It is an **unwrapped** `-n`, so `(neg -9223372036854775808)` aborts
the process with `attempt to negate with overflow` in a debug build
(`interpreter.rs:390`, `exec.rs:89`); `!abs` is the checked form and errors with
`integer overflow in !abs` (source) / `integer overflow in abs` (compiled)
(`interpreter.rs:586-588`, `exec.rs:1155-1157`).

## 8. Integer overflow (`i64`) — DECIDED 2026-09-08

**Normative (MUST):** `i64` overflow is a **runtime error on every arithmetic
path, on both backends, in every Rust profile**. The chosen policy is option
(a) of the former open decision: named error, not wrapping-with-flag (an
overflow flag no `.lycs` program can currently read would pin option (b) as
dead syntax). The Rust profile MUST NOT be observable: all i64 math goes
through `checked_*` ops, so the release binary no longer depends on
`overflow-checks = false`.

**CURRENT (implemented, pinned by `tests/semantics_parity.rs`):**

| Path | Error (identical text both backends) |
|---|---|
| `(+ INT64_MAX 1)` | `integer overflow in +` |
| `(- INT64_MIN 1)` | `integer overflow in -` |
| `(* INT64_MAX 3)` | `integer overflow in *` |
| `(/ INT64_MIN -1)` | `integer overflow in /` (the `x % y` divisibility test itself is run `checked_rem` first — otherwise the guard panics/wraps before the checked division) |
| `(% INT64_MIN -1)` | `integer overflow in %` |
| `(neg INT64_MIN)` | `integer overflow in neg` |
| `(!abs INT64_MIN)` | `integer overflow in !abs` (was already checked; `neg` was the hole) |

Float paths are unaffected (IEEE semantics: `inf`/`NaN` pass through), except
that **float→int conversions are errors, never silent saturation**: capability
int arguments (`kernels.rs::integer`, out-of-range float) raise `… out of i64
range`, and `ops.autoScaleRecommend` rejects a non-finite or out-of-range
`load/target` instead of clamping a saturated value.

**History (why this was dangerous):** before 2026-09-08 the crate had no
`[profile.*]` section, so release silently wrapped, debug aborted (exit 101),
and an evolved/cached `.lyc` could disagree between a debug test run and a
release deployment — the single most dangerous property the spec recorded.

## 9. Arity: fail-closed rules (MUST)

These rules are the current contract and MUST hold in any conforming implementation;
they exist because a 1-operand binary op or a `x % 0` was previously a **process panic**
reachable from ordinary source and from hostile `.lyc` bytes
(`tests/graph_guards_panic_holes.rs:1-14`).

### 9.1 The rules

1. **Binary operators take exactly 2 operands; `not` and `neg` take exactly 1.**
   Any other count is rejected. No operator is variadic; `(+ 1 2 3)` is an error, not a
   silent truncation.
2. **A statically-known modulo by zero is rejected** at verification time, and a
   dynamically-computed one is an execution error, never a panic.
3. **Rejected means "no partial execution" for the compiled path:** a graph containing
   such a node fails `verifier::verify` and therefore never starts under a fail-closed
   entrypoint (`capsule-format.md` §7.1).
4. **No panic is reachable from a malformed arity**, on either backend, for the operators
   covered by rule 1. (Residual holes outside rule 1 are §9.4 and Open decisions.)

### 9.2 Where each rule is enforced (per backend)

| Condition | Tree-walker (source) | Compiled graph |
|---|---|---|
| `(+ 1)` / `(+ )` / `(+ 1 2 3)` | `[runtime] operator Add expects exactly 2 operand(s), got {1,0,3}` — `interpreter.rs:364-381` | verifier: `node #{id} {Op:?}: binary arithmetic requires exactly 2 operands, has {k}` — `verifier.rs:148-158`; verified exit 1 at run |
| `(not x y)` / `(neg x y)` | `operator {Not,Neg} expects exactly 1 operand(s), got 2` (`interpreter.rs:368-381`) | `Neg` only: `node #{id} Neg: requires exactly 1 operand, has {k}` (`verifier.rs:170-177`); **`Not` is unchecked** (§9.4) |
| Decode-only graph with 1 operand on a binary opcode | n/a | defensive executor error `{Op:?} node #{id}: binary op requires 2 operands, has {k}` (`exec.rs:1091-1104`) |
| 0-operand `Neg` | covered by rule 1 | `neg requires 1 operand` (`exec.rs:84-87`) |
| `(% x 0)` with literal `0` | `[runtime] modulo by zero` (`interpreter.rs:403-406`) | `node #{id} Mod: static modulo by zero (divisor is Int(0))` **only when operand 1 is `Operand::Immediate(ImmValue::Int(0))`** (`verifier.rs:159-168`) |
| `(% x 0)` computed at run time | `modulo by zero` | `modulo by zero` (`exec.rs:77-82`); verified — and this is the path `lycan compile` always takes, because literals lower to `ConstInt` nodes, not immediates (`graph_compiler.rs:54-56`) |

Regression coverage: `tests/graph_guards_panic_holes.rs` —
`single_operand_arithmetic_is_rejected_and_never_panics` (`:87-105`),
`static_mod_zero_is_rejected_and_never_panics` (`:107-131`),
`dynamic_mod_zero_never_panics` (`:133-154`),
`lycs_arith_panic_holes_now_error_cleanly` (`:158-184`, CLI-level: `(+ 1)`, `(% 1 0)`,
`(not)` must not print `panicked` and must exit non-zero), and
`strategy_without_options_is_rejected_and_never_panics` (`:68-78`). The fuzz target
`fuzz/fuzz_targets/lycs_parse.rs` covers lex→parse→compile only, so executor panics are
outside fuzz coverage — the verifier is the only guard for shipped bytes.

### 9.3 Parser is deliberately arity-blind

`grammar.md` §5.5: the parser accepts any child count for operator and builtin forms.
A conforming implementation MUST NOT "fix" this by adding precedence-aware or
arity-aware parsing without also changing the verifier rules; the layering is
"parse anything, reject fail-closed downstream".

### 9.4 Residual non-fail-closed holes — CLOSED 2026-09-08

The following reachable process aborts (exit 101, `index out of bounds`)
were documented here when this spec drafted; all are now closed and
regression-pinned (`tests/graph_guards_panic_holes.rs`, round 2):

| Input | Was | Now |
|---|---|---|
| `(!len)` | panic both backends (`interpreter.rs` `args[0]`, `exec.rs` `operands[0]`) | `[runtime] !len expects exactly 1 argument(s), got 0` / verifier rule via `op_fixed_arity` |
| `(!atan2 1)` | panic both backends (`args[1]` / `operands[1]`) | clean arity error / verify rejection |
| `(not)` | compiled **panic** (`exec.rs` Not arm, no verifier rule) | verifier rejects `Not` ≠ 1 operand; executor pre-dispatch guard errors on decode-only paths |
| `(not 1 2)` | compiled **silently dropped operand 2** | verifier rejects (exact-arity rule) |

The shared table `graph::op_fixed_arity` now drives (a) the verifier's
arity rule for every fixed-arity opcode — arithmetic, comparison, logic,
`Not`, unary math, `Atan2`, `Index` — and (b) a pre-dispatch guard in
`exec_node_inner` so decode-only paths cannot index past `operands`;
the interpreter carries the equivalent name table in `exec_builtin`
(`len/str/num/chars/type/ln/exp` = 1, `atan2` = 2; `split` 1..2,
`lambert` ≥8, `p`/`cap` variadic remain range-arity by design).

## 10. Value-level behaviour of `!type`, `!abs`, `!num`, `!len`, `!chars`

Arity, coercion and per-form error texts for all 19 builtins are tabulated in
`scoping-and-execution.md` §8. Value-model-relevant points:

- `!len` on `Str` returns the **UTF-8 byte length**, while `!chars` returns per-`char`
  elements, so `(!len "héllo")` → `6` and `(!len (!chars "héllo"))` → `5`
  (`interpreter.rs:548`, `:681-683`; `exec.rs:732`, `:719-722`). Both backends agree; the
  inconsistency is *between the two builtins*, and it MUST be documented to program
  authors.
- `!num` trims, then tries `i64`, then `f64`, then errors
  `cannot parse '{s}' as number`; `Int`/`Float` pass through; anything else is
  `cannot convert {t} to number` (`interpreter.rs:558-579`, `exec.rs:769-786`). Note that
  `"6.0"` → `Float(6.0)` while `"6"` → `Int(6)`: `!num` is the only way to choose an
  integer/float representation from text.
- `!abs` is finite-only on **both** backends since 2026-09-08 (source used to pass
  `±inf` through; it now raises `!abs requires finite float` / `abs requires finite
  float`); `Int` overflow raises `integer overflow in !abs` (§8).
- `!type` returns the `type_name` string (`int`, `float`, `str`, `bool`, `null`,
  `array`, `fn`) on **both** backends since 2026-09-08, via the dedicated
  `TypeOf` opcode (byte `0x7E`, fixed arity 1). The former compiled mapping to
  `ToString` (`// close enough for now`) was a mis-compile and is removed.

## 11. Conformance requirements

A conformance vector set for the value model MUST pin, **with one expectation column per
backend** (`scoping-and-execution.md` §1):

1. **Truthiness table (§2).** Every row, with `(not …)` as the probe. Non-negotiable
   pins: `Float(0.0)` truthy, `Int(0)` falsy, `Str("")` falsy, `Array([])` falsy, every
   function truthy, `NaN`/`±inf` truthy.
2. **`type_name` strings** for all seven variants — identical on both backends
   since 2026-09-08 (`TypeOf` opcode; `tests/semantics_parity.rs`).
3. **Printing table (§4).** `6.0`→`6`; `-0.0`→`-0`; `NaN`→`NaN`; `inf`→`inf`;
   `0.1`→`0.1`; `(!p "hi")`→`hi`; `(!p (A "a" 1))`→`(A a 1)`; `(!p f)`→`(F f)` vs
   `(fn)`; `(!p)`→empty line. One vector MUST demonstrate that `!p` output fails to
   re-parse (e.g. printing a string containing `) ;`) — pinning the
   non-reparseability as a documented property.
4. **Equality cases (§5, resolved).** `(== 1 1.0)` false; `(== (A 1) (A 1.0))` false
   (non-coercion at depth); `(== 1.0 1.0)` true; `(== (A 1) (A 1))` **true**;
   `(== (A) (A))` **true**; `(== (A (A "a") 3) (A (A "a") 3))` true;
   `(== (A 1 2) (A 1))` false; `(== null null)` true; `(== "a" 97)` false;
   `(== nan nan)` false AND `(== (A nan) (A nan))` false; `(== f f)` false;
   each `!=` complement; the 65/66-level depth-boundary pair raising
   `structural equality depth limit (64) exceeded` with identical text on both
   backends (`tests/semantics_parity.rs`).
5. **Ordering table (§6).** `(< "a" "b")` → `true` **on both backends** since
   2026-09-08 (the former per-backend divergence pin is retired);
   `(<= nan 1)` → `true`; `(>= nan 1)` → `true`;
   `(< nan 1)` → `false`; `(< true true)` → `cannot compare bool and bool` both;
   `(<= 1 nan)` → `true`; `(< "a" 1)` → `cannot compare str and int` both.
6. **Division table (§7.3).** `(/ 20 4)`→`5`, `(/ 7 2)`→`3.5`, `(/ 6 3)`→`2` (`Int`),
   `(/ 6.0 3)`→`2` (`Float`, prints identically), `(/ 4 0)`→`division by zero`,
   `(/ 1.0 0.0)`→`inf`, `(/ 0.0 0.0)`→`NaN`, `(/ "a" 2)`→`cannot divide str by int`.
   Aligns with `tests/integration.rs:75-87`.
7. **Coercion matrix (§7.1–§7.2)** at least on the asymmetry cells:
   `(+ "a" 1)`, `(+ 1 "a")`, `(+ true "a")`, `(+ (A 1) "x")`, `(+ (A 1) 1)`→error,
   `(+ (A 1) (A 2))`→`(A 1 2)`, `(- "a" 1)`→`cannot do arithmetic on str and int`,
   `(+ true true)`→`cannot add bool and bool`, `(+ null null)`→error.
8. **Modulo and its zero rule (§7.4).** `(% 17 5)`→`2`; `(% 7 0)`→`modulo by zero`
   **on both backends**; `(% 1.0 0.0)`→`NaN`; and for the graph backend a hand-authored
   vector with `Mod(NodeRef, Immediate(Int(0)))` rejected by the exact verifier text.
9. **Overflow (§8, resolved).** `(+ 9223372036854775807 1)`, `(- INT_MIN 1)`,
   `(* INT_MAX k)`, `(/ INT_MIN -1)`, `(% INT_MIN -1)`, `(neg INT_MIN)`,
   `(!abs INT_MIN)` all raise their named `integer overflow …` error — same
   text, both backends, both profiles (`tests/semantics_parity.rs`, run under
   both `cargo test` and `cargo test --release`).
10. **Arity guards (§9).** Every case in §9.2 with its exact per-backend message, plus
    the §9.4 holes pinned as **known panics** (marked CURRENT) so that closing them is a
    visible, versioned change.
11. **Byte-vs-char length.** `(!len "héllo")` → `6`, `(!len (!chars "héllo"))` → `5`.
12. **Indexing/range/repeat error texts.** `(I (A 1 2) -1)` →
    `index 18446744073709551615 out of bounds (len 2)` (both); `(I "abc" 0)` →
    `cannot index str with int` (both); `(.. 1 5)` → `(A 1 2 3 4)` (half-open, both);
    `(.. 1.0 2.0)` → `range start must be int, got float` (source) vs
    `range requires integers` (compiled); `(each i 5 1)` → `cannot iterate over int`
    (source) vs `expected array, got int` (compiled).

## 12. Divergences from the upstream fact pass (tree wins)

| Claim received | Tree truth | Cite |
|---|---|---|
| "arity never validated; `(+ 1 2 3)` silently drops operands 3+; `(+ 1)`/`(- x)`/`(< x)` panic in both backends" | Both backends reject wrong arity with named errors; verifier rejects compiled `Add`/`Sub`/`Mul`/`Div`/`Mod` with ≠ 2 operands and `Neg` with ≠ 1 | `interpreter.rs:364-381`, `verifier.rs:148-177` |
| "`(% 1 0)` PANICS both backends (no zero guard)" | Errors with `modulo by zero` on both backends | `interpreter.rs:403-406`, `exec.rs:77-82` |
| "`!abs`/`!sin`/…: arity enforced only for abs/sin/cos/round/sqrt/floor" | Confirmed at draft time; `(!len)`/`(!atan2 1)` panicked until the 2026-09-08 `exec_builtin` arity table closed them (§9.4) | `interpreter.rs` `exec_builtin` head |
| "`Float(0.0)` truthy; NaN compares Equal" | Confirmed, and additionally: NaN makes `<=`/`>=` **true** while `==` stays false; the two operators disagree with each other | `value.rs:33`, `interpreter.rs:477`, `:491` |
| "float `/0` → inf/nan" | Confirmed; printed text is `inf` and `NaN` | `interpreter.rs:453`, `value.rs:54` |
| "i64 wrap-on-overflow (no overflow-checks in release)" | Confirmed for release, but debug builds **abort the process**; the observable behaviour is profile-dependent, which the report did not state | `Cargo.toml` (no `[profile]`), probe exit 101 |
| "arrays never equal / equality non-structural" | Confirmed **at draft time**; also `Fn == Fn` was false and NaN/NaN false. Superseded 2026-09-08: `Array` is deep-structural (§5, DECIDED); `Fn`/NaN cells stand | `interpreter.rs` `equal`, `exec.rs` `gval_eq` |
| "`!len` bytes vs `!chars` chars → non-ASCII disagree" | Confirmed, and clarified: the disagreement is **between the two builtins**; the two backends agree with each other | `interpreter.rs:548`, `:681`, `exec.rs:732`, `:719` |
| "`!p` prints with a space + newline, graph also buffers stdout" | Confirmed; also that zero operands prints an empty line and that `!p` always evaluates to `Null` | `interpreter.rs:520-526`, `exec.rs:746-753` |

## Open normative decisions

1. **`!type` — RESOLVED 2026-09-08.** Dedicated opcode `TypeOf` (byte `0x7E`,
   `op_fixed_arity` = 1, decode `0x7E`); `!type` returns the `type_name` string on
   both backends. The `// close enough for now` `ToString` mis-compile is gone.
2. **Structural vs representation equality — RESOLVED 2026-09-08 (§5).** `Array` is
   deep structural (64-level recursion cap, deeper is a named error); `Int`/`Float`
   equality does NOT coerce (`!num` remains the explicit bridge); `Fn` identity stays
   open with the closure register. Decisions compared across backends are now
   value-identical for array-valued outputs.
3. **`i64` overflow policy — RESOLVED 2026-09-08 (§8).** Checked ops everywhere,
   named runtime error, identical in debug and release, both backends; float→int
   out-of-range is an error, never saturation. Cross-machine `.lyc` exchange is
   therefore overflow-deterministic.
4. **Truthiness symmetry** (§2): make `0.0` falsy, or make `0` truthy, or drop numeric
   falsiness entirely.
5. **n-ary `+`/`*`/`&&`/`\|\|`** (§9.1 rule 1 vs common expectation) — the same decision is
   recorded in `grammar.md` Open normative decisions; it MUST be resolved once.
6. **Float-by-zero and NaN propagation policy** (§7.3, §6.1): error like integer division,
   or a documented IEEE-with-silence contract.
7. **Whether `!p` output is ever a data format.** If in-repo tests must keep asserting on
   it, a canonical (quoted, type-preserving) print mode MUST be specified instead (§4).
8. **Residual arity panics** (§9.4): `Not`, `Eq/…/Or` verifier rules, and builtin
   argument-count checks — the minimum set for "hostile bytes cannot abort the runtime".
