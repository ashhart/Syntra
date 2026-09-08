# Lycan Capsule / Container Formats

Status: Draft v0.1 — describes implementation as of 2026-09-08 (graph FORMAT_VERSION=5, memory schema v7)

Normative description of the three container shapes that wrap a `.lyc` graph binary
(the byte layout itself is specified in `graph-binary-format.md`, companion
section). RFC 2119 keywords. Citations are `file:line` as of the date above.
"CURRENT behavior" marks observed implementation facts that a future spec may
tighten.

## 1. The three container shapes

| # | Shape | Producer | Contents | Verifier run? |
|---|---|---|---|---|
| A | `<name>.lycap/` directory | `lycan capsule create` (`bin/lycan.rs:744-746`; impl `capsule.rs:53-114`) | `manifest.json` + `program.lyc` + `policy.json` (plus generated `inspect.json` / `journal.json`) (`capsule.rs:1-2`) | yes — decode + verify before create; `lycan capsule verify` re-verifies |
| B | `syntra author` compile bundle | `syntra author <spec.yaml> --out-dir` → `compile_to_dir` (`capsule_compiler.rs:13-70`) | `program.lyc`, `program.lycs`, sidecar JSON files | **no** — compile path never runs `verifier::verify`; no hashes, no policy |
| C | Server runtime store | `src/store.rs` install/save | `current.lyc` + `manifest.json` + `policy.json` + `snapshots/` | only at decide time (`server/decide.rs:136-143`) |

There is **no cryptographic signing anywhere** in any shape: a repo-wide search for
sign/ed25519/hmac finds no crypto. Integrity is SHA-256 plus substring checks
only (§6). See §6 for the normative marking of signing as future work.

## 2. Shape A — the `.lycap` capsule directory

### 2.1 Layout and write order

Output directory is `{name}.lycap` (`bin/lycan.rs:744-746`). Files are written in
exactly this order (`capsule.rs:60-113`):

| Order | File | Content | Cite |
|---|---|---|---|
| 1 | `program.lyc` | byte-verbatim copy of the input `.lyc` | `capsule.rs:68-71` |
| 2 | `inspect.json` | generated inspection dump; embeds `lycan-graph-v{header.version}` | `capsule.rs:74-78`, `:289` |
| 3 | `manifest.json` | §2.2 | `capsule.rs:95-99` |
| 4 | `journal.json` | generated journal dump | `capsule.rs:102-106` |
| 5 | `policy.json` | §2.4 | `capsule.rs:109-113` |

Precondition: the input graph is decoded **and verified** before anything is
written; failure aborts with `graph verification failed: {e}`
(`capsule.rs:62-67`). A conforming producer MUST NOT emit a capsule for an
unverified graph.

### 2.2 `manifest.json` fields (`generate_manifest`, `capsule.rs:246-282`)

| Key | Type | Value / source | Cite |
|---|---|---|---|
| `name` | string | capsule name argument | `capsule.rs:246-282` |
| `version` | string | hardcoded `"0.1.0"` | `capsule.rs:246-282` |
| `intent` | string | caller-supplied intent string | `capsule.rs:246-282` |
| `entry` | string | always `"program.lyc"` | `capsule.rs:246-282` |
| `inputs` | array | always `[]` | `capsule.rs:246-282` |
| `outputs` | array | always `["stdout"]` | `capsule.rs:246-282` |
| `capabilities` | array of string | detected effects ∪ caller-declared capabilities | `capsule.rs:246-282`, detection §2.5 |
| `created_by` | string | always `"lycan 0.1.0"` | `capsule.rs:246-282` |
| `format` | string | always `"lycan-capsule-v1"` | `capsule.rs:264` |
| `program_sha256` | string | lowercase hex SHA-256 over `program.lyc` bytes | `capsule.rs:246-282` |
| `inspect_sha256` | string | hex SHA-256 over `inspect.json` bytes | `capsule.rs:246-282` |
| `graph_stats` | object | `{nodes, live_nodes, edges, strings}`; `live_nodes` counts nodes with op != Noop | `capsule.rs:246-282` |

### 2.3 Manifest verification (`verify_capsule`, `capsule.rs:117-190`)

CURRENT behavior — the checks are, in order:

1. Required files exist: `manifest.json`, `program.lyc`, `policy.json`
   (`capsule.rs:121-128`). `inspect.json` is optional-checked (§ item 4).
2. **Format check is a substring test**, not a field equality: the raw manifest
   text must contain `"format"` followed by `lycan-capsule`, else
   `manifest.json missing format field` (`capsule.rs:140-143`). A conforming
   verifier SHOULD parse the `format` key and compare it to
   `"lycan-capsule-v1"` exactly; consumers MUST NOT treat the substring test as a
   security property.
3. `program.lyc` is re-hashed and compared to `program_sha256`; mismatch:
   `program.lyc hash mismatch (actual: {h})` (`capsule.rs:145-150`).
4. If `inspect.json` is present, its hash is compared to `inspect_sha256`;
   mismatch: `inspect.json hash mismatch` (`capsule.rs:152-161`).
5. Policy enforcement is a **raw-JSON substring** test: for each detected effect
   `x`, the policy text must contain the literal `"allow_{x}: true"`; else
   `graph uses {effect} but policy does not allow it`
   (`capsule.rs:163-180`). This is formatting-sensitive (requires the exact
   `serde_json`-style spacing); consumers SHOULD parse the JSON instead, and MUST
   NOT rely on the substring form surviving a re-serialization.
6. The journal consistency check is a no-op comment (CURRENT behavior)
   (`capsule.rs:117-190`).
7. `program.lyc` is decoded and `verifier::verify` runs; any error fails closed
   (§7).

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
| `max_execution_ms` | u64 | 30000 (from `Policy::default()`) | wall-clock budget |
| `max_memory_bytes` | u64 | 268435456 (= 256 × 1024 × 1024, from `Policy::default()`) | memory budget |

CURRENT-behavior hazard: `allow_self_modify: true` by default combined with
`lycan <f.lyc>` run rewriting the binary in place with evolved weights/journal
(`bin/lycan.rs:214-218`) means **executing a capsule mutates it**; determinism
claims in this spec family scope to `compile`, not to run.

### 2.5 Effect detection (`capsule.rs:194-256`)

| Graph feature | Effect / capability | Cite |
|---|---|---|
| any `Print` node | `stdout` | `capsule.rs:194-238` |
| any `ReadLine` node | `stdin` | `capsule.rs:194-238` |
| `OpCode::Capability` first operand | string-table name resolved via `StringRef`, or via a `ConstStr` node feeding the Capability node → capability-registry effects | `capsule.rs:194-238`, name resolution `capsule.rs:240-256` |

## 3. Shape B — the `syntra author` compile bundle

`syntra author <spec.yaml> --out-dir` validates a `CapsuleSpec` YAML
(`capsule_spec.rs:11-40`), emits deterministic Lycan S-expression source
(`emit_lycan_source`, `capsule_compiler.rs:78-158` — including
`($ ctx_i (!cap "runtime.inputGet" "…"))`, `(F option_name (idx) (? (== idx i) …))`,
`($ selected_option (choice 0 1 …))`), runs lexer → parser → `GraphCompiler`
(`capsule_compiler.rs` `compile_source` ~`:240-250`), then `graph.to_bytes()` into
(`capsule_compiler.rs:13-70`):

| File | Content | Cite |
|---|---|---|
| `program.lyc` | compiled graph bytes | `capsule_compiler.rs:22-24` |
| `program.lycs` | generated S-expr source | `capsule_compiler.rs:26-28` |
| `learning.json` | §3.2 | `capsule_compiler.rs:32-34`, shape `:175-199` |
| `reward_spec.json` | §3.3 | `capsule_compiler.rs:37-39`, `:202-221` |
| `context_schema.json` | `{contexts: […]}` | `capsule_compiler.rs:42-44` |
| (optional) hierarchical sidecar | emitted when `hierarchicalOptions` used | `capsule_compiler.rs:13-70` |

**This bundle has NO `policy.json`, NO hashes, NO `inspect.json`, and the compile
path NEVER runs `verifier::verify`** (CURRENT behavior; see §7). Consumers MUST
re-verify before execution.

### 3.1 `CapsuleSpec` YAML keys (`capsule_spec.rs:11-40`)

| Key | Type / constraint | Default | Cite |
|---|---|---|---|
| `name` | string | — | `capsule_spec.rs:11-40` |
| `version` | string | `""` | `capsule_spec.rs:11-40` |
| `options` | array, ≥ 2 entries | — | error `options must contain at least two entries` |
| `contexts` | array | — | `capsule_spec.rs:11-40` |
| `reward.type` | ∈ {`bernoulli`, `continuous`, `sparse_continuous`} | — | `capsule_spec.rs:11-40` |
| `reward.range` | required iff type `continuous` | — | error `reward.range is required when reward.type is continuous` |
| `reward.components[]` | `{name, weight, normalize ∈ {minmax, budget}, range?, budget?}` | — | `capsule_spec.rs:11-40` |
| `algorithm.type` | ∈ {`auto`, `thompson`, `ucb`, `epsilon_greedy`, `weighted`} | `auto` | `capsule_spec.rs:11-40` |
| `learning.min_exploration` | number | 0.02 | `capsule_spec.rs:96-104` |
| `decisions[]` | `{name, options, depends_on?}`; max 8 (`capsule_spec.rs:8`); dependency cycle rejected via Kahn (`capsule_spec.rs:~225-260`) | — | `capsule_spec.rs:11-40` |
| `hierarchicalOptions` | mutually exclusive with `decisions`; flat `options` must equal the enumerated leaf names | — | `capsule_spec.rs:180-215` |

### 3.2 `learning.json` schema (`capsule_compiler.rs:175-199`)

| Key | Values | Notes |
|---|---|---|
| `algorithm` | `thompson` \| `ucb1` \| `epsilonGreedy` \| `simpleWeighted` | note the wire names (`ucb1`, `epsilonGreedy`, `simpleWeighted`) differ from the YAML names (`ucb`, `epsilon_greedy`, `weighted`) |
| `safety.minExploration` | number | from `learning.min_exploration` |
| `safety.selectionMode` | `weighted` \| `greedy` \| `epsilonGreedy` | |
| `epsilon` | 0.10 | |
| `safety.selectionEpsilon` | 0.10 | emitted **only** when algorithm is `epsilonGreedy` |

### 3.3 `reward_spec.json` schema (`capsule_compiler.rs:202-221`)

`{type, range, components: [{name, weight, normalize, range, budget}]}` — the
`type`/`normalize` enums are the YAML ones passed through unchanged (§3.1).

## 4. Shape C — the server runtime store (`src/store.rs`)

| Path | Meaning | Cite |
|---|---|---|
| `<job dir>/capsules/<capsule>/` | capsule directory | `store.rs:143-146` |
| `…/current.lyc` | the live graph binary (atomic write) | `store.rs:148-150`, `:309` |
| `…/snapshots/<unix-sec>.lyc` | time-named snapshots | `store.rs:487-488` |
| `…/manifest.json` | install record | `store.rs:311-313` |
| `…/policy.json` | written **only if absent** | `store.rs:315-325` |

`install_capsule_bytes_in_job` (`store.rs:303-334`) writes `current.lyc`
atomically (`store.rs:309`), then a store manifest — **a different schema from
shape A's manifest**:

| Key | Value | Cite |
|---|---|---|
| `name` | capsule name | `store.rs:311-313` |
| `tenant` | tenant id | `store.rs:311-313` |
| `job` | job id | `store.rs:311-313` |
| `hash` | sha256 hex of the installed bytes | `store.rs:311-313` |
| `installed` | epoch seconds | `store.rs:311-313` |

No `format` key — store manifests are not `lycan-capsule-v1` documents.

### 4.1 Server default `policy.json` (load_policy install default)

Written only when the file does not already exist (`store.rs:315-325`), fixed text
(`store.rs:315-322`) — **6 keys, no budget fields**:

| Key | Value |
|---|---|
| `allow_stdout` | true |
| `allow_stdin` | false |
| `allow_file_read` | false |
| `allow_file_write` | false |
| `allow_network` | false |
| `allow_self_modify` | true |

Divergence from the `lycan capsule create` default set (§2.4): the server default
omits `max_execution_ms` and `max_memory_bytes` entirely, and its `allow_stdout`
is unconditionally true (not effect-derived). A consumer MUST treat missing
budget keys as "no budget configured", not as zero.

### 4.2 Write paths do not verify

`save_graph_in_job` is an atomic write with **no verify** (`store.rs:355-359`);
install likewise does not verify (§7). Verification happens at decide time only
(`server/decide.rs:136-143`).

## 5. Version strings across formats

| Format | Accepted version token | Check | Error on mismatch | Cite |
|---|---|---|---|---|
| Graph `.lyc` (inside every container) | version byte == 5 | exact | `unsupported version {version}` | `graph.rs:434-436` |
| Graph magic | prefix `LYCN`, len ≥ 5 | exact | `invalid .lyc file: bad magic` | `graph.rs:429-431` |
| Legacy AST `.lyc` | byte at offset 6 == 1; magic `LYCAN\0`, len ≥ 7 | exact | `unsupported .lyc version {n}` (`LycanError::Runtime`); bad magic → `invalid .lyc file: bad magic` | `binary.rs:59-64` |
| `.lycap` manifest | literal `lycan-capsule-v1` written; check is substring `"format"` + `lycan-capsule` only | substring (CURRENT) | `manifest.json missing format field` | `capsule.rs:264,140-143` |
| Derived display | `lycan-graph-v{header.version}` in `inspect.json` / `lycan inspect`; `Neural Graph v{n}` in `lycan explain` | display only | — | `capsule.rs:289`, `bin/lycan.rs:268,425` |

Normative: a producer MUST write exactly `lycan-capsule-v1` in the manifest
`format` field; a consumer SHOULD compare it with full-string equality even
though the CURRENT verifier only substring-matches. No format negotiates or
migrates versions; there is no forward-compat path (§3 of the graph spec).

## 6. Integrity model — and its limits

- The ONLY integrity primitives in the entire pipeline are **SHA-256 digests**
  (`program_sha256`, `inspect_sha256`, store `hash`) and **substring checks** over
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
call sites below; display `verification failed (N errors):` (`verifier.rs:12-25`).

### 7.1 Fail-closed (verify runs; reject aborts/500)

| Entrypoint | Behavior on verify failure | Cite |
|---|---|---|
| `lycan <f.lyc>` (run) | print + `exit(1)` | `bin/lycan.rs:198-202` |
| `lycan decide` / `decide --input` | reject | `bin/lycan.rs:1162-1164`, `:1279-1281` |
| `run_binary_with_context` | reject | `bin/lycan.rs:821-824` |
| `lycan capsule verify` → `verify_capsule` | `exit(1)` | `bin/lycan.rs:752-757` |
| `lycan capsule create` | refuses invalid input graph | `capsule.rs:63-66` |
| `capsule_run` | verifies first | `bin/lycan.rs:787-791` |
| server `POST /decide` | decode Err or verify Err → HTTP 500 JSON | `server/decide.rs:141-143` |

### 7.2 Decode WITHOUT verify (fail-open exposure — CURRENT behavior)

| Entrypoint | Note | Cite |
|---|---|---|
| `lycan compile` | never verifies | `bin/lycan.rs:234-255` |
| dump / stats / explain / learn-report / capsule inspect | inspection only | (report §4; display paths) |
| store save / install | `store.rs:305-325`, `:355-359` | atomic writes, no verify |
| evolution loop graph loads | operate on unverified graphs | `evolution_loop.rs:204,250` |
| server feedback | writes weights into a decode-only graph | `feedback.rs:283-286` |
| admin node listing | | `admin.rs:86-88` |
| inspect endpoints | | `inspect.rs:383`, `routes.rs:432,579` |

Normative consequence: any code path that mutates a stored graph (§7.2's feedback
and evolution writes included) MUST arrange for verification before the mutated
graph is executed, and consumers MUST NOT assume a stored `current.lyc` ever
passed the verifier — it only does if some fail-closed entrypoint ran since the
last write.

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
   `allow_self_modify: true`; budgets 30000 / 268435456); server-install policy
   matches §4.1 (6 keys, no budget fields) and is written only when absent.
4. **Verify-before-create.** `capsule create` on an invalid graph fails with
   `graph verification failed: {e}` and leaves no output directory.
5. **Integrity checks fail closed.** `capsule verify` rejects with the exact
   strings `manifest.json missing format field`,
   `program.lyc hash mismatch (actual: {h})`, `inspect.json hash mismatch`,
   `graph uses {effect} but policy does not allow it` — pinning the CURRENT
   substring semantics (a vector SHOULD also record that a whitespace-reformatted
   policy changes the substring outcome).
6. **No signing.** Vectors MUST NOT require any signature field; presence of a
   manifest signature today MUST be ignored (future-work marker, §6).
7. **Author bundle shape.** `syntra author` output contains §3's files and
   contains no `policy.json`, no hash field, and no `inspect.json`; its
   `learning.json`/`reward_spec.json`/`context_schema.json` match §3.2-3.3 key
   sets exactly, including the epsilon-only-under-epsilonGreedy rule.
8. **Author bundle is unverified.** A graph that fails the verifier can still be
   produced by `syntra author`; execution of it via a fail-closed entrypoint MUST
   be rejected.
9. **Store layout.** install produces `current.lyc` + store manifest with keys
   `{name, tenant, job, hash, installed}` and `hash` == sha256 of installed
   bytes; `save` never verifies; `/decide` maps decode/verify failure to HTTP 500.
10. **Version literals.** `lycan-capsule-v1` in `.lycap` manifests; the graph
    inside every container carries magic `LYCN` + version byte 5 (§5); store
    manifests carry no `format` key.
