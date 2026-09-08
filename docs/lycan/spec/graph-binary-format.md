# Lycan Graph Binary Format (`.lyc`, "LYCN" v5)

Status: Draft v0.1 — describes implementation as of 2026-09-08 (graph FORMAT_VERSION=5, memory schema v7)

Normative description of the serialized neural-graph binary produced and consumed by
`src/graph.rs`. The key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are used as
in RFC 2119. "CURRENT behavior" marks observed decoder leniency that a byte-exact
encoder MUST NOT rely on. All citations are `file:line` into the implementation as of
the date above. There is no `.lyg` format; `.lyc` is the only graph binary extension.

## 1. Container overview

A graph file is a flat byte stream:

1. 29-byte header (§2)
2. String table (§4)
3. Node records (§5)
4. Edge records (§8)
5. State vector (§9)
6. Journal records (§10)

Section order is fixed by the encoder (`graph.rs:365-424`) and the decoder
(`graph.rs:453-561`); a conforming file MUST present sections in exactly this order.
All integers are little-endian unsigned (`u8`/`u32`/`u64`), signed integers are
little-endian `i64`, and all reals are IEEE-754 binary64 (`f64`, 8 bytes, LE)
(`graph.rs:659-662`). There is no compression, alignment padding, or section table;
record positions are implicit from the header counts.

An unrelated legacy format ("AST `.lyc`", magic `LYCAN\0`, version byte 1 at offset 6,
`binary.rs:59-64`) is dispatched by magic: bytes `4C 59 43 4E` select the graph
decoder, anything else falls to the legacy AST decode path
(`bin/lycan.rs:193,420`). This document specifies only the graph format.

## 2. Header (29 bytes)

| Off | Width | Field | Encoding | Notes |
|---|---|---|---|---|
| 0 | 4 | magic | bytes `4C 59 43 4E` | ASCII "LYCN" (`graph.rs:250`) |
| 4 | 1 | version | `u8` == 5 | `FORMAT_VERSION` (`graph.rs:251`); see §3 |
| 5 | 4 | node_count | `u32` LE | guard-checked, §11 |
| 9 | 4 | edge_count | `u32` LE | guard-checked, §11 |
| 13 | 4 | state_size | `u32` LE | reserved; ignored on decode (`graph.rs:20-21`, `:440`) |
| 17 | 4 | string_count | `u32` LE | guard-checked, §11 |
| 21 | 4 | flags | `u32` LE | reserved; MUST be 0 (§12.2) |
| 25 | 4 | entry | `u32` LE | entry node id; verifier rule 1 |

The body (string table) begins at offset **29**. An in-repo comment
(`graph.rs:690-691`) claims 25; it is wrong and MUST NOT be copied into
implementations.

The encoder writes `node_count`/`edge_count`/`state_size`/`string_count` from the
live collection lengths, not from cached header fields (`graph.rs:357-363`). A
byte-exact encoder MUST do the same: the header counts and the state length MUST
equal the actual body contents.

## 3. Version rule

- The decoder MUST accept exactly `version == 5`. Any other value fails with the
  error string `unsupported version {version}` (a plain `String` error,
  `graph.rs:434-436`).
- A file shorter than 5 bytes or whose first 4 bytes are not `4C 59 43 4E` fails
  with `invalid .lyc file: bad magic` (`graph.rs:429-431`).
- Version is never negotiated, migrated, or forward-compatible: there is no path
  that reads a v4 or v6 file. A producer MUST emit 5; a consumer MUST reject
  everything else (CURRENT behavior; normative for both).
- Derived display strings (not on-disk fields): `inspect.json` and
  `lycan inspect` output render `lycan-graph-v{header.version}`
  (`capsule.rs:289`, `bin/lycan.rs:268`); `lycan explain` prints
  `Neural Graph v{n}` (`bin/lycan.rs:425`).
- Error propagation at the edges: the CLI prints the error and `exit(1)`
  (`bin/lycan.rs:194-197,223-227`); server `/decide` maps decode errors and
  verify errors to HTTP 500 JSON (`server/decide.rs:137-143`).

## 4. String table

`string_count` entries, each: `u32` byte-length prefix followed by that many raw
bytes (`graph.rs:365-369` encode, `graph.rs:453-463` decode). Strings are stored as
raw bytes — no length re-encoding, no escaping, no obfuscation
(`graph.rs:242-246`). Lookup (`get_string`) converts with lossy UTF-8.

- If `pos + len` exceeds the file, decode fails:
  `string table overflow at pos {pos}, len {len}` (`graph.rs:446-460`). This is the
  one in-body bound error; it MUST be produced, not replaced with truncation.
- An entry length of 0 is legal (empty string).
- The header count is guard-checked before allocation (§11) with
  `MIN_STRING_ENTRY_BYTES = 4` (`graph.rs:268,449`).
- The table MUST precede the node section: node operands reference strings by
  index into a table the decoder has already built.

## 5. Node record

`node_count` records, in file order = node vector order. Minimum record size is
34 bytes (`MIN_NODE_BYTES`, `graph.rs:266`), laid out:

| Field | Width | Present | Notes |
|---|---|---|---|
| id | `u32` | always | logical id; id == index is NOT enforced (verifier rule 2 checks range only) |
| op | `u8` | always | opcode, §7; unknown → hard fail |
| operand_count | `u32` | always | guard-checked, §11 (`MIN_OPERAND_BYTES = 1`, `graph.rs:272,473`) |
| operands | variable | ×operand_count | tagged, §6 |
| weight_count | `u32` | always | guard-checked (§11, `MIN_WEIGHT_BYTES = 8`, `graph.rs:273,480`) |
| weights | `f64` × weight_count | | learnable parameters; invariants §13 |
| bias | `f64` | always | type-specialization hint: 0 generic / 1 int / 2 float (`graph.rs:33`) |
| activation_count | `u64` | always | fire counter |
| has_state | `u8` | always | nonzero ⇒ next field present |
| state_slot | `u32` | iff has_state | NOT range-checked against state length |
| weight_kind | `u8` | always | enum, §5.1 — lenient coercion |
| has_annotation | `u8` | always | nonzero ⇒ next field present |
| annotation | `u32` | iff has_annotation | string-table index; verifier rule 7 bounds it |
| contract | `u8` | always | enum, §5.2 — lenient coercion |
| objective | `u8` | always | enum, §5.3 — lenient coercion |

Decode: `graph.rs:465-514`; encode: `graph.rs:371-396`.

### 5.1 weight_kind byte map

| Byte | Meaning |
|---|---|
| 1 | `Adaptive` |
| 2 | `TypeHint` |
| 3 | `Strategy` |
| any other | `Observational` (CURRENT behavior: silent coercion, `graph.rs:489-495`) |

### 5.2 contract byte map

| Byte | Meaning |
|---|---|
| 0 | `None` |
| 1 | `SameOutput` — all options must produce identical output (format-only; no in-repo emitter produces byte 1, §13.4) |
| 2 | `Validated` — output must satisfy a validator node (format-only; no in-repo emitter) |
| 3 | `WithinTolerance` — numeric outputs within tolerance; epsilon stored in `weights[weights.len()-1]` (`graph.rs:53-55`) |
| any other | `None` (CURRENT behavior: silent coercion, `graph.rs:498-504`) |

### 5.3 objective byte map

| Byte | Meaning |
|---|---|
| 1..8 | `Speed`, `Accuracy`, `Reliability`, `Cost`, `Risk`, `Confidence`, `Reward`, `MultiObjective` (in that order) |
| 0 or any other | `None` (CURRENT behavior: silent coercion, `graph.rs:505-512`) |

## 6. Operand tags

Each operand is a `u8` tag followed by its payload (`graph.rs:599-611`):

| Tag | Payload | Operand |
|---|---|---|
| `0x01` | `u32` | NodeRef |
| `0x10` | `i64` | Immediate(Int) |
| `0x11` | `f64` | Immediate(Float) |
| `0x12` | `u8` | Immediate(Bool) — any nonzero byte decodes `true` (`graph.rs:605`) |
| `0x13` | — | Immediate(Null) |
| `0x20` | `u32` | StateRef (NOT range-checked against state length) |
| `0x30` | `u32` | StringRef (verifier rule 4 bounds it) |
| `0x40` | `u32` | VarSlot |
| any other | — | hard fail: `unknown operand tag 0x{tag:02X}` (`graph.rs:610`) |

An unknown operand tag MUST abort the whole decode; it MUST NOT be skipped or
coerced (contrast §14's lenient enum corners).

## 7. Opcode byte map

`opcode_from_byte` (`graph.rs:614-655`); any byte not in this table MUST fail with
`unknown opcode 0x{b:02X}` (`graph.rs:653`).

| Byte | OpCode | Byte | OpCode | Byte | OpCode |
|---|---|---|---|---|---|
| `0x01` | ConstInt | `0x40` | Branch | `0x78` | Floor |
| `0x02` | ConstFloat | `0x41` | Merge | `0x79` | Round |
| `0x03` | ConstStr | `0x42` | Loop | `0x7A` | Sqrt |
| `0x04` | ConstBool | `0x43` | Sequence | `0x7B` | Ln |
| `0x05` | ConstNull | `0x44` | AdaptiveChoice | `0x7C` | Exp |
| `0x06` | LoadVar | `0x45` | Guard | `0x7D` | Atan2 |
| `0x07` | StoreVar | `0x46` | ForEach | `0x80` | Adapt |
| `0x10` | Add | `0x47` | Repeat | `0x81` | Weight |
| `0x11` | Sub | `0x50` | Define | `0x82` | Predict |
| `0x12` | Mul | `0x51` | Call | `0x83` | Feedback |
| `0x13` | Div | `0x52` | Return | `0x84` | Spawn |
| `0x14` | Mod | `0x53` | Lambda | `0x85` | Prune |
| `0x15` | Neg | `0x60` | Array | `0x86` | Strategy |
| `0x20` | Eq | `0x61` | Index | `0x90` | Pipe |
| `0x21` | Neq | `0x62` | Range | `0x91` | Filter |
| `0x22` | Lt | `0x63` | Length | `0x92` | Map |
| `0x23` | Gt | `0x64` | Chars | `0x93` | Reduce |
| `0x24` | Lte | `0x70` | Print | `0xA0` | Capability |
| `0x25` | Gte | `0x71` | ReadLine | `0xFE` | Noop |
| `0x30` | And | `0x72` | ParseNum | `0xFF` | Halt |
| `0x31` | Or | `0x73` | Split | | |
| `0x32` | Not | `0x74` | ToString | | |

Blocks: `0x01-0x07` values/vars; `0x10-0x15` arithmetic; `0x20-0x25` comparison;
`0x30-0x32` logic; `0x40-0x47` control flow; `0x50-0x53` functions; `0x60-0x64`
collections; `0x70-0x74` IO/string; `0x75-0x7D` math; `0x80-0x86` neural (incl.
Strategy); `0x90-0x93` pipeline; `0xA0` capability call; `0xFE` Noop (dead slot —
`live_nodes` counts op != Noop, `capsule.rs:253,280`); `0xFF` Halt.

## 8. Edge record

`edge_count` records; minimum 17 bytes (`MIN_EDGE_BYTES`, `graph.rs:267`); decode
`graph.rs:516-525`:

| Field | Width | Present |
|---|---|---|
| from | `u32` | always |
| to | `u32` | always |
| weight | `f64` | always |
| has_gate | `u8` | always |
| gate | `u32` | iff has_gate |

The header `edge_count` is guard-checked with `MIN_EDGE_BYTES = 17` before
allocation (`graph.rs:451`).

## 9. State section

`u32` length followed by `f64 × length` values (`graph.rs:527-534`). The length is
guard-checked (`MIN_STATE_BYTES = 8`, `MAX_STATE`, `graph.rs:530`).

CURRENT behavior: the length field is **optional at EOF** — if the file ends
exactly where the edges end, `pos >= data.len()` yields length 0 without error
(`graph.rs:528`). A conforming encoder MUST always write the length field
explicitly (even `00 00 00 00`). The header `state_size` field duplicates this
length and is ignored here (§12.1).

## 10. Journal section

`u32` count, then count records of (`graph.rs:536-561`), minimum 17 bytes each
(`MIN_JOURNAL_BYTES`, `graph.rs:270`):

| Field | Width | Notes |
|---|---|---|
| run_number | `u64` | |
| node_id | `u32` | verifier rule 16 bounds it |
| mutation | `u8` | byte map below |
| reason | `u32` | |

Mutation byte map (`graph.rs:546-557`): 1 `TypeSpecialized`, 2 `ConstantFolded`,
3 `PathPruned`, 4 `GuardInserted`, 5 `NodeSpawned`, 6 `FeedbackReceived`,
7 `EvolutionStarted`, 8 `ProposalAccepted`, 9 `ProposalRejected`,
10 `EvolutionCompleted`; **any other byte** decodes as `WeightUpdate` (CURRENT
behavior: lenient coercion).

CURRENT behavior corners: the count is optional at EOF (same rule as state,
`graph.rs:537`), and the record loop **breaks silently** if EOF is reached
mid-section (`graph.rs:542`) — a truncated journal decodes as a shorter journal
without error. A conforming encoder MUST write the count and every full record.

## 11. DoS guards (header/inner count validation)

Hard ceilings (`graph.rs:257-261,271`):

| Constant | Value | Applies to |
|---|---|---|
| `MAX_NODES` | 1 000 000 | header `node_count` |
| `MAX_EDGES` | 4 000 000 | header `edge_count` |
| `MAX_STRINGS` | 1 000 000 | header `string_count` |
| `MAX_STATE` | 16 000 000 | state length (128 MB cap) |
| `MAX_JOURNAL` | 10 000 000 | journal count |
| `MAX_NODE_ITEMS` | 1 000 000 | per-node `operand_count` and `weight_count` |

Per-item minimum sizes used for the "can it fit" check: node 34, edge 17, string
entry 4, state 8, journal 17, operand 1, weight 8 bytes (`graph.rs:266-273`).

Every count above MUST be validated before `Vec::with_capacity` allocation via
`check_count` (`graph.rs:279-293`, applied at `graph.rs:449-451,473,480,530,539`),
with exactly two error shapes:

1. ceiling exceeded:
   `{label} count {count} exceeds maximum {max} (header count too large; possible malicious or corrupt .lyc)`
2. cannot fit remaining bytes (saturating multiply, cannot overflow):
   `{label} count {count} too large for input size: needs at least {needed} bytes, have {remaining} remaining (header count out of bounds)`

Labels in use: `string`, `node`, `edge`, `operand`, `weight`, `state`, `journal`.

## 12. Reserved and dead fields

### 12.1 `header.state_size` (offset 13)

Written by the encoder as the actual state length (`graph.rs:360`) but **ignored on
decode** — the decoder re-reads the body state length instead
(`graph.rs:20-21` `#[allow(dead_code)]`, `:440,528`). Normatively: a producer
SHOULD write the true state length (for byte-exact round-tripping of the header);
a consumer MUST treat the field as reserved and MUST NOT derive behavior from it.

### 12.2 `header.flags` (offset 21)

Always 0 and never interpreted: the encoder writes the stored value verbatim
(`graph.rs:362`) and the decoder stores but never reads it. A conforming encoder
MUST write `flags = 0`. A consumer MUST NOT assign meaning to nonzero flags
without a format-version bump.

## 13. `weights` length invariants (per `Contract`)

Verifier rule 13 (`verifier.rs:129-147`, enforced lengths `:135-147`) is the
normative invariant; error template:
`node #{n} {Op:?}: weights count {W} does not match operand count {O} (contract {C:?} requires {E})`.

| Contract | Required `weights.len()` | Notes |
|---|---|---|
| `None` | `== operands.len()` | |
| `SameOutput` | `== operands.len()` | format-only (§13.4) |
| `Validated` | `== operands.len()` | format-only (§13.4) |
| `WithinTolerance` | `== operands.len() + 1` | last slot is epsilon |

Producer conventions (what `lycan compile` emits):

- `choice` → `AdaptiveChoice`, contract `None`, `WeightKind::Adaptive`,
  weights = `1/n` repeated `n` times, `len == operands.len()`
  (`graph_compiler.rs:278-289`).
- `strategy` → `Strategy`, contract `WithinTolerance`, `WeightKind::Strategy`,
  weights = `1/n × n` **plus trailing epsilon `1e-6`**, `len == operands.len() + 1`
  (`graph_compiler.rs:302-315`; epsilon pinned at `weights[weights.len()-1]` by the
  contract doc, `graph.rs:53-55`).
- Deterministic `Branch` → weights `vec![0.5, 0.5]` regardless of arity,
  `WeightKind::Observational`; no rule applies to Branch
  (`graph_compiler.rs:132-133`).

Consumers keep the same epsilon-slot convention, with one path-specific detail:
the `AdaptiveChoice` executor path computes `n_options = weights.len() - 1` iff
contract is `WithinTolerance` and `weights.len() > 1`, else `weights.len()`, then
caps at the operand count (`graph_executor/exec.rs:178-185`); the `Strategy`
executor path computes `n_options = min(weights.len(), operands.len())` and
returns `Null` early when weights or operands are empty
(`exec.rs:264-267`). `evolve.rs` uses the `-1` convention at
`evolve.rs:17-20,161-164,348-351`, and server feedback at
`server/feedback.rs:293-296`. Option insertion in evolution pops the epsilon
slot, appends the new average, and re-pushes epsilon (`evolve.rs:549-558`);
weight normalization excludes the epsilon slot (`evolve.rs:580-587`,
`exec.rs:441-464` clamps to `[0.01, 0.99]` and renormalizes only `weights[..n]`).
WithinTolerance execution reads `tol = weights.last().unwrap_or(1e-6)`
(`exec.rs:394`), compares each option against the median (`exec.rs:416-424`),
and requires `correct_count > n_options/2` for consensus (`exec.rs:426`).

### 13.4 Format-only contracts

Contracts `SameOutput` (byte 1) and `Validated` (byte 2) exist in the format and
verifier (`graph.rs:498-504`) but are **never emitted by any in-repo producer** —
only via hand-authored bytes or programmatic construction. Compiled fixtures
contain only `None` and `WithinTolerance`. Mark: format-only / producer-unknown.

### 13.5 Zero-operand Strategy (report flagged [INFERENCE]; DISPROVEN against tree)

A `Strategy` node with **0 operands and 1 weight** satisfies rule 13
(WithinTolerance requires `0 + 1 = 1`). The upstream fact report flagged, as an
explicit **[INFERENCE]**, that the executor would then index `operands[0]` and
panic. That inference is **disproven against the working tree**: the `Strategy`
handler returns `Flow::Val(GVal::Null)` before any indexing when weights or
operands are empty (`exec.rs:264-266`), and clamps `n_options` to the operand
count (`exec.rs:267`). Divergence logged; the tree wins. The shape is still
semantically degenerate: a conformance encoder MUST NOT emit it.

## 14. Lenient-decode corners (CURRENT behavior)

The decoder is strict about opcodes and operand tags (§6, §7) but lenient in the
corners below. These are **descriptions of CURRENT behavior only**; they are not
part of the file contract, and a byte-exact encoder MUST NOT rely on, produce, or
test against them:

| Corner | Current behavior | Normative rule for encoders |
|---|---|---|
| Primitive reads past EOF | `read_u8/u32/i64/f64/u64` return `0` / `0.0` instead of erroring, advancing `pos` to EOF (`graph.rs:664-683`); mid-record truncation can yield zero-padded but structurally valid nodes | Encoder MUST write complete records; MUST NOT depend on zero-padding |
| State length at EOF | missing length ⇒ 0 (`graph.rs:528`) | Encoder MUST always emit the length field |
| Journal count at EOF | missing count ⇒ 0 (`graph.rs:537`) | Encoder MUST always emit the count field |
| Journal mid-EOF | loop `break`s silently; short journal accepted (`graph.rs:542`) | Encoder MUST write `count` full records |
| weight_kind byte | anything ∉ {1,2,3} ⇒ `Observational` (`graph.rs:489-496`) | Encoder MUST emit 0/1/2/3; SHOULD NOT emit `2` unless the field is used |
| contract byte | anything ∉ {1,2,3} ⇒ `None` (`graph.rs:498-504`) | Encoder MUST emit 0..3 exactly |
| objective byte | anything ∉ 1..8 ⇒ `None` (`graph.rs:505-513`) | Encoder MUST emit 0..8 exactly |
| mutation byte | anything ∉ 1..10 ⇒ `WeightUpdate` (`graph.rs:545-557`) | Encoder MUST emit exact bytes 1..10; to encode `WeightUpdate` it MUST emit `0x00` explicitly, never an arbitrary out-of-range byte |

Consequence: corrupt enum bytes are **accepted and silently change semantics**,
while corrupt opcode/operand-tag bytes hard-fail. Consumers that care about
provenance SHOULD re-encode and byte-compare (§15) rather than trust decoded enum
values; corrupt enum bytes are also invisible to the verifier, which validates
semantics, not bytes.

## 15. Conformance requirements

A conformance test vector for this format MUST pin:

1. **Magic bytes.** The first 4 bytes are exactly `4C 59 43 4E`; a file with any
   other prefix (or shorter than 5 bytes) is rejected with
   `invalid .lyc file: bad magic`. Pin the bytes directly — no in-repo test
   asserts `FORMAT_VERSION == 5` or the magic today; fixture drift testing is
   implicit only.
2. **Version byte.** Offset 4 is exactly `0x05`; a vector with any other byte
   fails with `unsupported version {version}` (exact string, e.g.
   `unsupported version 4`).
3. **Byte-exact round-trip.** For each vector graph, `decode(bytes) → encode()`
   reproduces the input bytes exactly (header counts from live lengths, flags 0,
   complete state/journal sections, exact enum bytes). This is the primary
   anti-drift check, mirroring the fixture-drift property
   (`tests/fixture_drift.rs:43-77`).
4. **Guard rejections.** Vectors for each guard failure MUST pin the exact error
   text of §11 (both templates, at least the node-count ceiling case and the
   "needs at least {needed} bytes" case), and MUST confirm no allocation/panic
   occurs for headers claiming `0xFFFFFFFF` counts.
5. **Unknown-opcode hard fail.** A node record with an opcode byte outside §7
   fails with `unknown opcode 0x{b:02X}`; an unknown operand tag fails with
   `unknown operand tag 0x{tag:02X}`. Neither is coerced.
6. **String-table bound.** A truncated string entry fails with
   `string table overflow at pos {pos}, len {len}`.
7. **Weights invariants (§13).** A `WithinTolerance` Strategy with
   `weights.len() != operands.len()+1` (and a `None` AdaptiveChoice with
   `len != operands.len()`) is rejected by verifier rule 13 with the exact
   template; regressions pinned by `tests/verifier_strategy_weights.rs:50-54,57-64,66-73`.
8. **Enum coercion (documented CURRENT behavior).** A vector with an out-of-range
   weight_kind / contract / objective / mutation byte MUST decode (not error) to
   the coerced value listed in §14 — pinning that today's decoder accepts it, so
   any future strictness change is a deliberate, versioned break.
9. **Lenient EOF corners.** A zero-length-padded tail decodes without error
   (CURRENT behavior); such files MUST NOT be produced by conforming encoders.
