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
| B | `syntra author` compile bundle | `syntra author <spec.yaml> --out-dir` → `compile_to_dir` (`capsule_compiler.rs:13-76`) | `program.lyc`, `program.lycs`, sidecar JSON files (§3) | **no** — compile path never runs `verifier::verify`; no hashes, no policy |
| C | Server runtime store | `src/store.rs` install/save | `current.lyc` + `manifest.json` + `policy.json` + `snapshots/` | only at decide time (`server/decide.rs:136-143`) |

There is **no cryptographic signing anywhere** in any shape: a working-tree
search of `src/` for ed25519 / hmac / signature / signing finds no matches (only
the false-positive phrase "call signature" in `shared_state_strategy.rs:16`).
Integrity is SHA-256 plus substring checks only (§6). See §6 for the normative
marking of signing as future work.

**Reconciliation note.** `docs/capsule-schema.md` (~43 KB, ~3 months old at the
time of writing) predates this spec and was NOT verified against it; where the
two disagree about container shapes, defaults, or verification behavior, this
document reflects the 2026-09-08 tree and the older document must be reconciled
or superseded — do not cite it silently.

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

CURRENT-behavior hazard: `allow_self_modify: true` by default combined with
`lycan <f.lyc>` run rewriting the binary in place with evolved weights/journal
(`bin/lycan.rs:214-218`) means **executing a capsule mutates it**; determinism
claims in this spec family scope to `compile`, not to run.

### 2.5 Effect detection (`capsule.rs:195-242`)

| Graph feature | Effect / capability | Cite |
|---|---|---|
| any `Print` node | `stdout` | `capsule.rs:203,222` |
| any `ReadLine` node | `stdin` | `capsule.rs:204,223` |
| `OpCode::Capability` first operand | string-table name resolved via `StringRef`, or via a `ConstStr` node feeding the Capability node → capability-registry effects (`capabilities::get`) | `capsule.rs:205-217`, name resolution `capsule.rs:227-242` |

## 3. Shape B — the `syntra author` compile bundle

`syntra author <spec.yaml> --out-dir` validates a `CapsuleSpec` YAML
(`capsule_spec.rs:11-39`, `:117-131` validate), emits deterministic Lycan
S-expression source (`emit_lycan_source`, `capsule_compiler.rs:78-141` —
including `($ ctx_i (!cap "runtime.inputGet" "…"))` at `:85-88`,
`(F option_name (idx) <nested conditional>)` at `:94-96`,
`($ selected_option (choice 0 1 …))` at `:130-133`), runs lexer → parser →
`GraphCompiler` (`compile_source`, `capsule_compiler.rs:166-173` — the report
said `~:240-250`; the tree wins), then `graph.to_bytes()` into
(`compile_to_dir`, `capsule_compiler.rs:13-76`):

| File | Content | Cite |
|---|---|---|
| `program.lyc` | compiled graph bytes | `capsule_compiler.rs:22-24` |
| `program.lycs` | generated S-expr source | `capsule_compiler.rs:26-28` |
| `learning.json` | §3.2 | `capsule_compiler.rs:32-34`, shape `:175-200` |
| `reward_spec.json` | §3.3 | `capsule_compiler.rs:36-39`, `:202-220` |
| `context_schema.json` | `{contexts: […]}` | `capsule_compiler.rs:41-44` |
| `hierarchical_spec.json` (optional) | emitted only when `hierarchicalOptions` is set | `capsule_compiler.rs:49-54` |
| `manifest.json` | bundle manifest: `{name, version, options, algorithm, rewardType, componentNames, sidecars}` — **no `format` key, no hashes** | `capsule_compiler.rs:56-67` |

**This bundle has NO `policy.json`, NO hashes, NO `inspect.json`, and the compile
path NEVER runs `verifier::verify`** (`compile_to_dir` has no verify call,
`capsule_compiler.rs:13-76`; CURRENT behavior, see §7). Consumers MUST
re-verify before execution.

### 3.1 `CapsuleSpec` YAML keys (`capsule_spec.rs:11-39`)

| Key | Type / constraint | Default | Cite |
|---|---|---|---|
| `name` | string, required | — | `capsule_spec.rs:12` |
| `version` | string | `""` | `capsule_spec.rs:13-14` |
| `options` | array, ≥ 2 entries; with `decisions`, must equal `decisions[0].options` | — | count check `:131-132`; doc `:15-17` |
| `contexts` | array | `[]` | `capsule_spec.rs:18-19` |
| `reward.type` | ∈ {`bernoulli`, `continuous`, `sparse_continuous`} (snake_case wire) | required | `capsule_spec.rs:55-69` |
| `reward.range` | `[f64;2]`; required iff type `continuous` | — | `capsule_spec.rs:57-58,139-140`; error `reward.range is required when reward.type is continuous` |
| `reward.components[]` | `{name, weight, normalize ∈ {minmax, budget}, range? (required for minmax), budget? (required for budget)}` | `[]` | `capsule_spec.rs:59-60,71-88` |
| `algorithm.type` | ∈ {`auto`, `thompson`, `ucb`, `epsilon_greedy`, `weighted`} (snake_case wire) | `auto` | `capsule_spec.rs:90-107` |
| `learning.min_exploration` | f64 | 0.02 | `capsule_spec.rs:110-115` |
| `decisions[]` | `{name, options (≥2), depends_on?}`; max 8 (`MAX_DECISIONS_PER_CAPSULE`, `:7`); unknown parent rejected `:253-268`; cycle rejected `:279-311` | absent = single decision over `options` | `capsule_spec.rs:25-28,43-50` |
| `hierarchicalOptions` (alias `hierarchical_options`) | mutually exclusive with `decisions`; flat `options` must equal the enumerated leaf names | — | `capsule_spec.rs:30-38`, leaf check `:194-212` |

### 3.2 `learning.json` schema (`build_learning_json`, `capsule_compiler.rs:175-200`)

| Key | Values | Notes |
|---|---|---|
| `algorithm` | `thompson` \| `ucb1` \| `epsilonGreedy` \| `simpleWeighted` | wire names (`ucb1`, `epsilonGreedy`, `simpleWeighted`) differ from YAML names (`ucb`, `epsilon_greedy`, `weighted`) — `capsule_compiler.rs:176-182`; YAML `auto` resolves then serializes as the resolved algorithm (`:30`, resolved name via `:177`) |
| `safety.minExploration` | number | from `learning.min_exploration` (`:186`) |
| `safety.selectionMode` | `weighted` (YAML `weighted`) \| `greedy` (thompson/ucb/auto) \| `epsilonGreedy` | mapping at `:187-192` |
| `epsilon` | 0.10 | emitted **only** when resolved algorithm is `epsilonGreedy` (`:195-198`) |
| `safety.selectionEpsilon` | 0.10 | emitted **only** when resolved algorithm is `epsilonGreedy` (`:195-198`) |

### 3.3 `reward_spec.json` schema (`build_reward_spec_json`, `capsule_compiler.rs:202-220`)

`{type, range, components: [{name, weight, normalize, range, budget}]}` — the
`type`/`normalize` enums are the YAML snake_case names passed through unchanged
(§3.1); `range`/`budget` serialize as `null` when absent.

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
is unconditionally true (not effect-derived). Post BUG-8 semantics: a missing
`max_execution_ms` loads as 30 000 (fail to a ceiling, never to unlimited — see
capability-abi §5.2), so the 6-key default is a stdout-only, 30s-budget policy;
`max_memory_bytes` remains unenforced.

### 4.2 Write paths do not verify

`save_graph_in_job` is an atomic write with **no verify** (`store.rs:355-359`);
install likewise does not verify (§7). Verification happens at decide time only
(`server/decide.rs:136-143`).

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
call sites below; display `verification failed ({N} errors):`
(`verifier.rs:13-21`, signature `:25`).

### 7.1 Fail-closed (verify runs; reject aborts/500)

| Entrypoint | Behavior on verify failure | Cite |
|---|---|---|
| `lycan <f.lyc>` (run) | print + `exit(1)` | `bin/lycan.rs:198-202` |
| `lycan decide` / `decide --input` | reject | `bin/lycan.rs:1162-1164`, `:1279-1281` |
| `run_binary_with_context` | reject | `bin/lycan.rs:821-824` |
| `lycan capsule verify` → `verify_capsule` | `exit(1)` | `bin/lycan.rs:752-757` |
| `lycan capsule create` | refuses invalid input graph | `capsule.rs:60-66` |
| `capsule_run` | verifies first | `bin/lycan.rs:787-791` |
| server `POST /decide` | decode Err or verify Err → HTTP 500 JSON | `server/decide.rs:141-143` |

### 7.2 Decode WITHOUT verify (fail-open exposure — CURRENT behavior)

| Entrypoint | Note | Cite |
|---|---|---|
| `lycan compile` | never verifies | `bin/lycan.rs:234-255` |
| dump / stats / explain / diff / learn-report / capsule inspect | inspection only — decode sites carry no adjacent verify call | `bin/lycan.rs:262,421,485,557,566,611,770` |
| store install | atomic write, no verify | `store.rs:303-334` |
| store save | `save_graph_in_job` atomic write, no verify | `store.rs:355-359` |
| evolution loop graph loads | operate on unverified graphs | `evolution_loop.rs:204,250` |
| server feedback | writes weights into a decode-only graph | `server/feedback.rs:283-286` |
| admin node listing | | `server/admin.rs:86-88` |
| inspect endpoints | | `server/inspect.rs:383`, `server/routes.rs:488,635` |

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
   `program.lyc hash mismatch (actual: {actual_hash})`,
   `inspect.json hash mismatch (actual: {actual_hash})`,
   `graph uses {effect} but policy does not allow it` — pinning the CURRENT
   substring semantics (a vector SHOULD also record that a whitespace-reformatted
   policy changes the substring outcome, and that hash checks are skipped when
   the manifest omits the `*_sha256` keys).
6. **No signing.** Vectors MUST NOT require any signature field; presence of a
   manifest signature today MUST be ignored (future-work marker, §6).
7. **Author bundle shape.** `syntra author` output contains exactly the §3 file
   set (`program.lyc`, `program.lycs`, `learning.json`, `reward_spec.json`,
   `context_schema.json`, `manifest.json`, optional `hierarchical_spec.json`)
   and contains no `policy.json`, no hash field, no `inspect.json`, and no
   `format` key; `learning.json`/`reward_spec.json`/`context_schema.json` match
   §3.2-3.3 key sets exactly, including the epsilon-only-under-epsilonGreedy rule.
8. **Author bundle is unverified.** A graph that fails the verifier can still be
   produced by `syntra author`; execution of it via a fail-closed entrypoint MUST
   be rejected.
9. **Store layout.** install produces `current.lyc` + store manifest with keys
   `{name, tenant, job, hash, installed}` and `hash` == sha256 of installed
   bytes; `save` never verifies; `/decide` maps decode/verify failure to HTTP 500.
10. **Version literals.** `lycan-capsule-v1` in `.lycap` manifests; the graph
    inside every container carries magic `LYCN` + version byte 5 (§5); store
    manifests carry no `format` key.
