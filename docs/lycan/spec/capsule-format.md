# Lycan Capsule / Container Formats

Status: Draft v0.2 — shape A as of 2026-09-08, shape C as of 2026-09-23 (graph FORMAT_VERSION=5)

Normative description of the container shapes that wrap a `.lyc` graph binary
(the byte layout itself is specified in `graph-binary-format.md`, companion
section). RFC 2119 keywords. `file:line` citations are as of 2026-09-08; the
shape C section cites files only.
"CURRENT behavior" marks observed implementation facts that a future spec may
tighten.

v0.1 also specified shape B, the bundle `syntra author` compiled from a
capsule YAML, and a v1 layout of shape C. Syntra v2 removed `syntra author`
(a capsule is now a decision spec, `PUT .../spec`) and stores a program only
as an optional feature program; §3 and §4 below reflect that.

## 1. The container shapes

| # | Shape | Producer | Contents | Verifier run? |
|---|---|---|---|---|
| A | `<name>.lycap/` directory | `lycan capsule create` (`bin/lycan.rs:744-746`; impl `capsule.rs:53-114`) | `manifest.json` + `program.lyc` + `policy.json` (plus generated `inspect.json` / `journal.json`) (`capsule.rs:1-2`) | yes — decode + verify before create; `lycan capsule verify` re-verifies |
| C | Syntra store, feature program of a capsule | `POST .../install` (`src/server/capsules.rs` `install`; `src/store.rs` `save_program`) | `current.lyc` + `manifest.json`, beside the capsule's `spec.json` and `policy.json` | yes — decode + verify before anything is written, and again when the capsule loads |

There is **no cryptographic signing anywhere** in any shape: a working-tree
search of `src/` for ed25519 / hmac / signature / signing finds no matches (only
the false-positive phrase "call signature" in `shared_state_strategy.rs:16`).
Integrity is SHA-256 plus substring checks only (§6). See §6 for the normative
marking of signing as future work.

## 2. Shape A — the `.lycap` capsule directory

### 2.1 Layout and write order

Output directory is `{name}.lycap` (`bin/lycan.rs:744-746`). Files are written in
exactly this order (`capsule.rs:72-111`):

| Order | File | Content | Cite |
|---|---|---|---|
| 1 | `program.lyc` | byte-verbatim copy of the input `.lyc` | `capsule.rs:72-75` |
| 2 | `inspect.json` | generated inspection dump; embeds `lycan-graph-v{header.version}` | `capsule.rs:77-81`, `:289` |
| 3 | `manifest.json` | §2.2 | `capsule.rs:95-99` |
| 4 | `journal.json` | generated journal dump | `capsule.rs:101-105` |
| 5 | `policy.json` | §2.4 | `capsule.rs:107-111` |

Precondition: the input graph is decoded **and verified** before anything is
written; failure aborts with `graph verification failed: {e}`
(`capsule.rs:60-66`). A conforming producer MUST NOT emit a capsule for an
unverified graph.

### 2.2 `manifest.json` fields (`generate_manifest`, `capsule.rs:251-284`)

All keys are emitted by one fixed `format!` template (`capsule.rs:255-273`):

| Key | Type | Value / source | Cite |
|---|---|---|---|
| `name` | string | capsule name argument (intent string escaped for `"` at `:275`) | `capsule.rs:256,274-275` |
| `version` | string | hardcoded `"0.1.0"` | `capsule.rs:257` |
| `intent` | string | caller-supplied intent string | `capsule.rs:258` |
| `entry` | string | always `"program.lyc"` | `capsule.rs:259` |
| `inputs` | array | always `[]` | `capsule.rs:260` |
| `outputs` | array | always `["stdout"]` | `capsule.rs:261` |
| `capabilities` | array of string | detected effects ∪ caller-declared capabilities | `capsule.rs:262`, detection §2.5; merge `capsule.rs:87-93` |
| `created_by` | string | always `"lycan 0.1.0"` | `capsule.rs:263` |
| `format` | string | always `"lycan-capsule-v1"` | `capsule.rs:264` |
| `program_sha256` | string | lowercase hex SHA-256 over `program.lyc` bytes | `capsule.rs:265`, `sha256_hex :244-249` |
| `inspect_sha256` | string | hex SHA-256 over `inspect.json` bytes | `capsule.rs:266` |
| `graph_stats` | object | `{nodes, live_nodes, edges, strings}`; `live_nodes` counts nodes with op != Noop | `capsule.rs:267-272`, live count `:253,280` |

### 2.3 Manifest verification (`verify_capsule`, `capsule.rs:117-193`)

CURRENT behavior — the checks run, in this order; non-fatal findings accumulate
and are joined with `"; "` (`capsule.rs:118,191`):

1. Required files exist: `manifest.json`, `program.lyc`, `policy.json`
   (`capsule.rs:120-129`). `inspect.json` is optional-checked (item 5).
2. `program.lyc` is decoded and `verifier::verify` runs; any error aborts
   immediately with `graph verification failed: {e}` (`capsule.rs:131-137`; §7).
3. **Format check is two independent substring tests**, not a field parse: the
   raw manifest text must contain `"format"` AND contain `lycan-capsule`, else
   `manifest.json missing format field` (`capsule.rs:140-144`). A conforming
   verifier SHOULD parse the `format` key and compare it to
   `"lycan-capsule-v1"` exactly; consumers MUST NOT treat the substring test as a
   security property.
4. If the manifest text contains `"program_sha256"`, `program.lyc` is re-hashed
   and the manifest text must contain that hex string (a digest substring, not a
   field comparison); else `program.lyc hash mismatch (actual: {actual_hash})`
   (`capsule.rs:147-152`).
5. If `inspect.json` exists AND the manifest contains `"inspect_sha256"`, its
   hash must appear in the manifest text; else `inspect.json hash mismatch
   (actual: {actual_hash})` (`capsule.rs:154-163`).
6. Policy enforcement is a **raw-JSON substring** test: for each detected effect
   `x` in {stdout, stdin, file_read, file_write, network}, the policy text must
   contain the literal `"allow_x": true`; else `graph uses {effect} but policy
   does not allow it` (`capsule.rs:165-181`). Unmapped effects are skipped
   silently (`capsule.rs:176`). This is formatting-sensitive (requires exactly
   the create-path spacing); consumers SHOULD parse the JSON instead, and MUST
   NOT rely on the substring form surviving a re-serialization.
7. The journal consistency check is a no-op comment (CURRENT behavior): journal
   node refs are trusted to the graph verifier only (`capsule.rs:183-186`).

### 2.4 `policy.json` schema and defaults (create path)

`Policy` struct (`capsule.rs:25-35`), serialized by `generate_policy`
(`capsule.rs:342-368`), which derives the five `allow_*` effect flags from the
detected capability set (§2.5), keeps `allow_self_modify: true` unconditionally,
and takes the budgets from `Policy::default()` (`capsule.rs:37-50`):

| Key | Type | Create-path value | Meaning |
|---|---|---|---|
| `allow_stdout` | bool | true iff capabilities contain `stdout` (graph has Print) | stdout writes |
| `allow_stdin` | bool | true iff capabilities contain `stdin` (ReadLine) | stdin reads |
| `allow_file_read` | bool | true iff capabilities contain `file_read` | file reads |
| `allow_file_write` | bool | true iff capabilities contain `file_write` | file writes |
| `allow_network` | bool | true iff capabilities contain `network` | network access |
| `allow_self_modify` | bool | true (never derived) | self-modification |
| `max_execution_ms` | u64 | 30000 (from `Policy::default()`) | wall-clock budget — **enforced** by the graph executor since 2026-09-08 (BUG-8): error `execution exceeded max_execution_ms (budget N ms)` |
| `max_memory_bytes` | u64 | 268435456 (= 256 × 1024 × 1024, from `Policy::default()`) | memory budget — **NOT enforced** (documented gap) |

`allow_self_modify` is accepted and written but unused: running a program
or a capsule with the `lycan` CLI does not write the graph back, so
executing a capsule leaves its files unchanged (CURRENT behavior).

### 2.5 Effect detection (`capsule.rs:195-242`)

| Graph feature | Effect / capability | Cite |
|---|---|---|
| any `Print` node | `stdout` | `capsule.rs:203,222` |
| any `ReadLine` node | `stdin` | `capsule.rs:204,223` |
| `OpCode::Capability` first operand | string-table name resolved via `StringRef`, or via a `ConstStr` node feeding the Capability node → capability-registry effects (`capabilities::get`) | `capsule.rs:205-217`, name resolution `capsule.rs:227-242` |

## 3. (removed: shape B)

The `syntra author` bundle of v0.1 no longer exists.

## 4. Shape C — a capsule's feature program in the Syntra store

A Syntra capsule is a decision spec; a Lycan program is optional and only
computes derived features and eligibility before each decision. Files
under `<store>/tenants/<tenant>/jobs/<job>/capsules/<capsule>/`
(`src/store.rs`):

| Path | Meaning |
|---|---|
| `spec.json` | the decision spec (not a Lycan artifact) |
| `policy.json` | the program's execution policy; written when the capsule is created if absent |
| `current.lyc` | the installed program's graph binary, byte-verbatim (atomic write) |
| `manifest.json` | install record: `{programSha256, programBytes, installedAtMs}` |
| `data/` | the program's file sandbox root, created when file access is allowed |

`POST .../install` (`src/server/capsules.rs` `install`) decodes the body,
runs `verifier::verify`, and refuses a graph containing an
`AdaptiveChoice`, `Strategy` or `Feedback` node, answering 400 before
anything is written (`FeatureProgram::load`, `src/server/runtime.rs`).
The capsule loads the program through the same check. The store manifest
is not a `lycan-capsule-v1` document and has no `format` key.
`DELETE .../program` removes `current.lyc` and `manifest.json`.

### 4.1 Default `policy.json` of a new capsule

Written when a capsule is created, if absent (`src/store.rs`
`DEFAULT_POLICY`): every effect denied.

| Key | Value |
|---|---|
| `allow_stdout` | false |
| `allow_stdin` | false |
| `allow_file_read` | false |
| `allow_file_write` | false |
| `allow_network` | false |

Missing keys take the defaults of `ExecutionPolicy::from_policy_json`
(`max_execution_ms` 30 000; `deny_private_networks` true; see
`execution-policy.md`). A stored policy that fails validation runs the
program deny-all.

## 5. Version strings across formats

| Format | Accepted version token | Check | Error on mismatch | Cite |
|---|---|---|---|---|
| Graph `.lyc` (inside every container) | version byte == 5 | exact | `unsupported version {version}` | `graph.rs:434-436` |
| Graph magic | prefix `LYCN`, len ≥ 5 | exact | `invalid .lyc file: bad magic` | `graph.rs:429-431` |
| Legacy AST `.lyc` | byte at offset 6 == 1; magic `LYCAN\0` (6 bytes), len ≥ 7 | exact | `unsupported .lyc version {n}` (`LycanError::Runtime`); bad magic → `invalid .lyc file: bad magic` | `binary.rs:7-8,59-64` |
| `.lycap` manifest | literal `lycan-capsule-v1` written; check is two substrings `"format"` AND `lycan-capsule` only | substring (CURRENT) | `manifest.json missing format field` | `capsule.rs:264,142-144` |
| Derived display | `lycan-graph-v{header.version}` in `inspect.json` / `lycan inspect`; `Neural Graph v{n}` in `lycan explain` | display only | — | `capsule.rs:289`, `bin/lycan.rs:268,425` |

Normative: a producer MUST write exactly `lycan-capsule-v1` in the manifest
`format` field; a consumer SHOULD compare it with full-string equality even
though the CURRENT verifier only substring-matches. No format negotiates or
migrates versions; there is no forward-compat path (§3 of the graph spec).

## 6. Integrity model — and its limits

- The ONLY integrity primitives in the entire pipeline are **SHA-256 digests**
  (`program_sha256`, `inspect_sha256`, the Syntra store manifest's `programSha256`) and **substring checks** over
  raw JSON text (§2.3). A repo-wide search for signing (sign / ed25519 / hmac)
  finds no crypto and no key material anywhere (CURRENT limitation).
- Therefore capsules provide **tamper-evidence against a trusted manifest only**:
  nothing authenticates the manifest itself. A verifier MUST NOT claim origin,
  authorship, or non-repudiation.
- **Future work (normative marker):** cryptographic signing of
  `manifest.json` (e.g. a `sig` field over its canonical bytes, with a key
  resolution mechanism) is planned but unimplemented; no field is reserved for it
  and consumers MUST NOT require it today.
- `verify_capsule`'s substring checks are formatting-sensitive: any
  re-serialization of `manifest.json`/`policy.json` (key reorder, whitespace) can
  spuriously pass or fail them. Consumers SHOULD parse JSON and compare values;
  they MUST NOT rely on textual shape.

## 7. Verifier vs decode-only entrypoints

`verifier::verify(&NeuralGraph)` collects ALL errors and rejects (fail-closed) at
call sites below; display `verification failed ({N} errors):`
(`verifier.rs:13-21`, signature `:25`).

### 7.1 Fail-closed (verify runs; reject aborts/500)

| Entrypoint | Behavior on verify failure | Cite |
|---|---|---|
| `lycan <f.lyc>` (run, with or without `--input`) | print + `exit(1)` | `bin/lycan.rs` |
| `lycan capsule verify` → `verify_capsule` | `exit(1)` | `bin/lycan.rs:752-757` |
| `lycan capsule create` | refuses invalid input graph | `capsule.rs:60-66` |
| `capsule_run` | verifies first | `bin/lycan.rs:787-791` |
| Syntra `POST .../install` | decode Err, verify Err or a learning node → HTTP 400 JSON | `server/capsules.rs`, `server/runtime.rs` |
| Syntra capsule load (before a decide) | the stored program goes through the same check; failure → HTTP 500 JSON | `server/runtime.rs` |

### 7.2 Decode WITHOUT verify (fail-open exposure — CURRENT behavior)

| Entrypoint | Note | Cite |
|---|---|---|
| `lycan compile` | never verifies | `bin/lycan.rs:234-255` |
| dump / stats / explain / inspect / capsule inspect | inspection only — decode sites carry no adjacent verify call | `bin/lycan.rs` |

Normative consequence: any code path that mutates a stored graph MUST arrange
for verification before the mutated graph is executed. Syntra never mutates
a stored program: it verifies on install and on load.

## 8. Conformance requirements

A conformance test vector set for the container formats MUST pin:

1. **`.lycap` file set + order.** `lycan capsule create` emits exactly
   `program.lyc`, `inspect.json`, `manifest.json`, `journal.json`, `policy.json`
   under `{name}.lycap/`; `program.lyc` is byte-identical to the input (§2.1).
2. **Manifest exactness.** Every §2.2 key present with the pinned literal values
   (`version: "0.1.0"`, `entry: "program.lyc"`, `inputs: []`,
   `outputs: ["stdout"]`, `created_by: "lycan 0.1.0"`,
   `format: "lycan-capsule-v1"`); `program_sha256`/`inspect_sha256` equal SHA-256
   over the on-disk bytes; `graph_stats.live_nodes` counts op != Noop.
3. **Policy defaults, both sets.** Create-path policy matches §2.4 (8 keys; all
   five `allow_*` effect flags derived from the capability set;
   `allow_self_modify: true`; budgets 30000 / 268435456); a new Syntra
   capsule's policy matches §4.1 (5 keys, every effect denied) and is written
   only when absent.
4. **Verify-before-create.** `capsule create` on an invalid graph fails with
   `graph verification failed: {e}` and leaves no output directory.
5. **Integrity checks fail closed.** `capsule verify` rejects with the exact
   strings `manifest.json missing format field`,
   `program.lyc hash mismatch (actual: {actual_hash})`,
   `inspect.json hash mismatch (actual: {actual_hash})`,
   `graph uses {effect} but policy does not allow it` — pinning the CURRENT
   substring semantics (a vector SHOULD also record that a whitespace-reformatted
   policy changes the substring outcome, and that hash checks are skipped when
   the manifest omits the `*_sha256` keys).
6. **No signing.** Vectors MUST NOT require any signature field; presence of a
   manifest signature today MUST be ignored (future-work marker, §6).
7. **Store layout.** install produces `current.lyc` + store manifest with keys
   `{programSha256, programBytes, installedAtMs}` and `programSha256` == sha256
   of the installed bytes; install refuses (400) a graph that fails decode,
   verification or contains a learning node, and writes nothing.
8. **Version literals.** `lycan-capsule-v1` in `.lycap` manifests; the graph
    inside every container carries magic `LYCN` + version byte 5 (§5); store
    manifests carry no `format` key.
