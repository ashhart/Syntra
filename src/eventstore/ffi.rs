//! The two SQLite C APIs the store needs that rusqlite does not expose with
//! the crate's feature set (rusqlite is built without `backup`). All of the
//! event store's `unsafe` code is in this file.

use super::{Result, StoreError};
use rusqlite::{Connection, ffi};
use std::ffi::CStr;
use std::os::raw::c_int;

/// Copies the whole `main` database of `src` into `dst` with the online
/// backup API, in one `sqlite3_backup_step(-1)` call. The copy is a single
/// read transaction on `src`, so it is a consistent snapshot. In WAL mode it
/// does not block writers, and it cannot be restarted by concurrent writes
/// the way a stepwise backup can.
pub(crate) fn copy_database(src: &Connection, dst: &Connection) -> Result<()> {
    const OP: &str = "backup_to";
    // SAFETY: both handles come from open `Connection`s that this function
    // borrows for its whole duration, so they stay valid and no other thread
    // uses them (a `Connection` is `!Sync`, and the caller holds the source
    // exclusively through the read pool). The backup object is created and
    // finished within this block and never escapes it. `c"main"` is a static
    // NUL-terminated string. `sqlite3_errmsg` is read immediately after the
    // failing call, before any other call on that connection.
    unsafe {
        let dst_db = dst.handle();
        let src_db = src.handle();
        let backup = ffi::sqlite3_backup_init(dst_db, c"main".as_ptr(), src_db, c"main".as_ptr());
        if backup.is_null() {
            return Err(error_from(
                OP,
                ffi::sqlite3_extended_errcode(dst_db),
                dst_db,
            ));
        }
        let step = ffi::sqlite3_backup_step(backup, -1);
        // finish() releases the backup object and reports any error from the
        // step calls through its return value and the destination handle.
        let finish = ffi::sqlite3_backup_finish(backup);
        if step != ffi::SQLITE_DONE {
            let code = if finish != ffi::SQLITE_OK {
                finish
            } else {
                step
            };
            return Err(error_from(OP, code, dst_db));
        }
        if finish != ffi::SQLITE_OK {
            return Err(error_from(OP, finish, dst_db));
        }
    }
    Ok(())
}

/// Returns the number of pages `conn` has written since the previous call
/// and resets the counter (`SQLITE_DBSTATUS_CACHE_WRITE`). In WAL mode that
/// is the number of frames it appended to the WAL. On the unexpected error
/// path it reports 1, so callers still register that a write happened.
pub(crate) fn take_pages_written(conn: &Connection) -> u64 {
    let (mut current, mut highwater): (c_int, c_int) = (0, 0);
    // SAFETY: the handle belongs to an open connection borrowed for the
    // call; the out-pointers are valid locals. sqlite3_db_status only reads
    // and resets a counter on that connection.
    let rc = unsafe {
        ffi::sqlite3_db_status(
            conn.handle(),
            ffi::SQLITE_DBSTATUS_CACHE_WRITE,
            &mut current,
            &mut highwater,
            1,
        )
    };
    if rc == ffi::SQLITE_OK {
        u64::try_from(current).unwrap_or(0)
    } else {
        1
    }
}

/// # Safety
/// `db` must be a valid, open connection handle.
unsafe fn error_from(op: &'static str, code: i32, db: *mut ffi::sqlite3) -> StoreError {
    // SAFETY: guaranteed by the caller; sqlite3_errmsg never returns NULL for
    // a valid handle, but a NULL is handled anyway.
    let message = unsafe {
        let msg = ffi::sqlite3_errmsg(db);
        if msg.is_null() {
            None
        } else {
            Some(CStr::from_ptr(msg).to_string_lossy().into_owned())
        }
    };
    StoreError::from_sqlite(
        op,
        rusqlite::Error::SqliteFailure(ffi::Error::new(code), message),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_a_database_between_connections() {
        let src = Connection::open_in_memory().unwrap();
        src.execute_batch(
            "CREATE TABLE t (x INTEGER, s TEXT); INSERT INTO t VALUES (1, 'ü'), (2, '🚀');",
        )
        .unwrap();
        let dst = Connection::open_in_memory().unwrap();
        copy_database(&src, &dst).unwrap();
        let rows: Vec<(i64, String)> = dst
            .prepare("SELECT x, s FROM t ORDER BY x")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(1, "ü".to_string()), (2, "🚀".to_string())]);
    }

    #[test]
    fn reports_errors_instead_of_panicking() {
        // Backing up into a connection that is inside a read transaction on
        // its own database fails with SQLITE_ERROR ("destination database is
        // in use"). That exercises the error path.
        let src = Connection::open_in_memory().unwrap();
        src.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
        let dst = Connection::open_in_memory().unwrap();
        dst.execute_batch("CREATE TABLE u (y INTEGER); INSERT INTO u VALUES (1);")
            .unwrap();
        let mut stmt = dst.prepare("SELECT y FROM u").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let _row = rows.next().unwrap();
        let err = copy_database(&src, &dst).unwrap_err();
        assert!(err.to_string().contains("backup_to"), "{err}");
    }

    #[test]
    fn counts_pages_written_and_resets() {
        let dir = std::env::temp_dir().join(format!(
            "syntra-eventstore-ffi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("pages.db")).unwrap();
        conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        conn.execute_batch("CREATE TABLE t (x BLOB);").unwrap();
        take_pages_written(&conn);
        assert_eq!(take_pages_written(&conn), 0, "the counter resets");
        conn.execute("INSERT INTO t VALUES (zeroblob(40000))", [])
            .unwrap();
        // About ten 4 KiB pages of blob plus the table's b-tree page.
        let pages = take_pages_written(&conn);
        assert!((10..=16).contains(&pages), "{pages}");
        assert_eq!(take_pages_written(&conn), 0);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
