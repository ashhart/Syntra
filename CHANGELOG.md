# Changelog

All notable changes to Syntra. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the platform follows
[semver](https://semver.org/) once it reaches 1.0.

## [Unreleased] — v2: a decision service you can prove (2026-09-23)

Syntra v2 replaces the v1 learning core and server. v1 stores are refused
(their logs have no propensities); start a new store and recreate each
capsule with `PUT .../spec`.

### Added

- **Decision core** (`src/decision`): one contextual learner (hashed
  features, normalized online least squares), SquareCB or epsilon-greedy
  exploration with a floor, baseline-explore and frozen modes, and every
  decision's full probability distribution and seed, so any draw replays.
- **Event store** (`syntra.db`, SQLite WAL): decisions with propensities,
  rewards with idempotency keys (`first` or `sum` aggregation), model
  snapshots and the audit trail. A write-behind log keeps disk off the
  request path; `"durable": true` waits for the commit. Restarts rebuild
  each model exactly from its snapshot plus the rewards after it.
- **Server** on hyper/tokio: v2 routes under `/v1`, eventId idempotency,
  per-request actions, default rewards after a reward wait, periodic
  snapshots off the request path, graceful drain on SIGTERM.
- **Local evaluation**: SDKs decide in-process against a published model
  (`GET .../model?snapshot=true`, a versioned decide section and a
  `modelTag`), upload decisions (`decisions:batch`) that the server
  verifies by replay, and rewards (`rewards:batch`). Rust
  (`syntra::client::LocalDecider`) and Python (`sdk/python`, PyO3) SDKs.
- **Off-policy evaluation**: `syntra evaluate` (JSONL or the store,
  read-only) and `POST .../evaluate` with DM, IPS, SNIPS and cross-fitted
  DR, paired lift intervals and gates; `POST .../promote` applies a spec
  change only when its gates pass.
- **Azure Personalizer-compatible API**: rank, reward, activate (deferred
  activation) and service configuration, at `/personalizer/v1.0/...`.
  Deferred events and their held rewards survive a graceful restart.
- **Migration and adoption**: `syntra import dsjson` reads Vowpal Wabbit /
  Personalizer DSJSON logs into a capsule (optionally learning from them);
  `syntra.llm.ModelRouter` routes LLM calls through a `LocalDecider`;
  `--specs <dir>` applies capsule specs from YAML or JSON files at startup;
  `syntra demo` runs a server with simulated traffic to watch it learn.
- **Evaluation of candidates as they would serve**: `candidate:` policies
  score a spec change with its exploration, next to the greedy `spec:`.
  Uploads whose model is retired are stored `unverified` and left out of
  evaluation (`rowsUnverified`).
- **Admin console** at `/admin`: capsules, decisions, spec, evaluate and
  promote, audit and tokens, under a hash-pinned Content-Security-Policy.
- **OpenTelemetry tracing**: with an OTLP endpoint in the standard
  `OTEL_*` variables, a span per request over OTLP/HTTP (JSON, optional
  gzip), continuing the caller's W3C `traceparent`, carrying the decision
  id, action and probability; queries and endpoint credentials are
  redacted, and tracing costs about half a microsecond per request.
- **Releases**: a tag builds binaries, Python wheels and a container
  image, with an SPDX SBOM and build-provenance attestations, into a draft
  release. Helm chart on the production image.
- OpenAPI document with a drift test; fuzz targets for specs and model
  snapshots; a learning benchmark on simulated bandits with a known
  optimum (`examples/learning_bench.rs`, with `--learning-rate`); 480+
  tests including crash recovery under load.
- Guides for v2: quickstart, concepts, operating, deployment, the API,
  and moving off Azure Personalizer (`docs/README.md`), and an LLM
  routing example on simulated models (`examples/llm-routing`).
- **In-process TypeScript decisions**: the decision core compiled to
  WebAssembly (`sdk/typescript/wasm`, the same Rust source) behind the
  TypeScript SDK's `LocalDecider`: 3.4 to 3.9 µs per decision in Node,
  replay-verified uploads, tested in Node, Bun and Chromium.
- **Benchmarks against Vowpal Wabbit and the Open Bandit Pipeline**
  (`benchmarks/`): online learning, off-policy evaluation accuracy and
  interval coverage, and decision latency, with every setting, loss and
  caveat.
- `GET .../decisions?order=newest` pages the log back in time; the admin
  console uses it.
- `syntra --version`, and `--help` on every subcommand.
- The Docker image has a health check (`syntra health`) and declares its
  store volume; its build context is an allowlist (`Cargo.toml`,
  `Cargo.lock`, `src/`).
- `sdk/python/tests/test_litellm.py` runs `ModelRouter` with LiteLLM's own
  functions (offline, through `mock_response`); CI runs it against the
  latest LiteLLM.

### Changed

- `/metrics` needs an admin credential unless `--metrics-public`.
- `syntra serve` rejects unknown options; the default store is
  `./syntra-store`; `SYNTRA_ADMIN_KEY` is read (as is `LYCAN_ADMIN_KEY`).
- `syntra health` asks the running server; `syntra stop` only signals a
  syntra process.
- Deleting a capsule keeps its audit trail and records `capsule_deleted`;
  deleting a job or tenant records it for each of their capsules.
- One store has one server: `syntra serve` holds an OS lock on
  `server.lock`, a second server on the same store exits with an error, and
  `syntra restore` treats the lock as a live root.
- Evaluation needs a tenant-admin credential, and at most two run at once.
- Any global admin credential (the operator key or an `admin` token) may
  open a capsule to private networks; the docs said only the operator key.
- Helm chart 0.3.0 refuses `replicaCount` above 1 and autoscaling (the
  HPA template and the unused PDB values are gone).
- `syntra doctor` prints readable lines and a summary; `--json` prints one
  object per problem and then the summary object.
- The admin console opens without a key on a dev-mode server, lists
  decisions newest first, and scrolls to a decision's detail.
- Capsule stats include writes acknowledged but still queued.
- The Python SDK builds on pyo3 0.29 (0.28 carried RUSTSEC-2026-0176 and
  RUSTSEC-2026-0177, in APIs the SDK did not call); wheels leave out
  bytecode.
- The README quickstart works when pasted as one block, and every
  documented `syntra serve` takes the key from `SYNTRA_ADMIN_KEY`, which
  keeps it out of `ps`.
- The release workflow builds Intel macOS on `macos-15-intel`
  (`macos-13` runners are retired).
- Clippy is clean across every target; the Lycan guide describes the
  commands this CLI has.
- DM is a point estimate: its fixed-model intervals covered the true value
  in 14% of simulated datasets, so the report leaves them null and gates on
  them are refused (IPS, SNIPS and DR intervals covered 94 to 95%).
- Floats are parsed exactly (serde_json `float_roundtrip`): logged
  probabilities read back bit for bit, where about one in ten came back one
  unit in the last place off.

### Fixed

- Uploads made with an action excluded were refused for good when their
  model had been retired, instead of stored unverified (the check indexed
  the pmf by action; it is aligned with the eligible set). The console's
  decision detail showed the wrong probabilities for such decisions.
- The Rust and Python SDKs cut upload batches at 1000 items with no byte
  limit, so a queue past the server's 4 MiB body limit never drained; they
  now also bound bytes and report an item no request can carry.
- The Rust and Python SDKs retried forever a batch the server refuses as a
  whole (a capsule with a feature program, a deleted capsule); such
  batches are reported as rejected, and connecting to a capsule with a
  feature program fails with the reason.
- `@syntra/client` 0.2.0 (`sdk/typescript`) is rewritten for the v2 API:
  decide, reward, spec, model, logs, evaluate, promote, uploads and
  tokens, with typed errors, retries only for requests that are safe to
  repeat, and a `LocalDecider` interface for the coming WebAssembly core.

### Removed

- The v1 learners, report/contexts/memory routes, the v1 Python client
  (`syntra-client`), the extra clients, the sidecar and Terraform. The
  science demos, proof lab and self-evolving capsules moved to Lycan Lab.
- The v1 docs, docs site, examples, scripts, Grafana dashboards, the
  try-instance deployment and the v1 planning notes (`todo.md`, `bugs.md`,
  `evals/`).

## [Unreleased] — Security hardening and repository cleanup (2026-09-23)

### Security

- **Tenant sandbox escape closed.** A tenant-scoped admin token could set a
  capsule's `file_root` to any absolute path (for example the store root)
  and, with file capabilities enabled, read every tenant's data including
  `tokens.json`, or overwrite other tenants' files. Policies are now parsed
  strictly (`ExecutionPolicy::from_policy_json`): `file_root` must be
  relative with no `..`, unknown fields and wrong types are rejected, hosts
  must be bare names, and `max_execution_ms` is capped at 60 000. The server
  roots file capabilities in the capsule's `data/` directory, so capsule
  code can no longer read or rewrite its own policy, learned state, program
  or logs. The sandbox re-validates the root at call time and refuses a
  symlinked root that resolves outside the working directory.
- **Network sandbox hardened.** Sandboxed HTTP is https-only unless the
  policy sets `allow_insecure_http`. The allow-list and private-address
  checks now also run inside the HTTP client's DNS resolver, on the host it
  actually connects to, and the connection uses only the addresses that
  passed. This closes three bypasses: DNS rebinding between check and
  connect, URLs such as `https://evil.com?.example.com` that matched a
  `*.example.com` allow-list entry while the request went to `evil.com`, and
  resolution failures that previously let the request through. The private
  ranges now include 100.64.0.0/10, 0.0.0.0/8, benchmarking, documentation
  and reserved ranges, and IPv6 forms that embed an IPv4 address
  (`::ffff:169.254.169.254`, NAT64, 6to4, Teredo).
- **Only the operator admin key may set `deny_private_networks: false`**;
  tenant tokens get 403. Every accepted policy write is journalled as a
  `policy_updated` audit event with the document's SHA-256 and the principal.
- **`--dev-mode` binds loopback only.** An unauthenticated server on a
  non-loopback address now refuses to start unless
  `--dev-mode-allow-remote` is passed; the demo container, try-instance and
  Helm `devMode` pass it explicitly. `syntra serve` and `lycan serve` now
  share one implementation.

### Fixed

- **Lost learning updates under concurrent traffic.** `/decide` without
  `learn=true` skipped the capsule lock but still rewrote `memory.json`, so
  concurrent decides overwrote `/feedback` updates. In a regression test
  with four concurrent decide loops, 82 of 120 acknowledged feedback updates
  were lost. Every decide now takes the capsule lock.

### Removed

- Content written to steer AI summaries of the repository: the README
  section addressed to AI agents, the `demos/` index and its "summary rule
  for automated readers", `evals/repo-read/` (which scored summaries on
  whether they mentioned specific demos) and its CI job. `CONTEXT.md` is now
  a short architecture note with the known limitations of the current
  learning stack.

### Tests

- `tests/security_regressions.rs`: sandbox escape through policy, operator-
  only private networks, strict and audited policy writes, the lost-update
  race (fails on the old code), and the dev-mode bind refusal.
- Unit tests for strict policy parsing, file-root rules and the network
  guard (private ranges, wildcard label boundaries, parser-confusion URLs,
  resolver re-checks).
- The containment matrix is now 24/24: R10 (plain http to an allow-listed
  host) is enforced instead of printed as a known gap, and R3b checks that a
  policy cannot widen `file_root`.

## [Unreleased] — Real-time realignment: zero-clone executor, flagship control demo (2026-09-08)

### Changed

- **Project headline re-aligned to real-time verifiable control.** The
  compiled graph executor's per-node `GraphNode::clone()` — 98% of all heap
  allocations (dhat-profiled: 7.12M of 7.26M blocks on the chaos-control
  capsule) — is gone: dispatch copies the opcode, operands are fetched per
  use (`Operand`/`ImmValue` are now `Copy`), and `GraphFn` payloads are
  `Rc`-shared so function-value clones are allocation-free. Measured effect
  (release build, single thread, Apple M5 Max; machine-relative):
  chaos-control 7,247,987 → 11,936 allocations per decision and
  198.8 → 119.1 ms p50; adaptive-router 11,139 → 97 and 295.5 → 176.5 µs;
  anomaly-router 314 → 243 and 8.5 → 8.2 µs p50. Determinism hashes are
  byte-identical before/after; 167 executor-facing tests pass unchanged.

### Added

- **`examples/rt_baseline.rs`** — the real-time measurement instrument:
  per-capsule latency distribution (p50/p90/p99/p99.9/max), allocations per
  decision with a size histogram, and a 32-rep seeded determinism check
  (byte-identical result+stdout hash). `RT_DHAT=1` mode dumps a dhat
  allocation profile for one decision per capsule.
- **`examples/rt_control_loop.rs` +
  `examples/rt-control/rendezvous_burn_sequencer.lycs` — the flagship
  closed-loop control demo.** Plant state in every tick, in-graph delayed
  feedback (`(feedback choice reward)`) into an AdaptiveChoice burn policy,
  a Guard node deopting to a certified safe-hold, per-tick latency
  (20.4 µs p50) and allocation reporting, mission outcome, and a two-run
  byte-identical determinism proof. Docks in 401 ticks; CI-pinned in
  `tests/demo_smoke.rs::rt_control_loop_proves_realtime_claims`.

### Fixed

- **Fail-closed feedback targeting (BUG-5).** `Node::Feedback` resolving an
  `Ident` target used to fall back to a bare `LoadVar` reference when the name
  was unbound — or was bound to a non-choice value — and the executor silently
  dropped the credit. The compiler now refuses such programs with a named
  error; `$`-bound `choice`/`strategy` targets compile unchanged.
  Regression: `tests/bugfix_regressions.rs::feedback_target_must_resolve_to_choice_node`;
  spec: `docs/lycan/spec/learning-semantics.md` §4.2.

## [Unreleased] — Second-OS validation, TLS gateway eval, structural equality (2026-09-08)

### Added

- **The FULL test suite ran on a second operating system for the first
  time: a `rust:1-bookworm` Docker container (Linux aarch64/glibc),**
  with an anonymous volume shadowing `target/` so the host's macOS
  binaries stay untouched. Everything this project had ever claimed
  had only ever executed on macOS; this run is what that check turned
  up (see Fixed). An attempt to publish the repo to GitHub for hosted
  CI happened and was rolled back the same day at the owner's
  decision; nothing depends on it being up.
- **History hygiene, permanently.** The pre-publication sweep found one
  leak class — machine-absolute paths in `docs/evaluations/`
  reproduction commands — removed from the work tree and rewritten out
  of EVERY historical blob (`git filter-repo` replace-text, verified by
  grepping all commits). Identity is the GitHub noreply address; zero
  company/work markers; deploy templates carry placeholders only. The
  scrub stands regardless of publication plans.
- **`scripts/demo-tls-gateway.py` — the appliance behind a real TLS
  reverse proxy.** Generated self-signed CA + `ThreadingHTTPServer`
  wrapped in `ssl` (stdlib terminator, TLS ≥ 1.2), backend identical to
  the governor's. Positive half: 24 decide+feedback round-trips over
  TLSv1.3 through the proxy, header forwarding proven (no key → 401,
  Bearer → 200), decisions/audits routes reachable over TLS. Negative
  half (the actual point): a second unrelated CA is REJECTED
  (`SSLCertVerificationError`), a hostname mismatch is REJECTED, plain
  HTTP to the proxy port dies at the handshake (TLS-only exposure).
  8/8 checks, ~1 s, honest scope line printed (demo-grade terminator;
  production runs nginx/envoy/stunnel). Wired into
  `tests/demo_smoke.rs` — CI asserts a `TLSv1.` handshake, both
  rejections, and the scope note.

### Fixed (found by running green-macos tests in a clean environment)

- **`fbang_is_rejected_at_parse` was validating against stale `/tmp`
  detritus.** It spawned the fixed-path `.lycs` file BEFORE writing it
  and passed only where a previous session had left the file behind; a
  clean container turned the expected `F!` rejection into `error
  reading ... No such file` — same exit class, wrong reason, message
  assert failed. Writes first now; verified 10/10 on host with the
  stale file moved away and in the fresh container.
- **The TLS gateway CI marker asserted a truncated detail fragment**
  (`"Hostname mismatch"` vs the demo's 70-char detail `"...Hostname
  m"`). Retargeted to the deterministic check label; the demo's own
  assertion still inspects the full untruncated exception.

### Changed (language-visible)

- **Structural equality for arrays — DECIDED** (`value-model.md` §5
  register closed). `==` gains a recursive `Array` arm on BOTH backends:
  `(== (A 1) (A 1))` and `(== (A) (A))` flip `false → true`. Decided
  against coercion (`(== 1 1.0)` stays `false`, incl. at depth:
  `(== (A 1) (A 1.0))` false), against `Fn` identity (closure register
  stays open), with IEEE NaN kept at depth. Fail-closed recursion cap:
  >64 nesting levels raises `structural equality depth limit (64)
  exceeded` (arrays are immutable but `W`-loops nest arbitrarily deep;
  unbounded recursion is a stack overflow) — boundary pinned: 65
  compares, 66 raises, identical text both backends.
- **String-ordering divergence closed**: the compiled executor's
  `gval_cmp` had no `Str` arm, so `(< "a" "b")` ran in source and
  errored once compiled. Byte-order lexicographic arm added (matches
  `!len`'s byte semantics); the per-backend divergence pin in
  `scoping-and-execution.md` §1.1/§10 is retired.

### Tests

- `tests/semantics_parity.rs` +3 (10 total): the 13-case equality
  matrix, the 65/66 depth boundary with byte-identical error text, and
  string-ordering parity incl. mixed-type error agreement.

## [Unreleased] — Frontier evals: agent governor, gated self-evolution, containment matrix (2026-09-08)

### Added

- **`scripts/demo-agent-governor.py` — the runtime as an agent control
  plane.** 6 simulated agents across 2 tenants issue 2500 tool-call
  decisions (bash / web_fetch / file_read / file_write / sql / exec)
  through one compiled guardrail capsule, against a server with
  `file_root` jail, host allow-list, and private-network denial. Budget
  enforcement is a *structural rail* — a cost table the gateway applies
  after the learner's pick, and the learner is never fed back on rail
  blocks, so it cannot learn to trade budget for reward: 12 rail trips
  across the run, every one returned block, zero budget overshoots, and
  the rail outvoted the learner's more permissive pick 8 times. Context
  memory (`agent × tool × risk`) learns differentiated trust: after a
  rogue-agent `exec` storm its held-at-gate rate is 0.97 (strict block
  57/80) while the honest coder's `exec` allow rate stays at 0.87 —
  same tool, same policy, opposite outcomes learned from outcomes. Full
  stop/restart proves persistence (rogue held 5/5 on a fresh process
  against the same store); decision #1429 is then reconstructed from the
  persisted store alone (`decisions` + `audits` + `memory`), naming the
  context key, the winning weights, and its audit line. Cross-tenant
  capsule report 403. Every claim is a scored check: 10/10, with a
  receipt block (tenant-alpha decision-log sha256 over 1891 entries).
  Honest limits printed, not hidden: sandbox is in-process, budget
  accounting is gateway-side.
- **`scripts/demo-self-evolve.sh` — gated self-modification, then a
  compromised-proposer gauntlet.** Deterministic closed loop: traffic →
  win rate plateaus at 0.3200 → `lycan capsule improve` brief (no API key)
  → scripted proposer emits candidate programs → `capsule apply-proposal`
  gate (verify + benchmark + `--min-improvement`) → adopt → 0.7500 → 1.0000.
  Lineage recorded in `<program>.lyc.evolution.jsonl` with parent/child
  sha256. Then five attacks: contract-breaker rejected by the verifier;
  a *valid* proposal that grafts `file.writeText` to `/etc/passwd`; a
  structurally valid but measurably worse churn arm; a `--dry-run` proven
  non-mutating by checksum; and a proposer lying about its claimed output.
  15/15 checks, 13 s.
- **`scripts/demo-containment.py` — red-team containment eval.** A
  "compromised agent" capsule wired to every IO capability (read, write,
  traversal, symlink, absolute path, metadata-IP SSRF, RFC1918 SSRF,
  exfil POST, host/env/exec probes) plus an 8 GiB allocation and an
  unbounded loop, evaluated against 13 expected-denial vectors. 22/23:
  file jail (all three paths, verified absent on disk), symlink escape,
  SSRF with the allow-list and private-network guards, live policy flips
  taking effect immediately, compute-budget abort
  (`execution exceeded max_execution_ms=2000`), and attack-surface
  inventory (no env/exec/syscall capability exists to invoke). The single
  failure is printed as `KNOWN GAP`, not hidden: `allowed_hosts` matches
  host only — http/https is never consulted — so a plaintext endpoint on
  an allow-listed host is reachable. Every real denial lands in `/audits`
  as `execution_denied` *before* the client sees a 500.
- `tests/demo_smoke.rs::frontier_demos_prove_their_claims` runs all three
  in CI and asserts their headline claims.

### Fixed (found by these evals; full writeups in `bugs.md` BUG-7/8/9)

- **BUG-7 (high) — containment denials were not audited.** A sandbox
  denial, policy denial, or unknown-capability failure returned a bare
  HTTP 500 from `do_decide` *before* any audit append, so the audit
  trail — the product's core claim — recorded nothing about the most
  interesting events in the system. Executor errors now append an
  `execution_denied` audit event (`error`, `contextKey`, `graphHash`)
  before the 500; a graph failing verification on load appends
  `graph_verify_failed`.
- **BUG-8 (high) — `max_execution_ms` was advertised but never enforced.**
  Nothing outside `capsule.rs` read the field: a capsule with
  `max_execution_ms: 50` ran a ~1.2 s `while` loop to completion and
  returned 200. `ExecutionPolicy` gains `max_execution_ms`, the store and
  capsule loaders read it (absent field → `DEFAULT_EXECUTION_MS` =
  30 000 ms, a ceiling rather than unlimited), and the graph executor
  enforces it as a wall-clock deadline checked every 64 node evaluations
  (`execution exceeded max_execution_ms (budget N ms)`, audited via
  BUG-7's hook). `max_memory_bytes` remains **documented unenforced** —
  the demos print it as a gap rather than claiming containment.
- **BUG-9 (critical) — evolution verification ran candidates with the
  caller's privileges.** `lycan evolve` without `--policy` passed
  `policy: None` into `apply_proposal_with_policy`, so a *proposer's*
  graph executed unrestricted during benchmarking; purity checks only
  covered effectful opcodes, and capability calls are not opcodes. The
  self-evolution demo caught this while asserting the backdoor would be
  caught — its probe file existed at 281 bytes — which is the argument
  for writing assertions instead of narration. Raw `.lyc` evolution now
  defaults to `ExecutionPolicy::evolve_sandbox()` — deny-all effects
  (no file, no network, no stdin, 30 s budget) with stdout ON (the gate
  must run the host program to measure it, and stdout is not a registry
  effect), `capsule apply-proposal` always verifies under the sandbox,
  and `--policy <dir>` relaxes only in `evolve`. The fix round then
  exposed a second gate hole: a graft whose FULL program errored every
  benchmark trial skipped the speed gate and was ACCEPTED —
  `grafted_runs < eval_runs` now rejects with `grafted program failed to
  execute…` before promotion. A compromised proposer can influence what
  is proposed, never what is executed.

## [Unreleased] — Language decisions: overflow policy, backend alignment, `F!` (2026-09-08)

### Changed (language-visible — the first decisions taken from the spec's
open-decision registers)

- **`i64` overflow is a runtime error on every arithmetic path, both
  backends, both Rust profiles** (spec `value-model.md` §8, was: profile-
  dependent wrap-in-release / abort-in-debug — a `.lyc` could compute
  different values per profile). Named errors `integer overflow in
  {+,-,*,/,%,neg,!abs}`, identical text on both backends; the `INT_MIN / -1`
  divisibility guard runs through `checked_rem` first (the guard itself used
  to overflow). Capability float→int arguments reject out-of-range instead
  of saturating to `i64::MAX`.
- **`!type` aligned**: new `TypeOf` opcode (`0x7E`) — both backends return
  the `type_name` string (`int`, `fn`, …); the compiled `ToString`
  mis-compile (`1` for an int) is gone. Binary format opcode table updated.
- **Unknown builtins fail closed**: `GraphCompiler::compile` now returns
  `Result`; `(!frobnicate x)` is `compile error: unknown builtin` instead of
  a silent `Noop` → `Null` (exit 0). Interpreter already errored; both now.
- **Builtin arity: one shared table** `graph::builtin_fixed_arity`, derived
  from `op_fixed_arity` — the interpreter's hand list (which had drifted:
  it accepted what the verifier rejected) is gone.
- **Finiteness/typing fail-closed on both paths**: `!abs` requires finite
  floats (source used to return `inf`), `!atan2`/`!lambert` reject
  non-numeric args (both paths previously coerced to `0.0` silently).
- **`F!` removed from the grammar** (was inert syntax promising stateful
  functions the runtime never implemented): parse-rejected with a pointed
  message. A real stateful-functions feature would be designed, not revived
  from a dead flag.
- Open-decision registers in `scoping-and-execution.md`, `value-model.md`
  and `grammar.md` record each resolution; the remaining open items are the
  design-level ones (scoping model, closures, n-ary operators,
  type annotations, numeric literals).

### Tests

- `tests/semantics_parity.rs` (7 tests): every decided case runs through
  BOTH backends (and the debug profile for overflow) asserting identical
  exit class and stdout. Full-suite CI covers release; `cargo test` debug.

## [Unreleased] — Language spec, durability, evidence, buyer journey (2026-09-08)

### Added

- **Lycan language specification — `docs/lycan/spec/` (10 files).** The
  first spec written from re-opened `file:line` evidence rather than
  memory: grammar, value model, scoping/execution, graph binary format,
  capsule format, execution policy, learning semantics, capability ABI
  (+ the two pre-existing format docs corrected). Every constant,
  threshold, and formula cites its source line; divergences between an
  earlier fact-gathering pass and the tree are recorded in appendix
  deviation tables with tree-wins verdicts. Sections carry
  Conformance-requirements lists and honest Open-normative-decisions
  registers (12 for grammar/value, 10 for learning).
- **Executable conformance vectors — `tests/conformance_vectors.rs` +
  24 byte-pinned fixtures.** Graph and capsule formats now have
  machine-checkable vectors (legal shapes from real `to_bytes`, hostile
  shapes hand-assembled; regeneration env-gated and drift-tested).
  Magic/version pinned three ways. First run caught three spec errata
  (journal/zero-pad leniencies unreachable behind the guard lattice;
  `WeightKind::Decision(4)` decodes lossily) — spec corrected to match,
  vectors pin the truth.
- **`syntra doctor` — read-only store validator.** JSONL findings
  (`GRAPH_DECODE_FAIL`/`VERIFY_FAIL`, sidecar parse, memory version
  drift, orphan tmps, torn JSONL tails, stale locks, restore stranding,
  retention byte counts), exit 0/1/2, cleans nothing. Enforced: zero
  filesystem writes on doctor paths.
- **`syntra backup` / `syntra restore`.** Bundle the store to one
  fsynced versioned JSON; restore installs atomically and REFUSES a
  live root (readiness probe or live `.evolve.lock` pid) without an
  explicit `--force`.
- **Crash-semantics hardening.** `write_atomic` tmp names are now
  `<stem>.tmp.<pid>.<seq>` (shared-name cross-contamination closed);
  corrupt sidecars (memory/hierarchical state/tokens) no longer reset
  silently — loud `tracing::error!` plus one bounded
  `<name>.corrupt-<ts>` evidence copy before the availability-
  preserving reset. What IS durable vs is NOT is now stated in
  `docs/store-retention.md`. SIGKILL crash-harness (3 kill cycles
  under load, clean-restart assertions) + 12 doctor CLI tests.
- **Bandit evidence: measured, not asserted.**
  `simulate --compare-baseline random|first-arm|epsilon-greedy:N`
  (independent RNG stream, same context stream); three traffic specs
  (stationary / regime-shift / sparse-reward) with 7 arm capsules;
  `scripts/eval-report.sh` regenerates the dated report with a
  determinism gate that aborts if two identical runs disagree. Dated
  report `docs/evaluations/2026-09-08-adaptive-policy-baseline.md`
  states weaknesses plainly: `auto`/`simpleWeighted` never goes greedy
  (shareBest 0.404 is a weighted-sampling floor, not slow
  convergence — 45x/38x regret/round gap vs thompson/ucb1 shown),
  sparse-reward loses to eps-greedy, `metaBanditLeader` is a
  consultation artifact.
- **`/v1` API prefix.** Every data route now lives under `/v1` with
  `Deprecation`/`Link`/`successor-version` headers on the unversioned
  aliases (identical behavior, `warnings=action`); `/health`, `/ready`,
  `/metrics` stay unversioned by design. `docs/openapi.yaml` +
  `tests/openapi_drift.rs` + 15 auth-routing tests pin it.
- **SDKs, first pass.** `sdk/python` (`syntra-client` 0.1.0,
  stdlib-only, `py.typed`) and `sdk/typescript` (`@syntra/client`
  0.1.0, zero-dep, Node ≥22) — typed clients over the `/v1` surface
  with per-operation retry contracts (mutations never retried) and
  live server smoke suites (32/36 checks) run against a real instance.
- **Buyer docs.** `docs/why-syntra.md` (positioning: the governed
  promotion loop, honest limits) and
  `docs/quickstart-model-routing.md` (one install → decide → feedback
  → metrics → shadow → gated promotion → rollback journey).
- **Panic-hole closure round 2 (fail-closed).** The grammar spec's
  dual-backend probes found the residual reachable aborts: `(!len)`,
  `(!atan2 1)` panicked both backends; compiled `(not)` panicked;
  `(not 1 2)` silently dropped an operand. One shared
  `graph::op_fixed_arity` table now drives the verifier rule (was: six
  hand-listed opcode arms) and a pre-dispatch guard in the executor;
  the interpreter gained the equivalent builtin-arity table. No fixed-
  arity opcode can be made to abort either backend through verified or
  decode-only paths (9 regression tests).

### Fixed

- **`simulate --seed` did not pin the run.** The learning layer's RNG
  (Thompson samples, weighted roulette, meta tie-breaks) fell back to
  SystemTime entropy; eight identical pre-fix runs spread mean regret
  1329–1355. Per-seed seeding + entropy restore, pinned by a
  repeated-invocation reproducibility test.
- **`simulate --true-arm-rewards` silently dropped non-numeric
  tokens**, shrinking the arm list and masking spec/capsule mismatch —
  now exit 2 naming the offending token.
- **`ab_harness.py` arm extractor** mixed index-vs-name extraction,
  refusing valid arms.
- **Corrupt store sidecars reset silently** — see hardening above.

### Changed

- `README.md` gains `/v1`, SDK, evaluations, and doctor/backup
  pointers; the buyer-journey docs link from the docs index.

## [Unreleased] — demo suite, learning-rule fix, retention, perf baseline (2026-09-08)

### Added

- **Fuzzing in CI.** `.github/workflows/ci.yml` gains a nightly job:
  all five fuzz targets replayed from the tracked seed corpora
  (`fuzz/corpus/`, 5.4k inputs) plus 60 s of fresh fuzzing each,
  RSS-capped, crash artifacts uploaded on failure. The one open
  artifact (a 2026-07 OOM in `graph_from_bytes`) was triaged: the
  `check_count` header guards already fixed it, all corpora replay
  clean; the exact crashing input is now a byte-pinned regression
  (`tests/fuzz_regressions.rs`) instead of a stale local artifact.

- **`scripts/demo.sh` — one command, five proofs.** ~20 seconds, no
  mocks: derives the Feigenbaum constant and edge of chaos from
  dynamics; runs a Mars transfer decision on live NASA/JPL HORIZONS
  ephemerides (embedded Keplerian fallback offline); runs governed LLM
  routing against promotion gates plus a live bake-off measuring what a
  decision costs on your machine (client-side p50/p95/p99); runs a
  response-adaptive clinical trial; and asks the proof lab to solve an
  open math problem (Erdos #160 — computes, refuses to overclaim).
  Closes with a receipt block: decision/feedback log counts and a
  sha256 fingerprint of the append-only decision logs. `--no-live` for
  headless; sets `LYCAN_RNG_SEED=7` so every number reproduces. CI
  guard: `tests/demo_smoke.rs`.
- **`scripts/demo-trial.py` + `examples/lycan-internals/demo_adaptive_trial.lycs`**
  — the runtime as a response-adaptive clinical-trial allocator: every
  patient is a `/decide`, every outcome a delayed `/feedback`, the
  learned per-subgroup weights are the randomization schedule. A fixed
  1:1:1 control runs in parallel on the same hidden response rates; the
  headline compares observed responses (measured, not simulated).
  Subgroups stop on the standard Bayesian expected-loss rule. Own live
  dashboard.
- **`scripts/demo-live.py`** — live adaptation dashboard (watch weights
  converge from delayed feedback only, then flip the ground truth).
- **`tests/mega_demos.rs`** — QA suite running 14 substrate demos and
  asserting each one's headline claim, not just exit status.

- **Store retention: size-based JSONL log rotation.** Decision/
  feedback/audit/evolution logs now roll to `<name>.jsonl.1` (one
  rotated generation) once `<store_root>/retention.json`'s
  `maxLogBytes` (default 64 MiB, `0` disables) would be exceeded;
  API readers see one continuous oldest-first stream, so wire formats
  and replay/backup are unchanged. Invalid config fails closed at
  startup. Rotated-away decisions 404 on `/feedback` instead of
  crediting blindly. Unit tests in `store.rs::retention_tests`, e2e
  `decision_log_rotation_keeps_api_stream_continuous`. Closes the last
  unbounded-growth path in the filesystem store
  (`docs/store-retention.md`).

- **`/decide` performance baseline + hot-path fsync elimination.**
  `scripts/bench-decide.sh` gains a `keepalive` mode; results recorded
  in `docs/benchmarks.md` with a tiny_http-vs-axum decision note. The
  baseline exposed a per-request `fsync` in shadow-mode `/decide`
  (every request rewrote `memory.json` atomically with `sync_all`;
  APFS commits serialize even across processes — two instances shared
  one ~265 rps ceiling). Memory now saves content-aware: serialize,
  compare, write only on change — shadow-mode steady state does zero
  learning-state writes, any mutation persists as before. Shadow
  throughput: ~250 rps → ~11.6k rps sustained (p99 1.45 ms, server
  mean 0.59 ms at saturation, M5 Max; machine-specific). Rate limiter
  (default 1000 rps/token) is now raisable via
  `SYNTRA_RATE_LIMIT_RPS` / `SYNTRA_RATE_LIMIT_BURST`; invalid values
  warn and keep the safe default.

### Fixed

- **Prometheus metrics under/over-counted decisions.** Hierarchical
  decide/feedback recorded both inside handlers and at the routes
  (double count); legacy `/tenants/{t}/capsules/{c}/decide|feedback`
  recorded nothing and never observed latency. Recording cut over to
  routes uniformly with honest per-response status;
  `syntra_decide_latency_seconds_count` now matches client-side
  request counts exactly. `bench-decide.sh` metrics delta parsing
  fixed for label-less `_sum`/`_count` lines.

- **Flat feedback weight update tracked success flux, not mean reward
  (high).** `w[chosen] += lr * reward` with no movement on failure made
  normalized weights follow cumulative success counts (rate ×
  allocation), so under stochastic rewards a luckier inferior option
  could lock in — the clinical-trial demo converged to the wrong arm
  per subgroup. All four flat update sites (`server/feedback.rs`,
  `learning/feedback.rs` bucket weights and `OptionState::Weighted`,
  `bin/lycan.rs`) now apply the mean-seeking rule
  `w += lr * (reward - w)` — the rule the hierarchical learner already
  used. Consequences: weights converge to response rates; a reward of
  `0.0` lowers the chosen option instead of being a no-op. Rule
  documented in `docs/lycan/learning.md`; regression coverage in
  `src/learning/mod.rs` + the seeded trial assertions in
  `tests/demo_smoke.rs`. Full writeup: `bugs.md` BUG-5.
- **`capsule apply-proposal` speed gate rejected valid grafts under
  load (flaky `test_apply_proposal_increases_operand_and_node_count`).**
  The gate compared a 5-run **mean** of the original program against a
  5-run mean of the grafted program, measured in separate time blocks —
  preemption noise passed straight through the 10% ratio (3.61ms vs
  3.14ms under suite load). The gate now interleaves original/grafted
  runs back-to-back and scores each side by its minimum (load can only
  add time), with tolerance `min_orig * 1.1 + 0.05ms` so scheduling
  jitter can never reject a structurally sound graft. A genuinely slow
  graft is still rejected deterministically — new regression
  `test_slow_graft_rejected_by_speed_gate` (deterministic 2M-iteration
  loop, byte-identical binary preserved). Verified: 10/10 isolated and
  140/140 full-parallel integration runs under a 12-process CPU load.

### Changed

- Show-off assets renamed to demo naming: `scripts/demo.sh`,
  `scripts/demo-live.py`, `scripts/demo-trial.py`,
  `examples/lycan-internals/demo_live_bandit.lycs`,
  `tests/demo_smoke.rs`; tenant renamed `showoff` → `demo`.
- Showcase scripts `01/04/05` now point at the relocated
  `examples/lycan-internals/` demo files they actually reference.
- `tests/integration.rs` feedback tests re-pinned from old-rule
  arithmetic (exact weight strings) to direction assertions.


## [Unreleased] — Security & correctness bug hunt (2026-09-07)

Four bugs found in a systematic review of the decision / learning /
executor paths. Each was reproduced red, fixed, verified green. Full
symptom/evidence/root-cause writeup: `bugs.md`.

### Fixed

- **Warmup lifecycle advanced by invalid feedback (high).** Feedback
  with unknown `decisionId`s returned 404 but still counted toward the
  capsule's warmup sample target — 30 bogus posts flipped a capsule from
  `warmup` to `active` on garbage rewards. Warmup record/save now happens
  only after the decision lookup and option validation succeed, in both
  the flat and hierarchical feedback paths.
- **Verifier accepted malformed strategy graphs (high).** A
  `Strategy`/`AdaptiveChoice` node whose `weights.len()` did not match
  its operand count passed `verify` and then panicked the executor
  (`results.remove(best_idx)` out of bounds) — a remote DoS via crafted
  capsule. The verifier now rejects weight/operand mismatches
  (`+1` for `WithinTolerance`'s epsilon slot), and the executor clamps
  the index as defense in depth.
- **Decision lookup by substring (medium).** `find_decision_in_job`
  matched log lines with `line.contains(decision_id)`, so feedback for
  `dec_abc` could credit `dec_abcdef…`. Lines are now parsed and matched
  on the exact `id` field; substring matching remains only as a fallback
  for pre-v2 log lines without an `id`.
- **Read-scoped token could mutate learned policy (medium).**
  `POST /decide?learn=true` persisted graph weights and memory under a
  `Scope::Read` token. Read-scoped tokens now force `learn=false` on
  both decide routes; Admin/TenantAdmin keep the URL flag.

Regression tests: `tests/bugfix_regressions.rs` (3 tests),
`tests/verifier_strategy_weights.rs` (4 tests).


## [Unreleased] — repo merge: Lycan folded into Syntra as a single crate

The separate Lycan language repository and the vendored `Lycan/`
subdirectory are gone. The language core now lives in the root crate
alongside the Syntra wrapper modules; one `Cargo.toml` builds both
binaries. No runtime behavior changes.

### Changed — repo structure

- **Single crate.** `Lycan/src/*` moved to `src/` as a flat merge;
  internal `crate::` paths are unchanged. The former `lycan` crate's
  public modules are declared at the crate root, and
  `extern crate self as lycan;` keeps the existing `lycan::` paths in
  the Syntra wrapper modules working without edits.
- **Both binaries from one build.** `cargo build --release` produces
  `target/release/syntra` (appliance CLI) and `target/release/lycan`
  (language CLI, entrypoint `src/bin/lycan.rs`).
- **Language assets relocated:** `Lycan/examples/` → `examples/lycan/`
  (self-referential paths inside the demos, scripts, and three compiled
  `.lyc` fixtures updated and recompiled), `Lycan/docs/` →
  `docs/lycan/`, `Lycan/README.md` → `docs/lycan/README.md`,
  `Lycan/benchmarks/` → `benchmarks/`, `Lycan/tests/` → `tests/`.
- **Build surface:** `scripts/smoke-test.sh`, `scripts/run-demo.sh`,
  `scripts/demo-sandbox.sh`, `scripts/demo-boundary-api-tests.sh`, CI
  (`.github/workflows/ci.yml`, `publish-demo-image.yml`), and both
  Dockerfiles now use root-context single-crate builds;
  `docker-compose.yml` builds from the repo root instead of a parent
  directory. Also fixes pre-existing broken `$ROOT/examples/...`
  fixture paths in the scripts (they pointed at the old Lycan-repo
  layout and never existed at this repo's root).
- **Docs:** README "Repository relationship", CONTEXT.md, AGENTS.md
  (merged with the former `Lycan/agent.md`), and the docs tree describe
  one repo; all `ashhart/Lycan` links and stale `Lycan/`-prefixed paths
  are removed.

### Validation

- `cargo test --release -- --test-threads=1`: 502 passed, 0 failed
  (321 lib + 10 auth_routes + 1 change-detection characterization +
  139 integration + 31 doc-tests).
- `./scripts/smoke-test.sh`: 20/20 PASS.
  `./scripts/demo-sandbox.sh`: 7/7 PASS.
  `./scripts/demo-boundary-api-tests.sh`: 29/29 PASS against a live
  server.
- `docker build -f Dockerfile .` and
  `docker build -f docker/Dockerfile.demo .` both succeed from the repo
  root.

## [Unreleased] — Phase I followup 25: deterministic RNG + comment cleanup

Two threads of work. First, the root cause of the MAB-vs-VW headline
gap identified in followup 24 (Syntra's `rand_f64()` was unseedable)
was fixed: a `LYCAN_RNG_SEED` env var and a `POST /admin/rng/seed`
admin endpoint now thread a SplitMix64 PRNG through `rand_f64()`
when set. Second, a large pass of comment cleanup across the
codebase: agents removed roughly 1,450 lines of verbose / meta /
historical comments across the Lycan core modules (~985 lines) and
the Syntra wrapper modules + `docker/` + `scripts/` + benchmarks
(~467 lines).
Code paths untouched; build clean (228 + 47 tests pass).

### Added — deterministic RNG

- **`LYCAN_RNG_SEED` env var** read at server startup
  (`src/server/mod.rs`). When set to a u64, switches the
  global `rand_f64()` in `crate::learning` from SystemTime entropy
  to a SplitMix64 sequence. Without it, behavior matches the legacy
  non-deterministic path.

- **`POST /admin/rng/seed` admin endpoint** (`src/server/admin.rs`).
  Body `{"seed": <u64>}` switches to deterministic mode at runtime;
  `{"seed": null}` or `{}` reverts to legacy entropy. Returns the
  active seed in the response.

- **`src/learning.rs`** carries the seeded PRNG state
  (`Mutex<Option<u64>>`), the `seed_rng(seed)` / `rng_seed_state()`
  helpers, and a SplitMix64 mixer in `rand_f64()`. New unit test
  `deterministic_when_seeded` asserts that the same seed produces
  the same 5-element sequence and that re-seeding restarts it.

- **`docker/demo/entrypoint.sh`**: new `SYNTRA_DEMO_NO_TRAFFIC=1`
  env var that skips the background `generate.py` traffic driver.
  Required for reproducible benchmark runs — the traffic generator's
  /decide calls were consuming the global RNG sequence interleaved
  with benchmark requests, breaking determinism even with seeding.

- **MAB benchmark** (`examples/lycan-internals/benchmarks/syntra_vs_vw_mab/benchmark.py`):
  passes a deterministic `rng_seed` to each SyntraMAB constructor;
  the constructor seeds the running Syntra via the admin endpoint
  before install. Also replaced `hash(difficulty) % 1000` (which
  is randomised per Python process by default) with a fixed
  `{"easy": 0, "medium": 1, "hard": 2}` mapping so seeds are stable
  across Python invocations.

### Validation

Two full-scale MAB runs (10 seeds × 2000 rounds × 9 cells = 90
instances each) at deterministic mode against a container booted
with `SYNTRA_DEMO_NO_TRAFFIC=1`:

- **90 / 90 per-instance regret values identical** between runs
  (bit-exact reproducibility).
- Mean ratio: **0.946 → 1.06× lower regret vs VW** (bin A).
- Per-cell: 5/9 cells Syntra beats VW (2_easy 0.557, 2_hard 0.480,
  5_easy 0.880, 5_hard 0.934, 5_medium 1.022); 4/9 close-to-VW
  (2_medium 1.074, 10_hard 1.032, 10_medium 1.071); 1/9 10_easy at
  1.459 pulls the mean up.
- The documented Phase A-F headline (`ratio_mean=0.374` → 2.67×
  lower regret) does NOT reproduce on the current code. Bin A
  classification matches. The gap is now a deterministic measurement
  rather than swimming in run-to-run noise, so any future
  optimisation can be A/B tested against it cleanly.

### Changed — comment cleanup

Subtractive pass across the codebase. Two agents working in parallel
on the Lycan core and the Syntra wrapper modules (non-overlapping).
What was removed:

- `Phase I followup N`, `Item N`, `5C:`, `Phase A-F`, `debt item`,
  `May 2026 regression run`, `previous session`, `see known-issues`
  cross-references — these rot the moment they're committed and
  belong in CHANGELOG / commit history, not source comments.
- Multi-paragraph block comments explaining the WHY of code in 5+
  lines; compressed to one short sentence or removed when the
  next-engineer reader would figure it out anyway.
- Comments describing WHAT the code does (obvious from reading).
- Long narrative recaps of prior incidents ("before this fix...",
  "this was discovered when...", "the symptom was...").

What was kept:
- Short non-obvious WHY: bug workarounds for upstream issues, subtle
  invariants, asymmetric-cost branch in `helpers.rs`, security caveats.
- `///` doc-comments on public functions, trimmed to one or two lines.
- `SAFETY:` / `TODO:` / `FIXME:` markers.
- All documentation files (`.md`, README, CHANGELOG, known-issues,
  docs/) — those are intentional historical records, not comments.

Net: ~1,450 lines removed from `.rs`, `.py`, `.sh`, `Dockerfile`
files. Build clean (228 Lycan lib + 47 Syntra lib tests still pass).
Python files validated via `ast`; shell scripts via `bash -n`.

## [Unreleased] — Phase I followup 24: MAB-vs-VW bin regression fix

Full-scale benchmark validation against the locally-built demo image
revealed that the MAB-vs-VW benchmark had regressed from documented
Phase A-F bin A (mean ratio 0.374, 2.67× lower regret than VW) to bin B
(mean ratio 1.438, Syntra ~30% worse than VW on average). Other
benchmarks reproduced cleanly: vaccine reward-blindness at 4.36× vs
documented 4.4×, outbreak pandemic at 2/4 pass with 1.20 deaths vs
documented 0.5.

### Fixed

- **`src/server/helpers.rs` greedy-override branch on reward shape.**
  When the meta-bandit selects Thompson or UCB1 for a strategy node, the
  `apply_context_memory_to_graph` override previously nudged the
  algorithm's chosen weight to `max + 1e-3` and renormalised — which
  after re-distribution barely moved the actual selection probability.
  The legacy weighted-bucket dynamics (which never decrement on
  `reward=0` because `delta = clipped * learning_rate`) ended up
  dominating selection, so the bandit kept exploring inferior arms at
  ~25-30% probability long after Thompson's Beta posterior had
  identified the right one.

  The override now branches on reward shape:
  - **Binary**: hard greedy commit on the algorithm's argmax,
    `min_exploration` as uniform floor. This is the textbook Thompson
    Sampling specification.
  - **Continuous**: keep the legacy soft nudge so weighted-bucket
    dynamics provide exploration around UCB's optimistic argmax. The
    asymmetric cost of premature commitment in continuous-reward
    domains (e.g. outbreak: greedy commit to lockdown → ~3.8× more
    deaths than soft exploration) makes hard greedy wrong there.

  Discriminator: `warmup_state.current_algorithm()` returns
  `Some(PickedAlgorithm::Thompson { .. })` iff reward characterization
  is `Binary` (per the `pick_algorithm` mapping in
  `src/reward_characterization.rs`).

### Validation

Three benchmarks rerun at full documented scale (10 seeds × 52 weeks
or 10 seeds × 2000 rounds × 9 cells, depending) against the demo image
rebuilt with the fix:

| Benchmark | Pre-fix | Post-fix | Documented |
|---|---|---|---|
| Vaccine reward-blindness | 4.36× (matched docs) | **4.36×** ✓ | 4.4× |
| Outbreak pandemic | 2/4 pass, **1.20 deaths**, $29.5B | 2/4 pass, **0.40 deaths**, $25.4B ✓ | 2/4, 0.5 deaths, $26.3B |
| MAB vs VW | Bin **B**, ratio 1.438, 0.70× | Bin **A**, ratio 1.19-1.24, 0.81-0.84× | Bin A, ratio 0.374, 2.67× |

MAB classification restored to bin A across two independent reruns
(variance ~0.05 across runs). Outbreak's secondary metric (mean_deaths)
returned to documented baseline — the previous 1.20 deaths drift was
caused by the same broken override hurting binary-but-disguised-as-
continuous cases; with the conditional fix, outbreak's continuous
characterization correctly avoids the greedy collapse.

### Known issue filed (not fixed this round)

The MAB **headline number** "Syntra-Thompson 2.67× lower regret than
VW" still does not reproduce at full scale — mean ratio holds at
1.19-1.24 across reruns vs documented 0.374. Bin classification (A)
matches. Per-cell pattern is consistent: 8-9/9 cells stay within
1.5× VW, but easy-difficulty cells with more arms (5_easy ≈ 2.1,
10_easy ≈ 1.4-1.7) carry the gap. Filed in
`docs/known-issues.md` with the three likely investigation
targets (warmup-cost amortisation, weight-delta asymmetry on binary,
code drift since Phase A-F). External claim updated to "bin-A
competent with VW across the 9-cell benchmark grid" until the
headline number is recovered or the gap is explained.

## [Unreleased] — Phase I followup 23: README + local-development split

First-impression cleanup. The README's "Try the demo" prose was
replaced with a Docker-only OpenWA-style Quickstart; the full
local-development path moved into the tutorial site where developers
who click the link want depth. A new helper script reproduces the
Docker entrypoint locally so the build-from-source path is one
command, not five.

### Changed

- **`README.md` Quickstart rewritten** to lead with a single
  `docker run -d` block against `ghcr.io/ashhart/syntra:demo`,
  followed by the Dashboard / API URLs, a five-capsule bullet list
  (Predictive autoscaling, Anomaly-aware API routing, Seasonal fraud
  threshold, Shared-state action embeddings, Hierarchical region
  routing — matches what `install.py` actually installs), and a
  one-line pointer to the Local Development guide. The previous prose
  block that mixed build-from-source instructions with explanation
  was removed. The published-image caveat (workflow shipped in
  followup 22 but the first CI run is what makes `:demo` pullable)
  stays as a blockquote so first-time visitors aren't surprised.

### Added

- **`docs/site/docs/contributing/local-development.md`** —
  the full developer-onboarding path that used to be partly in the
  README and partly nowhere. Covers prerequisites (Rust 1.85+,
  python3 flask/requests, optional Docker, macOS xcode-select hint),
  clone-and-build with realistic timings, running the demo via
  `scripts/run-demo.sh`, building the demo container locally,
  running tests (with the `--test-threads=1` caveat),
  iterating with the debug profile, `syntra status` / `syntra stop`
  for port-conflict recovery, and a troubleshooting section.
  Cross-links to Concepts, Domain packs, API reference, Cookbook,
  Operations. Added to `mkdocs.yml` under a new `Contributing`
  navigation section. `mkdocs build --strict` passes.

- **`scripts/run-demo.sh`** — local equivalent of
  `docker/demo/entrypoint.sh`. Verifies release binaries
  exist at `target/release/lycan` and
  `target/release/syntra` (prints the build command and
  exits if not), creates a `mktemp -d` store, boots `syntra serve`
  with a random dev key, waits for `/health`, installs the same
  five flagship capsules through `docker/demo/capsule/install.py`
  pointed at `examples/`, runs the traffic generator
  against the configured `SYNTRA_DEMO_CAPSULE` (default
  `predictive-autoscaling`), and serves the dashboard in the
  foreground. `Ctrl-C` SIGTERMs all three subprocesses and removes
  the store. Honors `SYNTRA_ADDR`, `DASHBOARD_PORT`,
  `LYCAN_ADMIN_KEY`, and `SYNTRA_DEMO_CAPSULE` env-var overrides.

### Validation

- README Quickstart: rendered section is 36 lines (one viewport
  on a standard laptop screen). Essential content
  (`docker run` + URLs + 5-capsule list) is the first ~25 lines.
- `scripts/run-demo.sh` end-to-end: started against a clean store,
  /health returned 200 in ~1s, install.py installed all five
  flagship capsules in `/admin/capsules` with the expected scoring
  modes (3 × `meta-bandit`, 1 × `shared-state-linucb`,
  1 × `hierarchical`), traffic generator drove 6 decisions in
  6 seconds, `/api/state` reflected `warmupProgress: {collected:6,
  target:30}`, dashboard at `http://localhost:18089` served the
  new shape correctly. `Ctrl-C` cleaned up all subprocesses and
  removed the tempdir.
- `mkdocs build --strict` (via `docs/site/build.sh`):
  passes in 0.4s, no warnings against the new
  `contributing/local-development.md`. Internal links resolve.
- Build-time observation: incremental rebuilds finished in under
  10 seconds against an already-warm `target/`. A full clean
  rebuild is documented as ~3–5 minutes based on the
  ~750 MB combined `target/release/deps` size; not re-clocked
  this round (clean rebuild burns time the verification doesn't
  need).

### Caveats

- `ghcr.io/ashhart/syntra:demo` is still not pullable from
  GHCR — the workflow added in followup 22 only fires on push to
  `main`, and the repo state where the workflow lives hasn't been
  pushed. The README's Quickstart docker run command will fail
  until that push happens; the blockquote pointing readers at
  the Local Development guide is the documented fallback path.

## [Unreleased] — Phase I followup 22: adoption-readiness round

Seven-task round closing first-impression gaps for external evaluators.
Three tasks landed code, three landed honest findings of bigger-than-scope
issues, one was blocked on missing input data. Net: the platform's adoption
surface is materially better documented and operationally easier to recover
from; two real bugs were uncovered, scoped, and filed for a future round.

### Added

- **GitHub Actions workflow `publish-demo-image.yml`** at
  monorepo `.github/workflows/`. Builds and pushes
  `ghcr.io/ashhart/syntra:demo` (moving tag) and
  `ghcr.io/ashhart/syntra:demo-<sha>` (immutable per-commit tag)
  on push to `main`, on release publish, and on manual dispatch.
  Multi-arch (`linux/amd64`, `linux/arm64`). Uses `GITHUB_TOKEN`
  with `packages: write`. Lives at the repo root (not in a crate
  subdirectory) because `Dockerfile.demo`'s build context spans the
  whole repo. The Quickstart's Step 1 is rewritten
  to `docker run ghcr.io/ashhart/syntra:demo` as primary, with
  build-from-source kept as an alternative for offline / dev use.

- **`syntra status` and `syntra stop` subcommands** in
  `src/lib.rs`. `status` reports whether a server is
  listening on the configured port and emits
  `{"running": true/false, "port": N, "pid": ...}` on stdout.
  `stop` sends SIGTERM to whatever holds the configured port and
  emits `{"stopped": true, "pid": N, "signal": "TERM"}`. Both take
  `--addr host:port` or `--port N` (default `:8787`). `stop` does
  **not** verify the process is syntra — documented in the help
  text and in `runbook.md` §1.7. Implementation uses
  `lsof -ti :<port> -sTCP:LISTEN`, so macOS/Linux only.
  Cargo check + manual smoke test against a live syntra both pass.

- **`docs/operations/memory-profile.md`** — empirical growth
  characterization of `memory.json` over a long-running capsule.
  Documents what's bounded (`OptionStats` fixed-size, strategy
  bucket count, time-series window) and what's not (the OOD
  detector's per-observation accumulation). Records the May 2026
  measurement methodology and numbers so future operators have a
  reference point. Includes operator mitigations until the OOD
  fix lands.

- **`runbook.md` §1.7** updated: bind-failure row now points at
  `syntra status` / `syntra stop` as the one-liners, and a new
  "Inspecting / stopping a running Syntra" sub-section documents
  both commands with the safety caveat.

### Documented (no fix, filed in known-issues)

Three previously-unflagged gaps surfaced during verification. All
three added to `docs/known-issues.md` with reproduction,
scope, and likely-fix-shape. None fixed this round — they're real
engineering work that needs design beyond the scope of an adoption
cleanup.

- **OOD detector unbounded per-observation accumulation** on
  feature-context capsules. Empirically ≈1.3 KB / decide growth in
  `memory.json` even when the same context vector is re-observed.
  Strategy state itself is bounded; the offender is
  `memory.feature_ood_for(nid).record(x)` in `decide.rs:283`.
  Discrete-context capsules are not affected.

- **Multi-AdaptiveChoice graphs**: response wired, learning is not.
  `.lycs` programs with two or more `(choice ...)` blocks return
  one entry per node in `decisions[]`, but `/memory` records only
  the primary node's strategy state. Hand-authored only —
  YAML-authored capsules always produce one `(choice ...)` so the
  gap is invisible to capsule-spec users. Verified May 2026 via a
  two-choice test capsule. The "5C: per-node candidate selections"
  comment in `decide.rs:335-339` reflects an aspiration; the
  storage path doesn't yet honour it.

- **Strategy-node install-time warning never landed**. The
  `warn_if_strategy_nodes` helper referenced in earlier docs does
  not exist in `src/capsule_compiler.rs` — zero references
  to `Strategy`, `OpCode::Strategy`, `warn!`, or `eprintln!`. In
  practice mostly fine because the YAML compiler never emits
  `OpCode::Strategy`. Open ticket because hand-authored `.lycs`
  installs are a supported path and an install-time hint would
  help.

### Verified (no change required)

- **Dashboard rebuild**: confirmed landed. `docker/demo/dashboard/`
  contains the new `static/` architecture (`index.html`,
  `style.css`, `dashboard.js`, `chart.js`) and `app.py` is the
  Phase 2 Flask version with `/api/state`, `/api/capsules` proxy,
  hierarchical / shared-state / meta-bandit detection,
  `publishedSeries` / `publishedLatest` for Region 5. End-to-end
  smoke test passes: live syntra + dashboard, `/api/state` and
  `/api/capsules` both return the expected shape, `index.html`
  and `dashboard.js` serve correctly.

### Skipped

- **May 2026 regression report** (would have lived at
  `docs/benchmarks/regression-2026-05.md`). The previous
  session's benchmark output files do not exist at `/tmp/bench/`
  or any plausible alternate location; only the historical
  `phase_a_f_*` and `v3_full` JSONs are on disk. Per the round's
  honesty requirement: not fabricating from speculation. Re-run
  the benchmarks separately when the report is needed.

## [Unreleased] — Phase I followup 21: benchmark infrastructure cleanup

Three small fixes responding to lessons from the May 2026 regression run,
where a stale syntra process from the previous evening silently held
:8787 and the first batch of benchmarks ran against yesterday's binary
before being caught and restarted. Total runtime change: one startup
check. No benchmark numerical behavior changed.

### Added

- **Port-occupancy startup check** in `src/server/mod.rs`. Before
  `tiny_http::Server::http` binds, `run_server` now probes the address
  with `std::net::TcpListener::bind`. If the probe fails with
  `AddrInUse`, the process exits with an actionable error message
  including the `lsof -i :<port>` and `kill $(lsof -ti :<port>)`
  commands an operator needs to find and free the port. Other bind
  errors pass through unchanged. The probe is briefly racy — a different
  process could grab the port between the probe drop and the
  tiny_http bind — but the common case (a stale syntra still holding
  the port) is caught reliably. No integration test added; process-
  management timing makes the two-server scenario fragile in CI, and
  the behavior is documented in a comment near the bind site.

- **Output schema documentation** for the three flagship benchmark
  suites under `examples/lycan-internals/benchmarks/`:
  - `outbreak_early_warning_resilience/SCHEMA.md` — top-level
    `criteria.overall.passed`/`total` is the 2/4 pass headline;
    per-policy fields live under `aggregate.<policy>`. No top-level
    `spread_*` fields here (those are vaccine-only).
  - `vaccine_allocation_resilience/SCHEMA.md` — top-level
    `spread_corrected` / `spread_original` (the 4.4× reward-blindness
    ratio is their quotient); no `criteria` block, different cost
    field naming (`mean_cost_M` vs outbreak's `mean_econ_cost_M`).
  - `syntra_vs_vw_mab/SCHEMA.md` — `per_cell.<arms>_<diff>.ratio_mean`
    is the per-workload regret ratio; the "2.7× lower regret"
    headline is `1 / mean(per_cell.ratio_mean)`, derived on read
    (not stored). The `bin` field carries a verbatim pre-registered
    label string with an em-dash — match on prefix, not whole string.

  Each file is specific enough that a fresh agent reading it can write
  a parser without re-probing the JSON structure.

- **Drift-tracking entry** in `docs/known-issues.md`. Outbreak
  weighted=0.70 deaths and ucb1=1.00 deaths in the May 2026 regression
  run are within documented Phase A-F tolerance (~1 death, ~$400M)
  but at the upper bound. The entry documents the explicit thresholds
  (>1.5 deaths or >$28B econ cost) that would push a future run out
  of tolerance, plus likely investigation candidates if drift continues.

## [Unreleased] — Phase I followup 20: adoption infrastructure cleanups

Four cleanups responding to the flags raised in followup 19. No new
crate code or runtime behavior changes; all infrastructure and
documentation.

### Changed

- **Terraform layout** (`deploy/terraform/`). Phase 19 reported
  the prior agent had "overwritten pre-existing EKS/GKE/AKS K8s
  modules." Git archaeology (`git log --all` against the Syntra repo)
  shows **no prior Terraform files have ever been committed** — the
  entire `deploy/` tree is uncommitted local work and there is
  nothing in version control to recover. The previous report was
  unfounded. The serverless modules (ECS Fargate / Cloud Run /
  Container Apps) stay where they are. A new
  `deploy/terraform/README.md` documents the serverless
  scope and points users running Kubernetes at the existing Helm
  chart at `deploy/helm/syntra/` instead.

- **Helm chart `appVersion`**
  (`deploy/helm/syntra/Chart.yaml`) changed from `0.2.3`
  (the Lycan crate version) to `0.2.0` (the Syntra crate version,
  read from `Cargo.toml`). A new comment above the field
  documents the convention: appVersion tracks the Syntra binary,
  not Lycan. `helm lint` still passes.

- **try-instance image** (`deploy/try-instance/`) switched
  from `FROM ghcr.io/ashhart/syntra:demo` (an image that has not
  yet been published) to a **multi-stage build from source**,
  cloned from `docker/Dockerfile.demo`. No external image
  dependency; the build is fully self-contained.
  - New `entrypoint.sh` runs `syntra serve --dev-mode` (no admin
    key) and unsets `LYCAN_ADMIN_KEY` before the binary starts so
    dev-mode is actually engaged. install.py then runs against the
    dev-mode endpoint with a synthetic header.
  - All **five** flagship capsules are now installed at startup
    (previous build omitted `shared-state-action-embeddings`,
    causing `install.py` to hard-fail at the missing dir and
    leaving the fifth capsule, `hierarchical-region-routing`,
    uninstalled).
  - Local `capsules/` mirror removed; the Dockerfile copies from
    `examples/` directly at build time (single source of
    truth).
  - New `DEPLOY.md` carries the exact copy-paste deploy commands
    for a $20/mo VPS, including DNS + Cloudflare proxy + cron
    setup.
  - End-to-end verified locally: `docker build` succeeds, container
    starts in dev-mode, all five capsules install, `/decide`
    returns a real `decisionId` + `chosen_option` + `published`
    block. `shellcheck` passes on all three shell scripts.

- **Tutorial site quickstart** (`docs/site/docs/quickstart.md`)
  rewritten Step 1 + Step 2:
  - Step 1 no longer points at the unpublished
    `ghcr.io/ashhart/syntra:demo`. Instructs `git clone` +
    `docker build -t syntra:demo -f docker/Dockerfile.demo .`
    (the same path the try-instance now uses).
  - Step 2 no longer claims the admin key is the literal string
    `"demo"`. The demo entrypoint generates a fresh
    `demo-key-<timestamp>` per container; the quickstart now tells
    operators to copy the value from the first lines of
    `docker logs`.
  - End-to-end-verified: a user following the quickstart literally
    from a fresh repo gets a working `/decide` + `/feedback` loop.

### Added — three filled tutorial-site stubs (out of nine planned)

`mkdocs build --strict` produces 28 HTML pages (was 25). Total word
count of the new content: ~2,100 words.

- [`reference/cookbook/wiring-delayed-feedback.md`](docs/site/docs/reference/cookbook/wiring-delayed-feedback.md)
  — the `decisionId` persistence pattern for outcomes that resolve
  hours or days after a decision. Covers the round-trip pattern,
  storage and drift caveats, and partial / interim feedback.

- [`reference/operations/debugging-refusals.md`](docs/site/docs/reference/operations/debugging-refusals.md)
  — when `/decide` returns `refused: true`, the three possible
  `refusalReason` values (`ood`, `interval_too_wide`,
  `insufficient_calibration_data`), what each one means, how to
  diagnose via `/memory`, and which `learning.json` knob to nudge.

- [`reference/migration/from-static-rules.md`](docs/site/docs/reference/migration/from-static-rules.md)
  — full before/after walkthrough for replacing hand-tuned
  `if/elif` rule blocks: capsule YAML, integration code in Python,
  failure modes (cold start, reward noise, fallback path).

Cookbook / operations / migration **umbrella pages** updated so the
stub-warning blocks now read "Available recipes" first (the new
pages) and "Planned recipes (not yet written)" second. The "All
decisions are being refused" stub in the cookbook is cross-linked
to the new operations page.

### What's still deferred

- The six remaining cookbook recipes, the six remaining operations
  topics, and the two remaining migration paths (from-VW,
  from-custom-bandit) — left as planned stubs. The pattern that
  emerged for the three filled pages (real worked example, three
  failure modes, cross-link to related pages) is the template for
  future content.
- Publishing `ghcr.io/ashhart/syntra:demo` to a public registry.
  Until that happens, both the Quickstart and the try-instance
  artifact build from source. When the image is published, the
  Quickstart Step 1 can swap the build for a `docker pull`.

## [Unreleased] — Phase I followup 19: adoption infrastructure + known-debt cleanup

Six parallel deliverables landed in one round. Four ship the
production-deployment surface ("how do I run this in my
infrastructure?"); two close out known runtime/observability debt.

Test count: Lycan lib 223 → 227, Lycan integration 139 → 140 (1 new
characterization test), Syntra lib 47, Syntra integration 24 — total
**438 passing** (was 433). All previously-passing tests still pass.

### Added — production deployment artifacts

- **Helm chart** at `deploy/helm/syntra/`. Chart.yaml + verbose
  values.yaml + 9 templates (Deployment / Service / PVC / ConfigMap /
  Secret / Ingress / ServiceMonitor / HPA / ServiceAccount) + README.
  `helm lint` exits 0; `helm template` renders 184 lines of valid
  Kubernetes YAML. HPA disabled by default with an explanatory comment
  (local-filesystem store needs RWX volume to scale replicas).
  Binds the chart value `syntra.adminToken` to the env var the binary
  actually reads, **`LYCAN_ADMIN_KEY`** (not `SYNTRA_ADMIN_KEY` as a
  reader might guess); store mount path is **`/syntra/data`** to
  match the demo Dockerfile.

- **Terraform modules** at `deploy/terraform/{aws,gcp,azure}/`.
  Serverless container deployments: AWS ECS Fargate + EFS + ALB + ACM;
  GCP Cloud Run + Filestore + Google-managed cert; Azure Container
  Apps + Azure Files + Application Gateway. `terraform validate`
  passes for all three. Rough monthly cost estimates in each README:
  AWS ≈ $32–35, GCP ≈ $260 (Filestore 1024 GiB minimum dominates),
  Azure ≈ $225–230 (App Gateway dominates; "front Container Apps
  directly" alternative drops to ~$45).

  **Behavior note**: the previous Terraform layout at this path used
  EKS/GKE/AKS Kubernetes modules. Those were replaced by serverless
  modules per the deliverable spec — recover from git history if the
  Kubernetes variants are still needed.

- **Tutorial site** at `docs/site/` using MkDocs + the
  mkdocs-material theme (chosen over Docusaurus for dependency
  surface: 30 Python packages vs hundreds of npm). 25 pages, 3.8 MB
  built. `./build.sh` produces `site/build/` ready for GitHub Pages
  via `mkdocs gh-deploy`. Dark theme (`#08090b` background, `#00d9ff`
  accent). Content status:
  - **Full**: home, quickstart, 6 concept pages (capsule, kernel,
    strategy node, meta-bandit, drift, refusal), API reference, 10
    example/domain-pack pages, language-clients page.
  - **Stub** (per user-specified scope cut): cookbook, operations,
    migration guides — page scaffolds with TODO checklists for
    follow-up authoring.

- **`try.syntra.io` deployment artifact** at
  `deploy/try-instance/`. Hardened Dockerfile (FROM the demo
  image with four flagship capsules pre-installed), docker-compose
  with a Traefik front (auto Let's Encrypt TLS), `reset.sh` for the
  daily wipe, `monitor.sh` for `/health` polling + webhook alert,
  `landing.html`, and a deployment README. **Built only; not
  deployed.** Hetzner CPX21 sized for ~$8/month is documented; the
  user has not committed to standing this up.

  Per-IP rate-limiting in this artifact is handled by the **Traefik
  `ratelimit` middleware** (env var `SYNTRA_RATE_LIMIT_RPM`), not by
  the Syntra binary — the binary's `RateLimiter` is process-global,
  not per-IP. Documented in the try-instance README.

### Changed — `/inspect` and `/report` disambiguate live vs. graph weights

The bandit overlay introduced in earlier work (in Active state, read
the meta-bandit leader's candidate-context bucket weights rather than
the lag-prone graph-side weights) is now explicit at the response
level rather than implicit.

- New fields on `/report` strategy entries and `/inspect` node
  entries:
  - `liveSource` — the winning candidate id (e.g., `"Thompson"`,
    `"Ucb"`, `"LinUcb"`) when in Active state with overlay present;
    `null` otherwise.
  - `graphWeights` — always the on-graph weights. (The existing
    `weights` field keeps its semantics: live in Active, graph in
    Warmup/Frozen.)
  - On `/report`, each `options[i]` also gains a `graphWeight`
    alongside the existing `weight`.
- Invariant: when `liveSource` is `null`, `weights == graphWeights`.
- Existing fields (`weightsSource`, `leaderCandidate`, `contextKey`)
  preserved unchanged for backward compatibility — downstream
  consumers like `examples/export-tool/syntra_export/` already
  read `leaderCandidate` and will continue to work.
- The overlay logic was duplicated across `do_report` and
  `inspect_graph_json`; both now call a shared
  `bandit_overlay_for_node` helper so the two endpoints can't drift.
  4 new tests cover Warmup-without-overlay, Active-with-overlay,
  node-entry shape across both states, and non-strategy-node
  exclusion. `src/server/inspect.rs`: 491 → 726 lines (+165
  helper, +190 tests, –120 in shrunken handler bodies).

### Changed — ADWIN per-layer threshold defaults

Capsule-level and per-context ADWIN now use distinct deltas. Defaults:
- `capsule_adwin_delta = 0.0005` (looser; fires on broad drift)
- `context_adwin_delta = 0.002` (tighter; fires on narrow shifts)

Chosen from a 25-cell synthetic characterization in
`tests/change_detection_characterization.rs` (drift step at
sample 100, 100/100 split between N(0.2, 0.1) and N(0.8, 0.1)):

```
| capsule\context | 0.0001 | 0.0005 | 0.0010 | 0.0020 | 0.0050 |
| **0.0001**      |   =    |   X    |   X    |   X    |   X    |
| **0.0005**      |   C    |   =    |   X    |   X    |   X    |
| **0.0010**      |   C    |   C    |   =    |   X    |   X    |
| **0.0020**      |   C    |   C    |   C    |   =    |   X    |
| **0.0050**      |   C    |   C    |   C    |   C    |   =    |
```

(`X` = context fires first, `C` = capsule first, `=` = tie.) The
chosen pair sits in the context-first region with a 3-sample
detection buffer between the two layers.

New fields on `SafetyConfig`:
- `capsule_adwin_delta: f64` (default 0.0005)
- `context_adwin_delta: f64` (default 0.002)
- Legacy `adwinDelta` JSON-key alias accepted on parse for migration.

`SafetyConfig`-driven detector wiring updated in `warmup.rs`
(new `WarmupState::with_capsule_delta`) and `learning.rs` (new
`get_or_init_context_detector_with_delta`). Server hot paths in
`src/server/feedback.rs` updated to pass the configured deltas.

**Caveat documented in `known-issues.md`**: the defaults come from
synthetic data. Real workloads may need tuning via `SafetyConfig`.
If operators observe capsule-level firing before per-context on
stable workloads, the deltas likely need adjustment.

### Deferred

- **Pareto frontier exposure for multi-objective rewards** (Prompt B
  Deliverable 3) — optional in the user's prompt, no user has asked,
  skipped this round.
- **Hosted try.syntra.io deployment** — artifact built, not deployed.
  Standing it up commits the user to ~$8–24/month VPS spend plus
  ongoing operational responsibility (daily reset cron, health
  monitor); decision deferred to the user.
- **Cookbook / operations / migration tutorial pages** — scaffolded
  per user-specified scope cut. Content authoring is a separate
  follow-up.

## [Unreleased] — Phase I followup 18: server.rs split into server/ module

Pure mechanical refactor. `src/server.rs` (4130 lines) split
into a `src/server/` directory with one file per responsibility.
No behavior changes. No bug fixes. No reorganizing logic. 294/294
tests still passing (Lycan lib 223 + Syntra lib 47 + Syntra
integration 24, same counts as before).

### File layout

```
src/server/
├── mod.rs               # 110 lines: ServerConfig, run_server, module decls
├── state.rs             #  39 lines: SharedState, State, CapsuleLockManager
├── errors.rs            #  62 lines: Resp + response builders + body parsers
├── metrics.rs           # 179 lines: Metrics, LatencyHistogram, render_metrics
├── auth.rs              # 139 lines: AuthOutcome + authn/authz + rate_limit
├── helpers.rs           # 179 lines: audit_event, body reading, graph utils
├── admin.rs             # 735 lines: ADMIN_HTML + admin_html + list_admin_capsules
├── inspect.rs           # 490 lines: inspect/report/chaos/evaluate/evolve
├── decide.rs            # 922 lines: do_decide + do_decide_hierarchical
├── feedback.rs          # 667 lines: do_feedback + do_feedback_hierarchical
└── routes.rs            # 693 lines: fn route (the big match)
```

(`install.rs`, `learning_admin.rs`, `health.rs`, `rate_limit.rs` from
the original prompt skeleton were not created. Those routes are
inline match arms in `route()` and stay inline in `routes.rs`. Pulling
them out into single-line helper fns would have been reorganizing
logic rather than splitting responsibilities, which the prompt
explicitly forbids.)

### Visibility discipline

The only visibility change permitted by a pure refactor is the
mechanical one needed to keep cross-module access at exactly its
prior reach. Items shared between sibling modules in `server::*` are
marked `pub(super)`; struct fields accessed across siblings are
`pub(super)`. The two original `pub` items (`ServerConfig`,
`run_server`) keep `pub` visibility from `mod.rs`. The two
`pub(crate)` graph helpers (`primary_choice_node`,
`all_choice_nodes`) are re-exported from `mod.rs` so their existing
path `crate::server::primary_choice_node` continues to work for any
future caller.

### Validation

`/tmp/refac_replay.sh` drove 35 rounds of `/decide` + `/feedback`
against the refactored binary and captured `/memory`, `/report`,
`/admin/capsules`, and the structural shape of a final `/decide`
response. The byte-level baseline md5 doesn't match across runs
because the bandit's candidate selection is stochastic (`rand_f64`
draws differ each run); we observed `featureVector` emission in
3 of 6 follow-up runs, consistent with LinUcb/LinTs selection
probability. All 7 meta-bandit candidates (Thompson, UCB, Weighted,
EpsilonGreedy, Greedy, LinUcb, LinTs) appear across the runs and
the response top-level keys are stable.

### Docs touched

Stale line-number references to `server.rs` updated in
`docs/roadmap.md`, `docs/known-issues.md`,
`docs/api.md`,
`docs/investigations/greedy-lock-2026-05.md`. Conceptual
references in `src/hierarchical.rs`, `src/learning.rs`,
`src/shared_state_strategy.rs`,
`src/capsule_spec.rs`, `docs/runbook.md`,
`docs/whats-new-G-H.md` (which say "server.rs does X"
abstractly) left untouched — "server.rs" reads as a stand-in for
"the server module" in those contexts.

## [Unreleased] — Phase I followup 17: PITCH.md mentions all three adaptive flavors

Single-sentence refresh of the sendable pitch. Previously its
"what Syntra does" enumeration said "A meta-bandit picks the algorithm
... Seven candidate algorithms run in parallel under the hood";
hierarchical and shared-state weren't acknowledged in the doc that
gets sent to prospective users. Updated to say three structural
flavors are wired (flat / shared-state / hierarchical) and pick
themselves from the capsule's config — same `/decide` API for all
three.

### Changed

- `PITCH.md` step 3 in the "what Syntra does" enumeration
  replaces the meta-bandit-only framing with a three-flavor sentence.
  Net word count change: +25 words. PITCH.md now sits at 951 words
  (under the 1000-word sendable budget).

### Not changed

- Headline, fall-back-on-failure callout, MoEfolio production
  reference, install + decide curl block, "what Syntra is not" list,
  honesty-rooted "what it is" close. The pitch's structure and tone
  are unchanged; only the technical-capability bullet learned about
  the other two flavors.

## [Unreleased] — Phase I followup 16: Discounted reward propagation for hierarchical bandits

Additive capability on top of the hierarchical-bandit wiring landed
in followups 4–14. Operators can now dial down how much leaf-level
reward variance propagates upward to root-level meta-bandit
exploration. Backward-compatible — existing capsules behave exactly
as today.

### Added

- **`RewardPropagation` enum** in `src/hierarchical.rs`:
  - `Full` (default) — every level along the path sees the same
    reward unchanged. Matches pre-followup-16 behavior.
  - `Discounted { factor: f64 }` — per-level reward at depth `d` of a
    length-`N` path is `reward * factor.powi(N - 1 - d)`. Leaf gets
    the full reward; root is attenuated by `factor^(N-1)`. `factor =
    1.0` is mathematically equivalent to `Full`.
- **`HierarchicalSpec.reward_propagation: Option<RewardPropagation>`**
  field. `None` defaults to `Full`. Only the outermost spec's setting
  is read (nested `sub_capsule` entries' values are accepted by serde
  but ignored). Serializes only when set, so existing
  `hierarchical_spec.json` sidecars are unchanged.
- **YAML knob**: `reward_propagation: { mode: discounted, factor: 0.5 }`
  (or `{ mode: full }`) at the top level of `hierarchical_options`
  in `capsule.yaml`. Worked example in
  `docs/capsule-features/hierarchical-bandits.md`.
- 4 new unit tests:
  - `propagate_reward_discounted_attenuates_root_relative_to_leaf` —
    math-layer check that depth-0 reward is `factor^2` of leaf in a
    3-level tree.
  - `propagate_reward_full_is_default_and_returns_raw_reward` — `None`
    and `Some(Full)` and `Some(Discounted{1.0})` are all equivalent.
  - `reward_propagation_round_trips_through_json` — serde round-trip,
    absent field round-trips as `None` (not `Some(Full)`).
  - `apply_feedback_discounted_attenuates_root_bucket_stats` —
    runtime-layer check that bucket `reward_sum` on disk reflects the
    discount.

### Changed

- **`propagate_reward` in `src/hierarchical.rs`** now honors the
  spec's propagation mode. Existing callers (math-layer tests, the
  `apply_feedback_inner` in `hierarchical_state.rs`) see the
  propagated rewards directly; with `Full` set or absent the
  per-level reward equals the input reward, so backward compatibility
  is preserved.
- **`HierarchicalCapsuleState::apply_feedback_inner`** now reads its
  per-level rewards from `propagate_reward` instead of using the raw
  input reward at every level. The weight-update step, the per-arm
  stat accumulation, and the meta-bandit `record` call all use the
  level-specific reward.

### Verified end to end

Two capsules installed on a fresh `syntra serve --dev-mode`:
- **Capsule A** (Full propagation): 40 rounds rewarding `us_b` at 1.0.
  Root bucket `d0|` reward_sums = `[15.0, 0.0]`; us subtree `d1|0`
  reward_sums = `[0.0, 15.0, 0.0]`. Root sum equals leaf sum — no
  attenuation, as expected.
- **Capsule B** (Discounted, factor 0.5): same 40-round protocol.
  Root reward_sums = `[4.0, 0.0]`; us subtree reward_sums = `[0.0,
  8.0, 0.0]`. **Root is exactly half the leaf**, matching the
  `factor^(N-1-depth) = 0.5^1 = 0.5` attenuation expected at the
  root of a 2-level tree.

### Tests

- 223 Lycan lib (was 219 → +4 new tests).
- 47 Syntra lib unchanged.
- 24 CLI integration unchanged.
- No regressions.

### Not changed

- The YAML schema for the recursive `sub_capsule` tree is unchanged.
  No reshape to a flat-list-with-children-map form.
- The `/decide` response shape is unchanged. `decisions[0].path /
  leafName / perLevelCandidateIds` remain the source of truth.
- POSITIONING.md is unchanged. The headline ("hierarchical bandits
  wired in") was already accurate.

## [Unreleased] — Phase I followup 15: concept doc covers all three adaptive flavors

Documentation-only sweep adding hierarchical bandits to
`docs/concepts/operational-intelligence.md` and introducing
the broader "adaptive flavors" framing. The doc was written when only
the meta-bandit flavor existed; shared-state LinUCB (followup 2) and
hierarchical (followups 4–14) were never mentioned at the concept
layer despite being first-class capabilities.

### Changed

- **New "Adaptive flavors" section** between the kernel discussion
  and "What this is not". Describes the three flavors orthogonally:
  - Meta-bandit over per-option LinUCB (default).
  - Shared-state LinUCB — when options carry semantic similarity.
  - Hierarchical bandits — when the action space factors into a tree.
  Notes that the flavors are orthogonal to the kernel-feature story:
  a capsule can mix-and-match (e.g. EWMA + shared-state LinUCB).
- **ASCII pattern diagram** updated: the strategy-node box now
  describes the bandit pick as "flavor depends on capsule config"
  rather than "meta-bandit picks one" — the latter was only accurate
  for flavor 1.
- **"Where to go next" section** expanded with links to
  shared-state-action-embeddings and hierarchical-region-routing
  worked examples plus the two capsule-features concept docs that
  the original version omitted.

### What stays

- The kernel-feature pattern (the bulk of the doc) is unchanged. It
  still describes the canonical request → inputGet → compute features
  → bandit pick → response flow.
- The "What this is not" honesty list is unchanged.
- The `runtime.publish` section from followup 2c is unchanged.

## [Unreleased] — Phase I followup 14: hierarchical feedback credits the actual fired candidate

The hierarchical `/feedback` path now credits the per-level
meta-bandit candidate that actually fired at decide time, rather than
falling back to the current-leader greedy proxy. This closes the
last of the v1 limitations carried in roadmap.md "Future polish".

### Added

- **`HierarchicalCapsuleState::apply_feedback_with_candidates`**
  (`src/hierarchical_state.rs`): new sibling method taking an
  explicit `per_level_candidates: &[CandidateId]` argument. Credits
  each level's meta-bandit with the supplied id rather than the
  greedy-leader proxy. Length-mismatch falls back to the proxy as a
  data-integrity safeguard rather than silently using a partial
  mapping.
- **2 new unit tests**: `apply_feedback_with_candidates_credits_supplied_candidate`
  asserts Weighted/EpsilonGreedy specifically receive credit when
  supplied; `apply_feedback_with_candidates_falls_back_on_length_mismatch`
  proves the fallback path doesn't silently misattribute.

### Changed

- **`do_feedback_hierarchical` in `src/server.rs`** now recovers
  `perLevelCandidateIds` from the decision-log event (parsed back to
  `CandidateId` via `from_str`) and calls the new method instead of
  the original `apply_feedback`. Decision events already carried the
  field — followup 4 wrote them; we just weren't reading them back.
- **Original `apply_feedback`** unchanged. Math-layer tests that
  don't track candidate provenance continue to call it with the
  greedy-proxy semantics.

### Verified end to end

30-round run against the hierarchical demo capsule. Before this fix
the persisted `metaBandit.candidates[*].trials` would have shown ~30
trials concentrated on one candidate (the greedy leader at each
level). With the fix, trials distribute across the candidates that
actually fired:

```
bucket d0|:
  Thompson        trials=7.86  cum_reward=3.93
  Greedy          trials=7.91  cum_reward=3.96
  EpsilonGreedy   trials=6.88  cum_reward=3.44
  Ucb             trials=5.91  cum_reward=2.96
  Weighted        trials=1.00  cum_reward=0.50
```

Histogram of `(level0_candidate, level1_candidate)` pairs across the
30 decides matches the trial counts within the meta-bandit's 0.999
forgetting-factor decay. The fix makes downstream meta-bandit
selection logic (`exploration_probability`, `current_leader`)
honest about which candidate is genuinely performing best at each
level rather than reinforcing the lead of whichever candidate
happened to be first to converge.

### Test counts

- 219 Lycan lib (was 217 → +2 new tests).
- 47 Syntra lib unchanged.
- 24 CLI integration unchanged.
- No regressions in any existing flat / multi-decision / shared-state
  path.

### Roadmap status

After this followup, `docs/roadmap.md` "Future polish" has
been crossed out for the per-level candidate id threading. Two items
remain queued for future work: graph execution inside hierarchical
decides (so `runtime.publish` fires) and refusal/OOD wiring for
hierarchical capsules. Both are smaller deltas than this one and
documented in the roadmap.

## [Unreleased] — Phase I followup 13: Docker doc refresh for the 5-capsule line-up

Documentation-only sweep aligning `docker/README.md` and
`docker/demo/VERIFIED.md` with the five-capsule reality from
followup 12.

### Changed

- **`docker/README.md`**:
  - Header capsule list updated from "four flagship capsules" to
    "five flagship capsules, one per adaptive flavor plus a
    multi-decision example" with explicit `meta-bandit /
    shared-state LinUCB / hierarchical bandit` flavor labels.
  - `SYNTRA_DEMO_CAPSULE` table grew to five rows + a new "Adaptive
    flavor" column so operators can pick by flavor name.
  - Region 2 description extended to cover the hierarchical case
    (one line per HierState bucket, labelled with bucket key + the
    meta-bandit candidate currently leading that level).
  - Region 5 per-capsule expectations added a hierarchical entry
    explaining why the placeholder shows (`.lycs` graph not executed
    in v1).
  - "Three of the four" idle-capsule line updated to "Four of the
    five".

- **`docker/demo/VERIFIED.md`** (appended, not rewritten —
  the original Part-3 four-capsule observations are preserved as the
  historical record):
  - New "Follow-up verification — 2026-05-18" section at the bottom
    documenting:
    - The five-line `install.py` output with the new
      `+ hierarchical_spec.json` annotation on the hierarchical
      capsule.
    - `/admin/capsules` post-install showing all five with distinct
      `scoringMode` values.
    - A `SYNTRA_DEMO_CAPSULE=hierarchical-region-routing` run with
      the captured per-bucket convergence: root `[0.67, 0.33]`, us
      subtree `[0.22, 0.45, 0.34]`, eu subtree
      `[0.15, 0.55, 0.30]` (both subtrees independently learning
      `medium > large > small`).
    - Two new rough-edge notes: (a) hierarchical capsules don't
      execute their graph in v1, so `runtime.publish` calls don't
      fire; (b) `chosen_option` is always `0` for hierarchical
      decisions and clients should read `decisions[0].leafName`.

### Verified

- Both files compile (`python3 -m py_compile` doesn't apply, but
  grep sweep for stale "four" references comes back empty except for
  two post-update-accurate uses where "four" correctly means "the
  other four when one is being driven").
- The CHANGELOG entry above this one (followup 12) closes the actual
  Docker wiring; this entry is purely the doc catch-up.

## [Unreleased] — Phase I followup 12: hierarchical demo ships in Docker image

The Docker demo image now installs and drives the hierarchical
capsule alongside the four flagships. `docker run syntra:demo`
brings up five capsules in the dropdown without any extra install
step.

### Changed

- **`docker/Dockerfile.demo`**: new COPY layer for
  `examples/hierarchical-region-routing/`.
- **`docker/demo/capsule/install.py`**: adds
  `hierarchical-region-routing` to the install list and uploads
  `hierarchical_spec.json` (when present in a capsule's source dir)
  via `PUT /tenants/.../hierarchical_spec`. Install log line now
  shows the extra `+ hierarchical_spec.json` annotation for
  hierarchical capsules. Idempotent — re-running against a
  populated store replaces all three sidecars in place.
- **`docker/demo/entrypoint.sh`**: case-arm for
  `SYNTRA_DEMO_CAPSULE=hierarchical-region-routing` mapping it to
  `demo/region/router`.
- **`docker/demo/traffic/generate.py`**:
  - Adds `hierarchical-region-routing` to `CAPSULE_PATHS`,
    `OPTIONS`, `STEP_FNS`, `REWARD_FNS`.
  - Reward function: region bonus (us 0.30, eu 0.00) + size bonus
    (small 0.00, medium 0.40, large 0.20) + small noise. Produces a
    clean per-level signal at both the root (region) and child
    (size) buckets.
  - Driver loop now handles hierarchical decide responses, which
    carry the chosen leaf in `decisions[0].leafName` rather than as
    an integer `chosen_option`. Falls through to the index-based
    lookup for the other flavors.

### Verified end to end

Ran `install.py` against a fresh `syntra serve --dev-mode` — all
five capsules installed with the expected scoring modes:

```
demo/autoscale/orders     scoringMode=meta-bandit          options=[option_0..option_3]
demo/embeddings/router    scoringMode=shared-state-linucb  options=[A, B, C, D, E, F]
demo/fraud/threshold      scoringMode=meta-bandit          options=[option_0..option_3]
demo/region/router        scoringMode=hierarchical         options=[us_small..eu_large]
demo/routing/api          scoringMode=meta-bandit          options=[option_0..option_3]
```

Drove the hierarchical capsule with the traffic generator for ~8
seconds at 0.05s tick interval (106 decisions). Final bucket
weights:

- `d0|` (root): `[0.667, 0.333]` — us preferred at 67%, matching
  the +0.30 region bonus.
- `d1|0` (us subtree): `[0.215, 0.448, 0.337]` — medium (45%) >
  large (34%) > small (21%), matching the size bonuses.
- `d1|1` (eu subtree): `[0.149, 0.548, 0.303]` — same medium-first
  ordering. Both sub-buckets converged on the size signal even
  though the regions differ — that's the hierarchical-bandit
  benefit of credit sharing within each parent.

### What this unlocks

Anyone running `docker run --rm -p 8080:8080 syntra:demo` now sees
all three adaptive flavors live in the dashboard dropdown:
meta-bandit (flat capsules), shared-state-linucb (action-embedding
capsules), and hierarchical (tree-structured capsules). The
`SYNTRA_DEMO_CAPSULE=hierarchical-region-routing` env var lets
operators point the traffic generator at the hierarchical capsule
specifically to watch the per-level convergence in real time.

## [Unreleased] — Phase I followup 11: hierarchical concept doc refreshed

Documentation-only sweep removing stale "queued" / "prep" / "not yet
wired" framing from `docs/capsule-features/hierarchical-bandits.md`.
The doc was written when hierarchical bandits were still prep-only;
followups 4–10 (May 2026) closed the runtime wiring end to end, and
the doc now reflects that.

### Changed

- **Status callout at the top of the doc** explicitly states the
  feature is wired end to end and references the v1 limitations in
  `docs/roadmap.md`.
- **YAML schema section** updated from the raw `HierarchicalSpec`
  shape (which only the in-process tests could consume) to the full
  `CapsuleSpec` shape with `hierarchical_options:` + the flat
  `options:` list that `syntra author` accepts. Adds the
  globally-unique-leaf-name convention with rationale.
- **New "Install flow" section** walks through the three-step
  install: `syntra author` → `POST /install` → `PUT /hierarchical_spec`.
  Notes what falls back to flat-AdaptiveChoice when step 3 is
  skipped.
- **`/decide` and `/feedback` shape**: rewritten with actual response
  bodies captured from a live run (the prior version showed a
  hypothetical pre-wiring shape that didn't match reality).
  `/feedback` response now includes the `levelsUpdated` field added
  in followup 7.
- **New "Validated convergence" section** captures the 100-round
  end-to-end test results (root weights `[0.94, 0.06]`, us-subtree
  `[0.05, 0.91, 0.04]`) so a reader has a concrete number for what
  convergence looks like in practice.
- **"Worked example and persistence" section** updated to mention
  the dashboard's per-bucket summary in `/api/state.hierarchical`
  (followup 10) and the actual install path against a running
  binary, not just the in-process test.

The "Where this fits in the appliance" closing section stays
conceptual and unchanged.

### Verified

`grep -E "queued|follow-up|not yet|reserved for|prep|will route|once.*lands|planned"`
on the refreshed doc returns one match — describing per-candidate
selection inside each level as actually-future-polish work. That's
accurate, not stale.

## [Unreleased] — Phase I followup 10: dashboard renders hierarchical capsules

The demo dashboard now special-cases hierarchical capsules in Region
2 (the reward chart). Previously it had two render paths — 7 candidate
lines for meta-bandit, 1 line for shared-state — and hierarchical
capsules fell through to the meta-bandit path with empty state. After
this followup, hierarchical capsules render one line per HierState
bucket showing the currently-leading meta-bandit candidate's mean
reward.

### Changed

- **`docker/demo/dashboard/app.py`** `_detect_scoring_mode` now
  recognises hierarchical capsules by the presence of
  `hierarchical_spec.json` on disk. Detection order matches
  `/admin/capsules`: hierarchical → shared-state-linucb → meta-bandit.
- **New helper `_load_hierarchical_summary(disk_dir)`** reads
  `hierarchical_state.json` and emits a compact per-bucket summary:
  `{key, depth, parentPath, branchingFactor, totalRounds,
  currentLeader, leaderMean, weights}` per bucket. Returns `null` for
  freshly-installed capsules with no state file yet. The bucket key
  (e.g. `d0|`, `d1|0`) is parsed for depth + parent path so consumers
  don't have to.
- **`/api/state`** carries a new top-level `hierarchical` field with
  the summary above. Null for meta-bandit / shared-state capsules.
- **`docker/demo/dashboard/static/dashboard.js`** `pushChartSamples`
  now has three branches: shared-state (1 line), hierarchical (one
  line per bucket, labeled with key + currently-leading
  CandidateId), meta-bandit (7 candidate lines). `setChartChrome` sets
  the chart subtitle to "last 5 minutes · per-HierState
  meta-bandits" for hierarchical capsules.

### Verified end to end

Hierarchical demo capsule installed, 40 rounds rewarding only
`us_medium`. `/api/state.hierarchical.buckets` returned:

| bucket | leader   | leaderMean | totalRounds | weights              |
|--------|----------|-----------:|------------:|----------------------|
| `d0|`  | Thompson | 0.376      | 40          | [0.76, 0.24]         |
| `d1|0` | Thompson | 0.578      | 26          | [0.13, 0.73, 0.14]   |
| `d1|1` | Thompson | 0.000      | 14          | [0.36, 0.32, 0.32]   |

The dashboard chart renders three lines (one per bucket) showing the
us-branch (`d1|0`) has converged on a clear winner while the eu-branch
(`d1|1`) is still flat (zero reward observed). That's the per-level
signal hierarchical bandits exist to surface.

## [Unreleased] — Phase I followup 9: hierarchical demo installs via `syntra author`

The hierarchical-region-routing example capsule has been rewritten
from a raw `HierarchicalSpec` YAML (which only the in-process
`src/hierarchical_state.rs` tests could consume) into a proper
CapsuleSpec that `syntra author` can compile and the runtime can
install end to end. The demo is now demoable in the same one-line
flow as the flat capsules.

### Changed

- **`examples/hierarchical-region-routing/capsule.yaml`**:
  rewritten to the CapsuleSpec shape with `name`, `version`,
  top-level flat `options` (`us_small, us_medium, us_large, eu_small,
  eu_medium, eu_large` — equal to
  `hierarchical_options.enumerate_paths().map(resolve_path)`),
  `reward`, and the nested `hierarchical_options:` tree. Sub-tree
  leaf names are globally unique so the flat-options compat check in
  `CapsuleSpec.validate_hierarchical` passes.
- **`examples/hierarchical-region-routing/program.lycs`,
  `program.lyc`, `hierarchical_spec.json`, `learning.json`,
  `reward_spec.json`, `context_schema.json`, `manifest.json`**: all
  auto-emitted by `syntra author capsule.yaml --out-dir .`. The
  manifest carries `"sidecars": ["hierarchical_spec.json"]`,
  matching the new `capsule_compiler` shape from followup 4.
- **`examples/hierarchical-region-routing/README.md`**:
  rewritten to drop the "Status: prep" framing and document the full
  install flow (`syntra author` → POST .lyc → PUT
  `/hierarchical_spec`), the actual `/decide` response shape from a
  live run, the validated convergence numbers from followup 7's
  end-to-end test, and the v1 limitations that still apply.

### Verified end to end

`syntra author capsule.yaml --out-dir .` returned
`{"bytes":2286,"edges":11,"nodes":42,"ok":true,"options":6}` —
expected for a 6-leaf hierarchical capsule. The emitted bundle
installed cleanly via `POST /install` + `PUT /hierarchical_spec`,
`/admin/capsules` reported `scoringMode: "hierarchical"` with real
leaf labels, and 50 rewarded rounds converged the root bucket to
`[0.70, 0.30]` and the us sub-bucket to `[0.11, 0.79, 0.10]` —
consistent with the 100-round trajectory captured in followup 7.

## [Unreleased] — Phase I followup 8: `/admin/capsules` detects hierarchical capsules

A small polish on top of followups 4–7 so hierarchical capsules
appear in the dashboard's capsule switcher with the right label.

### Changed

- **`/admin/capsules` (`list_admin_capsules`)** now checks for a
  `hierarchical_spec.json` sidecar first and reports
  `scoringMode: "hierarchical"` when present. Detection order is
  hierarchical → shared-state-linucb → meta-bandit (default). The
  `options` field for hierarchical capsules carries the *real* leaf
  names from `enumerate_paths().map(resolve_path)` — one notch better
  than the `option_0..option_{n-1}` placeholders meta-bandit capsules
  fall back to, because the tree carries proper labels.
- New integration test
  `admin_capsules_reports_hierarchical_scoring_mode_with_real_leaf_labels`
  in `tests/syntra_cli.rs` covers PUT-then-list flow against a
  2×2 hierarchical capsule and asserts both the scoring mode and the
  exact `enumerate_paths` leaf order.

### Verified end to end

Against a fresh `syntra serve --dev-mode` with one capsule of each
flavor installed, `GET /admin/capsules` returned:

| path                        | scoringMode             | options                                              |
|-----------------------------|-------------------------|------------------------------------------------------|
| demo/scale/autoscaler       | `meta-bandit`           | `[option_0, option_1, option_2, option_3]`           |
| demo/embeddings/router      | `shared-state-linucb`   | `[A, B, C, D, E, F]`                                 |
| demo/region/router          | `hierarchical`          | `[us_small, us_medium, eu_small, eu_medium]`         |

All three flavors are now distinguishable at the listing layer, which
is what the dashboard switcher needs to render them correctly.

### Tests

- 217 Lycan lib (unchanged) + 47 Syntra lib (unchanged) + 24 CLI
  integration (was 23 → +1).
- No regressions.

## [Unreleased] — Phase I followup 7: hierarchical bandits /feedback branch (third adaptive flavor closed)

Roadmap step 4 — hierarchical capsules are now fully reachable end
to end through the API. With this entry, the third adaptive flavor
joins the meta-bandit-over-per-option-LinUCB default and the
shared-state LinUCB flavor as a complete capability.

### Added

- **`do_feedback_hierarchical` in `src/server.rs`**: dispatched
  from the top of `do_feedback` when the capsule has a
  `hierarchical_spec.json` sidecar. Parses reward via the same
  surface as the flat path (`reward`, `components` + `rewardSpec`,
  or `outcome` + `reward_policy`), updates warmup state for
  `/report` consistency, looks up the decision by `decisionId`,
  extracts the recorded `path` from `decisions[0].path`, calls
  `HierarchicalCapsuleState::apply_feedback(&path, &path, reward)`
  to propagate the observed reward across every level of the tree,
  persists the updated state via
  `save_hierarchical_state_in_job`, and writes audit + feedback log
  entries that mirror the flat path's shape with `kind:
  "hierarchical"` markers.

### Verified end to end

A 2×3 hierarchical capsule (regions × server-types, 6 leaves total)
installed on a fresh `syntra serve --dev-mode`, then driven through
100 `/decide` + `/feedback` rounds where only the `us_medium` leaf
path `[0, 1]` was rewarded at 1.0 (every other leaf at 0.0):

- **Root bucket `d0|`** converged to weights `[0.94, 0.06]` — the
  bandit learned to prefer the `us` branch (93.5% selection share).
- **us sub-bucket `d1|0`** converged to weights
  `[0.05, 0.91, 0.04]` — within the `us` branch, the bandit learned
  `medium` is best (90.8% selection share).
- **eu sub-bucket `d1|1`** stayed near-uniform `[0.42, 0.34, 0.25]`
  with only 16 rounds of observation — expected, because the `eu`
  branch was selected 16 times and all received reward 0, providing
  no signal to differentiate the three eu leaves.
- **Leaf histogram in the last 30 of 100 rounds**: `us_medium`
  chosen 26/30 times (87%).
- `totalRounds` advances correctly: root 100, us 84, eu 16
  (summing the per-level updates).

This is exactly the convergence behavior the math layer's in-process
test demonstrated; the runtime now exposes it through the API.

### Changed

- `do_feedback`'s entry path is unchanged for flat / multi-decision /
  shared-state capsules — the hierarchical branch is a pure early
  dispatch on the sidecar presence. No regressions in the 217 Lycan
  lib tests, 139 Syntra crate tests, or 23 CLI integration tests.

### What this closes

The third adaptive flavor in Syntra's positioning is now wired all
the way through. POSITIONING.md's claim that "hierarchical bandits
are queued, see roadmap.md" no longer applies. The three adaptive
flavors reachable through the unified `/decide` and `/feedback` API:

1. **Meta-bandit over per-option LinUCB** (default flat capsules,
   Phase A–H).
2. **Shared-state LinUCB** (Phase I followup 2, May 2026) — single
   θ over `[x_context, x_option]` for capsules whose options carry
   semantic similarity.
3. **Hierarchical bandits** (Phase I followups 4–7, this round) —
   nested tree of per-level meta-bandits with reward propagation
   along the chosen path.

Operators select among them via `learning.json::sharedState.enabled`
(flavor 2) or `PUT /hierarchical_spec` after install (flavor 3);
absent both, flavor 1 is the default.

### v1 limitations carrying forward (tracked in roadmap.md "Future polish")

- The capsule's `.lyc` graph is **not executed** for hierarchical
  decides. `runtime.publish` calls in a hierarchical capsule's
  `.lycs` body do not fire. Selection happens entirely outside the
  executor in v1. Lifting this is a follow-up.
- Refusal / OOD / conformal calibration not yet wired for
  hierarchical. Hierarchical decides always return `refused: false`.
- `apply_feedback` credits the per-level meta-bandit's current leader
  as a greedy proxy rather than the candidate actually selected at
  decide time. Threading the per-level candidate id back into
  feedback is queued.

## [Unreleased] — Phase I followup 6: hierarchical bandits /decide branch

Roadmap step 3 — hierarchical capsules are now reachable through the
real `/decide` API. Step 4 (`/feedback`) is the only remaining
blocker to closing the third adaptive flavor end to end.

### Added

- **`do_decide_hierarchical` in `src/server.rs`**: dispatched
  from the top of `do_decide` when the capsule has a
  `hierarchical_spec.json` sidecar. Loads the spec + state, walks the
  tree via `HierarchicalCapsuleState::select_path`, persists the
  updated state via `save_hierarchical_state_in_job`, and writes a
  decision-log entry whose `decisions[0]` carries the new fields:
  `kind: "hierarchical"`, `path: [int,…]`, `leafName: string`,
  `perLevelCandidateIds: [CandidateId,…]`. The response top-level
  carries `algorithm: "hierarchical"` so dashboards / clients can
  detect the flavor.
- **`GET` / `PUT /tenants/.../hierarchical_spec`** endpoints
  mirroring the `/learning` pattern. `PUT` validates the JSON via
  `HierarchicalSpec::validate()` and atomically writes the sidecar
  into the runtime store, so an operator who compiled their capsule
  out-of-band can upload `hierarchical_spec.json` after `/install`.
  Returns `{"ok": true, "leaves": <n>, "depth": <d>}` on success.
- **`LycanStore::save_hierarchical_spec_in_job`**: counterpart to the
  load helper from followup 5. Runs `spec.validate()` before writing.

### Changed

- `do_decide`'s entry path is unchanged for flat / multi-decision /
  shared-state capsules — the hierarchical branch is a pure early
  dispatch. No regressions in the existing 217 Lycan lib tests, 23 CLI
  integration tests, or 47 Syntra lib tests.

### Verified end to end

A fresh `syntra serve --dev-mode` with a hand-uploaded 2x3
hierarchical spec (regions × server-types):

- `PUT /hierarchical_spec` returns `{"ok": true, "leaves": 6, "depth": 2}`.
- `GET /hierarchical_spec` round-trips the tree cleanly.
- 30 `/decide` calls with no feedback distribute near-uniformly across
  the 6 leaves (6/6/5/5/4/4) — expected behaviour for per-level meta-
  bandits in pure exploration.
- `hierarchical_state.json` (9.4 KB) is persisted in the capsule
  directory; the three buckets (`d0|`, `d1|0`, `d1|1`) all show
  `totalRounds: 0` (because step 4 isn't wired yet — feedback is
  the only thing that advances totalRounds).
- Decision JSONL entries carry the new shape with full audit trail.

### v1 limitations (documented in roadmap.md)

- **Graph is not executed for hierarchical capsules.** The `.lyc` is
  decorative for legacy compat; `runtime.publish` and any `!cap` calls
  in the capsule body do not fire. Selection happens entirely outside
  the executor.
- **Warmup gating is bypassed.** Per-level meta-bandits handle their
  own exploration via the rate-adaptive schedule.
- **Refusal / OOD / conformal** are not wired for hierarchical.

### Not done in this round (queued in roadmap.md)

- `/feedback` branch — step 4. Without it, `apply_feedback` is never
  called, so the per-level meta-bandits' `totalRounds` stays at 0 and
  the bucket weights never update. Decide works; feedback is a
  one-tick follow-up.

## [Unreleased] — Phase I followup 5: hierarchical bandits store layer

Roadmap step 5 — persistence helpers in `LycanStore` for the
hierarchical-tree spec and the per-`HierState` bandit state. With this
landed, the runtime branches in steps 3 and 4 have everything they
need to read and persist hierarchical capsule state through the same
sidecar pattern the rest of the runtime uses (`warmup.json`,
`memory.json`).

### Added

- **`LycanStore::load_hierarchical_spec_in_job`**: reads the
  `hierarchical_spec.json` sidecar written by `capsule_compiler` at
  install time. Returns `None` for flat capsules with no sidecar; the
  runtime branch in step 3 will use this to detect "treat as flat".
- **`LycanStore::load_hierarchical_state_in_job`**: reads the
  `hierarchical_state.json` sidecar containing the per-`HierState`
  bandit buckets. Returns `None` when the capsule is freshly installed
  and has no state yet; the runtime can lazily construct an empty
  state at first `/decide`.
- **`LycanStore::save_hierarchical_state_in_job`**: atomic-write
  matching the existing `save_warmup_state_in_job` /
  `save_memory_in_job` pattern. Propagates I/O errors as `String`.
- 3 new tests covering spec load round-trip, state save/load with
  structural equality (tree shape + bucket keys + weights to 1e-9
  precision — serde_json's numeric round-trip drops 1 ULP per f64
  value, so byte-for-byte JSON-string equality isn't a meaningful
  assertion at this layer; the structural check is what matters), and
  the absence-of-sidecar path on legacy flat capsules.

### Changed

- `docs/roadmap.md` updated to mark step 5 complete. Steps 3–4
  (server.rs `do_decide` + `do_feedback` branches) remain queued and
  are the only blockers to hierarchical capsules being reachable
  through the API.

### Not done in this round (queued in roadmap.md)

- `src/server.rs` `do_decide` branch that calls
  `load_hierarchical_spec_in_job` + `load_hierarchical_state_in_job`
  on `/decide` and walks the tree per level via
  `HierarchicalCapsuleState::select_path`.
- `src/server.rs` `do_feedback` matching branch that calls
  `apply_feedback` and persists via `save_hierarchical_state_in_job`.

After steps 3 and 4 land, hierarchical capsules will be a third
adaptive flavor reachable through the same `/decide` and `/feedback`
contract — joining the meta-bandit-over-per-option-LinUCB default and
shared-state LinUCB.

## [Unreleased] — Phase I followup 4: hierarchical bandits spec + install layer

First half of the hierarchical-bandits runtime wiring (steps 1+2 of
`docs/roadmap.md`). The capsule spec now accepts a
hierarchical-tree option set; the install pipeline persists it as a
sidecar JSON next to the compiled `.lyc`. The runtime branches in
`do_decide` / `do_feedback` and the matching store loader are queued
for follow-up ticks (steps 3–5).

### Added

- **`CapsuleSpec.hierarchical_options`** (`src/capsule_spec.rs`,
  re-exports `lycan::hierarchical::HierarchicalSpec`). Optional field
  declaring a nested-tree option set. Validation rules:
  - `hier.validate()` must succeed (depth ≤ 4, ≥ 2 options per level,
    unique branch names, valid reward shapes).
  - Mutually exclusive with `decisions[]` — a capsule is either a
    sequential DAG or a nested tree, not both.
  - Flat `options[]` must equal the `enumerate_paths().map(resolve_path)`
    sequence so the legacy single-decision view stays consistent.
- **`hierarchical_spec.json` sidecar** emitted by
  `src/capsule_compiler.rs::compile_to_dir` when
  `hierarchical_options` is present. Round-trips cleanly through
  `HierarchicalSpec::from_json` (preserves `subCapsule` camelCase
  keys). `manifest.json` gains a `sidecars` array referencing the
  file so an operator listing the install directory can see at a
  glance which optional capabilities are wired.
- 5 new tests in `capsule_spec.rs` covering parse, mismatched flat
  options, mutual-exclusion with decisions, invalid internal shape,
  and confirmation that pre-existing flat capsules are unaffected.
- 2 new tests in `capsule_compiler.rs` covering sidecar emission,
  manifest-pointer presence, and confirmation that flat capsules
  emit no sidecar and have an empty `sidecars` array.

### Changed

- `manifest.json` now carries a `sidecars` array (empty for flat
  capsules, `["hierarchical_spec.json"]` for hierarchical ones).
  Additive change; existing manifest readers that don't look at the
  field are unaffected.
- `docs/roadmap.md` updated to mark steps 1+2 complete and
  clarify what's still queued (steps 3–5 in `server.rs` and
  `store.rs`).

### Not done in this round (queued in roadmap.md)

- `src/server.rs` `do_decide` branch that loads
  `HierarchicalCapsuleState` and walks the tree per level.
- `src/server.rs` `do_feedback` branch that calls
  `propagate_reward` across the decision path.
- `src/store.rs` `load_hierarchical_state_in_job` /
  `save_hierarchical_state_in_job` against a new `hierarchical_state.json`
  sidecar.

A capsule that sets `hierarchical_options` today **installs and
validates correctly** and the spec is persisted, but `/decide` still
treats the flattened leaf names as a flat AdaptiveChoice. Runtime
selection of one option per level requires the queued steps.

## [Unreleased] — Phase I followup 3: `/report` formatter completeness

A small but real ergonomics fix: `GET /tenants/.../report` now surfaces
the lifecycle, the resolved post-warmup algorithm, and the per-node
meta-bandit summary. These were previously only reachable via
`/memory` + the on-disk `warmup.json`, which forced anyone debugging a
capsule from the CLI to round-trip through two endpoints.

### Changed

- **`/report` response shape** now includes:
  - `warmup`: `{state: "warmup"|"active"|"frozen", ...}` with
    `collected`/`target` during warmup, `characterization` once
    active, `reason` once frozen.
  - `algorithm`: the resolved `PickedAlgorithm` post-warmup (e.g.
    `"Weighted { learning_rate: 0.1 }"`), `null` during warmup.
  - `metaBandit`: object keyed by strategy node id, each value
    `{totalRounds, currentLeader, candidates: [{id, trials,
    meanReward, cumulativeReward}, ...]}`.
- `docs/known-issues.md` updated to mark the gap resolved.

### Fixed

- Closes the presentation gap documented in `known-issues.md` since
  the greedy-lock investigation (Item 1). The fix is purely additive
  to the response — the on-disk state schema is unchanged. Existing
  callers that only read `tenant`/`job`/`capsule`/`hash`/`strategies`
  continue to work without changes.

## [Unreleased] — Phase I followup 2: shared-state LinUCB wired into the runtime

The shared-state LinUCB foundation that landed in Phase G+H (a single θ
over `[x_context, x_option]` rather than one θ per option) is now wired
end to end through `/decide` and `/feedback`. The hierarchical-bandits
foundation that landed alongside it remains queued — its prep work
(state module, test capsule, doc) is complete, but the runtime branch
in `server.rs` is intentionally deferred to a follow-up session to
avoid shipping both wirings half-done in the same pass. See
`docs/roadmap.md` for the explicit follow-up plan.

### Added

- **Shared-state LinUCB runtime.** A capsule that sets
  `sharedState.enabled = true` in its `learning.json` now routes
  selection through `SharedStateOptionStrategy` instead of the
  per-option LinUcb path. New fields on `LearningConfig`
  (`SharedStateConfig`) and on `CapsuleMemory` (`shared_state:
  Option<SharedStateOptionStrategy>`). The decide path computes scores
  for every registered option using the shared θ — including options
  that have never been chosen — and surfaces them as
  `sharedStateScores` on the response. The feedback path calls
  `apply_feedback` on the shared θ instead of a per-option matrix.
  Persisted in `memory.json` as a `sharedState` block alongside
  `strategies` and `timeSeriesWindows`. Generalises to unseen options
  by construction (`src/shared_state_strategy.rs`).
- **`docs/roadmap.md`** — explicit deferred-work index.
  Currently documents the hierarchical-bandits runtime wiring task
  with its concrete integration plan (capsule_spec field, server.rs
  decide/feedback branches, store sidecar) so a follow-up session can
  pick it up cleanly.
- **Worked cross-terms example in
  `docs/capsule-features/shared-state-linucb.md`**. The doc
  flagged that capsules with bilinear reward needed to feature-engineer
  interaction terms, but the advice was abstract. The example now
  shows a concrete `.lycs` program emitting `ctx_x0 = workload * x0`
  and `ctx_x1 = workload * x1` as features, along with the
  augmented `learning.json` schema and the resulting request body
  shape. Makes the caveat actionable.

### Fixed

- **`OptionStats::to_json` is now self-round-trippable.** Previously
  only the legacy `serialize_bucket` persistence path injected the
  `rewardSum` / `rewardSqSum` / `window` / Page-Hinkley fields, so a
  direct `to_json` → `from_json` round-trip lost reward accumulators.
  This was the persistence shape the new `hierarchical_state` module
  was relying on. Added a regression test
  (`option_stats_to_from_json_is_self_round_tripping`). The
  `effectiveTries` precision still rounds to two decimals in
  `to_json`; documented in-place.

### Changed

- **`POSITIONING.md`** — added a "Shared-state LinUCB" bullet to the
  capability list (wired in, validated end to end). Hierarchical
  bandits get a one-paragraph note pointing at `roadmap.md` for the
  follow-up plan.

### Runtime validation (captured from a real end-to-end test)

Against the `shared-state-action-embeddings` test capsule:

1. Install + `learning.json` attach.
2. 30 warmup rounds of `/decide` with `reward = 0.5` to drive the
   capsule from Warmup into Active.
3. 80 targeted rounds where only picks on the four corner options
   (A/B/C/D) receive feedback; the true reward is the linear function
   `r = 0.10·workload + 0.40·x_opt[0] + 0.60·x_opt[1]`. Picks on E/F
   are skipped — 0 of 80 (the bandit converged on D before any
   exploratory E/F pick fired).
4. Final `/decide` at `workload = 0.5` returns `sharedStateScores` for
   all 6 options:

   | option | true reward | shared-state score |
   |--------|------------:|-------------------:|
   | D      | 0.95        | 1.06               |
   | B      | 0.63        | 1.04               |
   | C      | 0.47        | 1.01               |
   | F (untrained) | 0.59 | **0.93**           |
   | E (untrained) | 0.55 | **0.88**           |
   | A      | 0.15        | 0.82               |

E and F are never directly trained, yet their shared-state scores at
`workload = 0.5` are non-zero, non-trivial, and bracket the
correctly-ordered relationship to their action features (F > E, since
F's features sum higher). The UCB exploration bonus inflates absolute
score values; the *relative* ordering and the *presence* of a
non-zero prior on E/F are the runtime proofs of generalisation.

### Known debt / not yet wired

- **Hierarchical-bandits runtime**: prep complete
  (`src/hierarchical_state.rs`, 7 tests passing, worked test
  capsule, doc page), runtime branches in `server.rs`/`store.rs` not
  yet landed. Tracked in `docs/roadmap.md`.
- **`/report` endpoint formatting**: returns `algorithm: None` and
  `warmup: None` even when state is correct on disk. Pre-existing,
  flagged in `docs/known-issues.md`.

## [Unreleased] — Phase I followup: demo capsules now exercise the meta-bandit

End-to-end validation of the Phase I demos found that the three flagship
demos compiled to `OpCode::Strategy` (Lycan's self-converging strategy
node) rather than `OpCode::AdaptiveChoice` (the Syntra-aware adaptive
choice node). The two forms exist by design — `(strategy ...)` is for
Lycan-standalone programs that learn from execution-time auto-updates,
`(choice ...)` is for capsules whose feedback loop is owned by an
external runtime like Syntra. The demos picked the wrong one. The
practical consequence: the Phase I demos as originally shipped did not
actually exercise Syntra's contextual-bandit or meta-bandit; weight
movement came primarily from execution-time auto-updates inside the
Lycan executor, not from `/feedback` rewards.

Full investigation: `docs/investigations/greedy-lock-2026-05.md`.

### Changed

- **Three flagship demos rewritten to use `(choice ...)`**:
  `examples/predictive-autoscaling/program.lycs`,
  `examples/anomaly-routing/program.lycs`, and
  `examples/seasonal-fraud-threshold/program.lycs`. Each now
  compiles to `OpCode::AdaptiveChoice` with uniform initial weights
  `[0.25, 0.25, 0.25, 0.25]`. Verified via `lycan explain`.
- **Three demo READMEs**: rewrote the "What to expect" sections with
  realistic 30–50-round convergence figures captured from a 100-round
  end-to-end test (option 2 wins 62/100 rounds; weight peaks at 0.81).

### Added

- **`docs/investigations/greedy-lock-2026-05.md`**: full root-cause
  write-up, validation trajectory, meta-bandit state inspection, and
  resolution rationale.
- **`docs/lycan/language/strategy-nodes.md`**: rewritten lead with a
  "When to use which" table distinguishing `(strategy ...)` from
  `(choice ...)`. The doc now leads with the distinction instead of
  presenting `(strategy ...)` as the single form.
- **`syntra author` warning**: emits a stderr warning when it
  encounters `(strategy ...)` in a capsule being authored. One-line,
  non-blocking. Catches the same authoring mistake in the future.

### Known gap (not blocking)

- **`/report` endpoint formatting**: returns `algorithm: None` and
  `warmup: None` even when `memory.json` and `warmup.json` are
  populated. State is correct on disk and reachable through `/memory`;
  this is a presentation gap in the `/report` formatter. Not fixed in
  this round; flagged for a future pass.

## [Unreleased] — Phase I: operational repositioning

A documentation, examples, and tooling pass — no runtime changes. The
appliance's bandit core, `/decide` / `/feedback` contract, capsule store
format, and operational endpoints are unchanged. Phase I makes visible
the Lycan capability surface (`series.ewmaForecast`,
`ops.autoScaleRecommend`, `stats.mean / stdDev / percentile`,
`http.get / post`, `sql.sqliteQuery`, `file.readText / writeText`,
`json.get / has / len`, `runtime.input / inputGet`) that the Phase A–H
framing left buried under bandit-only positioning.

### Added

- **`POSITIONING.md`** — the honest, ground-up positioning doc.
  What Syntra is, what its capsules can compute, what users can do with
  it, what it is explicitly not (not arbitrary forecasting; not a model
  platform; not modern-data-stack scale; not a metric store; not for
  one-shot decisions), and how the operational framing relates to the
  earlier bandit-only framing. This is the document the README is now
  aligned with.
- **`PITCH.md`** — under-1000-word sendable pitch describing
  Syntra in operational-intelligence terms, with three named capsule
  use cases and a first-decide curl flow.
- **Three capsule demos under `examples/`**:
  - `predictive-autoscaling/` — `series.ewmaForecast` +
    `stats.percentile` + `ops.autoScaleRecommend`, strategy node over
    four scaling policies (`hold`, `forecast_match`, `forecast_headroom`,
    `p95_safe`).
  - `anomaly-routing/` — `stats.mean` + `stats.stdDev`, derived z-score,
    strategy node over four routing policies (`primary`, `secondary`,
    `degraded_cache_only`, `circuit_break`).
  - `seasonal-fraud-threshold/` — `series.ewmaForecast` +
    `stats.percentile` on a recent fraud-rate series, strategy node over
    four threshold-adjustment policies (`loose`, `baseline`, `tight`,
    `very_tight`); intended for delayed-feedback (chargeback-window)
    reward flow.
  Each demo ships `capsule.yaml`, `program.lycs`, `learning.json`, and a
  `README.md` walkthrough.
- **Metrics-ingestion sidecar at `sidecar/`** (`syntra-ingest`).
  Python service, YAML-configured, polls four source types
  (`prometheus`, `datadog`, `sql`, `file_watch`) on per-source intervals
  and exposes `GET /features/current` returning the latest snapshot plus
  `_meta` (source + stale_seconds). Best-effort, stateless, single
  process, latest-value only. Ships four example configs
  (`prometheus.yaml`, `datadog.yaml`, `sql.yaml`, `mixed.yaml`) and a
  README that explicitly states this is not a metric store. Tests and
  Docker image are noted as pending.
- **`docs/concepts/operational-intelligence.md`** — new concept
  doc describing the kernel-feature-derivation-to-strategy-node pattern
  the three demos illustrate. Complements the existing
  `docs/concepts.md` on contextual bandits.

### Changed

- **`README.md`** — top sections rewritten to lead with the
  operational positioning. Capability-surface table from Lycan is now
  surfaced in the README. Bandit-core details, lifecycle, refusal, and
  drift sections are preserved and demoted to "How the learning layer
  works". `/decide` and `/feedback` API examples are unchanged.
- **Top-level `README.md`** — README positioning pointer updated to describe
  Syntra in operational-intelligence terms, with a pointer to
  `POSITIONING.md` for the full statement.

### Not done in this phase

- No runtime changes. The bandit core, meta-bandit, refusal, drift
  detection, capsule store format, and HTTP API are byte-identical to
  Phase G+H.
- 2D (hierarchical bandits runtime integration) and 2E (shared-state
  LinUCB runtime integration) remain pending. The respective foundations
  in `src/hierarchical.rs` and `src/linucb.rs::LinUcbSharedState`
  are still wired only at the module / test level.
- Sidecar tests, sidecar Dockerfile, and sidecar CI are pending.

## [Unreleased] — Phase G + H: hardening + capability expansion

### Added

- **Observability.** `/metrics` exposes a Prometheus-compatible exposition
  document (request counters keyed by kind/tenant/job/capsule/status, decide
  latency histogram with 12 buckets, refusal counters by reason). `/ready`
  performs a store-writability probe and returns `{"ready": true}` only when
  the backing store is writable. JSON structured logging via the `tracing` +
  `tracing-subscriber` crates: output goes to stderr in JSON format, level
  controlled by `RUST_LOG` (defaults to `info`). Grafana dashboard
  (`deploy/grafana/dashboards/syntra-overview.json`, 10 content panels across
  Traffic / Latency / Refusals / Lifecycle / Meta-Bandit / Volume /
  Stale-Capsules row groups) and Prometheus alerting rules
  (`deploy/grafana/alerts/syntra-alerts.yaml`).
- **AuthN/AuthZ.** Scoped token store (`src/auth_tokens.rs`). Three
  scopes: `Admin` (any route, any tenant), `TenantAdmin` (all routes on one
  tenant), `Read` (decide + read-only inspection of one capsule). Tokens are
  SHA-256 hashed at rest; raw value returned only at issuance. Admin HTTP
  surface: `POST /admin/tokens` (issue), `DELETE /admin/tokens/<hash>`
  (revoke), `GET /admin/tokens` (list). All mutation routes (install, feedback,
  learning, decide) are gated by scope-aware checks in `server.rs`.
- **Rate limiting.** Per-principal token-bucket (`src/rate_limit.rs`).
  Default 1000 req/sec refill, 2000-token burst. Over-limit requests receive
  HTTP 429 with a `Retry-After` header (whole seconds, rounded up).
- **Backup/restore.** `POST /admin/backup` serializes the full store to a
  version-stamped JSON bundle returned as an attachment. `POST /admin/restore`
  accepts the bundle, validates the version field, and applies it via atomic
  stage-then-rename. Path components in the bundle are traversal-validated
  before any file I/O (`src/backup.rs`).
- **LinTs (linear Thompson sampling).** Seventh meta-bandit candidate
  (`CandidateId::LinTs`). Samples θ̃ from N(μ, v²·A⁻¹) via Cholesky
  factorisation of A⁻¹, then scores each option as x·θ̃. Falls back to
  posterior-mean x·θ̂ on Cholesky failure (numerical PSD drift) — still
  finite and well-typed. `CandidateId::all()` is now 7-long;
  `discrete_only()` is unchanged at 5. Implementation lives on
  `LinUcbState::lin_ts_score` in `src/linucb.rs`.
- **Shared-state LinUCB foundation.** `LinUcbSharedState`
  (`src/linucb.rs`) trains a single A / A⁻¹ / b triplet over
  `concat(x_context, x_option)` embeddings rather than one matrix per option.
  Enables generalisation to unseen options at inference time. Uses the same
  Sherman-Morrison + periodic Gauss-Jordan rebuild pattern as per-option
  `LinUcbState`. Not yet wired into `server.rs` decide/feedback — foundation
  + isolated tests only.
- **Continuous action space.** `ActionSpace::Continuous { range, buckets }`
  (`src/learning.rs`). When set, the decide response includes a
  `chosenAction` field carrying the bucket midpoint so callers can apply the
  value directly without a secondary lookup. `LearningConfig` gains an
  `actionSpace` field defaulting to `ActionSpace::Discrete` for backward
  compatibility.
- **Multi-objective per-component reward recording.** `bucket.stats` now
  accumulates Q estimates per named objective in an `objectiveRewards` map
  (`src/learning.rs`). The feedback path records per-objective values
  into the bucket and derives the scalar reward by averaging across objectives
  when the map is non-empty.
- **Hierarchical-bandits foundation.** `src/hierarchical.rs` (new
  module). Defines `HierarchicalSpec`, `propagate_reward`, and supporting
  types for nested discrete-choice capsules. Integration surface documented
  in the module header; `server.rs` decide/feedback wiring is not yet done.
- **Time-series feature type foundation.** `FeatureType::TimeSeries` in
  `src/feature_schema.rs`. Declares a rolling-window feature with one or
  more aggregations (Mean, Max, Min, P50, P95, Slope) each of which
  contributes one dimension to the encoded feature vector. Validator enforces
  `window_size >= 1`, P95 requires `window_size >= 5`, Slope requires
  `window_size >= 2`. Server-side `TimeSeriesWindow` accumulation and
  `do_decide` wiring are not yet connected.
- **Multi-AdaptiveChoice graphs (5C).** `do_decide` runs the meta-bandit
  independently per `AdaptiveChoice` node and embeds each node's selected
  `candidateId` in its decision-log entry. The `/feedback` route accepts a
  `decisionIndex` field to target a specific node in a multi-decision sequence.
  Capsule YAML gains an optional `decisions: []` list (`src/capsule_spec.rs`,
  `DecisionSpec` with `name`, `options`, and optional `depends_on`).
- **Batched feedback (2B).** `POST /feedback/batch` accepts up to 10,000
  events per request under a single per-capsule lock, with per-event error
  reporting in the response body.
- **Extended `syntra simulate` CLI** (`src/simulate.rs`). Traffic spec
  consumed from YAML (`TrafficSpec` with `arms`, `regime_shifts`, `seeds`,
  `rounds`). Regime-shift support (reward vector replacement mid-run at
  declared round boundaries). Multi-seed runs with per-seed regret reporting.
  Optional Vowpal Wabbit comparison (best-effort, skipped gracefully when `vw`
  is not on `PATH`). Multiple output formats (JSON via `to_json`; per-seed
  regret time series).
- **Domain packs.** Fraud-tuning (`examples/fraud-tuning/`), queue-selection
  (`examples/queue-selection/`), and LLM-routing (`examples/llm-routing/`)
  join the existing retry-tuning pack. Each follows the same pattern:
  `SyntraClient` wrapper, fail-safe when Syntra is unreachable or refuses,
  7 unit tests, no live Syntra required.
- **Language clients.** Go (`examples/syntra-go/`, 7 tests), Node.js/TypeScript
  (`examples/syntra-node/`, 11 tests), Java (`examples/syntra-java/`, 7
  tests), Rust (`examples/syntra-rs/`, 7 tests). All four ship with README,
  retry-client example, and a full test suite exercising the fail-safe paths.
- **Deployment.** Helm chart (`deploy/helm/syntra/`) and Terraform modules for
  AWS, GCP, and Azure (`deploy/terraform/{aws,gcp,azure}/`).
- **CI/CD.** GitHub Actions workflows: `ci.yml` (build + test), `release.yml`
  (publish), `docs.yml` (OpenAPI + docs site). Workflows live at
  `.github/workflows/` in the repo root.
- **Reference documentation.** OpenAPI 3.0 spec (`docs/openapi.yaml`, 31
  paths, 41 schemas). Capsule schema reference (`docs/capsule-schema.md`).
  Concept tutorial (`docs/concepts.md`). Deployment guide
  (`docs/deployment.md`). Operator runbook (`docs/runbook.md`, 5,051 words).
  API reference (`docs/api.md`). Three migration guides under
  `docs/migrating/`: `from-static.md`, `from-vowpal-wabbit.md`,
  `from-custom-bandit.md`.
- **Tooling.** Offline policy evaluation (`examples/offline-eval/`):
  IPS and doubly-robust estimators with bootstrap confidence intervals.
  A/B simulation harness (`examples/ab-harness/`): paired t-test over
  simulation runs. Performance benchmark harness (`examples/bench/`).
  Snapshot export CLI (`examples/export-tool/`, `syntra-export`).
- **Cross-domain validation.** Third benchmark (`examples/lycan-internals/
  benchmarks/traffic_split_resilience/`): A/B/n traffic-split action space,
  pre-registered as a null-hypothesis test for the reward-blindness pattern
  first observed in the outbreak-early-warning and vaccine-allocation
  benchmarks.

### Changed

- `memory.json` schema remains at version 7 — all Phase G/H additions
  (`objectiveRewards`, per-candidate tracking for LinTs, backup bundle
  version field) are additive; existing readers are unaffected.
- `LearningConfig` gained an `actionSpace` field. Defaults to
  `ActionSpace::Discrete`; capsules that do not set it behave identically
  to Phase F.
- `CandidateId::all()` is now 7-long (`LinTs` added). `discrete_only()`
  remains 5-long and is unchanged.
- Rate-limit default tightened from "off" to 1000 req/sec / 2000-burst per
  principal. Pre-existing callers that share a single admin key now share that
  bucket. Raise the limit via a future per-token override config knob (not yet
  implemented).

### Internal

- New Rust modules: `auth_tokens`, `backup`, `rate_limit`, `hierarchical`
  (all in `src/`).
- New dependencies: `tracing`, `tracing-subscriber` (with `env-filter` and
  `json` features).
- `Lycan` lib test count grew from 128 (post-Phase F) to **190** (62 new
  tests across the new modules and expanded coverage of existing ones).
- `Syntra` crate test count grew from 17 to **40**.
- Python test suites grew from 7 tests (retry-tuning only) to **115** across
  8 packages (retry-tuning 7, fraud-tuning 7, queue-selection 7, llm-routing
  7, offline-eval 13, export-tool 18, bench 28, ab-harness 28).

### Known debt / not yet wired

- **Hierarchical bandits**: `src/hierarchical.rs` is in place and tested
  in isolation; `server.rs` decide/feedback integration is not yet connected.
- **Shared-state LinUCB** (`LinUcbSharedState`): foundation and tests exist;
  not yet selected by the meta-bandit or called from any request path.
- **Sequential decision dependencies**: `DecisionSpec.depends_on` is parsed
  and stored; the runtime does not yet pass the upstream choice as context
  to downstream nodes.
- **Time-series feature contexts**: `FeatureType::TimeSeries` encodes
  correctly; `server.rs` does not yet maintain `TimeSeriesWindow` state
  across requests or call `encode_with_windows` at decide time.
- **Per-token rate-limit override**: a single global 1000 req/sec per
  principal is the only knob; per-token config is a future addition.

## [Unreleased] — Phases A through F: platform completion

The adaptive core moves from "single configurable algorithm" to "auto-pick
algorithm, detect drift, refuse when uncertain". The Docker demo and the
Python integration example land alongside.

### Added

- **Reward characterization at warmup transition** (`src/reward_characterization.rs`).
  Watches the first ~30 feedback rewards and classifies the problem as
  binary / continuous / sparse-continuous. The capsule's first active
  algorithm is picked from this characterization.
- **Capsule lifecycle** (`src/warmup.rs`). Warmup → Active → Frozen.
  Persisted as `warmup.json` next to the graph. Active state is reverted
  back to Warmup on capsule-level drift detection.
- **Two-layer ADWIN change detection** (`src/change_detection.rs`).
  Capsule-level detector triggers re-warmup on global regime shifts;
  per-context detectors reset just the drifted context bucket on narrower
  shifts.
- **Rate-adaptive meta-bandit** (`src/meta_bandit.rs`). Six candidate
  algorithms run in parallel (Thompson, UCB1, EpsilonGreedy, Weighted,
  Greedy, LinUCB). The meta-bandit converges on whichever performs best on
  the capsule's actual traffic. Configurable per-candidate geometric
  forgetting (default 0.999).
- **LinUCB algorithm** (`src/linucb.rs`) for feature-vector contexts.
  Sherman-Morrison rank-1 updates with periodic Gauss-Jordan rebuild for
  numerical stability. Defensive against degenerate features (NaN, Inf,
  wrong dimension).
- **YAML feature schema** (`src/feature_schema.rs`). Continuous,
  categorical (one-hot, reference level dropped), and cyclic
  (sin/cos-encoded) feature types. Declared in `learning.json` as
  `contextSpec`, encoded to fixed-length vectors at request time.
- **Split-conformal calibration** (`src/conformal.rs`). Per-bucket
  sliding-window calibration over reward residuals; produces prediction
  intervals at user-chosen coverage (default 95%).
- **Out-of-distribution detection** (`src/ood.rs`). Discrete contexts
  tracked by novelty + staleness; feature contexts scored by Mahalanobis
  distance against an online Welford-covariance estimate.
- **Confidence-based refusal** (`src/learning.rs` `RefusalConfig`).
  When `refusal.enabled=true`, `/decide` returns `{"refused": true,
  "confidence": {oodScore, intervalWidth, refusalReason}}` for OOD inputs
  or wide intervals. Refusal is Active-only — Warmup decisions never
  refuse, so the bootstrap path can never deadlock on its own cold start.
- **Reference Docker demo** (`docker/Dockerfile.demo`). Multi-stage
  build, retry-tuning capsule pre-installed, traffic generator, live
  dashboard on `:8080`.
- **Python retry-tuning domain pack** (`examples/retry-tuning/`).
  `RetryClient` wraps `requests` with Syntra-driven policy selection;
  fail-safe when Syntra is unreachable, refuses, or returns malformed
  data. Seven unit tests, no Syntra required.

### Changed

- **`memory.json` schema bumped 2 → 7**, with full backward-compat readers
  for each prior version. Added: candidate-context buckets, meta-bandit
  state, per-context detectors, conformity calibrators, discrete and
  feature OOD detectors.
- **`LearningConfig` gained `contextSpec` and `refusal` blocks.** Both
  default to backward-compatible values (Discrete context, refusal off).
- **`do_decide` now persists memory at end of request.** OOD detector
  observations and candidate-context initialization survive across decides
  rather than only being saved on feedback. (Pre-existing latent issue:
  meta-bandit selection state was discarded between decides until this
  change.)
- **`parse_meta_bandit` rebuild bug** fixed. The deserializer was
  re-initializing the candidate list to the 5-candidate `discrete_only`
  set regardless of what was persisted, silently dropping LinUcb data on
  every memory reload. Now uses the saved candidates list directly. Bug
  was masked before because memory wasn't persisted from decide; the new
  persistence above exposed it.
- **Docker demo image** is local-build only at the moment — the published
  `ghcr.io/ashhart/syntra:*` tags promised in earlier docs do not exist
  yet and references to them have been removed.

### Internal

- 128 unit tests in `Lycan` (was ~30 at Phase A start).
- 21 end-to-end integration tests in `Syntra`.
- 7 unit tests in the Python integration example.

### Known debt

- ADWIN drift threshold is hard-coded (`delta=0.002`); per-capsule
  configurability not yet exposed via `learning.json`.
- `/inspect` returns graph-shape only; the dashboard reads `warmup.json`,
  `/memory`, and `/decisions` to assemble the live state view rather than
  going through one endpoint.
- Capsules with more than one `AdaptiveChoice` node still only have
  meta-bandit decisions attached to `decisions[0]`; multi-node graphs use
  uniform weights for the trailing nodes.

## [0.2.0] — pre-Phase-A baseline

Initial Syntra appliance with per-capsule contextual learning and
single-algorithm selection via `learning.json`. Documented in the v0.2.0
package under `packages/Syntra-0.2.0/`.
