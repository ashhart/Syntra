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
