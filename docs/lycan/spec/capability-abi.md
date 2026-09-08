# Lycan Capability ABI and Evolution Gate Chain

Status: Draft v0.1 — describes implementation as of 2026-09-08

Normative description of the capability registry (names, purity, effects, resource caps), the
three-layer enforcement model (central effect gate, opcode gates, path/network sandbox), the
`policy.json` provenance and cross-check rules, and the evolution proposal gate chain in its
normative execution order.

RFC 2119 keywords. Citation conventions as in `learning-semantics.md`: `[verified path:line]`,
`[GAP]` (unenforced / fail-open), `[CURRENT-BEHAVIOR]`, `[UNVERIFIED]`. Appendix A reconciles this
document with the 2026-09-08 fact report, a substantial part of which was fabricated for this
section and is listed there entry by entry.

## 1. Capability registry

The static registry is the sole capability ABI surface: `REGISTRY: &[CapabilitySpec]` with exactly
**35 entries** [verified `src/capabilities/registry.rs:69-564`]. `CapabilitySpec` fields:
`name, version, package, summary, inputs, output, purity, deterministic, effects, cost, failure,
safety` [verified `:3-17`]. `Purity ∈ {Pure, ReadOnlyEffect, Effectful}` [verified `:19-24`].
Effect vocabulary in use: `[]`, `["file_read"]`, `["file_write"]`, `["network"]`,
`["publish"]` — plus the effect-gate names `file_read` / `file_write` / `network` (§3). Lookup by
exact name; there is no version negotiation at dispatch [verified `:566-568`].

### 1.1 Registry table (all 35)

"cap" = hard resource cap enforced by the kernel, NOT policy-tunable [verified `kernels.rs:441-442`
`MAX_BYTES = 1 MiB`, `MAX_SQL_ROWS = 1000`; timeouts `kernels.rs:115, :135`; per-entry `cost`
fields]. "det" = `deterministic`. Purity abbreviations: P / RO / E.

| # | name | ver | pkg | purity | det | effects | caps |
|---|---|---|---|---|---|---|---|
| 1 | `runtime.capabilities` | 1.0.0 | runtime | P | ✔ | — | — |
| 2 | `runtime.input` | 1.0.0 | runtime | P | ✔ | — | — |
| 3 | `runtime.inputGet` | 1.0.0 | runtime | P | ✔ | — | — |
| 4 | `runtime.publish` | 0.1.0 | runtime | E | ✔ | `publish` | journal buffer only (§4.4) |
| 5 | `file.exists` | 0.1.0 | io | RO | ✔ | `file_read` | — (sandboxed stat) |
| 6 | `file.readText` | 0.1.0 | io | RO | ✔ | `file_read` | 1 MiB [verified `kernels.rs:85-87`] |
| 7 | `file.writeText` | 0.1.0 | io | E | ✘ | `file_write` | 1 MiB contents [verified `kernels.rs:97-99`] |
| 8 | `http.get` | 0.1.0 | net | RO | ✘ | `network` | 10 s timeout, 1 MiB response [verified `kernels.rs:114-118, 555-562`] |
| 9 | `http.post` | 0.1.0 | net | E | ✘ | `network` | 10 s timeout, 1 MiB request + 1 MiB response [verified `kernels.rs:126-139`] |
| 10 | `json.get` | 0.1.0 | data | P | ✔ | — | — |
| 11 | `json.has` | 0.1.0 | data | P | ✔ | — | — |
| 12 | `json.len` | 0.1.0 | data | P | ✔ | — | — |
| 13 | `sql.sqliteQuery` | 0.1.0 | data | RO | ✔ | `file_read` | 1000 rows, read-only SQL [verified `kernels.rs:641-644`] |
| 14 | `stats.mean` | 0.1.0 | math | P | ✔ | — | — |
| 15 | `stats.stdDev` | 0.1.0 | math | P | ✔ | — | — |
| 16 | `stats.min` | 0.1.0 | math | P | ✔ | — | — |
| 17 | `stats.max` | 0.1.0 | math | P | ✔ | — | — |
| 18 | `stats.percentile` | 0.1.0 | math | P | ✔ | — | — |
| 19 | `series.ewmaForecast` | 0.1.0 | math | P | ✔ | — | — |
| 20 | `ops.autoScaleRecommend` | 0.1.0 | ops | P | ✔ | — | — |
| 21 | `comb.apTuples` | 0.1.0 | comb | P | ✔ | — | proof-lab input bounds |
| 22 | `comb.isGoodColoring` | 0.1.0 | comb | P | ✔ | — | — |
| 23 | `comb.badAp` | 0.1.0 | comb | P | ✔ | — | — |
| 24 | `comb.goodColoringWitness` | 0.1.0 | comb | P | ✔ | — | `node_limit`-bounded search |
| 25 | `comb.hasThreeDistinct4ApColoring` | 0.1.0 | comb | P | ✔ | — | — |
| 26 | `comb.badThreeDistinct4Ap` | 0.1.0 | comb | P | ✔ | — | — |
| 27 | `comb.threeDistinct4ApWitness` | 0.1.0 | comb | P | ✔ | — | `node_limit`-bounded DFS |
| 28 | `comb.threeDistinct4ApSatWitness` | 0.1.0 | comb | P | ✔ | — | bounded DPLL; not a formal proof |
| 29 | `nav.norm3` | 1.0.0 | nav | P | ✔ | — | — |
| 30 | `nav.distance3` | 1.0.0 | nav | P | ✔ | — | — |
| 31 | `nav.dot3` | 1.0.0 | nav | P | ✔ | — | — |
| 32 | `nav.radialVelocity` | 1.0.0 | nav | P | ✔ | — | — |
| 33 | `nav.ephemerisState` | 1.0.0 | nav | RO | ✔ | `file_read` | sandboxed file read |
| 34 | `nav.horizonsVectors` | 1.0.0 | nav | RO | ✘ | `network` | 10 s timeout, 1 MiB response; NASA/JPL endpoint [verified `capabilities/horizons.rs:28-32`] |
| 35 | `astro.lambertSolve` | 0.1.0 | astro | P | ✔ | — | bounded iterative solve; returns `status` instead of panicking |

Registry metadata effects and enforcement are separate concerns: the effect gate reads
`registry.effects` (§3), but the `publish` effect name is not gateable (§3.2) and
`file_write`-vs-`file_read` classification inside the path sandbox is per-call-site, not per-
registry entry (§4.1).

The fact report's registry enumeration (`io.httpGet`, `io.readFile`, `io.writeFile`, `data.query`,
`strategic.*`, `compute.*`, `memory.*`, `math.speck*`, `keccak*`, "runtime.* random") matches no
code; its claimed `self_modify`/`publish` effects on `io.writeFile`/`io.httpPost` and "1 MiB at
:287-303" style anchors are likewise fabricated (Appendix A-0).

## 2. Enforcement model — three layers

Layering, with the dependency direction policy JSON → `capsule::load_policy` (CLI) /
`store::load_execution_policy_in_job` + `parse_execution_policy` (server) → `context::ExecutionPolicy`
→ gates [verified `src/capsule.rs:371-397`; `src/store.rs:376-380, 857-878`]:

1. **Central effect gate** (capability plane): every dispatch through
   `capabilities::execute` checks the spec's effects against the active policy before any kernel
   runs [verified `src/capabilities/kernels.rs:13-29`].
2. **Opcode gates** (graph plane): `Print` requires `allow_stdout`, `ReadLine` requires
   `allow_stdin`; these are opcodes, not registry capabilities, and bypass the effect plane
   entirely [verified `src/graph_executor/exec.rs:736-745, :754-763`; legacy interpreter mirror
   `src/interpreter.rs:510-516, :529-535`].
3. **Sandbox** (resource plane): `resolve_sandbox_path` (§4.1) for the five file-touching caps and
   `check_network_sandbox` (§4.2) for the three network caps, called by the kernels themselves;
   the boolean "sandbox active" return additionally disables HTTP redirects (§4.2).

`ExecutionContext.policy == None` (no policy) ⇒ ALL THREE layers pass everything through: the
effect gate is skipped, `resolve_sandbox_path` returns the requested path unchanged,
`check_network_sandbox` returns `Ok(false)` (unrestricted, redirects allowed) [verified
`kernels.rs:13`; `sandbox.rs:29-33, :120-127`]. This is the documented "no policy = unrestricted"
developer mode (see `execution-policy.md`), reached for raw `.lyc` runs via `lycan` CLI, and NOT
reachable on the server (§5).

## 3. Central effect gate

[verified `src/capabilities/kernels.rs:11-29`] For each `effect` in the spec:

| effect | deny condition |
|---|---|
| `file_read` | `!policy.allow_file_read` |
| `file_write` | `!policy.allow_file_write` |
| `network` | `!policy.allow_network` |
| any other effect (incl. `publish`) | never denied (`_ => false`) |

Denial returns `Err("capability={name} effect={effect} denied by policy")` before the kernel runs.

### 3.1 Effects are only declared per registry entry — no allowlist of capabilities

There is no per-capability allowlist: a capability is permitted iff its declared effects are all
permitted by the policy flags [verified `kernels.rs:16-20`]. Adding a new gated effect requires
editing both `registry.rs` and the gate's `match` (`kernels.rs:18-23`) [CURRENT-BEHAVIOR].

### 3.2 `publish` is not centrally enforced [GAP]

`runtime.publish` declares `effects: ["publish"]` [verified `registry.rs:121`], but `"publish"`
falls into the gate's `_ => false` arm: the effect name appears in NO policy flag and NO other
check [verified grep over `src/` — `"publish"` effect string occurs only in `registry.rs:121`].
The fact report's variants of this claim ("deny_private_networks only logs for publish",
"`publish.allowedHosts` REQUIRED else denied" at `hierarchical_spec.rs:52-62`) are fabricated — no
`allowedHosts` concept exists anywhere, and `hierarchical_spec.rs` is not a file
(Appendix A-6). See §4.4 for why the practical exposure of the GAP is limited.

## 4. Sandbox rules

### 4.1 Path sandbox — `resolve_sandbox_path` [verified `src/capabilities/sandbox.rs:4-111`]

Callers pass a per-call effect string (`"file.readText"`, `"file.writeText"`, `"file.exists"`,
`"sql.sqliteQuery"`, `"nav.ephemerisState"`), which doubles as the read/write classifier.

Root selection precedence [verified `:9-34`]:

| Condition | Root |
|---|---|
| policy with relative `file_root` | `working_dir.join(file_root)` (root joined only if a `working_dir` exists, else the relative path itself) |
| policy with absolute `file_root` | `file_root` (policy root WINS over working dir) |
| policy, no `file_root`, with `working_dir` | `working_dir` |
| policy, no `file_root`, no `working_dir` | **deny**: `"no file_root or working_dir configured"` |
| no policy / no context | unrestricted (path passed through) |

Request rejection (sandbox active): leading `/` or `\` → absolute-path denial `[:37-39]`;
**any occurrence of the substring `..`** → traversal denial `[:40-42]` (note: this also rejects
legitimate filenames containing `..`).

Containment [verified `:44-110`]:

* Read-like (`effect.contains("read")` or `file.exists` / `nav.ephemerisState`): if the joined
  target exists → `canonicalize` and require `starts_with(canonical root)` (symlink escape
  defeated: an in-root symlink pointing outside is rejected: `"path escapes sandbox"`); if it does
  not exist → return the joined path unchecked (so `file.exists` can answer `false`)
  `[:47-63]`. `[GAP]` For reads of a nonexistent path the parent-directory symlink case is not
  canonicalized (write path below covers it).
* Write: existing target → canonicalize the target itself (follows symlinks) and require
  containment; nonexistent target → parent MUST exist, parent canonicalized + contained, final path
  re-formed as `canonical_parent.join(filename)` `[:64-110]`.

### 4.2 Network sandbox — `check_network_sandbox` [verified `src/capabilities/sandbox.rs:115-191`]

Host extraction: text after `://` up to next `/`; IPv6 bracket syntax `[::1]:8080 → ::1`; empty
host → error `[:129-141]`.

**Allow-list is mandatory**: `allowed_hosts` empty (with policy active) ⇒ EVERY outbound HTTP is
denied (`"no allowed_hosts configured — outbound HTTP denied"`) [verified `:143-158`]. Matching:

| Pattern | Matches | Does not match |
|---|---|---|
| `example.com` | exactly `example.com` (exact only — `evil-example.com`, `example.com.evil.io` denied) | — |
| `*.example.com` | apex `example.com` (via `host == &h[2..]`) and any `*.suffix` (via `ends_with(".example.com")`) — so `a.example.com`, `a.b.example.com` | `evil-example.com`, `notexample.com` [verified `:145-152`] |
| any `*.x.y.z` wildcard | MAY also suffix-match a **literal IP** host: `allowed_hosts=["*.1.1"]` admits `10.0.0.1` (subject to the private-IP rules below) | the fact-report rule "literal IPs never match wildcards" is NOT implemented — REFUTED (Appendix A-4) |

`deny_private_networks` (default **true**; policy key `deny_private_networks`) [verified
`:161-188, 193-210`]:

* Host string `localhost` (case-insensitive) → denied. **Loopback is denied**, not allowed — the
  fact report's "loopback allowed" is REFUTED (Appendix A-4).
* Literal IPv4 denied iff: loopback, unspecified (0.0.0.0), broadcast, `10.0.0.0/8`,
  `172.16.0.0/12`, `192.168.0.0/16`, `169.254.0.0/16` (hence metadata IPs `169.254.169.254` /
  `169.254.170.1` are covered by the /16, no separate metadata rule exists), or multicast.
* Literal IPv6 denied iff: loopback, unspecified, unique-local `fc00::/7`, link-local `fe80::/10`,
  or multicast.
* **DNS-resolved domains**: hostnames are resolved via `to_socket_addrs((host, 80))`; ANY resolved
  private address → denied `[:177-187]`. `[GAP]` If resolution itself FAILS, the check is skipped
  and the request proceeds (fail-open on resolution error; port 80 probe only).
* TOCTOU `[GAP]`: containment is checked at check time; the actual `ureq` connection re-resolves.
* Redirect escape is closed only for sandboxed calls: when the network sandbox is ACTIVE the HTTP
  agents set `redirects(0)` [verified `kernels.rs:108-113, :129-133`; `horizons.rs:28-32`]; an
  unsandboxed program follows redirects unrestricted.

### 4.3 Resource caps

`MAX_BYTES = 1 MiB`, `MAX_SQL_ROWS = 1000` [verified `kernels.rs:441-442`]; enforced on:
`file.readText` stat-size precheck, `file.writeText` contents, `http.post` request body, HTTP
response reads (1 MiB+1 streamed then rejected), SQL row truncation (silently stops at 1000)
[verified `kernels.rs:85-99, 126-128, 555-562, 641-644`]. HTTP timeouts: 10 s on `http.get`/
`http.post` [verified `kernels.rs:115, :135`] and the Horizons kernel. Caps are compile-time
constants — NOT policy-tunable [CURRENT-BEHAVIOR; the registry `cost` strings mirror them].

### 4.4 `runtime.publish` semantics [verified `kernels.rs:46-62`; `src/context.rs:12-20`]

Writes `(name, scalar)` into the per-decision `PublishedBuffer`
(`Rc<RefCell<BTreeMap<String, Value>>>`, sorted keys for deterministic order). Values:
Int/Float (non-finite → error)/Str/Bool/Null; arrays rejected in v1; when no buffer is wired (CLI,
tests), the call is a silent no-op so the same program runs everywhere. The server snapshots the
buffer into the decision log [verified `src/server/decide.rs:534`]. No filesystem or network
surface exists for publish — hence the §3.2 GAP is integrity-of-journal only, not egress.

## 5. Policy provenance, defaults, and the effect cross-check

### 5.1 Materialization paths

`ExecutionPolicy` [verified `src/context.rs:22-51`]:

| Source | Notes |
|---|---|
| `capsule::load_policy(dir)` (CLI) | reads `<dir>/policy.json`; defaults §5.2; errors if file missing/invalid (CLI exits 1 at `bin/lycan.rs:795-798`) |
| `store::load_execution_policy_in_job` → `parse_execution_policy` (server) | byte-identical defaults to `load_policy` [verified `src/store.rs:857-878`] |
| Deny-all literals | on load failure: server `/decide` [verified `src/server/decide.rs:145-156`], evolve endpoint [verified `src/server/inspect.rs:321-329`], CLI `evolve` on `.lycap` [verified `bin/lycan.rs:1077-1088`] — all construct `{all allow_*=false, file_root: None, allowed_hosts: [], deny_private_networks: true}` |
| `ExecutionPolicy::default()` | PERMISSIVE (every allow true; `deny_private_networks: true`) — used by TESTS only, never by CLI/server paths [verified `src/context.rs:38-51`; sole non-test references are `capabilities/mod.rs:341` test helper]. Report claim CONFIRMED |

Fail-closed summary [CURRENT-BEHAVIOR]: server never runs with `policy: None` — a capsule with no
or corrupt `policy.json` executes deny-all; raw `.lyc` CLI execution runs with `policy: None`
(unrestricted developer mode; `capsule run` loads or exits).

### 5.2 Field defaults at load (both parsers) [verified `capsule.rs:371-397`; `store.rs:857-878`]

| JSON key | Default when absent |
|---|---|
| `allow_stdout` | **true** |
| `allow_stdin` | false |
| `allow_file_read` | false |
| `allow_file_write` | false |
| `allow_network` | false |
| `file_root` | `null` → with policy active, file caps DENY unless a working_dir is set (§4.1) |
| `allowed_hosts` | `[]` → all outbound HTTP denied (§4.2) |
| `deny_private_networks` | true |

### 5.3 Generated policies and the effect cross-check

`capsule::generate_policy` (capsule create) emits an 8-key policy — `allow_stdout` …
`allow_network` = "graph declares that capability" (presence of `stdout`/`stdin`/`file_read`/
`file_write`/`network` in the created capsule's capability list), `allow_self_modify: true`,
`max_execution_ms: 30000`, `max_memory_bytes: 268435456` [verified `src/capsule.rs:25-50,
342-368`]. Server install writes a minimal default `{"allow_stdout": true, "allow_stdin": false,
"allow_file_read": false, "allow_file_write": false, "allow_network": false,
"allow_self_modify": true}` only when no policy.json exists [verified `src/store.rs:316-326`].

Cross-check rules (normative):

1. A capability whose declared effect is denied still **fails at call time**, even if the capsule
   manifest/graph claims it [verified `kernels.rs:16-27`]. Generated policy flags ⊇ effects used by
   the graph at creation time, but later graph mutation (evolution) does NOT re-derive policy —
   effect flags are a runtime ceiling, not a static mirror [CURRENT-BEHAVIOR].
2. `allow_network: true` with empty `allowed_hosts` is self-inconsistent-by-construction ⇒ every
   call still denies (§4.2 mandatory allow-list). Generated policies OMIT `file_root`,
   `allowed_hosts`, `deny_private_networks` [verified `capsule.rs:354-367`], so a network-enabled
   generated capsule also needs a hand-edited `allowed_hosts` to egress, and a file-enabled one
   needs `file_root` or a working dir (§4.1) [GAP-adjacent footgun].
3. `allow_self_modify`, `max_execution_ms`, `max_memory_bytes` appear ONLY in the `Policy` struct,
   the generator, and templates; NO enforcement site reads them [verified grep: zero references in
   `exec.rs`/`kernels.rs`/`context.rs`/`sandbox.rs`] [GAP] [CURRENT-BEHAVIOR] — the "self-modify"
   surface (evolution writing `program.lyc`) is governed by the evolution gate chain (§6), not by
   this flag.
4. `stdout`/`stdin` are NOT registry effects: they gate only via §2 layer 2, so a pure-Print graph
   run with `allow_stdout: false` fails with `"capability=print effect=stdout denied by policy"`
   [verified `exec.rs:742`].

## 6. Evolution proposal gate chain (normative order)

Single-proposal application `apply_proposal_with_policy` [verified `src/evolve.rs:330-670`], driven
per iteration by `evolution_loop` with `eval_runs = 5` (hard-coded) on a `/tmp` candidate copy
[verified `src/evolution_loop.rs:340-346`]. Gates run in this order; the first failure rejects with
the listed reason and NOTHING is promoted:

| # | Gate | Exact rule | Anchor |
|---|---|---|---|
| 1 | Target exists + is adaptive | `nodes.get(target_strategy)` else `"strategy node #N does not exist"`; op MUST be `Strategy \| AdaptiveChoice` | `:342-346` |
| 2 | Compile | lex → parse → `GraphCompiler::compile`, errors `candidate compile/parse error` | `:356-363` |
| 3 | Purity | reject if any candidate node op ∈ **`{Print, ReadLine, Adapt, Spawn, Prune}`** (`"...must be pure"`) — the report's deny-set `{Print,ReadLine,StoreVar,Loop,Capability}` is REFUTED | `:366-371` |
| 4 | Fresh baseline | run ORIGINAL `eval_runs`×, mean of OK runs → `winner_ms`; only successful runs count | `:379-401` |
| 5 | Candidate executes | any execution error → `"candidate execution error"` | `:403-427` |
| 6 | Consistency | candidate's own output string MUST be **exactly equal across the `eval_runs` repetitions** (string equality; no tolerance, no ≥-input-count condition) | `:432-440` |
| 7 | Non-degenerate | `null`/empty output → reject | `:442-449` |
| 8 | Correctness | `expected_output` is **REQUIRED** (`"expected_output is required — cannot verify correctness without it"`); then exact string match, else numeric `|candidate − expected| < 1e-6` (BARE 1e-6; the report's `1e-6·(1+|o|)` on ≥3 inputs does not exist) | `:454-487` |
| 9 | Graft (fair-start weights) | clone original; copy candidate nodes at `id_offset`; NodeRefs shifted, strings re-interned, `state_slot: None`, `activation_count: 0`, stats slots zero-extended. New option weight = **mean of the target's existing selector weights** (`0.5` if none); WithinTolerance epsilon slot popped and re-pushed (preserved); selector weights renormalized by their sum (epsilon excluded). No `w_target/(k+1)` formulas, no sibling inheritance, no `1e-9` drift bound — all REFUTED | `:494-587` |
| 10 | Speed gate | interleaved trials in one loop: ORIGINAL then GRAFTED per trial; per-program **min-of-trials**; reject iff `grafted_ms > 1.1·orig_gate_ms + 0.05` (ms); gate skipped when baseline or grafted runs unavailable. Report's inequality direction (`prop_ms·1.1+0.05 > orig_ms`) REFUTED — the ×1.1 applies to the ORIGINAL's min | `:597-653` |
| 11 | Journal + save | graph journal `MutationKind::NodeSpawned`, reason = interned proposal name (no `"evolve:"` prefix — the report's `reason="evolve:accepted"` exists nowhere); candidate saved by plain `fs::write` (not atomic — promotion is where atomicity lives) | `:656-670` |

Loop-level gates [verified `src/evolution_loop.rs`]: `no_baseline` (`before_score ≤ 0` or
`≥ f64::MAX`, `:413-414`); `no_candidate_score` (`after_score ≤ 0`, `:441-442`); `min_improvement` —
`improvement = (before − after)/before`, enforced only when configured `> 0` (`:468-474`; CLI/server
default **0.05** [verified `bin/lycan.rs:1035-1037, :1050`; `server/inspect.rs:302`]). Promotion =
`fs::rename` candidate→live `program.lyc`, cross-filesystem fallback `fs::write`, then **sha256
read-back verification**; mismatch logs `"CRITICAL: promotion hash mismatch"` [verified
`:533-547`; the report's "evolution_loop.rs:275-313" anchor points at brief-generation]. Journal
events are JSONL `EvolutionStarted` / `BriefGenerated` / `ProposalReceived` / `ProposalRejected` /
`ProposalAccepted` / `EvolutionCompleted` with reason `"accepted: X% improvement (…)"` [verified
`:254, :276, :334, :309, :551, :566`].

## 7. Conformance requirements

**Registry (R)**

* C-R1 — `REGISTRY.len() == 35`; every entry resolves via `get(name)`; every declared effect ∈
  `{file_read, file_write, network, publish}`; `runtime.capabilities` output lists all registry
  names.
* C-R2 — Effects exactly per §1.1: `file.writeText` MUST NOT declare `self_modify`;
  `http.post` MUST NOT declare `publish`; `runtime.publish` MUST declare `publish`.

**Effect gate (E)**

* C-E1 — For each gated capability and each `allow_* = false`: dispatch fails with
  `capability=<name> effect=<effect> denied by policy` and the kernel performs no I/O.
* C-E2 — With `policy: None` (raw `.lyc` CLI): all capabilities run (developer mode).
* C-E3 — CURRENT-BEHAVIOR pin: `runtime.publish` succeeds under a deny-everything policy
  (effect unenforced, §3.2 [GAP]); with no publish buffer it is a silent no-op.
* C-E4 — `Print` under `allow_stdout: false` → denied with the exact §5.3(4) message; `ReadLine`
  under `allow_stdin: false` → `capability=readline effect=stdin denied by policy`; with no policy
  both succeed.

**Path sandbox (P)**

* C-P1 — Absolute (`/`, `\`) and any `..`-containing request rejected when policy active.
* C-P2 — Symlink inside root → outside: read AND write both fail `path escapes sandbox`; symlinked
  PARENT dir for a NEW file likewise; new file with missing parent → `parent directory does not
  exist`.
* C-P3 — `file.exists` on a nonexistent in-root path returns `false` (no containment error), but
  `file.exists` on a path escaping via symlinked parent MAY pass containment unchecked [GAP pin:
  read-like nonexistent path is returned uncanonicalized, §4.1].
* C-P4 — Relative `file_root` resolves against `working_dir`; absolute `file_root` wins;
  policy with neither root nor working dir denies all file caps with the exact §4.1 message.

**Network sandbox (N)**

* C-N1 — `allowed_hosts: []` + `allow_network: true` ⇒ every call denied
  (`no allowed_hosts configured`); no-policy ⇒ unrestricted and redirects followed.
* C-N2 — Exact match: `evil-example.com` denied under `["example.com"]`. Wildcard:
  `["*.example.com"]` admits apex `example.com` and `deep.sub.example.com`, denies
  `notexample.com`; IPv6 bracket hosts parse; empty host → parse error.
* C-N3 — Under `deny_private_networks: true`, denied hosts must include: `localhost`, `127.0.0.1`,
  `0.0.0.0`, `10.x.x.x`, `172.16-31.x.x`, `192.168.x.x`, `169.254.169.254`, `169.254.170.1`,
  `224.0.0.1`, `[::1]`, `[fc00::1]`, `[fe80::1]` — even when present in `allowed_hosts`.
* C-N4 — A domain resolving to any private address (port-80 `to_socket_addrs` probe) → denied with
  the resolves-to-private-IP message. CURRENT-BEHAVIOR pin [GAP]: a domain whose resolution FAILS
  is allowed through.
* C-N5 — Sandboxed `http.get` MUST NOT follow redirects (`redirects(0)`): a 302 target is never
  fetched; unsandboxed follows normally.

**Caps (C)**

* C-C1 — 1 MiB enforcement on read-size precheck, write contents, POST body, streamed response;
  10 s request timeouts; `sql.sqliteQuery` truncates at 1000 rows and rejects non-read-only SQL.

**Policy provenance (O)**

* C-O1 — Loader defaults per §5.2 byte-for-byte (only `allow_stdout` defaults true).
* C-O2 — Server with missing/corrupt `policy.json`: `/decide` logs deny-all and every effectful
  capability plus Print/ReadLine fail; same literals at all three fallback sites (§5.1).
* C-O3 — Generated policy.json omits `file_root`/`allowed_hosts`/`deny_private_networks`;
  pin CURRENT-BEHAVIOR that `allow_self_modify`/`max_execution_ms`/`max_memory_bytes` have no
  observable runtime effect [GAP].

**Gate chain (G)** — each exercised against one unmodified baseline program:

* C-G1 — Purity: a candidate containing each of `Print`, `ReadLine`, `Adapt`, `Spawn`, `Prune` is
  rejected with `"...must be pure"`; `StoreVar`/`Loop`/`Capability` opcodes are NOT purity-denied
  (pin against regression toward the refuted deny-set).
* C-G2 — Ordering: a candidate that is fast but has NO `expected_output` is rejected with the
  required-field message BEFORE any speed benchmark outcome matters.
* C-G3 — Consistency: nondeterministic candidate (outputs differ across the 5 runs) →
  `"inconsistent results across runs"` even if one output equals `expected_output`.
* C-G4 — Correctness: `|c−e| = 1e-6` exactly → REJECTED (rule is strict `< 1e-6`).
* C-G5 — Speed: grafted min-ms exactly `= 1.1·orig_min + 0.05` → ACCEPTED (strict `>`); gate does
  not run when baseline unavailable.
* C-G6 — Graft fairness: post-graft selector weights sum to 1 (epsilon slot excluded), new option
  weight equals the pre-graft mean, WithinTolerance epsilon slot value unchanged.
* C-G7 — Loop: `min_improvement` default 0.05; improvement measured on full-program timing
  `(before−after)/before`; promotion MUST verify sha256 of the promoted bytes (fault-injection:
  byte-corrupt candidate pre-rename ⇒ `CRITICAL: promotion hash mismatch` and no silent success).
* C-G8 — Journal: accepted graft writes `NodeSpawned` with the interned proposal name; accepted
  loop iteration writes JSONL `ProposalAccepted` with an `"accepted: N% improvement"` reason.

## 8. Appendix A — Fact-report deviation table (this section)

| # | Report claim (§6–§7 of the fact report) | Status → actual |
|---|---|---|
| A-0 | Capability table: `runtime.inputGet/now/setState (random)`, `memory.recall/remember`, 8 `strategic.*`, 9 `compute.*` + keccak, `io.httpGet/httpPost/readFile/writeFile`, `data.query`, `math.speck*`; effects `io.writeFile→[file_write,self_modify]`, `io.httpPost→[network,publish]`; runtime caps "1 MiB at :287-303" etc. | **REFUTED (fabricated)** — none of those names exist anywhere in `src/` (grep-verified). Real registry: 35 entries per §1.1 at `registry.rs:69-564`; the count 35 is the only surviving fact. `self_modify`/`publish` effects on writeFile/httpPost do not exist; publish is `runtime.publish`'s effect |
| A-1 | Central gate "only file_read/file_write/network; publish NOT central-checked" (`kernels.rs:19-26`) | **CONFIRMED** (concept; exact anchors `kernels.rs:13-29`). "deny_private_networks only logs for publish" → **REFUTED** (no publish code path touches it) |
| A-2 | Print/ReadLine gates (`exec.rs:546-553`) | **CONFIRMED** concept, anchors corrected to `exec.rs:736-763` (+ `interpreter.rs:510-535` legacy) |
| A-3 | Path root = `policy.file_root` else cwd else `store_root` ("policy root WINS") | **CORRECTED** — `file_root` (relative→joined with working_dir) else `working_dir` else DENY; there is no `store_root` fallback (`sandbox.rs:9-34`) |
| A-4 | Empty allow-list deny-all; `*.` wildcard; literal IPs never match wildcards; deny_private default true; **loopback allowed**; ranges incl. metadata `/16` | Empty-list deny-all, wildcard form, deny-default-true, and the range list (incl. 169.254/16 covering metadata): **CONFIRMED**. "Loopback allowed": **REFUTED** — loopback/`localhost` are denied. "Literal IPs never match wildcards": **REFUTED** — not implemented; wildcard suffixes CAN match IP literals (§4.2) |
| A-5 | `policy.json` create defaults incl. `allow_self_modify:true, max_execution_ms 30000, max_memory_bytes 268435456`; server load stdout-true/others-false | **CONFIRMED** values (`capsule.rs:37-49, 342-368; store.rs:857-878`) — with the **GAP** that `allow_self_modify`/`max_execution_ms`/`max_memory_bytes` are never enforced (§5.3.3), which the report presented as if effective |
| A-6 | "`publish.allowedHosts` REQUIRED else denied; publish NOT effect-checked (`hierarchical_spec.rs:52-62`)" | allowedHosts gate: **REFUTED** — file `hierarchical_spec.rs` does not exist and `allowedHosts` appears nowhere; "publish NOT effect-checked": **CONFIRMED** (§3.2 [GAP]), though the report's exploitation scenario ("httpPost publishes to allow-listed host without allowedHosts") is impossible: publish writes only to the per-decision journal buffer (§4.4) |
| A-7 | Gate chain order compile→purity→consistency `1e-6·(1+\|o\|)` on ≥3 inputs→expected_output required→graft `w_target/(k+1)` + sibling sharing + drift ≤1e-9→speed `prop·1.1+0.05 > orig`→loop gates→sha256 atomic promote; journal `evolve:accepted` | **CORRECTED/REFUTED per §6**: expected_output IS required (CONFIRMED); tolerance is bare `1e-6` vs expected only, and consistency is exact cross-run string equality; graft is mean-weight fair-start + renorm + epsilon-slot preservation (no k+1 formulas, no drift bound); speed gate inequality `grafted > 1.1·orig + 0.05` on min-of-mins interleaved (constants confirmed, side corrected); promote mechanism rename + write-fallback + sha256 read-back CONFIRMED at `:533-547`; `evolve:accepted` reason string **REFUTED** (events per §6); purity deny-set corrected to `{Print, ReadLine, Adapt, Spawn, Prune}` |
| A-8 | "graph weight-mirror skipped when memory owns weights" | **REFUTED** — mirrored update is skipped iff the decision carries a meta-bandit `candidateId` (`learning-semantics.md` §5.6) |
