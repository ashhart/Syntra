//! Schema definition and migrations.
//!
//! `schema_version` holds one row per applied migration; the current version
//! is the maximum. Migrations run inside one `BEGIN IMMEDIATE` transaction,
//! so concurrent openers (other processes included) serialize, and a failed
//! migration leaves the file unchanged. A file whose version is newer than
//! [`SCHEMA_VERSION`] is refused instead of being written by an older build.

use super::error::SqlContext;
use super::{Result, StoreError};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

/// Schema version written by this build.
pub const SCHEMA_VERSION: i64 = 1;

/// Version 1: the design's schema (docs/design/v2-decision-core.md,
/// "Storage") with these changes:
///
/// * Decisions are keyed by `(tenant, job, capsule, id)`, not by `id` alone.
///   Ids are client-supplied (`eventId`) and unique per capsule, so a global
///   key would let one tenant's id collide with, or reveal, another's.
///   Rewards reference decisions by the same four columns.
/// * Columns a record always carries are `NOT NULL`. A NULL in a key column
///   would also silently disable the foreign key.
/// * `rewards` has a foreign key to `decisions`, so a reward for an unknown
///   decision is rejected by the same statement that would insert it.
/// * Indexes are scoped by capsule and justified inline.
const V1: &str = r#"
CREATE TABLE decisions (
  id             TEXT    NOT NULL,
  tenant         TEXT    NOT NULL,
  job            TEXT    NOT NULL,
  capsule        TEXT    NOT NULL,
  ts_ms          INTEGER NOT NULL,
  model_version  INTEGER NOT NULL,
  mode           TEXT    NOT NULL,
  context        TEXT    NOT NULL,
  actions        TEXT    NOT NULL,
  eligible       TEXT    NOT NULL,
  pmf            TEXT,
  chosen_index   INTEGER NOT NULL,
  chosen_id      TEXT    NOT NULL,
  probability    REAL,
  seed           INTEGER NOT NULL,
  derived        TEXT    NOT NULL,
  reason         TEXT,
  request_sha256 TEXT    NOT NULL,
  program_sha256 TEXT,
  PRIMARY KEY (tenant, job, capsule, id)
);

-- Listing, OPE windows and pruning, in (ts_ms, id) order: the index serves
-- ORDER BY and cursor seeks without a sort. It is UNIQUE because
-- (tenant, job, capsule, id) already is; declaring it tells the planner the
-- order is distinct, so the decision/reward join in logged_rows needs no
-- temporary sort either.
CREATE UNIQUE INDEX decisions_capsule_ts ON decisions (tenant, job, capsule, ts_ms, id);

CREATE TABLE rewards (
  seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  decision_id     TEXT    NOT NULL,
  tenant          TEXT    NOT NULL,
  job             TEXT    NOT NULL,
  capsule         TEXT    NOT NULL,
  ts_ms           INTEGER NOT NULL,
  value           REAL    NOT NULL,
  value_norm      REAL    NOT NULL,
  idempotency_key TEXT    NOT NULL,
  detail          TEXT,
  UNIQUE (tenant, job, capsule, idempotency_key),
  FOREIGN KEY (tenant, job, capsule, decision_id)
    REFERENCES decisions (tenant, job, capsule, id)
);

-- Rewards of one decision (reward listing, OPE join). It is also the
-- child-key index SQLite uses to check the foreign key when decisions are
-- deleted; without it every deleted decision would scan the rewards table.
CREATE INDEX rewards_decision ON rewards (tenant, job, capsule, decision_id);

-- Model replay: one capsule's rewards after a watermark, in seq order.
CREATE INDEX rewards_capsule_seq ON rewards (tenant, job, capsule, seq);

CREATE TABLE models (
  tenant     TEXT    NOT NULL,
  job        TEXT    NOT NULL,
  capsule    TEXT    NOT NULL,
  version    INTEGER NOT NULL,
  reward_seq INTEGER NOT NULL,
  ts_ms      INTEGER NOT NULL,
  state      BLOB    NOT NULL,
  PRIMARY KEY (tenant, job, capsule, version)
);

CREATE TABLE audit (
  seq     INTEGER PRIMARY KEY AUTOINCREMENT,
  tenant  TEXT    NOT NULL,
  job     TEXT    NOT NULL,
  capsule TEXT    NOT NULL,
  ts_ms   INTEGER NOT NULL,
  event   TEXT    NOT NULL,
  detail  TEXT    NOT NULL
);

-- Latest-N audit listing per capsule, and batched capsule deletion.
CREATE INDEX audit_capsule_seq ON audit (tenant, job, capsule, seq);
"#;

/// `(version, sql)` in application order.
const MIGRATIONS: &[(i64, &str)] = &[(1, V1)];

/// Classifies the file without writing to it. Returns the applied version
/// (0 for an empty database), or a [`StoreError::Schema`] error for a file
/// that is not an event store or that a newer build wrote. `open` calls this
/// before anything that would modify the file, such as switching to WAL.
pub(crate) fn inspect(conn: &Connection) -> Result<i64> {
    match read_version(conn)? {
        Some(v) if v > SCHEMA_VERSION => Err(StoreError::Schema(format!(
            "database schema version {v} is newer than this build supports \
             ({SCHEMA_VERSION}); refusing to open it"
        ))),
        Some(v) => Ok(v),
        None => {
            let user_objects: i64 = conn
                .query_row(
                    r"SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite\_%' ESCAPE '\'",
                    [],
                    |r| r.get(0),
                )
                .op("schema_version")?;
            if user_objects > 0 {
                return Err(StoreError::Schema(
                    "the database has tables but no schema_version table; \
                     it is not a Syntra event store"
                        .into(),
                ));
            }
            Ok(0)
        }
    }
}

/// Brings the database up to [`SCHEMA_VERSION`]. Idempotent.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    const OP: &str = "migrate";
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .op(OP)?;
    // Re-inspect under the write lock: another process may have migrated
    // the file since `open` looked at it.
    let current = inspect(&tx)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL PRIMARY KEY);",
    )
    .op(OP)?;
    for (version, sql) in MIGRATIONS {
        if *version > current {
            tx.execute_batch(sql).op(OP)?;
            tx.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                [version],
            )
            .op(OP)?;
        }
    }
    tx.commit().op(OP)
}

/// The applied schema version, or `None` when the file has no
/// `schema_version` table.
pub(crate) fn read_version(conn: &Connection) -> Result<Option<i64>> {
    const OP: &str = "schema_version";
    let has_table: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .op(OP)?;
    if has_table.is_none() {
        return Ok(None);
    }
    let version: Option<i64> = conn
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .op(OP)?;
    Ok(Some(version.unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type IN ('table', 'index') ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn fresh_database_gets_latest_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), Some(SCHEMA_VERSION));
        let names = tables(&conn);
        for expected in [
            "audit",
            "audit_capsule_seq",
            "decisions",
            "decisions_capsule_ts",
            "models",
            "rewards",
            "rewards_capsule_seq",
            "rewards_decision",
            "schema_version",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing {expected}: {names:?}"
            );
        }
    }

    #[test]
    fn migration_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let before = tables(&conn);
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(tables(&conn), before);
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, MIGRATIONS.len() as i64);
    }

    #[test]
    fn newer_schema_is_refused_and_left_untouched() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [SCHEMA_VERSION + 1],
        )
        .unwrap();
        let err = migrate(&mut conn).unwrap_err();
        assert!(matches!(err, StoreError::Schema(_)), "{err}");
        assert!(err.to_string().contains("newer"), "{err}");
        assert_eq!(read_version(&conn).unwrap(), Some(SCHEMA_VERSION + 1));
    }

    #[test]
    fn foreign_database_is_refused_and_left_untouched() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE shops (name TEXT); INSERT INTO shops VALUES ('a');")
            .unwrap();
        let err = inspect(&conn).unwrap_err();
        assert!(matches!(err, StoreError::Schema(_)), "{err}");
        let err = migrate(&mut conn).unwrap_err();
        assert!(matches!(err, StoreError::Schema(_)), "{err}");
        assert_eq!(tables(&conn), vec!["shops".to_string()]);
    }

    #[test]
    fn inspect_classifies_without_writing() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(inspect(&conn).unwrap(), 0);
        assert!(tables(&conn).is_empty(), "inspect must not create tables");
        migrate(&mut conn).unwrap();
        assert_eq!(inspect(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn failed_migration_rolls_back() {
        // A half-applied migration must not leave a version row behind: the
        // DDL and the version insert commit together or not at all.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY);
             CREATE TABLE audit (x INTEGER);",
        )
        .unwrap();
        assert!(migrate(&mut conn).is_err());
        assert_eq!(read_version(&conn).unwrap(), Some(0));
        let names = tables(&conn);
        assert!(!names.iter().any(|n| n == "decisions"), "{names:?}");
    }
}
