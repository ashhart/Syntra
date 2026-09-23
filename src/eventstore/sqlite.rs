//! SQLite implementation of [`EventStore`].
//!
//! Connections:
//!
//! * one **writer** behind a `Mutex`, used for every write. Statements are
//!   prepared once and cached. Single-row writes run in autocommit mode;
//!   batches and other multi-statement writes use `BEGIN IMMEDIATE`, so the
//!   write lock is taken up front and a transaction never fails half-way on
//!   lock upgrade;
//! * a **read pool** of read-only connections. Under WAL a reader never
//!   waits for the writer, so decision and reward lookups proceed while a
//!   write is in flight;
//! * a **checkpointer** connection owned by a background thread (see
//!   `checkpoint.rs`).
//!
//! No lock is held while caller code runs: the trait takes no callbacks, and
//! every method releases its connection before returning.

use super::checkpoint::Checkpointer;
use super::error::{SqlContext, is_foreign_key_violation};
use super::ffi;
use super::pool::ReadPool;
use super::schema::{self, SCHEMA_VERSION};
use super::validate;
use super::{
    AuditRecord, CapsuleKey, CapsuleStats, DecisionRecord, EventStore, InsertOutcome, LoggedRow,
    ModelSnapshot, PruneCounts, Result, RewardOutcome, RewardRecord, RewardsMode, StoreError,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, ToSql, TransactionBehavior, params};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// Distinct statements per connection stay well under this.
const STATEMENT_CACHE: usize = 64;
/// Maximum WAL size kept on disk after the log restarts.
const JOURNAL_SIZE_LIMIT: &str = "67108864";
/// Rows per transaction in batched deletes. Keeps the write lock short so
/// decisions and rewards of other capsules keep flowing during a prune.
const DELETE_BATCH: i64 = 1000;
/// Attempts when a conflicting row disappears between the insert and the
/// lookup (only possible when another process deletes it concurrently).
const CONFLICT_RETRIES: usize = 3;

/// Tuning for [`SqliteStore::open_with`].
#[derive(Debug, Clone)]
pub struct SqliteOptions {
    /// Read-only connections in the read pool (at least 1). Default 4.
    pub read_connections: usize,
    /// Start a background checkpoint once this many WAL pages have been
    /// written since the last one. Default 1000, SQLite's own
    /// `wal_autocheckpoint` default (about 4 MiB of WAL).
    pub checkpoint_after_pages: u64,
    /// Start a background checkpoint at most this long after the first
    /// write no checkpoint has covered yet. This bounds the power-loss window.
    /// Default 1 s.
    pub checkpoint_interval: Duration,
}

impl Default for SqliteOptions {
    fn default() -> Self {
        SqliteOptions {
            read_connections: 4,
            checkpoint_after_pages: 1000,
            checkpoint_interval: Duration::from_secs(1),
        }
    }
}

/// Result of [`SqliteStore::integrity_check`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    /// Human-readable problems; empty when the database is sound.
    pub problems: Vec<String>,
}

impl IntegrityReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Event store backed by one SQLite file (see the `eventstore` module docs
/// for durability semantics).
pub struct SqliteStore {
    path: PathBuf,
    // Field order is drop order. The checkpoint thread stops first, then the
    // readers close, and the writer closes last. As the last connection,
    // the writer's close checkpoints the WAL fully and deletes it, which a
    // read-only connection cannot do.
    checkpointer: Checkpointer,
    readers: ReadPool,
    writer: Mutex<Connection>,
}

impl fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

// Used to name backup temp files; the store never reads the clock.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl SqliteStore {
    /// Opens or creates the database at `path` with default options.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, SqliteOptions::default())
    }

    /// Opens or creates the database at `path`: checks that the file is an
    /// event store this build understands (refusing foreign or newer files
    /// before writing anything), enables WAL, migrates the schema, then
    /// opens the read pool and starts the checkpoint thread. The parent
    /// directory must exist.
    pub fn open_with(path: impl AsRef<Path>, options: SqliteOptions) -> Result<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err(StoreError::InvalidInput(
                "the event store needs a file path: WAL mode and the read pool \
                 do not work on an in-memory database"
                    .into(),
            ));
        }
        let mut writer = open_writer(path).map_err(|e| with_path(path, e))?;
        schema::migrate(&mut writer).map_err(|e| with_path(path, e))?;
        // Start the page count from zero; migration pages are not traffic.
        ffi::take_pages_written(&writer);
        let readers = (0..options.read_connections.max(1))
            .map(|_| open_reader(path))
            .collect::<Result<Vec<_>>>()
            .map_err(|e| with_path(path, e))?;
        let checkpointer = Checkpointer::spawn(
            open_checkpointer(path).map_err(|e| with_path(path, e))?,
            options.checkpoint_after_pages,
            options.checkpoint_interval,
        )?;
        Ok(SqliteStore {
            path: path.to_path_buf(),
            checkpointer,
            readers: ReadPool::new(readers),
            writer: Mutex::new(writer),
        })
    }

    /// The database file this store was opened on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a consistent copy of the database to `dest` with SQLite's
    /// online backup API. Writers are not blocked while it runs.
    ///
    /// The copy goes to a temporary file next to `dest`, is switched to a
    /// rollback journal (so it is one self-contained file), passes
    /// `PRAGMA quick_check`, and is then renamed over `dest`. `dest` is
    /// either left as it was or fully replaced. Refuses to write over the
    /// live database, or over a file that has `-wal`/`-shm`/`-journal`
    /// companions, since that may be an open database.
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> Result<()> {
        const OP: &str = "backup_to";
        let dest = dest.as_ref();
        let file_name = dest.file_name().ok_or_else(|| {
            StoreError::InvalidInput(format!(
                "backup destination {} has no file name",
                dest.display()
            ))
        })?;
        let dir = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let live = std::fs::canonicalize(&self.path).map_err(|e| io_error(OP, &self.path, e))?;
        let dir_canonical = std::fs::canonicalize(&dir).map_err(|e| io_error(OP, &dir, e))?;
        if dir_canonical.join(file_name) == live {
            return Err(StoreError::InvalidInput(
                "backup destination is the live database".into(),
            ));
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            if sidecar(dest, suffix).exists() {
                return Err(StoreError::InvalidInput(format!(
                    "backup destination {} has a {suffix} file and may be an open database; \
                     refusing to replace it",
                    dest.display()
                )));
            }
        }

        let tmp = dir.join(format!(
            ".{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = self.backup_into(&tmp).and_then(|()| {
            std::fs::rename(&tmp, dest).map_err(|e| io_error(OP, dest, e))?;
            // Make the rename durable. Best effort: not every filesystem
            // can fsync a directory.
            if let Ok(d) = std::fs::File::open(&dir) {
                let _ = d.sync_all();
            }
            Ok(())
        });
        if result.is_err() {
            for path in [
                tmp.clone(),
                sidecar(&tmp, "-journal"),
                sidecar(&tmp, "-wal"),
                sidecar(&tmp, "-shm"),
            ] {
                let _ = std::fs::remove_file(path);
            }
        }
        result
    }

    fn backup_into(&self, tmp: &Path) -> Result<()> {
        const OP: &str = "backup_to";
        let _ = std::fs::remove_file(tmp);
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let dst = Connection::open_with_flags(tmp, flags).op(OP)?;
        dst.busy_timeout(BUSY_TIMEOUT).op(OP)?;
        dst.execute_batch("PRAGMA synchronous = FULL;").op(OP)?;
        {
            let src = self.readers.get();
            ffi::copy_database(&src, &dst)?;
        }
        // The copied header still says WAL. Switch the copy to a rollback
        // journal so the backup is a single self-contained file; opening it
        // with `SqliteStore::open` switches it back to WAL.
        dst.query_row("PRAGMA journal_mode = DELETE", [], |r| {
            r.get::<_, String>(0)
        })
        .op(OP)?;
        let check: String = dst
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .op(OP)?;
        if check != "ok" {
            return Err(StoreError::corrupt(
                OP,
                format!("backup copy failed quick_check: {check}"),
            ));
        }
        dst.close().map_err(|(_, e)| StoreError::from_sqlite(OP, e))
    }

    /// Runs `PRAGMA integrity_check` and `PRAGMA foreign_key_check` and
    /// verifies the schema version, on a read connection (writers are not
    /// blocked). Cost is a full read of the database. Structural damage that
    /// stops the check itself is returned as [`StoreError::Corrupt`].
    pub fn integrity_check(&self) -> Result<IntegrityReport> {
        const OP: &str = "integrity_check";
        let conn = self.readers.get();
        let mut problems = Vec::new();
        {
            let mut stmt = conn.prepare("PRAGMA integrity_check").op(OP)?;
            let mut rows = stmt.query([]).op(OP)?;
            while let Some(row) = rows.next().op(OP)? {
                let line: String = row.get(0).op(OP)?;
                if line != "ok" {
                    problems.push(line);
                }
            }
        }
        {
            let mut stmt = conn.prepare("PRAGMA foreign_key_check").op(OP)?;
            let mut rows = stmt.query([]).op(OP)?;
            while let Some(row) = rows.next().op(OP)? {
                let table: String = row.get(0).op(OP)?;
                let rowid: Option<i64> = row.get(1).op(OP)?;
                let parent: String = row.get(2).op(OP)?;
                let rowid = rowid.map_or_else(|| "?".to_string(), |r| r.to_string());
                problems.push(format!(
                    "{table} row {rowid} references a missing {parent} row"
                ));
            }
        }
        match schema::read_version(&conn)? {
            Some(SCHEMA_VERSION) => {}
            other => problems.push(format!(
                "schema version is {other:?}, expected {SCHEMA_VERSION}"
            )),
        }
        Ok(IntegrityReport { problems })
    }

    /// Takes the writer, recovering from a poisoned lock. A panic that
    /// unwound through a write may have left a transaction open (rusqlite
    /// rolls back on drop, but this does not rely on it).
    fn lock_writer(&self) -> MutexGuard<'_, Connection> {
        match self.writer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                if !guard.is_autocommit() {
                    let _ = guard.execute_batch("ROLLBACK");
                }
                self.writer.clear_poison();
                guard
            }
        }
    }

    /// Runs `f` on the writer connection, then reports the WAL pages it
    /// wrote to the checkpointer (after releasing the writer).
    fn write<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let (result, pages) = {
            let mut conn = self.lock_writer();
            let result = f(&mut conn);
            (result, ffi::take_pages_written(&conn))
        };
        self.checkpointer.note_pages(pages);
        result
    }

    /// Shared driver of the batch inserts. Rows that fail `check` get
    /// `invalid(message)`; the rest go through `insert`, in input order,
    /// inside one `BEGIN IMMEDIATE` transaction. An error from `insert`
    /// rolls the whole transaction back (the `Transaction` guard does it on
    /// drop).
    fn write_batch<R, O>(
        &self,
        rows: &[R],
        op: &'static str,
        check: impl Fn(&R) -> Result<()>,
        invalid: impl Fn(String) -> O,
        insert: impl Fn(&Connection, &R) -> Result<O>,
    ) -> Result<Vec<O>> {
        validate::batch_len(rows.len())?;
        // Validate outside the writer lock.
        let rejections: Vec<Option<String>> = rows
            .iter()
            .map(|row| check(row).err().map(validate::rejection))
            .collect();
        if rejections.iter().all(Option::is_some) {
            // Nothing to write (this includes the empty batch).
            return Ok(rejections.into_iter().flatten().map(invalid).collect());
        }
        self.write(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .op(op)?;
            let mut out = Vec::with_capacity(rows.len());
            for (row, rejection) in rows.iter().zip(rejections) {
                out.push(match rejection {
                    Some(message) => invalid(message),
                    None => insert(&tx, row)?,
                });
                // A row-level constraint failure rolls back only its own
                // statement. Should SQLite ever end the whole transaction
                // instead, stop rather than keep writing outside it.
                if tx.is_autocommit() {
                    return Err(StoreError::from_sqlite(
                        op,
                        rusqlite::Error::SqliteFailure(
                            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
                            Some("SQLite ended the batch transaction early".into()),
                        ),
                    ));
                }
            }
            tx.commit().op(op)?;
            Ok(out)
        })
    }

    /// Deletes decisions in batches of `DELETE_BATCH`, one transaction per
    /// batch, until a batch comes up short. `rewards_sql` deletes the rewards
    /// of the batch that `decisions_sql` then deletes. Both statements take
    /// the same arguments and select the batch with the same predicate and
    /// total order inside one transaction, so they agree on its members. If
    /// they did not, the foreign key would abort the batch rather than
    /// orphan a reward.
    fn delete_decision_batches(
        &self,
        rewards_sql: &'static str,
        decisions_sql: &'static str,
        args: &[&dyn ToSql],
        op: &'static str,
    ) -> Result<PruneCounts> {
        let mut counts = PruneCounts::default();
        loop {
            let (rewards, decisions) = self.write(|conn| {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .op(op)?;
                let rewards = tx
                    .prepare_cached(rewards_sql)
                    .op(op)?
                    .execute(args)
                    .op(op)?;
                let decisions = tx
                    .prepare_cached(decisions_sql)
                    .op(op)?
                    .execute(args)
                    .op(op)?;
                tx.commit().op(op)?;
                Ok((rewards as u64, decisions as u64))
            })?;
            counts.rewards += rewards;
            counts.decisions += decisions;
            if decisions < DELETE_BATCH as u64 {
                return Ok(counts);
            }
        }
    }

    /// Runs a `DELETE ... LIMIT ?4` statement until it deletes a short batch.
    fn delete_batched(&self, sql: &'static str, key: &CapsuleKey, op: &'static str) -> Result<u64> {
        let mut total = 0;
        loop {
            let deleted = self.write(|conn| {
                let n = conn
                    .prepare_cached(sql)
                    .op(op)?
                    .execute(params![
                        key.tenant(),
                        key.job(),
                        key.capsule(),
                        DELETE_BATCH
                    ])
                    .op(op)?;
                Ok(n as u64)
            })?;
            total += deleted;
            if deleted < DELETE_BATCH as u64 {
                return Ok(total);
            }
        }
    }
}

impl EventStore for SqliteStore {
    fn insert_decision(&self, d: &DecisionRecord) -> Result<InsertOutcome> {
        validate::decision(d)?;
        self.write(|conn| insert_decision_row(conn, d, "insert_decision"))
    }

    fn insert_decisions(&self, decisions: &[DecisionRecord]) -> Result<Vec<InsertOutcome>> {
        const OP: &str = "insert_decisions";
        self.write_batch(
            decisions,
            OP,
            validate::decision,
            InsertOutcome::Invalid,
            |conn, d| insert_decision_row(conn, d, OP),
        )
    }

    fn get_decision(&self, key: &CapsuleKey, id: &str) -> Result<Option<DecisionRecord>> {
        let conn = self.readers.get();
        select_decision(&conn, key, id, "get_decision")
    }

    fn list_decisions(
        &self,
        key: &CapsuleKey,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        limit: usize,
        after_id: Option<&str>,
    ) -> Result<Vec<DecisionRecord>> {
        const OP: &str = "list_decisions";
        let Some((lo, hi)) = validate::ts_bounds(since_ms, until_ms) else {
            return Ok(Vec::new());
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = validate::limit(limit);
        let (t, j, c) = (key.tenant(), key.job(), key.capsule());
        let conn = self.readers.get();
        let cursor = match after_id {
            None => None,
            Some(id) => {
                let ts: Option<i64> = conn
                    .prepare_cached(SELECT_DECISION_TS)
                    .op(OP)?
                    .query_row(params![t, j, c, id], |r| r.get(0))
                    .optional()
                    .op(OP)?;
                match ts {
                    Some(ts) => Some((ts, id)),
                    None => {
                        return Err(StoreError::UnknownCursor {
                            key: key.clone(),
                            decision_id: id.to_string(),
                        });
                    }
                }
            }
        };
        match cursor {
            // Seek straight past the cursor row.
            Some((ts, id)) if ts >= lo => query_decisions(
                &conn,
                LIST_DECISIONS_AFTER,
                params![t, j, c, ts, id, hi, limit],
                key,
                OP,
            ),
            // No cursor, or a cursor before the window: start at the window.
            _ => query_decisions(
                &conn,
                LIST_DECISIONS,
                params![t, j, c, lo, hi, limit],
                key,
                OP,
            ),
        }
    }

    fn insert_reward(&self, r: &RewardRecord) -> Result<RewardOutcome> {
        validate::reward(r)?;
        match self.write(|conn| insert_reward_row(conn, r, "insert_reward"))? {
            RewardOutcome::UnknownDecision => Err(StoreError::UnknownDecision {
                key: r.key.clone(),
                decision_id: r.decision_id.clone(),
            }),
            outcome => Ok(outcome),
        }
    }

    fn insert_rewards(&self, rewards: &[RewardRecord]) -> Result<Vec<RewardOutcome>> {
        const OP: &str = "insert_rewards";
        self.write_batch(
            rewards,
            OP,
            validate::reward,
            RewardOutcome::Invalid,
            |conn, r| insert_reward_row(conn, r, OP),
        )
    }

    fn rewards_for_decision(
        &self,
        key: &CapsuleKey,
        decision_id: &str,
    ) -> Result<Vec<RewardRecord>> {
        const OP: &str = "rewards_for_decision";
        let conn = self.readers.get();
        let mut stmt = conn.prepare_cached(REWARDS_FOR_DECISION).op(OP)?;
        let mut rows = stmt
            .query(params![key.tenant(), key.job(), key.capsule(), decision_id])
            .op(OP)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().op(OP)? {
            out.push(reward_from_row(key, row, 0, OP)?);
        }
        Ok(out)
    }

    fn rewards_since(
        &self,
        key: &CapsuleKey,
        after_seq: i64,
        limit: usize,
    ) -> Result<Vec<(RewardRecord, DecisionRecord)>> {
        const OP: &str = "rewards_since";
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.readers.get();
        let mut stmt = conn.prepare_cached(REWARDS_SINCE).op(OP)?;
        let mut rows = stmt
            .query(params![
                key.tenant(),
                key.job(),
                key.capsule(),
                after_seq,
                validate::limit(limit)
            ])
            .op(OP)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().op(OP)? {
            let reward = reward_from_row(key, row, 0, OP)?;
            // LEFT JOIN, so an orphaned reward shows up as a NULL decision
            // instead of vanishing from the replay. The foreign key prevents
            // orphans; one here means the file was written with foreign
            // keys off.
            let joined: Option<String> = row.get(REWARD_COLS).op(OP)?;
            if joined.is_none() {
                return Err(StoreError::corrupt(
                    OP,
                    format!(
                        "reward seq {} in {key} references missing decision {:?}",
                        reward.seq, reward.decision_id
                    ),
                ));
            }
            let decision = decision_from_row(key, row, REWARD_COLS, OP)?;
            out.push((reward, decision));
        }
        Ok(out)
    }

    fn logged_rows(
        &self,
        key: &CapsuleKey,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        rewards: RewardsMode,
    ) -> Result<Vec<LoggedRow>> {
        query_logged_rows(&self.readers.get(), key, since_ms, until_ms, rewards)
    }

    fn unrewarded_decisions(
        &self,
        key: &CapsuleKey,
        after: Option<(i64, &str)>,
        until_ms: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        const OP: &str = "unrewarded_decisions";
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = validate::limit(limit);
        let (after_ts, after_id) = after.unwrap_or((i64::MIN, ""));
        let conn = self.readers.get();
        let mut stmt = conn.prepare_cached(UNREWARDED_DECISIONS).op(OP)?;
        let rows = stmt
            .query_map(
                params![
                    key.tenant(),
                    key.job(),
                    key.capsule(),
                    after_ts,
                    after_id,
                    until_ms,
                    limit
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .op(OP)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().op(OP)
    }

    fn save_model(&self, s: &ModelSnapshot) -> Result<()> {
        const OP: &str = "save_model";
        validate::snapshot(s)?;
        let version = validate::u64_to_i64("model version", s.version)?;
        let k = &s.key;
        self.write(|conn| {
            conn.prepare_cached(SAVE_MODEL)
                .op(OP)?
                .execute(params![
                    k.tenant(),
                    k.job(),
                    k.capsule(),
                    version,
                    s.reward_seq,
                    s.ts_ms,
                    s.state
                ])
                .op(OP)?;
            Ok(())
        })
    }

    fn load_latest_model(&self, key: &CapsuleKey) -> Result<Option<ModelSnapshot>> {
        const OP: &str = "load_latest_model";
        let conn = self.readers.get();
        let mut stmt = conn.prepare_cached(LOAD_LATEST_MODEL).op(OP)?;
        let mut rows = stmt
            .query(params![key.tenant(), key.job(), key.capsule()])
            .op(OP)?;
        let Some(row) = rows.next().op(OP)? else {
            return Ok(None);
        };
        let version: i64 = row.get(0).op(OP)?;
        let version = u64::try_from(version).map_err(|_| {
            StoreError::corrupt(
                OP,
                format!("model snapshot in {key} has negative version {version}"),
            )
        })?;
        Ok(Some(ModelSnapshot {
            key: key.clone(),
            version,
            reward_seq: row.get(1).op(OP)?,
            ts_ms: row.get(2).op(OP)?,
            state: row.get(3).op(OP)?,
        }))
    }

    fn prune_models(&self, key: &CapsuleKey, keep: usize) -> Result<u64> {
        const OP: &str = "prune_models";
        self.write(|conn| {
            let n = conn
                .prepare_cached(PRUNE_MODELS)
                .op(OP)?
                .execute(params![
                    key.tenant(),
                    key.job(),
                    key.capsule(),
                    validate::limit(keep)
                ])
                .op(OP)?;
            Ok(n as u64)
        })
    }

    fn append_audit(
        &self,
        key: &CapsuleKey,
        ts_ms: i64,
        event: &str,
        detail_json: &str,
    ) -> Result<i64> {
        const OP: &str = "append_audit";
        validate::audit(event, detail_json)?;
        self.write(|conn| {
            conn.prepare_cached(APPEND_AUDIT)
                .op(OP)?
                .execute(params![
                    key.tenant(),
                    key.job(),
                    key.capsule(),
                    ts_ms,
                    event,
                    detail_json
                ])
                .op(OP)?;
            Ok(conn.last_insert_rowid())
        })
    }

    fn list_audit(&self, key: &CapsuleKey, limit: usize) -> Result<Vec<AuditRecord>> {
        const OP: &str = "list_audit";
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.readers.get();
        let mut stmt = conn.prepare_cached(LIST_AUDIT).op(OP)?;
        let mut rows = stmt
            .query(params![
                key.tenant(),
                key.job(),
                key.capsule(),
                validate::limit(limit)
            ])
            .op(OP)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().op(OP)? {
            out.push(AuditRecord {
                seq: row.get(0).op(OP)?,
                key: key.clone(),
                ts_ms: row.get(1).op(OP)?,
                event: row.get(2).op(OP)?,
                detail: row.get(3).op(OP)?,
            });
        }
        Ok(out)
    }

    fn delete_capsule(&self, key: &CapsuleKey) -> Result<u64> {
        const OP: &str = "delete_capsule";
        // Batches by primary key, not by time: every row goes, including
        // one whose ts_ms is damaged.
        let pruned = self.delete_decision_batches(
            DELETE_REWARDS_OF_DECISION_BATCH,
            DELETE_DECISION_BATCH,
            params![key.tenant(), key.job(), key.capsule(), DELETE_BATCH],
            OP,
        )?;
        let mut total = pruned.decisions + pruned.rewards;
        // Rewards without a decision can only exist in a file written with
        // foreign keys off; remove them too so the capsule is really gone.
        total += self.delete_batched(DELETE_REWARD_BATCH, key, OP)?;
        total += self.write(|conn| {
            let n = conn
                .prepare_cached(DELETE_MODELS)
                .op(OP)?
                .execute(params![key.tenant(), key.job(), key.capsule()])
                .op(OP)?;
            Ok(n as u64)
        })?;
        total += self.delete_batched(DELETE_AUDIT_BATCH, key, OP)?;
        Ok(total)
    }

    fn prune_decisions_before(&self, key: &CapsuleKey, before_ms: i64) -> Result<PruneCounts> {
        // `ts_ms < before_ms` as an inclusive bound; nothing is older than
        // i64::MIN.
        let Some(last_ts) = before_ms.checked_sub(1) else {
            return Ok(PruneCounts::default());
        };
        self.delete_decision_batches(
            DELETE_REWARDS_OF_OLDEST_DECISIONS,
            DELETE_OLDEST_DECISIONS,
            params![
                key.tenant(),
                key.job(),
                key.capsule(),
                last_ts,
                DELETE_BATCH
            ],
            "prune_decisions_before",
        )
    }

    fn stats(&self, key: &CapsuleKey) -> Result<CapsuleStats> {
        const OP: &str = "stats";
        let conn = self.readers.get();
        // One statement, so all five values come from one snapshot.
        let (decisions, rewards, first, last, last_seq) = conn
            .prepare_cached(STATS)
            .op(OP)?
            .query_row(params![key.tenant(), key.job(), key.capsule()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .op(OP)?;
        Ok(CapsuleStats {
            decisions: u64::try_from(decisions).unwrap_or(0),
            rewards: u64::try_from(rewards).unwrap_or(0),
            first_decision_ms: first,
            last_decision_ms: last,
            last_reward_seq: last_seq,
        })
    }
}

// ---------------------------------------------------------------------------
// Row writers shared by the single-row and batch paths. Both run on the
// writer connection, inside or outside an explicit transaction, and expect
// an already validated record.

fn insert_decision_row(
    conn: &Connection,
    d: &DecisionRecord,
    op: &'static str,
) -> Result<InsertOutcome> {
    let model_version = validate::u64_to_i64("model_version", d.model_version)?;
    let seed = validate::seed_to_sql(d.seed);
    let k = &d.key;
    for _ in 0..CONFLICT_RETRIES {
        let inserted = conn
            .prepare_cached(INSERT_DECISION)
            .op(op)?
            .execute(params![
                k.tenant(),
                k.job(),
                k.capsule(),
                d.id,
                d.ts_ms,
                model_version,
                d.mode,
                d.context,
                d.actions,
                d.eligible,
                d.pmf,
                d.chosen_index,
                d.chosen_id,
                d.probability,
                seed,
                d.derived,
                d.reason,
                d.request_sha256,
                d.program_sha256,
            ])
            .op(op)?;
        if inserted > 0 {
            return Ok(InsertOutcome::Inserted);
        }
        if let Some(existing) = select_decision(conn, k, &d.id, op)? {
            return Ok(InsertOutcome::Duplicate(Box::new(existing)));
        }
    }
    Err(StoreError::Busy {
        op,
        message: format!(
            "decision {:?} kept conflicting with a row deleted concurrently",
            d.id
        ),
    })
}

/// A reward for an unknown decision comes back as
/// [`RewardOutcome::UnknownDecision`]. The foreign-key failure rolls back
/// only the one statement, so an enclosing batch transaction stays open.
fn insert_reward_row(
    conn: &Connection,
    r: &RewardRecord,
    op: &'static str,
) -> Result<RewardOutcome> {
    let k = &r.key;
    for _ in 0..CONFLICT_RETRIES {
        // ON CONFLICT DO NOTHING makes the unique constraint the idempotency
        // check, atomic with the insert. The foreign key rejects unknown
        // decisions in the same statement.
        let inserted = conn.prepare_cached(INSERT_REWARD).op(op)?.execute(params![
            r.decision_id,
            k.tenant(),
            k.job(),
            k.capsule(),
            r.ts_ms,
            r.value,
            r.value_norm,
            r.idempotency_key,
            r.detail,
        ]);
        match inserted {
            Ok(0) => {}
            Ok(_) => return Ok(RewardOutcome::Applied(conn.last_insert_rowid())),
            Err(e) if is_foreign_key_violation(&e) => return Ok(RewardOutcome::UnknownDecision),
            Err(e) => return Err(StoreError::from_sqlite(op, e)),
        }
        let existing: Option<i64> = conn
            .prepare_cached(SELECT_REWARD_SEQ_BY_KEY)
            .op(op)?
            .query_row(
                params![k.tenant(), k.job(), k.capsule(), r.idempotency_key],
                |row| row.get(0),
            )
            .optional()
            .op(op)?;
        if let Some(seq) = existing {
            return Ok(RewardOutcome::Duplicate(seq));
        }
    }
    Err(StoreError::Busy {
        op,
        message: format!(
            "idempotency key {:?} kept conflicting with a row deleted concurrently",
            r.idempotency_key
        ),
    })
}

// ---------------------------------------------------------------------------
// Connections

fn open_writer(path: &Path) -> Result<Connection> {
    const OP: &str = "open";
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).op(OP)?;
    conn.busy_timeout(BUSY_TIMEOUT).op(OP)?;
    // Refuse foreign and newer files before anything writes to them
    // (switching to WAL rewrites the header).
    schema::inspect(&conn)?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .op(OP)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Io {
            op: OP,
            message: format!(
                "cannot enable WAL mode (journal_mode stays {mode}); \
                 the filesystem may not support shared-memory locking"
            ),
        });
    }
    conn.execute_batch(&format!(
        "PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA wal_autocheckpoint = 0;
         PRAGMA journal_size_limit = {JOURNAL_SIZE_LIMIT};"
    ))
    .op(OP)?;
    // Unknown-decision rejection relies on the foreign key; fail closed if
    // this SQLite build cannot enforce it.
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .op(OP)?;
    if fk != 1 {
        return Err(StoreError::Schema(
            "this SQLite build does not enforce foreign keys".into(),
        ));
    }
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(conn)
}

fn open_reader(path: &Path) -> Result<Connection> {
    const OP: &str = "open";
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).op(OP)?;
    conn.busy_timeout(BUSY_TIMEOUT).op(OP)?;
    conn.execute_batch("PRAGMA query_only = ON;").op(OP)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(conn)
}

/// [`EventStore::logged_rows`] on any connection.
fn query_logged_rows(
    conn: &Connection,
    key: &CapsuleKey,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    rewards: RewardsMode,
) -> Result<Vec<LoggedRow>> {
    const OP: &str = "logged_rows";
    let Some((lo, hi)) = validate::ts_bounds(since_ms, until_ms) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare_cached(LOGGED_ROWS).op(OP)?;
    let mut rows = stmt
        .query(params![key.tenant(), key.job(), key.capsule(), lo, hi])
        .op(OP)?;
    // Rows arrive ordered by (ts_ms, id, seq): one run of rows per
    // decision, its rewards in sequence order (one row with NULL reward
    // columns when it has none).
    let mut out = Vec::new();
    let mut current: Option<LoggedRow> = None;
    while let Some(row) = rows.next().op(OP)? {
        let id: String = row.get(0).op(OP)?;
        if current.as_ref().is_none_or(|cur| cur.decision.id != id) {
            out.extend(current.take());
            current = Some(LoggedRow {
                decision: decision_from_row(key, row, 0, OP)?,
                reward: None,
                reward_norm: None,
                reward_count: 0,
            });
        }
        let seq: Option<i64> = row.get(DECISION_COLS).op(OP)?;
        let Some(cur) = current.as_mut() else {
            continue;
        };
        if seq.is_none() {
            continue;
        }
        let value: f64 = row.get(DECISION_COLS + 1).op(OP)?;
        let norm: f64 = row.get(DECISION_COLS + 2).op(OP)?;
        cur.reward_count += 1;
        match rewards {
            RewardsMode::First => {
                if cur.reward.is_none() {
                    cur.reward = Some(value);
                    cur.reward_norm = Some(norm);
                }
            }
            RewardsMode::Sum => {
                cur.reward = Some(cur.reward.map_or(value, |acc| acc + value));
                cur.reward_norm = Some(cur.reward_norm.map_or(norm, |acc| acc + norm));
            }
        }
    }
    out.extend(current);
    Ok(out)
}

/// Reads one capsule's logged rows (see [`EventStore::logged_rows`]) from
/// the database at `path` without writing to it: no WAL switch, no
/// migration, no checkpoints. Safe on a live server's store, a backup copy
/// or a read-only mount. Refuses files that are not an event store of this
/// schema version (open an older one with the server once to migrate it).
pub fn read_logged_rows(
    path: impl AsRef<Path>,
    key: &CapsuleKey,
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    rewards: RewardsMode,
) -> Result<Vec<LoggedRow>> {
    let path = path.as_ref();
    let conn = open_read_only(path).map_err(|e| with_path(path, e))?;
    match schema::read_version(&conn).map_err(|e| with_path(path, e))? {
        Some(v) if v == schema::SCHEMA_VERSION => {}
        Some(v) => {
            return Err(with_path(
                path,
                StoreError::Schema(format!(
                    "database schema version {v}, this build reads {}",
                    schema::SCHEMA_VERSION
                )),
            ));
        }
        None => {
            return Err(with_path(
                path,
                StoreError::Schema("not a Syntra event store".into()),
            ));
        }
    }
    query_logged_rows(&conn, key, since_ms, until_ms, rewards)
}

/// A read-only connection that never writes: `immutable=1` when the file
/// has no WAL companions (so no `-shm` is created either), otherwise
/// `mode=ro`, which reads a live writer's WAL.
fn open_read_only(path: &Path) -> Result<Connection> {
    const OP: &str = "open";
    if !path.is_file() {
        return Err(StoreError::InvalidInput(format!(
            "{} does not exist",
            path.display()
        )));
    }
    let abs = path.canonicalize().map_err(|e| StoreError::Io {
        op: OP,
        message: e.to_string(),
    })?;
    let companion = |suffix: &str| {
        let mut p = abs.clone().into_os_string();
        p.push(suffix);
        PathBuf::from(p).exists()
    };
    let mode = if companion("-wal") || companion("-shm") {
        "mode=ro"
    } else {
        "immutable=1"
    };
    let uri = format!("file:{}?{mode}", uri_path(&abs));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(uri, flags).op(OP)?;
    conn.busy_timeout(BUSY_TIMEOUT).op(OP)?;
    conn.execute_batch("PRAGMA query_only = ON;").op(OP)?;
    Ok(conn)
}

/// Percent-encodes a path for a `file:` URI (everything but unreserved
/// characters and `/`).
fn uri_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn open_checkpointer(path: &Path) -> Result<Connection> {
    const OP: &str = "open";
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).op(OP)?;
    conn.busy_timeout(BUSY_TIMEOUT).op(OP)?;
    // synchronous = NORMAL makes each checkpoint sync the WAL before copying
    // and the database after; that sync is the durability point.
    conn.execute_batch(&format!(
        "PRAGMA synchronous = NORMAL;
         PRAGMA wal_autocheckpoint = 0;
         PRAGMA journal_size_limit = {JOURNAL_SIZE_LIMIT};"
    ))
    .op(OP)?;
    Ok(conn)
}

/// Adds the database path to errors raised while opening it.
fn with_path(path: &Path, err: StoreError) -> StoreError {
    let at = |message: String| format!("{}: {message}", path.display());
    match err {
        StoreError::Io { op, message } => StoreError::Io {
            op,
            message: at(message),
        },
        StoreError::Corrupt { op, message } => StoreError::Corrupt {
            op,
            message: at(message),
        },
        StoreError::Busy { op, message } => StoreError::Busy {
            op,
            message: at(message),
        },
        StoreError::Schema(message) => StoreError::Schema(at(message)),
        other => other,
    }
}

fn io_error(op: &'static str, path: &Path, err: std::io::Error) -> StoreError {
    StoreError::Io {
        op,
        message: format!("{}: {err}", path.display()),
    }
}

/// `db` + `-wal` etc. (SQLite appends to the full file name).
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Row decoding

/// Decision columns in `DecisionRecord` order, optionally table-qualified.
macro_rules! decision_columns {
    ($p:literal) => {
        concat!(
            $p,
            "id, ",
            $p,
            "ts_ms, ",
            $p,
            "model_version, ",
            $p,
            "mode, ",
            $p,
            "context, ",
            $p,
            "actions, ",
            $p,
            "eligible, ",
            $p,
            "pmf, ",
            $p,
            "chosen_index, ",
            $p,
            "chosen_id, ",
            $p,
            "probability, ",
            $p,
            "seed, ",
            $p,
            "derived, ",
            $p,
            "reason, ",
            $p,
            "request_sha256, ",
            $p,
            "program_sha256"
        )
    };
}

/// Number of columns produced by `decision_columns!`.
const DECISION_COLS: usize = 16;

/// Number of reward columns in `seq, decision_id, ts_ms, value, value_norm,
/// idempotency_key, detail` order.
const REWARD_COLS: usize = 7;

fn decision_from_row(
    key: &CapsuleKey,
    row: &Row<'_>,
    at: usize,
    op: &'static str,
) -> Result<DecisionRecord> {
    let id: String = row.get(at).op(op)?;
    let model_version: i64 = row.get(at + 2).op(op)?;
    let model_version = u64::try_from(model_version).map_err(|_| {
        StoreError::corrupt(
            op,
            format!("decision {id:?} in {key} has negative model_version {model_version}"),
        )
    })?;
    Ok(DecisionRecord {
        key: key.clone(),
        ts_ms: row.get(at + 1).op(op)?,
        model_version,
        mode: row.get(at + 3).op(op)?,
        context: row.get(at + 4).op(op)?,
        actions: row.get(at + 5).op(op)?,
        eligible: row.get(at + 6).op(op)?,
        pmf: row.get(at + 7).op(op)?,
        chosen_index: row.get(at + 8).op(op)?,
        chosen_id: row.get(at + 9).op(op)?,
        probability: row.get(at + 10).op(op)?,
        seed: validate::seed_from_sql(row.get(at + 11).op(op)?),
        derived: row.get(at + 12).op(op)?,
        reason: row.get(at + 13).op(op)?,
        request_sha256: row.get(at + 14).op(op)?,
        program_sha256: row.get(at + 15).op(op)?,
        id,
    })
}

fn reward_from_row(
    key: &CapsuleKey,
    row: &Row<'_>,
    at: usize,
    op: &'static str,
) -> Result<RewardRecord> {
    Ok(RewardRecord {
        seq: row.get(at).op(op)?,
        decision_id: row.get(at + 1).op(op)?,
        key: key.clone(),
        ts_ms: row.get(at + 2).op(op)?,
        value: row.get(at + 3).op(op)?,
        value_norm: row.get(at + 4).op(op)?,
        idempotency_key: row.get(at + 5).op(op)?,
        detail: row.get(at + 6).op(op)?,
    })
}

fn select_decision(
    conn: &Connection,
    key: &CapsuleKey,
    id: &str,
    op: &'static str,
) -> Result<Option<DecisionRecord>> {
    let mut stmt = conn.prepare_cached(SELECT_DECISION).op(op)?;
    let mut rows = stmt
        .query(params![key.tenant(), key.job(), key.capsule(), id])
        .op(op)?;
    match rows.next().op(op)? {
        Some(row) => Ok(Some(decision_from_row(key, row, 0, op)?)),
        None => Ok(None),
    }
}

fn query_decisions(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
    key: &CapsuleKey,
    op: &'static str,
) -> Result<Vec<DecisionRecord>> {
    let mut stmt = conn.prepare_cached(sql).op(op)?;
    let mut rows = stmt.query(args).op(op)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().op(op)? {
        out.push(decision_from_row(key, row, 0, op)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// SQL. Every statement filters on (tenant, job, capsule) first, the leading
// columns of every index, so no query crosses capsule boundaries.

const INSERT_DECISION: &str = "\
INSERT INTO decisions (tenant, job, capsule, id, ts_ms, model_version, mode, context, actions,
  eligible, pmf, chosen_index, chosen_id, probability, seed, derived, reason, request_sha256,
  program_sha256)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
ON CONFLICT (tenant, job, capsule, id) DO NOTHING";

const SELECT_DECISION: &str = concat!(
    "SELECT ",
    decision_columns!(""),
    " FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND id = ?4"
);

const SELECT_DECISION_TS: &str =
    "SELECT ts_ms FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND id = ?4";

const LIST_DECISIONS: &str = concat!(
    "SELECT ",
    decision_columns!(""),
    " FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3",
    " AND ts_ms >= ?4 AND ts_ms <= ?5 ORDER BY ts_ms, id LIMIT ?6"
);

const LIST_DECISIONS_AFTER: &str = concat!(
    "SELECT ",
    decision_columns!(""),
    " FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3",
    " AND (ts_ms, id) > (?4, ?5) AND ts_ms <= ?6 ORDER BY ts_ms, id LIMIT ?7"
);

const INSERT_REWARD: &str = "\
INSERT INTO rewards (decision_id, tenant, job, capsule, ts_ms, value, value_norm,
  idempotency_key, detail)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT (tenant, job, capsule, idempotency_key) DO NOTHING";

const SELECT_REWARD_SEQ_BY_KEY: &str = "SELECT seq FROM rewards \
WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND idempotency_key = ?4";

const REWARDS_FOR_DECISION: &str = "\
SELECT seq, decision_id, ts_ms, value, value_norm, idempotency_key, detail FROM rewards
WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND decision_id = ?4 ORDER BY seq";

const REWARDS_SINCE: &str = concat!(
    "SELECT r.seq, r.decision_id, r.ts_ms, r.value, r.value_norm, r.idempotency_key, r.detail, ",
    decision_columns!("d."),
    " FROM rewards r LEFT JOIN decisions d",
    " ON d.tenant = r.tenant AND d.job = r.job AND d.capsule = r.capsule AND d.id = r.decision_id",
    " WHERE r.tenant = ?1 AND r.job = ?2 AND r.capsule = ?3 AND r.seq > ?4",
    " ORDER BY r.seq LIMIT ?5"
);

const LOGGED_ROWS: &str = concat!(
    "SELECT ",
    decision_columns!("d."),
    ", r.seq, r.value, r.value_norm",
    " FROM decisions d LEFT JOIN rewards r",
    " ON r.tenant = d.tenant AND r.job = d.job AND r.capsule = d.capsule AND r.decision_id = d.id",
    " WHERE d.tenant = ?1 AND d.job = ?2 AND d.capsule = ?3 AND d.ts_ms >= ?4 AND d.ts_ms <= ?5",
    " ORDER BY d.ts_ms, d.id, r.seq"
);

const UNREWARDED_DECISIONS: &str = "\
SELECT d.ts_ms, d.id FROM decisions d
WHERE d.tenant = ?1 AND d.job = ?2 AND d.capsule = ?3
  AND (d.ts_ms, d.id) > (?4, ?5) AND d.ts_ms <= ?6
  AND NOT EXISTS (SELECT 1 FROM rewards r WHERE r.tenant = d.tenant AND r.job = d.job
                  AND r.capsule = d.capsule AND r.decision_id = d.id)
ORDER BY d.ts_ms, d.id LIMIT ?7";

const SAVE_MODEL: &str = "\
INSERT INTO models (tenant, job, capsule, version, reward_seq, ts_ms, state)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT (tenant, job, capsule, version) DO UPDATE SET
  reward_seq = excluded.reward_seq, ts_ms = excluded.ts_ms, state = excluded.state";

const LOAD_LATEST_MODEL: &str = "\
SELECT version, reward_seq, ts_ms, state FROM models
WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 ORDER BY version DESC LIMIT 1";

const PRUNE_MODELS: &str = "\
DELETE FROM models WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND version NOT IN (
  SELECT version FROM models WHERE tenant = ?1 AND job = ?2 AND capsule = ?3
  ORDER BY version DESC LIMIT ?4)";

const APPEND_AUDIT: &str = "\
INSERT INTO audit (tenant, job, capsule, ts_ms, event, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

const LIST_AUDIT: &str = "\
SELECT seq, ts_ms, event, detail FROM (
  SELECT seq, ts_ms, event, detail FROM audit
  WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 ORDER BY seq DESC LIMIT ?4)
ORDER BY seq";

const DELETE_REWARDS_OF_OLDEST_DECISIONS: &str = "\
DELETE FROM rewards WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND decision_id IN (
  SELECT id FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND ts_ms <= ?4
  ORDER BY ts_ms, id LIMIT ?5)";

const DELETE_OLDEST_DECISIONS: &str = "\
DELETE FROM decisions WHERE rowid IN (
  SELECT rowid FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND ts_ms <= ?4
  ORDER BY ts_ms, id LIMIT ?5)";

const DELETE_REWARDS_OF_DECISION_BATCH: &str = "\
DELETE FROM rewards WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 AND decision_id IN (
  SELECT id FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3
  ORDER BY id LIMIT ?4)";

const DELETE_DECISION_BATCH: &str = "\
DELETE FROM decisions WHERE rowid IN (
  SELECT rowid FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3
  ORDER BY id LIMIT ?4)";

const DELETE_REWARD_BATCH: &str = "\
DELETE FROM rewards WHERE seq IN (
  SELECT seq FROM rewards WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 ORDER BY seq LIMIT ?4)";

const DELETE_MODELS: &str = "DELETE FROM models WHERE tenant = ?1 AND job = ?2 AND capsule = ?3";

const DELETE_AUDIT_BATCH: &str = "\
DELETE FROM audit WHERE seq IN (
  SELECT seq FROM audit WHERE tenant = ?1 AND job = ?2 AND capsule = ?3 ORDER BY seq LIMIT ?4)";

const STATS: &str = "\
SELECT
  (SELECT count(*) FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3),
  (SELECT count(*) FROM rewards WHERE tenant = ?1 AND job = ?2 AND capsule = ?3),
  (SELECT ts_ms FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3
     ORDER BY ts_ms LIMIT 1),
  (SELECT ts_ms FROM decisions WHERE tenant = ?1 AND job = ?2 AND capsule = ?3
     ORDER BY ts_ms DESC LIMIT 1),
  (SELECT seq FROM rewards WHERE tenant = ?1 AND job = ?2 AND capsule = ?3
     ORDER BY seq DESC LIMIT 1)";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    /// A fresh directory per test, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "syntra-eventstore-unit-{tag}-{}-{nanos}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn db(&self) -> PathBuf {
            self.0.join("syntra.db")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn key() -> CapsuleKey {
        CapsuleKey::new("t", "j", "c").unwrap()
    }

    fn decision(id: &str, ts_ms: i64) -> DecisionRecord {
        DecisionRecord {
            id: id.to_string(),
            key: key(),
            ts_ms,
            model_version: 1,
            mode: "learner".into(),
            context: r#"{"x":1}"#.into(),
            actions: r#"[{"id":"a"},{"id":"b"}]"#.into(),
            eligible: r#"["a","b"]"#.into(),
            pmf: Some("[0.5,0.5]".into()),
            chosen_index: 0,
            chosen_id: "a".into(),
            probability: Some(0.5),
            seed: 7,
            derived: "{}".into(),
            reason: None,
            request_sha256: "ab".into(),
            program_sha256: None,
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn plan(conn: &Connection, sql: &str) -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = std::iter::repeat_n(rusqlite::types::Null, stmt.parameter_count());
        let lines: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(nulls), |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        lines.join("\n")
    }

    #[test]
    fn queries_stay_on_capsule_indexes() {
        // Regression guard: a full scan here costs time proportional to every
        // tenant's history, and a temp B-tree costs memory proportional to it.
        let dir = TempDir::new("plans");
        let store = SqliteStore::open(dir.db()).unwrap();
        let conn = store.readers.get();
        let statements = [
            ("SELECT_DECISION", SELECT_DECISION),
            ("SELECT_DECISION_TS", SELECT_DECISION_TS),
            ("LIST_DECISIONS", LIST_DECISIONS),
            ("LIST_DECISIONS_AFTER", LIST_DECISIONS_AFTER),
            ("SELECT_REWARD_SEQ_BY_KEY", SELECT_REWARD_SEQ_BY_KEY),
            ("REWARDS_FOR_DECISION", REWARDS_FOR_DECISION),
            ("REWARDS_SINCE", REWARDS_SINCE),
            ("LOGGED_ROWS", LOGGED_ROWS),
            ("LOAD_LATEST_MODEL", LOAD_LATEST_MODEL),
            ("PRUNE_MODELS", PRUNE_MODELS),
            ("LIST_AUDIT", LIST_AUDIT),
            (
                "DELETE_REWARDS_OF_OLDEST_DECISIONS",
                DELETE_REWARDS_OF_OLDEST_DECISIONS,
            ),
            ("DELETE_OLDEST_DECISIONS", DELETE_OLDEST_DECISIONS),
            (
                "DELETE_REWARDS_OF_DECISION_BATCH",
                DELETE_REWARDS_OF_DECISION_BATCH,
            ),
            ("DELETE_DECISION_BATCH", DELETE_DECISION_BATCH),
            ("DELETE_REWARD_BATCH", DELETE_REWARD_BATCH),
            ("DELETE_MODELS", DELETE_MODELS),
            ("DELETE_AUDIT_BATCH", DELETE_AUDIT_BATCH),
            ("STATS", STATS),
        ];
        for (name, sql) in statements {
            let p = plan(&conn, sql);
            for table in ["decisions", "rewards", "models", "audit", "d", "r"] {
                assert!(
                    !p.contains(&format!("SCAN {table}")),
                    "{name} scans {table}:\n{p}"
                );
            }
            // LIST_AUDIT re-sorts at most `limit` rows into ascending order.
            if name != "LIST_AUDIT" {
                assert!(
                    !p.contains("TEMP B-TREE"),
                    "{name} sorts in a temp B-tree:\n{p}"
                );
            }
        }
        assert!(plan(&conn, REWARDS_SINCE).contains("rewards_capsule_seq"));
        assert!(plan(&conn, LIST_DECISIONS_AFTER).contains("(ts_ms,id)>(?,?)"));
        assert!(plan(&conn, LOGGED_ROWS).contains("rewards_decision"));
        // Deleting decisions checks the foreign key through the child index.
        assert!(plan(&conn, DELETE_OLDEST_DECISIONS).contains("rewards_decision"));
    }

    #[test]
    fn pooled_connections_are_read_only() {
        let dir = TempDir::new("readonly");
        let store = SqliteStore::open(dir.db()).unwrap();
        let conn = store.readers.get();
        let err = conn
            .execute(
                "INSERT INTO audit (tenant, job, capsule, ts_ms, event, detail) \
                 VALUES ('t', 'j', 'c', 1, 'e', '{}')",
                [],
            )
            .unwrap_err();
        let mapped = StoreError::from_sqlite("test", err);
        assert!(matches!(mapped, StoreError::Io { .. }), "{mapped}");
    }

    #[test]
    fn corrupt_rows_surface_as_errors() {
        let dir = TempDir::new("corrupt");
        let store = SqliteStore::open(dir.db()).unwrap();
        let k = key();
        for id in ["neg-version", "text-ts", "fine"] {
            store.insert_decision(&decision(id, 1)).unwrap();
        }
        // Write what this build never writes, through a side connection with
        // foreign keys off.
        let raw = Connection::open(dir.db()).unwrap();
        raw.execute_batch(
            "PRAGMA foreign_keys = OFF;
             UPDATE decisions SET model_version = -5 WHERE id = 'neg-version';
             UPDATE decisions SET ts_ms = 'soon' WHERE id = 'text-ts';
             INSERT INTO rewards (decision_id, tenant, job, capsule, ts_ms, value, value_norm,
                                  idempotency_key)
             VALUES ('ghost', 't', 'j', 'c', 1, 1.0, 1.0, 'ghost');",
        )
        .unwrap();

        let err = store.get_decision(&k, "neg-version").unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");
        assert!(err.to_string().contains("negative model_version"), "{err}");

        let err = store.get_decision(&k, "text-ts").unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");

        assert!(store.get_decision(&k, "fine").unwrap().is_some());

        let err = store.rewards_since(&k, 0, 10).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");
        assert!(err.to_string().contains("missing decision"), "{err}");

        let report = store.integrity_check().unwrap();
        assert!(!report.is_ok());
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.contains("references a missing decisions row")),
            "{report:?}"
        );

        // delete_capsule removes the orphan too, which leaves a sound file.
        assert_eq!(store.delete_capsule(&k).unwrap(), 4);
        assert!(store.integrity_check().unwrap().is_ok());
    }

    #[test]
    fn checkpointer_counts_pages_of_single_writes() {
        let dir = TempDir::new("ckpt-single");
        let options = SqliteOptions {
            checkpoint_after_pages: 20,
            checkpoint_interval: Duration::from_secs(3600),
            ..SqliteOptions::default()
        };
        let store = SqliteStore::open_with(dir.db(), options).unwrap();
        // Each single-row commit appends a few pages (table and index leaves).
        for i in 0..12 {
            store
                .insert_decision(&decision(&format!("d{i}"), i))
                .unwrap();
        }
        wait_until("a page-threshold checkpoint", || {
            store.checkpointer.completed() >= 1
        });
    }

    #[test]
    fn checkpointer_counts_pages_of_batches() {
        // One batch call is one transaction but many pages (about 40 for
        // these 1000 small rows); counting calls would never reach 20.
        let dir = TempDir::new("ckpt-batch");
        let options = SqliteOptions {
            checkpoint_after_pages: 20,
            checkpoint_interval: Duration::from_secs(3600),
            ..SqliteOptions::default()
        };
        let store = SqliteStore::open_with(dir.db(), options).unwrap();
        let batch: Vec<DecisionRecord> = (0..1000)
            .map(|i| decision(&format!("b{i:04}"), i))
            .collect();
        let outcomes = store.insert_decisions(&batch).unwrap();
        assert!(outcomes.iter().all(|o| *o == InsertOutcome::Inserted));
        wait_until("a checkpoint after one large batch", || {
            store.checkpointer.completed() >= 1
        });
    }

    #[test]
    fn checkpointer_runs_after_interval_and_not_when_idle() {
        let dir = TempDir::new("ckpt-interval");
        let options = SqliteOptions {
            checkpoint_after_pages: u64::MAX,
            checkpoint_interval: Duration::from_millis(20),
            ..SqliteOptions::default()
        };
        let store = SqliteStore::open_with(dir.db(), options).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            store.checkpointer.completed(),
            0,
            "an idle store must not checkpoint"
        );

        let main_file_len = || std::fs::metadata(dir.db()).map(|m| m.len()).unwrap_or(0);
        let before = main_file_len();
        for i in 0..50 {
            store
                .insert_decision(&decision(&format!("d{i}"), i))
                .unwrap();
        }
        wait_until("an interval checkpoint", || {
            store.checkpointer.completed() >= 1
        });
        // The checkpoint copied WAL pages into the main database file.
        wait_until("the main file to grow", || main_file_len() > before);
    }

    #[test]
    fn writer_recovers_from_a_panic_mid_transaction() {
        let dir = TempDir::new("poison");
        let store = SqliteStore::open(dir.db()).unwrap();
        let k = key();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = store.write(|conn| -> Result<()> {
                conn.execute_batch(
                    "BEGIN IMMEDIATE;
                     INSERT INTO audit (tenant, job, capsule, ts_ms, event, detail)
                     VALUES ('t', 'j', 'c', 1, 'half-done', '{}');",
                )
                .unwrap();
                panic!("simulated panic while holding the writer");
            });
        }));
        assert!(outcome.is_err());
        assert!(store.writer.is_poisoned());

        store.append_audit(&k, 2, "after", "{}").unwrap();
        assert!(!store.writer.is_poisoned());
        let events: Vec<String> = store
            .list_audit(&k, 10)
            .unwrap()
            .into_iter()
            .map(|a| a.event)
            .collect();
        assert_eq!(
            events,
            vec!["after".to_string()],
            "the half-done insert must roll back"
        );
    }

    #[test]
    fn open_refuses_bad_targets_without_modifying_them() {
        let dir = TempDir::new("open");

        let err = SqliteStore::open(dir.0.join("missing-dir").join("syntra.db")).unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }), "{err}");
        assert!(err.to_string().contains("missing-dir"), "{err}");

        let err = SqliteStore::open(":memory:").unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)), "{err}");

        let junk_path = dir.0.join("junk.db");
        let junk = vec![b'#'; 4096];
        std::fs::write(&junk_path, &junk).unwrap();
        let err = SqliteStore::open(&junk_path).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");
        assert!(err.to_string().contains("junk.db"), "{err}");
        assert_eq!(std::fs::read(&junk_path).unwrap(), junk);

        let foreign = dir.0.join("foreign.db");
        Connection::open(&foreign)
            .unwrap()
            .execute_batch("CREATE TABLE shops (name TEXT); INSERT INTO shops VALUES ('a');")
            .unwrap();
        let before = std::fs::read(&foreign).unwrap();
        let err = SqliteStore::open(&foreign).unwrap_err();
        assert!(matches!(err, StoreError::Schema(_)), "{err}");
        // Refused before the WAL switch, so not a byte changed.
        assert_eq!(std::fs::read(&foreign).unwrap(), before);
        assert!(!sidecar(&foreign, "-wal").exists());
    }
}
