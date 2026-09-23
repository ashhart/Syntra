//! Background WAL checkpointer.
//!
//! The writer connection runs with `wal_autocheckpoint = 0`. Otherwise the
//! commit that crosses SQLite's 1000-page threshold would copy the WAL into
//! the database and fsync twice while holding the write lock. This thread
//! runs PASSIVE checkpoints on its own connection instead. A PASSIVE
//! checkpoint never blocks readers or the writer; it copies what it can and
//! syncs the WAL first, which also bounds how much a power loss can take
//! (see the module docs of `eventstore`).
//!
//! The writer reports the WAL pages each write appended (SQLite's own
//! per-connection counter), the same unit `wal_autocheckpoint` uses, so a
//! 512-row batch counts for what it wrote, not as one write. A checkpoint
//! starts after `after_pages` pages, or `interval` after the first page no
//! checkpoint has covered yet, whichever comes first. An idle store runs
//! none. The thread stops when the store drops; the writer's close then
//! performs the final checkpoint.

use super::{Result, StoreError};
use rusqlite::Connection;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(crate) struct Checkpointer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    after_pages: u64,
    interval: Duration,
}

#[derive(Default)]
struct State {
    /// WAL pages written since the last checkpoint started.
    pending: u64,
    shutdown: bool,
    /// Checkpoints run so far (tests use this).
    completed: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // State is a few plain counters, always consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Checkpointer {
    pub(crate) fn spawn(conn: Connection, after_pages: u64, interval: Duration) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            after_pages: after_pages.max(1),
            interval,
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("syntra-eventstore-checkpoint".into())
            .spawn(move || run(&conn, &thread_shared))
            .map_err(|e| StoreError::Io {
                op: "open",
                message: format!("cannot start the checkpoint thread: {e}"),
            })?;
        Ok(Checkpointer {
            shared,
            thread: Some(thread),
        })
    }

    /// Records `pages` WAL pages written by one committed write.
    pub(crate) fn note_pages(&self, pages: u64) {
        if pages == 0 {
            return;
        }
        let mut st = self.shared.lock();
        let before = st.pending;
        st.pending = before.saturating_add(pages);
        // Wake the thread when the interval clock should start (first page)
        // and when the threshold is crossed. The thread re-checks the
        // counters under the lock before every wait, so a wake-up that
        // arrives while it is busy is not lost.
        if before == 0
            || (before < self.shared.after_pages && st.pending >= self.shared.after_pages)
        {
            self.shared.wake.notify_one();
        }
    }

    #[cfg(test)]
    pub(crate) fn completed(&self) -> u64 {
        self.shared.lock().completed
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(conn: &Connection, shared: &Shared) {
    loop {
        let (pending, shutdown) = {
            let mut st = shared.lock();
            while st.pending == 0 && !st.shutdown {
                st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            let deadline = Instant::now() + shared.interval;
            while st.pending < shared.after_pages && !st.shutdown {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                st = shared
                    .wake
                    .wait_timeout(st, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            (std::mem::take(&mut st.pending), st.shutdown)
        };
        if shutdown {
            return;
        }
        if pending > 0 {
            checkpoint(conn);
            shared.lock().completed += 1;
        }
    }
}

fn checkpoint(conn: &Connection) {
    let result = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    });
    match result {
        Ok((busy, wal_frames, checkpointed)) => {
            tracing::debug!(busy, wal_frames, checkpointed, "eventstore WAL checkpoint");
        }
        // Retried on the next write. A checkpoint that keeps failing lets the
        // WAL grow; the writes themselves surface a full disk.
        Err(e) => tracing::warn!(error = %e, "eventstore WAL checkpoint failed"),
    }
}
