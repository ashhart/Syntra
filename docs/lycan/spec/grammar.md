# Lycan Grammar (Lexical + Syntactic)

Status: Draft v0.1 — describes implementation as of 2026-09-08 (graph FORMAT_VERSION=5)

Normative description of the Lycan source surface as produced by `src/lexer.rs` and
`src/parser.rs`. RFC 2119 keywords. Citations are `file:line` into the working tree as
of the date above; every claim was re-verified against that tree (source build plus
behavioural probes), and where an upstream fact-gathering pass disagreed with the tree,
the tree wins and the difference is recorded in §12. "CURRENT behavior" marks observed
implementation facts that a future revision may tighten; a conforming **producer** MUST
NOT rely on them.

The two execution backends that consume this grammar (tree-walking interpreter vs
compiled graph executor) and every place their *semantics* diverge are specified in
`scoping-and-execution.md`. Lexing and parsing are shared: one source text yields one
AST for both backends. Where a syntactic construct is accepted by the parser but
rejected later by the verifier, that split is called out here and pinned there.

## 1. Conformance surface

| Pipeline | Steps | Entrypoint | Cite |
|---|---|---|---|
| Source run | `Lexer::tokenize` → `Parser::parse_program` → `Interpreter::run` | `lycan <file>` for every extension except `.lyc` | `bin/lycan.rs:167-172`, `:175-184`, `:1412-1423` |
| Compile | lexer → parser → `GraphCompiler::compile` → `NeuralGraph::to_bytes` | `lycan compile <f.lycs>` → `<f.lyc>` | `bin/lycan.rs:234-254` |
| Graph run | decode (`.lyc` magic `LYCN`) → `verifier::verify` → `GraphExecutor::run` | `lycan <file.lyc>` | `bin/lycan.rs:186-221` |
| Legacy AST run | `binary::decode` → `Interpreter::run` | `lycan <file.lyc>` with magic `LYCAN\0` | `bin/lycan.rs:222-231`, `binary.rs:59-64` |

Dispatch is by **file extension first** (`.lyc` → binary path, anything else → source
path, `bin/lycan.rs:167-172`) and then by magic bytes inside the binary path
(`bin/lycan.rs:193`). A REPL loop sharing the same parse path exists at
`bin/lycan.rs:1374-1408`. Errors reach the user as `LycanError` display strings and the
CLI exits 1 (`error.rs:10-24`, `bin/lycan.rs:179-184`).

## 2. Lexical grammar

### 2.1 Token classes (`token.rs:5-22`)

| Token | Payload | Source syntax | Cite |
|---|---|---|---|
| `LParen` | — | `(` | `token.rs:7`, `lexer.rs:47` |
| `RParen` | — | `)` | `token.rs:8`, `lexer.rs:48` |
| `Int` | `i64` | decimal integer literal | `token.rs:9`, `lexer.rs:89-111` |
| `Float` | `f64` | decimal literal containing `.` | `token.rs:10`, `lexer.rs:89-111` |
| `Str` | `String` | `"…"` | `token.rs:11`, `lexer.rs:61-87` |
| `Bool` | `bool` | the atoms `true`, `false` | `token.rs:12`, `lexer.rs:156-160` |
| `Null` | — | the atom `null` | `token.rs:13`, `lexer.rs:159` |
| `Ident` | `String` | any atom (§2.3) | `token.rs:14`, `lexer.rs:151-162` |
| `TypeInt` … `TypeNull` | — | `:i` `:f` `:s` `:b` `:n` | `token.rs:16-20`, `lexer.rs:113-149` |
| `Eof` | — | end of input (synthesized) | `token.rs:21`, `lexer.rs:40` |

There is **no operator token class, no statement terminator, no newline token, no quote
or escape mechanism for symbols, and no reserved words** (`token.rs:1-3` header comment;
`parser.rs:51-101` matches structural tags as ordinary `Ident` strings). Every span is
`(line, col)` with 1-based origin (`token.rs:24-29`, `lexer.rs:12-19`, `:172-177`).

### 2.2 Token loop and position tracking (`lexer.rs:21-42`)

The loop skips whitespace, then checks for a comment, then records the current
`(line, col)` and lexes one token; `Eof` is appended once at the end.
`advance` increments `line` and resets `col` on `\n` only, otherwise increments `col`
(`lexer.rs:172-177`). Whitespace is `char::is_whitespace` (Unicode) (`lexer.rs:183-187`),
so **any Unicode whitespace separates tokens**.

### 2.3 Atoms, identifiers, and the character classes

`is_atom_char(c) == !c.is_whitespace() && c != '(' && c != ')' && c != '"' && c != ';'`
(`lexer.rs:195-197`). An atom is the maximal run of atom chars starting at the current
position (`lexer.rs:151-155`). Consequences, all normative as stated:

- Identifiers may contain `. # | > < $ % & + * / = ! ? : -` and any Unicode letter or
  symbol. `a.b`, `foo!`, `x<y`, `#5`, `+5`, `-.5`, `:if` are each **one** `Ident`.
- There is no quoting or escaping mechanism for identifiers; no `|` fence, no `#|…|#`.
- Atom characters therefore cannot appear *inside* a token that starts some other way
  (`(`, `)`, `"`, `;` always break; whitespace always breaks).
- A name beginning with a digit is not an identifier: a leading ASCII digit routes to
  number lexing first (`lexer.rs:51`), so `1abc` lexes as `Int(1)` + `Ident("abc")`.

### 2.4 Comments, and the lone `;` error

| Input | Result | Cite |
|---|---|---|
| `;; text` to end of line | comment, discarded (`;;;` and deeper also fine) | `lexer.rs:29-34` |
| `;` (single, not followed by `;`) | **lex error** `[lex L:C] unexpected character ';'` | `lexer.rs:52-57`, `:195-197` |

`;` is not an atom char, so a lone semicolon falls to the catch-all error arm. A
comment is consumed up to — but not including — the next `\n`, so line numbering stays
correct (`lexer.rs:30-32`, `:172-177`). There are no block comments, no reader macros,
and no escape that makes `;` part of an atom. A conforming producer MUST emit `;;` for
comments and MUST NOT emit a lone `;` anywhere outside a string literal.

### 2.5 Lexical error strings

| Condition | Exact message | Cite |
|---|---|---|
| character outside every class (e.g. lone `;`) | `unexpected character '{ch}'` | `lexer.rs:54-57` |
| `\` at EOF inside a string | `unterminated escape` | `lexer.rs:66-69` |
| EOF before the closing `"` | `unterminated string` | `lexer.rs:82-84` |
| numeric body that Rust's `f64` parse rejects | `invalid float '{s}'` | `lexer.rs:105` |
| integer body outside `i64` | `invalid int '{s}'` | `lexer.rs:108` |

Rendered as `[lex {line}:{col}] {msg}` (`error.rs:13-15`). Verified:
`;` → `[lex 1:9] unexpected character ';'`; `9223372036854775808` →
`[lex 1:29] invalid int '9223372036854775808'` (note `-9223372036854775808` **is**
lexable, because the `-` joins the body, `lexer.rs:93-95`).
`invalid float` is CURRENT-behavior-unreachable through the documented body grammar
(the two-dot rule splits before an unparseable body can form); it MUST still be
implemented by conforming lexers.

## 3. Literal syntax

### 3.1 Numbers (`lexer.rs:89-111`)

```
number := [ "-" ] digit { digit | "." digit }        ; at most one "."
```

The `-` sign is taken **only when the immediately following char is an ASCII digit**
(`lexer.rs:52`); otherwise `-` begins an atom. The body loop consumes ASCII digits and
`.`; the first `.` marks the token a float; a second `.`, or a `.` followed by another
`.`, **stops the token without error** (`lexer.rs:96-103`).

| Input | Tokens | Note |
|---|---|---|
| `42`, `-7` | `Int(42)`, `Int(-7)` | `i64`; out of range → `invalid int` |
| `3.5`, `1.` | `Float(3.5)`, `Float(1.0)` | a trailing dot is legal (Rust `"1."` parses) |
| `1.2.3` | `Float(1.2)`, `Ident(".3")` | **splits silently, no error** (`lexer.rs:97-99`) |
| `1..5` | `Int(1)`, `Ident("..")`, `Int(5)` | the `.`-followed-by-`.` rule protects `(.. 1 5)` (`lexer.rs:99`) |
| `.5` | `Ident(".5")` | a leading `.` never reaches number lexing (`lexer.rs:51-53`) |
| `1e3` | `Int(1)`, `Ident("e3")` | **no exponent form** |
| `+5` | `Ident("+5")` | **no unary `+` literal** |
| `0x1f`, `1_000` | `Int(0)`, `Ident("x1f")` / `Int(1)`, `Ident("_000")` | no hex/octal/binary/`_` |
| `-x` | `Ident("-x")` | `-` not followed by a digit is an atom start |

Verified: `(!p 1.2.3)` → `[runtime] undefined '.3'`; `(!p 1e3)` (compiled) prints
`1 null`; `(!p .5)` → `[runtime] undefined '.5'`; `(!p +5)` → `[runtime] undefined '+5'`.
A conforming lexer MUST reproduce the silent split of `1.2.3` and the `..` protection;
a conforming program MUST NOT contain a numeric literal that relies on either.

### 3.2 Strings (`lexer.rs:61-87`)

`"` opens and closes. Recognised escapes are **exactly** `\n`, `\t`, `\r`, `\\`, `\"`
(`lexer.rs:70-75`). Any other escape sequence is preserved **verbatim as backslash +
char** (`lexer.rs:76`), so `"\q"` is the two-char string `\q` and `"\z"` is `\z`. A
string may span newlines. There are no `"""` blocks, no raw-string prefix, no `\u{…}`,
no `\0`, and no byte escapes; a `"` inside a string requires `\"` (there is no
`''` quoting). Line/col advance normally inside a string literal (`lexer.rs:172-177`),
so a multi-line literal yields correct error positions.

### 3.3 Bool and null atoms (`lexer.rs:156-160`)

The only spellings are `true`, `false`, `null` — lowercase, exact. `True`, `TRUE`,
`NULL`, `Nil`, `0`, `1`, `""` are ordinary identifiers or numbers; `0` and `1` are truthy
or falsy per the truthiness table in `value-model.md` §2, never as literals of type bool.

### 3.4 Type tokens (`lexer.rs:113-149`) — lexed, never enforced

`:i :f :s :b :n` are recognised **only when the next char is neither alphanumeric nor
`_`** (`lexer.rs:119-139`). A bare `:` (EOF or non-alphanumeric next char, which
includes `_`) is the atom `Ident(":")` (`lexer.rs:115-117`), so `:_x` is `Ident(":")` +
`Ident("_x")`. Anything else after `:` becomes one atom `Ident(":name")`
(`lexer.rs:140-147`), e.g. `:if` → `Ident(":if")`.

Type tokens are legal **only in annotation position** (§5.3). In head or child position
they are not nodes: `(:i 1)` fails with `[parse 1:2] unexpected token TypeInt`
(`parser.rs:26-35`).

**CURRENT BEHAVIOR:** annotations are parsed into `Type` values (`parser.rs:368-377`,
`ast.rs:153-167`) and then **discarded by both backends**: the interpreter destructures
`Node::Bind { name, mutable, value, .. }` and `Node::Fn { …, .. }` without touching
`ty`/`ret` (`interpreter.rs:63`, `:75`), and the compiler does the same
(`graph_compiler.rs:76`, `:172`). Verified: `($ x :i "s")` then `(!p (!type x))` prints
`str` in source (compiled: `s`, see `value-model.md` §10). There is **no source syntax
for `Type::Array`** (`ast.rs:166`), which is reachable only through the legacy v1 AST
binary decoder (`binary.rs:506-509`).

**Normative note:** a language whose annotation has no effect is misleading to the AIs
that generate it. A future revision MUST either (a) enforce annotations at bind, call,
and return with a named error, or (b) remove them from the grammar; it MUST NOT keep
them as decorative syntax while claiming conformance. Until then, a conforming producer
SHOULD omit annotations, and a conforming consumer MUST NOT promise type errors for
them.

## 4. No infix syntax, no precedence (normative)

`token.rs:1-3` and `parser.rs:39-101` implement **pure prefix tag dispatch**: recursive
descent over `( head child* )` with a string switch on the head. There is no precedence
table, no pratt/climbing loop, no fixity declaration, and no operator token anywhere in
the crate. Therefore:

- Every operation MUST be written with explicit parentheses: `(+ 1 2)`, not `1 + 2`.
- `1 + 2` is `Int(1)` followed by the atom `Ident("+")` — two nodes, not an expression.
- The set of operators, their symbols, and their arity are fixed (§6); it MUST NOT be
  extended by precedence rules that do not exist.
- Nesting depth is the only composition mechanism; the CLI allocates a 64 MiB stack for
  deep recursion (`bin/lycan.rs:3-7`).

## 5. Form grammar

### 5.1 Top level and nodes

```
program := { node }                                   ; parser.rs:17-23
node    := INT | FLOAT | STRING | "true" | "false" | "null" | IDENT | list
                                                      ; parser.rs:25-36
list    := "(" ")"                                    ; → Node::Null   parser.rs:42-45
         | "(" head children ")"                      ; parser.rs:39-102
```

Only the six atom token classes are legal in node position; anything else (including
every `Type*` token) fails with `unexpected token {token:?}` (`parser.rs:34`).
**`()` is `null`**, not an empty call and not an error: `(!p ())` prints `null`
(verified both backends).

### 5.2 Head dispatch, in exact implementation order (`parser.rs:49-101`)

Order is normative because the classes overlap textually:

1. `Ident` equality match against the structural tags (§5.3).
2. `Ident` in the operator set → operator form (`parser.rs:95`, set at `:411-413`).
3. Any other `Ident` **starting with `!`** → builtin form (`parser.rs:97`).
4. `Ident` in the pipe set → pipe form (`parser.rs:99`, set at `:415-417`).
5. otherwise → call form (`parser.rs:101`).

Consequences that MUST be preserved:

- Operators are tested **before** the `!` rule, so `(!= a b)` is inequality, not a
  builtin (`parser.rs:93-95`).
- Pipe glyphs never start with `!`, so classes 3 and 4 are disjoint.
- `*` is **always** multiplication (`parser.rs:295`); the for-each tag is the word
  `each` (`parser.rs:70`). The comment at `parser.rs:66-69` describing a
  `(* ident expr body…)` ambiguity is **stale and unimplemented**; it MUST NOT be
  copied into implementations.
- `#` must be a standalone atom: `(#5 1)` is a call of the undefined name `#5`
  (`[runtime] undefined '#5'`, verified), not a repeat form.

### 5.3 Form table (exact arity/shape as coded)

`?` = optional, `*` = zero-or-more, `IDENT` requires an `Ident` token (a non-`Ident`
there fails with `expected identifier, got {tok:?}`, `parser.rs:398-401`),
`ty` = one of `:i :f :s :b :n` consumed only if present (`parser.rs:368-377`),
`param` = `IDENT ty?` (`parser.rs:350-358`), `body` = `{ node }` (`parser.rs:360-366`).
Any *extra* child where the production requires `)` fails with
`expected RParen, got {tok:?}` (`parser.rs:393-396`); verified for `($ x 1 2)` →
`[parse 1:8] expected RParen, got Int(2)`.

| Form | Exact shape | Parser |
|---|---|---|
| immutable bind | `($ IDENT ty? node)` — **exactly one** value | `parser.rs:51`, `:107-114` |
| mutable bind | `($! IDENT ty? node)` | `parser.rs:53`, `:107-114` |
| assignment | `(= IDENT node)` | `parser.rs:55`, `:116-122` |
| function | `(F IDENT (param*) ret? node*)` — body MAY be empty | `parser.rs:57`, `:124-135` |
| "stateful" function | `(F! IDENT (param*) ret? node*)` — see §5.4 | `parser.rs:59`, `:124-135` |
| lambda | `(\ (param*) ret? node*)` | `parser.rs:61`, `:137-146` |
| conditional | `(?? node node node?)` → `(cond then)` or `(cond then else)` — **2 or 3 children only** | `parser.rs:63`, `:148-159` |
| while | `(W node node*)` | `parser.rs:65`, `:161-167` |
| for-each | `(each IDENT node node*)` | `parser.rs:70`, `:169-176` |
| repeat | `(# node node*)` | `parser.rs:72`, `:178-184` |
| return | `(^ node)` — exactly one | `parser.rs:74`, `:186-191` |
| block | `(B node*)` | `parser.rs:76`, `:193-198` |
| array | `(A node*)` | `parser.rs:78`, `:200-208` |
| index | `(I node node)` | `parser.rs:80`, `:210-216` |
| range | `(.. node node)` | `parser.rs:82`, `:218-224` |
| adapt | `(~> IDENT node*)` | `parser.rs:84`, `:226-232` |
| adaptive choice | `(choice node*)` | `parser.rs:86`, `:234-242` |
| guard | `(guard node node node)` — **exactly 3 at parse time** | `parser.rs:88`, `:244-255` |
| strategy | `(strategy node*)` | `parser.rs:90`, `:257-265` |
| feedback | `(feedback node node)` | `parser.rs:92`, `:267-276` |
| operator | `(OP node*)` — **no arity check at parse time** (§5.5) | `parser.rs:95`, `:290-316` |
| builtin | `(!NAME node*)` | `parser.rs:97`, `:278-288` |
| pipe | `(|> node node)`, `(\|? node node)`, `(\|* node node)`, `(\|+ node node [node])` | `parser.rs:99`, `:318-336` |
| call | `(node node*)` — callee is any node | `parser.rs:101`, `:338-347` |

Verified parse-time rejections: `(F)` → `expected identifier, got RParen`;
`(each 1 2)` → `expected identifier, got Int(1)`; `(|> 1)` → `unexpected token RParen`
(a pipe needs data **and** function; the third `|+` child alone is optional,
`parser.rs:329-333`); `(guard true 1)` → `unexpected token RParen`; an unclosed form at
EOF → `expected RParen, got Eof`.

`(F …)` and `(\ …)` with a **zero-form body** parse; calling them yields `null`
(`scoping-and-execution.md` §4). `(^ node)` takes exactly one child — `(^)` is a parse
error and `(^ a b)` fails with `expected RParen, got …`.

### 5.4 Head tags are reserved; there are no reserved words

Because dispatch is by exact head string (§5.2), the following strings are **reserved in
head position** and MUST NOT be used as function or variable names there:
`$`, `$!`, `=`, `F`, `F!`, `\`, `?`, `W`, `each`, `#`, `^`, `B`, `A`, `I`, `..`, `~>`,
`choice`, `guard`, `strategy`, `feedback`, the 15 operator spellings of §6, `|>`, `|?`,
`|*`, `|+`. There is **no escape mechanism**: writing `(A 1 2)` can never mean "call the
function named `A`", and `(F A () 1)` defines a callable that is unreachable in head
position. A conforming program MUST NOT bind these names; a conforming tool SHOULD warn
when it sees one bound. Outside head position they are ordinary identifiers, so
`($ A 3)` then `(!p A)` prints `3` on both backends, and `($ each 7)`, `($ F 7)` are
legal bindings (`parser.rs:338-347` reaches the call path only for unrecognized heads).

`F!` is **inert CURRENT behavior**: the `stateful` flag is parsed (`parser.rs:134`),
stored in `LycanFn.stateful` under `#[allow(dead_code)]` (`value.rs:21-22`), and never
read by either backend (`interpreter.rs:75-86`; the compiler drops it entirely,
`graph_compiler.rs:172`). `F!` is therefore exactly `F` today; see
`scoping-and-execution.md` §5 and Open normative decisions.

### 5.5 Arity is never a parse-time property

The operator, builtin, choice, strategy, array and call productions all collect children
with the same `while !RParen && !Eof` loop (`parser.rs:283-285`, `:311-313`, `:341-343`),
so `(+ 1)`, `(+ 1 2 3)`, `(!len)` and `(not)` all **parse successfully**. Fail-closed
rejection is a *later* layer — runtime arity rules in the tree-walker
(`interpreter.rs:364-381`) and verifier rules for the compiled path
(`verifier.rs:148-177`) — and is normative in `value-model.md` §9. A grammar conformance
vector MUST therefore pin parse **acceptance** for these inputs and defer the rejection
to the semantic vectors.

## 6. Operators

`OpKind` is fixed at 15 values (`ast.rs:169-174`); the source spelling → `OpKind`
mapping is `parser.rs:292-309`, and the accepted head set is `parser.rs:411-413`.

| Head | `OpKind` | Compiled opcode | Semantic arity (normative) |
|---|---|---|---|
| `+` | Add | Add | 2 |
| `-` | Sub | Sub | 2 |
| `*` | Mul | Mul | 2 |
| `/` | Div | Div | 2 |
| `%` | Mod | Mod | 2 |
| `==` `!=` | Eq, Neq | Eq, Neq | 2 |
| `<` `>` `<=` `>=` | Lt, Gt, Lte, Gte | Lt, Gt, Lte, Gte | 2 |
| `&&` `\|\|` | And, Or | And, Or | 2 (eager: both operands evaluated) |
| `not` | Not | Not | 1 |
| `neg` | Neg | Neg | 1 |

- There is **no unary minus**: `(- x)` is binary `Sub` with one operand and is rejected
  (`[runtime] operator Sub expects exactly 2 operand(s), got 1`; compiled: verifier
  rejection). Negation is `(neg x)`.
- There is no word-form `and`/`or` and no prefix `!` operator; `!`-prefixed heads are
  builtins (§7). `&&`/`\|\|` are **eager**, so side effects in the right operand always
  happen (`interpreter.rs:415-416`).
- `unknown operator '{s}'` (`parser.rs:308`) and `unknown pipe '{s}'`
  (`parser.rs:325`) are unreachable today: both guards are pre-filtered by the sets at
  `parser.rs:411-417`. Implementations MUST still carry the strings.

## 7. Builtin head form

Any head whose text starts with `!` and that is not an operator is a builtin call; the
name is the head text **after the leading `!`** (`parser.rs:278-288`, slice at
`:281`). The recognized name set is the 19 entries in
`scoping-and-execution.md` §8; unknown names are an error in the tree-walker
(`unknown builtin '!{name}'`, `interpreter.rs:780-782`) and a silent `Noop` once
compiled (`graph_compiler.rs:365`) — a divergence pinned there.

The degenerate head `!` alone yields the empty builtin name and is reported as
`unknown builtin '!'` (`interpreter.rs:781` interpolates the empty name; verified).

## 8. Parser error strings

| Condition | Exact message | Cite |
|---|---|---|
| non-node token in node position | `unexpected token {tok:?}` | `parser.rs:34` |
| missing/extra child vs production | `expected {tok:?}, got {tok:?}` | `parser.rs:393-396` |
| name slot not an `Ident` | `expected identifier, got {tok:?}` | `parser.rs:398-401` |
| operator/pipe head outside the sets (dead today) | `unknown operator '{s}'` / `unknown pipe '{s}'` | `parser.rs:308`, `:325` |

Rendered `[parse {line}:{col}] {msg}` (`error.rs:16-18`); the span is the token at the
failure position, or the last token at EOF (`parser.rs:405-408`) — hence
`(!p 1` (unclosed) reports `[parse 2:1] expected RParen, got Eof` at the `Eof` span.
Runtime errors carry **no** position, node id, or call stack (`error.rs:19-21`); this is
the largest diagnosability gap in the language and a candidate normative requirement.

## 9. EBNF for what actually parses

```ebnf
(* lexical *)
ws         := unicode-whitespace ; char::is_whitespace
comment    := ";;" { any-char-except-newline } ; lexer.rs:29-34
atomchar   := any-char - ws - "(" - ")" - "\"" - ";" ; lexer.rs:195-197
lp         := "(" ; rp := ")"
digits     := digit { digit }
number     := [ "-" ] digits [ "." digits ]        ; ≤1 dot; stops at 2nd dot
            | "-" digits                             ; integer
float      := number-with-dot                        ; token Float
string     := "\"" { string-char } "\""
escape     := "\\n" | "\\t" | "\\r" | "\\\\" | "\\\"" ; only these five
typeTok    := ":" ( "i" | "f" | "s" | "b" | "n" ) - ( alnum | "_" )
ident      := ( atomchar - "(" - ")" - "\"" - ";" ) { atomchar }
              ; also produced for ":" and ":name" (lexer.rs:113-149)
boolTok    := "true" | "false" ; nullTok := "null"  ; atoms, exact match

(* syntactic — prefix only, no infix, no precedence *)
program    := { node }
node       := float | number | string | boolTok | nullTok | ident | list
list       := lp rp                                  (* → null *)
            | lp head children rp
head       := ident                                  ; exact-string dispatch
children   := { node }                               ; shape per §5.3, arity unchecked
bind       := "$" [ "!" ] ident [ ty ] node
fnish      := ( "F" | "F!" ) ident "(" { ident [ ty ] } ")" [ ty ] { node }
lamish     := "\" "(" { ident [ ty ] } ")" [ ty ] { node }
cond       := "?" node node [ node ]
loopish    := ( "W" | "#" ) node { node } | "each" ident node { node }
ret        := "^" node
blockish   := "B" { node } | "A" { node } | "choice" { node } | "strategy" { node }
access     := "I" node node | ".." node node
adaptish   := "~>" ident { node }
guarded    := "guard" node node node                 ; exactly three
feed       := "feedback" node node
op         := operator { node }                      ; arity fixed later, not here
builtin    := "!" { atomchar } { node }
pipe       := ( "|>" | "|?" | "|*" ) node node | "|+" node node [ node ]
call       := node { node }
ty         := ":i" | ":f" | ":s" | ":b" | ":n"       ; recorded, never enforced (§3.4)
```

## 10. What this grammar does not have

No infix operators or precedence; no statement terminators; no block/indentation
syntax; no namespaces, imports, or modules; no macros or reader forms; no character,
byte, map, struct, or set literals; no comments other than `;;`; no raw/multi-char
string forms; no quotation/escape for symbols; no `else`/`elif` keyword (only the
optional third `?` child); no variadic arity in operators (§6); no lexical distinction
between special forms and calls other than the exact head strings (§5.4).

## 11. Conformance requirements

A conformance vector set for the grammar MUST pin:

1. **Token-class exactness.** The token enum of §2.1 is closed: `:i`, `:f`, `:s`, `:b`,
   `:n` produce type tokens only at a non-`alnum`/non-`_` boundary; `:` alone and `:name`
   produce `Ident`; type tokens in node position fail with `unexpected token TypeInt`.
2. **Comment and terminator rules.** `;;` to EOL is discarded and line/col stay correct;
   a lone `;` fails with `[lex {line}:{col}] unexpected character ';'`. Pin the exact
   `[lex …]` rendering.
3. **Numeric quirk table (§3.1).** Each row of the table — `1.2.3`, `1..5`, `.5`, `1e3`,
   `+5`, `1.`, `0x1f`, `1_000`, `-x`, `-9223372036854775808`,
   `9223372036854775808` — MUST be pinned to its token sequence or exact error text.
   Silent `1.2.3` splitting is a pinned behavior, not a bug to "fix" silently.
4. **Escape set (§3.2).** `\n \t \r \\ \"` decode; every other escape round-trips as
   backslash + char; `unterminated string` and `unterminated escape` fire exactly as
   specified.
5. **`()` is `null`.** `(!p ())` prints `null` on both backends.
6. **Head-dispatch precedence.** `(!= 1 2)` evaluates inequality (→ `true`), never a
   builtin; `(* 2 3)` is multiplication; `(#5 1)` is an undefined-name call; `(A 1 2)`
   is an array literal even when a binding named `A` exists.
7. **Parse-time arity freedom (§5.5).** `(+ 1)`, `(+ 1 2 3)`, `(not)`, `(!len)`,
   `(!atan2 1)` all parse. Pin parse acceptance here; pin rejection to the semantic
   vectors of `value-model.md` §9 (which currently leaves `not`/builtin arity
   un-fail-closed in the compiled path).
8. **Structural rejections.** Exact texts of §8, including the `Eof`-span behavior of
   an unclosed form.
9. **No-precedence property.** A program containing `1 + 2`, `(+ 1 2) * 3`, or `x < y`
   outside parens MUST NOT be accepted as an expression; it lexes as separate atoms.
10. **Annotation inertness (CURRENT).** `($ x :i "s")`, `(F f (:i x) x)`→
    `expected identifier` (annotation follows the name: `(x :i)`), and `(F f () :i 3)`
    MUST behave exactly as the un-annotated forms on both backends, so that a future
    enforcement change is a deliberate, versioned break.
11. **Round-trip shape.** Every form of §5.3 has at least one vector that lexes, parses,
    compiles (`lycan compile`), and runs — pinning the shared front end across backends.

## 12. Divergences from the upstream fact pass (tree wins)

| Claim received | Tree truth | Cite |
|---|---|---|
| "`(+ 1 2 3)` silently drops operands 3+; `(+ 1)` panics in both backends; arity is validated nowhere" | Arity is now enforced in **both** paths: the tree-walker errors (`operator Add expects exactly 2 operand(s), got 1`/`got 3`) and the verifier rejects compiled binary-arithmetic nodes | `interpreter.rs:364-381`, `verifier.rs:148-169`, `tests/graph_guards_panic_holes.rs:87-105,158-184` |
| "`(% x 0)` panics in both backends" | `(% 7 0)` errors `[runtime] modulo by zero` on both backends (float `% 0` still yields `NaN`) | `interpreter.rs:403-408`, `exec.rs:77-82` |
| "capability registry has 33 names" | `REGISTRY` contains **35** names | `registry.rs:69-564` |
| "float `/0` silently yields inf/nan" (no formatting given) | Printed forms are `inf` and **`NaN`** (Rust `f64` `Display`), not `nan` | `value.rs:54`, probe `(!p (/ 0.0 0.0))` |
| "`static mod-zero rejected` (implying compiled source hits it)" | The verifier's static rule fires only on `Operand::Immediate(Int(0))`; `lycan compile` lowers every literal to a `ConstInt` **node**, so `lycs`-sourced `%` by zero is caught by the *executor* guard instead | `verifier.rs:161-168`, `graph_compiler.rs:54-56`, `exec.rs:77-82` |
| "`1.2.3` … `Ident('.3')` with no error" (framed as lexer-only) | Confirmed, but the user-visible effect is a *later* runtime error `undefined '.3'` — the split itself is silent | `lexer.rs:96-103` |
| "`guard` compiled under-supply → Null" | Reachable only via hand-authored graphs: the parser requires exactly 3 children and the verifier rejects `< 3` operands, so the `Null` path needs a decode-without-verify entrypoint | `parser.rs:244-255`, `verifier.rs:103-109`, `exec.rs:257-259` |

## Open normative decisions

To be settled by the language owner; recorded rather than silently resolved:

1. **`F!` is inert.** Either give stateful functions real semantics (persistent per-name
   state across calls, as the name promises) or delete `F!` from the grammar. Today it
   is a lie in the surface syntax (`value.rs:21-22`, `graph_compiler.rs:172`).
2. **Type annotations: enforce or remove** (§3.4). If enforced, decide the error text,
   the coercion rules for `Int`↔`Float`, and the missing syntax for array types.
3. **Numeric literals.** `1.2.3` silently splitting and `.5`/`+5`/`1e3` not being numbers
   are silent foot-guns. Decide whether the lexer MUST reject ambiguous numeric bodies
   (a breaking change pinned by §11.3 today).
4. **n-ary `+` (and `*`, `&&`, `\|\|`).** Only `!p` is variadic
   (`scoping-and-execution.md` §8); arithmetic operators are strictly binary and reject
   3+ operands. Decide whether to make the foldable operators n-ary (the common AI
   expectation) or keep strict binary arity and say so.
5. **Reserved-head ergonomics.** Reserved heads are unquotable and un-overridable
   (§5.4). Decide whether to add a call-escape form (e.g. `(call A 1 2)`) or to
   formally forbid the names and have tooling reject their definition.
6. **Runtime diagnostics.** No runtime error carries line/col, node id, or call stack
   (`error.rs:19-21`). Decide the minimum required position information.
