//! Error type for the event store and the mapping from SQLite errors.

use super::CapsuleKey;
use rusqlite::ErrorCode;
use std::fmt;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Everything the event store can fail with. `op` names the store operation
/// (`insert_reward`, `open`, ...) so messages say what was being attempted.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// A caller-supplied value was rejected before touching the database:
    /// invalid capsule key or id, text that is not JSON, a non-finite
    /// number, an integer outside SQLite's range, an oversized batch.
    InvalidInput(String),
    /// A reward named a decision id that does not exist in its capsule.
    UnknownDecision {
        key: CapsuleKey,
        decision_id: String,
    },
    /// A paging cursor named a decision that does not exist (any more).
    UnknownCursor {
        key: CapsuleKey,
        decision_id: String,
    },
    /// The database stayed locked for longer than the busy timeout. The
    /// operation had no effect and can be retried.
    Busy { op: &'static str, message: String },
    /// SQLite reported corruption, or a stored value failed validation on
    /// read (wrong type, negative version, orphaned reward, ...).
    Corrupt { op: &'static str, message: String },
    /// Filesystem-level failure: cannot open, disk full, I/O error,
    /// permission denied, read-only filesystem.
    Io { op: &'static str, message: String },
    /// The file is not a Syntra event store, or its schema version is newer
    /// than this build understands.
    Schema(String),
    /// A constraint violation not covered by a more specific variant.
    Constraint { op: &'static str, message: String },
    /// Any other SQLite error.
    Sqlite {
        op: &'static str,
        source: rusqlite::Error,
    },
}

impl StoreError {
    /// True for transient lock contention; the operation can be retried.
    pub fn is_busy(&self) -> bool {
        matches!(self, StoreError::Busy { .. })
    }

    /// Maps a rusqlite error, attributing it to `op`.
    pub(crate) fn from_sqlite(op: &'static str, err: rusqlite::Error) -> Self {
        match &err {
            rusqlite::Error::SqliteFailure(code, msg) => {
                let message = match msg {
                    Some(m) => format!("{m} (sqlite code {})", code.extended_code),
                    None => code.to_string(),
                };
                match code.code {
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                        StoreError::Busy { op, message }
                    }
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => {
                        StoreError::Corrupt { op, message }
                    }
                    ErrorCode::SystemIoFailure
                    | ErrorCode::DiskFull
                    | ErrorCode::CannotOpen
                    | ErrorCode::PermissionDenied
                    | ErrorCode::ReadOnly
                    | ErrorCode::NoLargeFileSupport
                    | ErrorCode::FileLockingProtocolFailed => StoreError::Io { op, message },
                    ErrorCode::ConstraintViolation => StoreError::Constraint { op, message },
                    ErrorCode::TooBig => {
                        StoreError::InvalidInput(format!("{op}: value too large: {message}"))
                    }
                    _ => StoreError::Sqlite { op, source: err },
                }
            }
            // Type or range mismatches while decoding a stored row: the data
            // is not what this schema version writes.
            rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::Utf8Error(..) => StoreError::Corrupt {
                op,
                message: err.to_string(),
            },
            _ => StoreError::Sqlite { op, source: err },
        }
    }

    pub(crate) fn corrupt(op: &'static str, message: impl Into<String>) -> Self {
        StoreError::Corrupt {
            op,
            message: message.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::InvalidInput(m) => write!(f, "invalid input: {m}"),
            StoreError::UnknownDecision { key, decision_id } => {
                write!(
                    f,
                    "decision {decision_id:?} does not exist in capsule {key}"
                )
            }
            StoreError::UnknownCursor { key, decision_id } => write!(
                f,
                "paging cursor {decision_id:?} does not exist in capsule {key}; restart paging"
            ),
            StoreError::Busy { op, message } => {
                write!(f, "event store busy during {op} (retryable): {message}")
            }
            StoreError::Corrupt { op, message } => {
                write!(f, "event store data corrupt during {op}: {message}")
            }
            StoreError::Io { op, message } => {
                write!(f, "event store I/O error during {op}: {message}")
            }
            StoreError::Schema(m) => write!(f, "event store schema error: {m}"),
            StoreError::Constraint { op, message } => {
                write!(f, "event store constraint violated during {op}: {message}")
            }
            StoreError::Sqlite { op, source } => {
                write!(f, "event store error during {op}: {source}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlite { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Attaches an operation name to rusqlite results.
pub(crate) trait SqlContext<T> {
    fn op(self, op: &'static str) -> Result<T>;
}

impl<T> SqlContext<T> for rusqlite::Result<T> {
    fn op(self, op: &'static str) -> Result<T> {
        self.map_err(|e| StoreError::from_sqlite(op, e))
    }
}

/// True when `err` is a foreign-key violation (SQLITE_CONSTRAINT_FOREIGNKEY).
pub(crate) fn is_foreign_key_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ffi;

    fn failure(code: i32, msg: &str) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some(msg.to_string()))
    }

    #[test]
    fn maps_sqlite_codes_to_variants() {
        let busy = StoreError::from_sqlite("t", failure(ffi::SQLITE_BUSY, "database is locked"));
        assert!(busy.is_busy(), "{busy}");
        assert!(busy.to_string().contains("database is locked"));

        let corrupt = StoreError::from_sqlite("t", failure(ffi::SQLITE_CORRUPT, "malformed"));
        assert!(matches!(corrupt, StoreError::Corrupt { .. }), "{corrupt}");

        let notdb = StoreError::from_sqlite("t", failure(ffi::SQLITE_NOTADB, "not a database"));
        assert!(matches!(notdb, StoreError::Corrupt { .. }), "{notdb}");

        let full = StoreError::from_sqlite("t", failure(ffi::SQLITE_FULL, "disk full"));
        assert!(matches!(full, StoreError::Io { .. }), "{full}");

        let cantopen = StoreError::from_sqlite("t", failure(ffi::SQLITE_CANTOPEN, "cannot open"));
        assert!(matches!(cantopen, StoreError::Io { .. }), "{cantopen}");

        let unique = StoreError::from_sqlite(
            "t",
            failure(ffi::SQLITE_CONSTRAINT_UNIQUE, "UNIQUE constraint failed"),
        );
        assert!(matches!(unique, StoreError::Constraint { .. }), "{unique}");
        assert!(unique.to_string().contains("2067"), "{unique}");

        let decode = StoreError::from_sqlite("t", rusqlite::Error::IntegralValueOutOfRange(0, -1));
        assert!(matches!(decode, StoreError::Corrupt { .. }), "{decode}");

        let other = StoreError::from_sqlite("t", rusqlite::Error::QueryReturnedNoRows);
        assert!(matches!(other, StoreError::Sqlite { .. }), "{other}");
        assert!(std::error::Error::source(&other).is_some());
    }

    #[test]
    fn detects_foreign_key_violations_only() {
        assert!(is_foreign_key_violation(&failure(
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            "FOREIGN KEY constraint failed"
        )));
        assert!(!is_foreign_key_violation(&failure(
            ffi::SQLITE_CONSTRAINT_UNIQUE,
            "UNIQUE constraint failed"
        )));
        assert!(!is_foreign_key_violation(
            &rusqlite::Error::QueryReturnedNoRows
        ));
    }
}
