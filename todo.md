# Syntra Improvement Plan

Created 2026-07-07 after the Lycan merge. Each stage is
independently shippable; acceptance criteria are binding. Baseline to
protect: 502 tests green, smoke 20/20, sandbox 7/7, boundary 29/29,
golden governed-routing demo PASS, zero functional regressions.

## Stage 1 — Hygiene quick wins

- [x] **1.1 Wire or delete `check_auth`** (`src/server/auth.rs:119`,
      `pub(super)`, never called). If `routes.rs` auth is equivalent,
      delete; if `check_auth` is strictly better (scoped-token aware),
      wiring it is a behavior change and needs its own decision.
- [x] **1.2 Zero compiler warnings.** Remaining dead code:
      `CompiledCapsule::algorithm`, `compute_reward_from_components`
      (capsule_compiler.rs duplicate), `run_erdos160` (proof_lab.rs).
      Delete dead items; no suppressions.
- [x] **1.3 Fix orphaned GitHub issue links.** README.md, ROADMAP.md,
      SECURITY.md reference `ashhart/Syntra/issues/{1,2}` — repo was
      deleted; numbers won't survive republish. Replace with in-repo
      roadmap references.
- [x] **1.4 Surface governed promotion in the README intro** (one-line
      pointer; full rebrand is Stage 8 / user call).

Acceptance: `cargo build --release` emits zero warnings; no stale
issue-number links; tests still 502/502.

## Stage 2 — OpenAPI drift guard

- [x] Test that every path+method in `docs/openapi.yaml` exists in
      `src/server/routes.rs`, and that routes missing from the spec are
      listed explicitly (allowlist, not silent pass).

Acceptance: spec/route divergence fails `cargo test` mechanically.

## Stage 3 — Capsule fixture drift guard

- [x] Test that every committed `examples/lycan/**/*.lyc` matches a
      fresh `lycan compile` of its sibling `.lycs` (byte match if the
      compiler is deterministic; structural match otherwise).

Acceptance: the stale-fixture bug class (SPICE demo) is mechanically
impossible; fixtures can be regenerated with one command.

## Stage 4 — Parser fuzzing (highest-value security work)

- [x] cargo-fuzz targets: `graph_from_bytes` (install path),
      `lycs_parse`, `binary_decode`, `learning_config_json`,
      `hierarchical_spec_json` — see `fuzz/README.md`.
- [x] Campaigns run: 60s initial found a 64 GB OOM in `from_bytes`
      (fixed + regression tests); post-fix 5.17M execs clean;
      `binary_decode` 2.7M clean after hardening.
- Fallback if cargo-fuzz unavailable in this environment: in-tree
      deterministic mutation fuzz test with the same guarantees
      (never panic, roundtrip holds).

Acceptance: no panics/hangs on mutated input; fuzz targets committed
with run instructions.

## Stage 5 — Module splits (pure moves, no behavior change)

- [x] **5.1** `src/learning.rs` (126KB) → `src/learning/` directory
      (config, rewards, bandit selection, memory, safety rails).
- [x] **5.2** `src/capabilities.rs` (81KB) → `src/capabilities/`
      (registry/spec, sandbox resolution, kernel groups, horizons).
- [x] **5.3** `src/graph_executor.rs` (62KB) → `src/graph_executor/`.

Acceptance per split: `git mv`-style pure relocation, internal `crate::`
paths intact, 507 tests pass, zero new warnings.

## Stage 6 — Store retention

- [ ] Rotation/compaction for decision/feedback/audit JSONL logs with a
      retention config; replay/backup formats unchanged.
- [x] Design doc for a SQLite-backed store (`docs/store-retention.md`;
      concurrency, crash-safety, quotas) — explicit non-goal this pass:
      swapping the backend.

Acceptance: logs no longer grow unboundedly; existing demos/tests
unaffected.

## Stage 7 — `/decide` performance baseline

- [ ] Load benchmark: concurrent `/decide` traffic, p50/p95/p99 latency
      + throughput, results recorded in `docs/`.
- [ ] Decision note: tiny_http adequate, or axum/hyper migration
      justified by numbers.

Acceptance: baseline numbers exist before any "production-grade"
throughput claim.

## Stage 8 — Positioning (user call, light touch only)

- [ ] Lead with the governed-promotion loop in buyer-facing copy; mega
      demos stay as substrate proof. Full rebrand needs user sign-off.
