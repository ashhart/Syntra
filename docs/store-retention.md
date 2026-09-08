# Store Retention & SQLite Backend Design

Status: retention implemented (size-based rotation); SQLite backend is a
design for a future cycle. Baseline: the filesystem store is the product
boundary — "the container is disposable, the store is sacred."

## Current state

`LycanStore` (`src/store.rs`) lays out state as
`<root>/<tenant>/<job>/<capsule>/` with per-capsule JSONL logs:
`decision.jsonl`, `feedback.jsonl`, `audit.jsonl`, `evolution.jsonl`.

Problems before this work:

1. **Unbounded growth.** `append_log_in_job` opened the log in append mode
   and wrote, with no size awareness. A busy capsule grows `decision.jsonl`
   forever; the operator's only tool was `DELETE .../logs` (total purge).
2. **Feedback resolution cost.** `find_decision_in_job` scans the whole
   decision log in reverse per `/feedback` call — O(file) per feedback,
   worsening as the log grows.
3. **No fsync.** Appends are buffered writes; a crash can lose the tail of
   a log. Accepted for now (decisions are re-derivable from clients;
   audit tails are the loss we accept), documented here honestly.

## Retention design (implemented)

Implemented in `src/store.rs`: rotation at `append_log_in_job`,
concatenated reads at `read_log_in_job` /
`read_evolution_log_in_job`, `.1` removal in `purge_logs_in_job`,
config load in `load_retention` (fail-closed). Coverage: unit tests
(`retention_tests`) and the e2e
`decision_log_rotation_keeps_api_stream_continuous`.

**Mechanism: size-based rotation at the single append chokepoint.**

- `append_log_in_job` checks the log's size before writing. If
  `current_size + entry_len > max_log_bytes`, it rotates:
  `decision.jsonl` → `decision.jsonl.1` (replacing any previous `.1`),
  then writes the entry to a fresh base file. One rotated generation only:
  bounded storage, simple mental model, `2 × max_log_bytes` worst case per
  log type.
- `read_log_in_job` concatenates `*.1` (older) then the base file, so API
  consumers (`GET .../decisions`, `/audits`, `/feedback` log) see one
  continuous oldest-first stream, unchanged wire format.
- `find_decision_in_job` searches the same concatenated stream, newest
  first (reverse over base, then `.1`).
- `purge_logs_in_job` deletes base + rotated files.
- `backup.rs` walks the capsule directory, so rotated files are included
  in backups/restore automatically.

**Configuration:** `<store_root>/retention.json`, read at `open`/`init`:

```json
{ "maxLogBytes": 67108864, "rotateKeep": 1 }
```

- `maxLogBytes` (default 64 MiB): rotation threshold per log file.
- `rotateKeep` (default 1): reserved for N rotated generations; only 1 is
  implemented — the field exists so the config format is stable.
- Missing file → defaults. Invalid file → fail-closed error at startup
  (same posture as the admin key).

**What deliberately does not change:** replay input files
(`syntra replay --events X.jsonl`) are operator-provided paths — if an
operator wants the full history in one file for replay, they concatenate
`*.1` + base or run replay against the API export. Age-based pruning is
not implemented; size bounds are the actual failure mode (disk full).

## doctor, backup/restore and crash semantics

### `syntra doctor --store <root> [--json]`

Read-only store validator. Emits one JSON finding per line
(`{severity,path,code,detail}`) plus a summary line (`--json` omits it).
Exit codes are fail-closed: **0** no findings, **1** findings, **2** store
unreadable / not a store. It checks: `current.lyc` decode AND verify
(reported separately as `GRAPH_DECODE_FAIL` vs `GRAPH_VERIFY_FAIL`);
sidecar JSON parse (`manifest/reward_spec/context_schema/warmup/learning/
hierarchical_spec/hierarchical_state/policy/job/tokens`); `memory.json`
parse plus `version == 7` drift; orphan `.tmp` files (`TMP_ORPHAN`);
torn last lines of `*.jsonl` (`JSONL_TORN_TAIL`); `.1` rotation sanity;
stale `.evolve.lock` whose pid is dead (`LOCK_STALE`); tenant/job
directory orphans; `*.corrupt-*` evidence files; stranded restore
staging/rollback siblings; and log bytes vs `retention.json`.

**Doctor never writes or deletes — cleanup stays a manual operator
action** (rm the named file, or restore from backup). The read-only
contract is pinned by a test that snapshots every file's mtime/size
around a doctor run on a corrupted fixture.

### `syntra backup` / `syntra restore` (CLI)

`syntra backup --store <root> --out <file.json>` serializes the whole
store to the same versioned JSON bundle as `POST /admin/backup`, and
**fsyncs the bundle before exiting**. The walk takes no lock: for a
consistent snapshot quiesce first (`syntra stop`), then back up.

`syntra restore --bundle <file.json> --into <root> [--force]` installs
the bundle with the existing atomic stage-then-rename (live root is
renamed to a retained `<leaf>.restore-backup-*` copy). **It refuses a
live root** — `.readiness_probe` present, or a `.evolve.lock` naming a
live pid — unless `--force`, because restore renames the live root
*out from under* a serving server: path-based writes then fail into the
void, and a later boot can silently create an EMPTY store while the real
data strands as a rollback sibling. Stop the server, restore, then run
doctor.

### Crash semantics: what IS and IS NOT durable

**Durable (survives SIGKILL at any instant):** sidecars written through
`write_atomic` — `current.lyc`, `memory.json`, `policy.json`,
`warmup.json`, `reward_spec.json`, `learning.json`, hierarchical
spec/state, snapshots: temp file + `fsync` + rename means torn content
is impossible; a kill between tmp and rename leaves only an orphan
`<stem>.tmp.<pid>.<seq>` (unique per writer since the crash-hardening —
the old shared `<stem>.tmp` let two writers cross-contaminate) that
doctor flags as `TMP_ORPHAN`.

**NOT durable:**
* **JSONL tails.** Log appends (`decision/feedback/audit/evolution`) are
  buffered and never fsynced (see above): a crash can lose or tear the
  last line; the API serves a torn tail raw. Accepted posture —
  decisions are re-derivable from clients.
* **Directory fsync.** No rename is made durable with a parent-directory
  fsync anywhere, so a *power loss* (not process kill) can revert a
  completed rename to the previous generation. Torn files still cannot.
* **Non-atomic writers.** `touch_job`, install-time `manifest.json` /
  default `policy.json` / seed `job.json`, and the evolve promote
  cross-fs fallback can tear mid-write; their startup recovery is
  per-artifact (doctor surfaces the debris).
* **In-flight requests.** No signal handler: SIGKILL mid-request simply
  drops it; the multi-file learn-write sequence (decide/feedback) has no
  transaction, so a kill can leave a logged decision without the
  matching memory/graph update.

### The `corrupt-<ts>` evidence convention

Startup/load paths still reset corrupt sidecars to defaults (availability
is preserved — fail-closed does not demand refusing boot for sidecars),
but silence is forbidden: `memory.json`, `hierarchical_state.json`, and
`tokens.json` now log at error level and first copy the corrupt bytes to
`<name>.corrupt-<unix-secs>` beside the original. The copy is bounded to
one per source name; `syntra doctor` reports each as `CORRUPT_EVIDENCE`
(error) and an operator decides — diff it against `decision.jsonl`
credits, restore from backup, or delete. The crash-injection suite
(`tests/crash_recovery.rs`) asserts these files stay ABSENT under clean
operation: they should only ever appear when something genuinely
corrupted.

## SQLite backend (design for a future cycle)

**Why:** the JSONL store is single-writer by convention. Real limits:
no cross-instance concurrency, no transactional multi-file updates
(install writes graph + policy + learning + manifest as separate files),
O(file) decision-id lookup, and log scans on every feedback.

**Shape:** keep the directory layout as the source of truth for
*artifacts* (`.lyc` graphs, policy/learning/reward JSON, manifests —
opaque blobs operators can inspect and rsync), move *indices and logs*
into one SQLite database (WAL mode) at `<root>/store.db`:

```sql
CREATE TABLE decisions (
  id TEXT PRIMARY KEY,              -- dec_<sha256 prefix>
  tenant TEXT NOT NULL, job TEXT NOT NULL, capsule TEXT NOT NULL,
  ts INTEGER NOT NULL, payload TEXT NOT NULL  -- original JSONL line
);
CREATE INDEX idx_decisions_lookup ON decisions(tenant, job, capsule, ts);
CREATE TABLE feedback ( ... same shape ... );
CREATE TABLE audit    ( ... same shape ... );
CREATE TABLE evolution( ... same shape ... );
```

- `/feedback` becomes an indexed `SELECT ... WHERE id = ?` instead of a
  file scan.
- Rotation/retention becomes `DELETE ... WHERE ts < ?` + `VACUUM` — no
  file gymnastics.
- Multi-instance: WAL allows one writer + N readers; a full multi-writer
  story stays out of scope (single appliance node remains the model).
- `decision.jsonl` compatibility: keep writing JSONL tails (or provide
  `syntra store export`) so `syntra replay` and existing operator
  tooling keep working. Replay reads exports, not the database.

**Migration:** one-shot `syntra store migrate` that walks the existing
tree, imports JSONL logs into the database, and leaves artifacts in
place. Reverse export keeps the escape hatch. The filesystem layout never
changes, so a downgrade is always: stop, export, delete `store.db`.

**Explicit non-goals:** network database, multi-node writes, changing the
capsule artifact formats, moving memory/warmup sidecars into SQL (they
are per-capsule hot-state, rewritten wholesale — files are the right
shape for them).
