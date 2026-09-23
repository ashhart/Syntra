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
**20 entries** [verified `src/capabilities/registry.rs`] (the 15 `comb.*`, `nav.*` and `astro.*` entries moved to Lycan Lab on 2026-09-23). `CapabilitySpec` fields:
`name, version, package, summary, inputs, output, purity, deterministic, effects, cost, failure,
safety` [verified `:3-17`]. `Purity ∈ {Pure, ReadOnlyEffect, Effectful}` [verified `:19-24`].
Effect vocabulary in use: `[]`, `["file_read"]`, `["file_write"]`, `["network"]`,
`["publish"]` — plus the effect-gate names `file_read` / `file_write` / `network` (§3). Lookup by
exact name; there is no version negotiation at dispatch [verified `:566-568`].

### 1.1 Registry table (all 20)

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
`"sql.sqliteQuery"`), which doubles as the read/write classifier.

Root selection [verified `resolve_sandbox_path`, `src/capabilities/sandbox.rs:9-42`]:

| Condition | Root |
|---|---|
| policy, `working_dir` set, relative `file_root` | `working_dir.join(file_root)`; `file_root` is re-validated (`validate_file_root`, `src/context.rs:79-102`: relative, no `..`, no root or prefix component) and the canonical root must stay inside the canonical `working_dir`, so a symlinked component cannot lift it out |
| policy, `working_dir` set, no `file_root` | `working_dir` |
| policy, absolute or escaping `file_root` | **deny** (`must be a relative path` / `must not contain '..'`); policy parsing already refuses such a file (§5.2) |
| policy, no `working_dir` | **deny**: `"no working_dir configured for the file sandbox"` |
| no policy / no context | unrestricted (path passed through) |

The server sets `working_dir` to the capsule's `data/` directory (`<capsule>/data`, created on
first use when a file effect is allowed) [verified `src/server/decide.rs`], never the capsule
directory itself: capsule code cannot read or rewrite `policy.json`, learned state,
`current.lyc` or the audit and decision logs. `capsule run` (CLI) uses the `.lycap` directory.

Request rejection (sandbox active): leading `/` or `\` → absolute-path denial `[:37-39]`;
**any occurrence of the substring `..`** → traversal denial `[:40-42]` (note: this also rejects
legitimate filenames containing `..`).

Containment [verified `:44-110`]:

* Read-like (`effect.contains("read")` or `file.exists`): if the joined
  target exists → `canonicalize` and require `starts_with(canonical root)` (symlink escape
  defeated: an in-root symlink pointing outside is rejected: `"path escapes sandbox"`); if it does
  not exist → return the joined path unchecked (so `file.exists` can answer `false`)
  `[:47-63]`. `[GAP]` For reads of a nonexistent path the parent-directory symlink case is not
  canonicalized (write path below covers it).
* Write: existing target → canonicalize the target itself (follows symlinks) and require
  containment; nonexistent target → parent MUST exist, parent canonicalized + contained, final path
  re-formed as `canonical_parent.join(filename)` `[:64-110]`.

### 4.2 Network sandbox — `check_network_sandbox` and `NetworkGuard` [verified `src/capabilities/sandbox.rs:131-347`]

Two layers enforce the same rules. `check_network_sandbox` (`:225-288`) runs before any I/O and
gives readable denials; `NetworkGuard::resolve` (`:156-184`) runs inside the HTTP client's DNS
resolver on the `host:port` the client is about to connect to, and the client connects only to
the addresses it returns. The second layer is authoritative: a URL that the pre-check and the
client would parse differently, or a DNS answer that changes between check and connect
(rebinding), cannot reach a denied host.

Scheme: `https` only; `http` only when the policy sets `allow_insecure_http: true`
(`"plain http:// is denied by policy"`); anything else is denied.

Host extraction (pre-check, `url_host` `:204-220`): the authority after `://` ends at the first
`/`, `?`, `#` or `\`; an authority containing `@` (userinfo) is denied; IPv6 bracket syntax
`[::1]:8080 → ::1`; a trailing `.` is dropped; the host is lowercased; empty host → error.

**Allow-list is mandatory**: `allowed_hosts` empty (with policy active) ⇒ EVERY outbound HTTP is
denied (`"no allowed_hosts configured — outbound HTTP denied"`). Matching (`host_allowed`
`:189-199`):

| Pattern | Matches | Does not match |
|---|---|---|
| `example.com` | exactly `example.com` | `evil-example.com`, `example.com.evil.io` |
| `*.example.com` | apex `example.com` and any subdomain on a label boundary (`a.example.com`, `a.b.example.com`) | `evil-example.com`, `notexample.com` |
| any `*.x.y.z` wildcard | MAY also suffix-match a **literal IP** host on a label boundary: `allowed_hosts=["*.1.1"]` admits `10.0.0.1` (then subject to the private-address rules) | — |

`deny_private_networks` (default **true**; only the operator admin key may set it to `false` over
the API, §5.2) denies, for literal IPs in the pre-check and for every resolved address in the
resolver (`is_private_ip` `:294-329`, `is_private_v4` `:331-347`):

* the host names `localhost` and `*.localhost`;
* IPv4: `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10` (shared/CGNAT), `127.0.0.0/8`,
  `169.254.0.0/16` (link-local; covers the `169.254.169.254` / `169.254.170.1` metadata
  addresses), `172.16.0.0/12`, `192.0.0.0/24`, `192.0.2.0/24`, `192.88.99.0/24`,
  `192.168.0.0/16`, `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`, and `224.0.0.0/3`
  (multicast, reserved, broadcast);
* IPv6: `::`, `::1`, `fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`, `2001::/32` (Teredo),
  `2001:db8::/32`, `64:ff9b:1::/48`, `100::/64`, and any address embedding an IPv4 address that
  is denied above: IPv4-mapped `::ffff:a.b.c.d`, IPv4-compatible `::a.b.c.d`, NAT64
  `64:ff9b::/96`, and 6to4 `2002::/16`.

Resolution failures deny the request (fail closed). If ANY resolved address is denied, the whole
request is denied.

Redirect escape is closed for sandboxed calls: the guard's agent sets `redirects(0)`
[verified `NetworkGuard::agent` `:148-154`; call sites `kernels.rs` `http.get`/`http.post`]; an unsandboxed program follows redirects unrestricted.

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

`ExecutionPolicy` [verified `src/context.rs:24-74`]:

| Source | Notes |
|---|---|
| `capsule::load_policy(dir)` (CLI) | reads `<dir>/policy.json`; defaults §5.2; errors if file missing/invalid (CLI exits 1 at `bin/lycan.rs:795-798`) |
| `store::load_execution_policy_in_job` → `parse_execution_policy` (server) | both this and `load_policy` call `ExecutionPolicy::from_policy_json` [verified `src/context.rs:129-215`], so defaults and validation are identical (§5.2) |
| Deny-all | `ExecutionPolicy::deny_all()` [verified `src/context.rs`] — all `allow_*=false`, `file_root: None`, `allowed_hosts: []`, `deny_private_networks: true`, `max_execution_ms: Some(30000)` — used ONLY where a policy could not be read and execution must still proceed: server `/decide` load-failure, evolve endpoint |
| Evolution sandbox | `ExecutionPolicy::evolve_sandbox()` [verified `src/context.rs`] — `deny_all()` but `allow_stdout: true`; used for CLI `evolve` default (raw `.lyc`), `.lycap` policy load-failure in `evolve`, and `capsule apply-proposal` (unconditionally — the command takes no `--policy`). Stdout stays ON by design: the gate must RUN the host program to measure its baseline (host programs report via `!p`/Print), and stdout is not a registry effect — §5.3(4). A strict-stdout variant made the baseline unmeasurable (`no_baseline` rejection of every proposal against a printing host); BUG-9 sandboxes *side effects*, and that is what it denies |

Fail-closed summary [CURRENT-BEHAVIOR, post BUG-9]: server never runs with `policy: None` — a capsule with no
or corrupt `policy.json` executes deny-all; evolution verification runs candidates under an
explicit policy on every CLI path (evolution sandbox unless `--policy` given); raw `.lyc` *plain
execution* (`lycan f.lyc`, `lycan decide`) remains developer-mode `policy: None`; `capsule run`
loads or exits.

### 5.2 Validation and field defaults [verified `ExecutionPolicy::from_policy_value`, `src/context.rs:136-215`]

Parsing is strict and fail-closed. A document is rejected when it is not a JSON object, contains
a key outside `POLICY_KEYS` (`src/context.rs:60-73`), has a value of the wrong type, has an
absolute or `..`-containing `file_root` (§4.1), has an `allowed_hosts` entry that is not a bare
host name or `*.suffix` (no scheme, port, path or whitespace; `validate_allowed_host`
`:107-122`), or has `max_execution_ms` outside 1..=60 000 (`MAX_EXECUTION_MS_LIMIT`). Over the
API, `PUT .../policy` answers 400 with the reason, answers 403 when a non-operator token sets
`deny_private_networks: false`, and journals every accepted write as a `policy_updated` audit
event carrying the SHA-256 of the stored document and the principal [verified
`src/server/routes.rs` `validate_policy_put`, `audit_policy_update`]. A stored file that fails
validation makes `/decide` run deny-all (§5.1).

| JSON key | Default when absent |
|---|---|
| `allow_stdout` | **true** |
| `allow_stdin` | false |
| `allow_file_read` | false |
| `allow_file_write` | false |
| `allow_network` | false |
| `allow_insecure_http` | false → sandboxed requests are https-only (§4.2) |
| `file_root` | `null` → the root is the working directory (§4.1) |
| `allowed_hosts` | `[]` → all outbound HTTP denied (§4.2) |
| `deny_private_networks` | true |
| `max_execution_ms` | absent ⇒ **30 000** (`DEFAULT_EXECUTION_MS`) — missing key fails to a ceiling, not to unlimited (2026-09-08, BUG-8) |

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
3. `max_execution_ms` IS enforced (2026-09-08, BUG-8): the store/capsule loaders read it into
   `ExecutionPolicy.max_execution_ms`, and the graph executor aborts with
   `execution exceeded max_execution_ms (budget N ms)` — wall-clock, checked every 64th node
   evaluation, surfaced as audited `execution_denied` + HTTP 500 on the server. `max_memory_bytes`
   and `allow_self_modify` remain UNENFORCED [GAP] [CURRENT-BEHAVIOR] — the "self-modify"
   surface (evolution writing `program.lyc`) is governed by the evolution gate chain (§6), not by
   this flag.
4. `stdout`/`stdin` are NOT registry effects: they gate only via §2 layer 2, so a pure-Print graph
   run with `allow_stdout: false` fails with `"capability=print effect=stdout denied by policy"`
   [verified `exec.rs:742`].

## 6. Evolution proposal gate chain (normative order)

Single-proposal application `apply_proposal_with_policy` [verified `src/evolve.rs:330-704`], driven
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
| 10 | Grafted program must RUN | every grafted trial that errors counts against promotion: `grafted_runs < eval_runs` ⇒ `"grafted program failed to execute in N of 5 runs — candidate is syntactically valid but breaks the full program"` (a graft that only misbehaves in full-program context cannot slip through the timing comparison; 2026-09-08) | `:633-648` |
| 11 | Speed gate | interleaved trials in one loop: ORIGINAL then GRAFTED per trial; per-program **min-of-trials**; reject iff `grafted_ms > 1.1·orig_gate_ms + 0.05` (ms); gate skipped when the ORIGINAL's baseline is unavailable. Report's inequality direction (`prop_ms·1.1+0.05 > orig_ms`) REFUTED — the ×1.1 applies to the ORIGINAL's min | `:656-672` |
| 12 | Journal + save | graph journal `MutationKind::NodeSpawned`, reason = interned proposal name (no `"evolve:"` prefix — the report's `reason="evolve:accepted"` exists nowhere); candidate saved by plain `fs::write` (not atomic — promotion is where atomicity lives) | `:674-689` |

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

* C-R1 — `REGISTRY.len() == 20`; every entry resolves via `get(name)`; every declared effect ∈
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
* C-P4 — Relative `file_root` resolves against `working_dir`; an absolute or `..`-containing
  `file_root` is refused by policy parsing and, if built in code, denied at call time; a
  `file_root` whose symlinked component resolves outside `working_dir` is denied; a policy with
  no working dir denies all file caps with the exact §4.1 message. On the server, file caps are
  rooted in `<capsule>/data`, and `policy.json` in the capsule directory is unreachable.

**Network sandbox (N)**

* C-N1 — `allowed_hosts: []` + `allow_network: true` ⇒ every call denied
  (`no allowed_hosts configured`); no-policy ⇒ unrestricted and redirects followed.
* C-N2 — Exact match: `evil-example.com` denied under `["example.com"]`. Wildcard:
  `["*.example.com"]` admits apex `example.com` and `deep.sub.example.com`, denies
  `notexample.com`; IPv6 bracket hosts parse; empty host → parse error.
* C-N3 — Under `deny_private_networks: true`, denied hosts must include: `localhost`, `127.0.0.1`,
  `0.0.0.0`, `10.x.x.x`, `100.64.0.1`, `172.16-31.x.x`, `192.168.x.x`, `169.254.169.254`,
  `169.254.170.1`, `224.0.0.1`, `[::1]`, `[fc00::1]`, `[fe80::1]`, `[::ffff:169.254.169.254]`,
  `[64:ff9b::a9fe:a9fe]`, `[2002:0a00:0001::1]` — even when present in `allowed_hosts`.
* C-N4 — A domain resolving to any denied address is refused inside the resolver with the
  resolves-to-private-IP message, and the connection uses only the addresses the resolver
  returned. A domain whose resolution fails is denied.
* C-N5 — Sandboxed `http.get` MUST NOT follow redirects (`redirects(0)`): a 302 target is never
  fetched; unsandboxed follows normally.
* C-N6 — Under a policy, `http://` is denied unless `allow_insecure_http: true`; `https://` to an
  allow-listed public host proceeds.
* C-N7 — Parser-confusion URLs are denied before any request under `["*.example.com"]`:
  `https://evil.com?.example.com`, `https://evil.com#.example.com`,
  `https://evil.com\.example.com/`, `https://example.com@evil.com/`; and the resolver denies a
  host outside the allow-list even if the pre-check was bypassed.

**Caps (C)**

* C-C1 — 1 MiB enforcement on read-size precheck, write contents, POST body, streamed response;
  10 s request timeouts; `sql.sqliteQuery` truncates at 1000 rows and rejects non-read-only SQL.

**Policy provenance (O)**
* C-O1 — Loader defaults per §5.2 byte-for-byte (only `allow_stdout` defaults true;
  absent `max_execution_ms` loads as 30 000).
* C-O2 — Server with missing/corrupt `policy.json`: `/decide` logs deny-all and every effectful
  capability plus Print/ReadLine fail; all load-failure sites use `ExecutionPolicy::deny_all()` (§5.1).
* C-O3 — Generated policy.json omits `file_root`/`allowed_hosts`/`deny_private_networks`;
  pin that `max_execution_ms` HAS observable runtime effect (§5.3.3: a `while`-loop capsule with
  budget 50 must fail with the timeout error, audited as `execution_denied`), while
  `allow_self_modify`/`max_memory_bytes` have NO observable runtime effect [GAP pin].
* C-O4 — Evolution verification is sandboxed by default (BUG-9): `lycan evolve f.lyc
  --proposal p.json` with a candidate whose only flaw is a `file.writeText` side effect (purity
  ops clean) MUST leave the filesystem untouched (denial string `effect=file_write denied by
  policy` on stderr, REJECTED) and `capsule apply-proposal` likewise; the sandbox allows stdout,
  so a host program that reports via `!p` still gets a measurable baseline (§5.1 evolution
  sandbox — the `no_baseline` reason must NOT appear against a printing host).

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
* C-G9 — Run-failure: a graft whose FULL program errors on every trial (e.g. under a stricter
  policy than the candidate alone sees) is REJECTED with `"grafted program failed to execute"`
  and the binary stays byte-identical — `grafted_ms = f64::MAX` must never print as `0.000ms`
  next to `ACCEPTED` (§6 gate 10; the hole the stdout-off sandbox regression exposed).
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
| A-5 | `policy.json` create defaults incl. `allow_self_modify:true, max_execution_ms 30000, max_memory_bytes 268435456`; server load stdout-true/others-false | **CONFIRMED** values (`capsule.rs:37-49, 342-368; store.rs:857-878`) — the stated **GAP** that `max_execution_ms` was never enforced was FIXED 2026-09-08 (BUG-8, §5.3.3); `allow_self_modify`/`max_memory_bytes` remain unenforced |
| A-7 | Gate chain order compile→purity→consistency `1e-6·(1+\|o\|)` on ≥3 inputs→expected_output required→graft `w_target/(k+1)` + sibling sharing + drift ≤1e-9→speed `prop·1.1+0.05 > orig`→loop gates→sha256 atomic promote; journal `evolve:accepted` | **CORRECTED/REFUTED per §6**: expected_output IS required (CONFIRMED); tolerance is bare `1e-6` vs expected only, and consistency is exact cross-run string equality; graft is mean-weight fair-start + renorm + epsilon-slot preservation (no k+1 formulas, no drift bound); speed gate inequality `grafted > 1.1·orig + 0.05` on min-of-mins interleaved (constants confirmed, side corrected); promote mechanism rename + write-fallback + sha256 read-back CONFIRMED at `:533-547`; `evolve:accepted` reason string **REFUTED** (events per §6); purity deny-set corrected to `{Print, ReadLine, Adapt, Spawn, Prune}` |
| A-8 | "graph weight-mirror skipped when memory owns weights" | **REFUTED** — mirrored update is skipped iff the decision carries a meta-bandit `candidateId` (`learning-semantics.md` §5.6) |
