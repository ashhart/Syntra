//! Fixed-size pool of read-only connections.
//!
//! In WAL mode readers never wait for the writer, so reads served from here
//! keep running while a write transaction is open. A caller takes one
//! connection, runs its statements, and the guard returns the connection on
//! drop. Store methods never hold two pooled connections, and never hold one
//! together with the writer, so waiting for a free connection cannot
//! deadlock.

use rusqlite::Connection;
use std::ops::Deref;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

pub(crate) struct ReadPool {
    idle: Mutex<Vec<Connection>>,
    returned: Condvar,
}

impl ReadPool {
    pub(crate) fn new(conns: Vec<Connection>) -> Self {
        ReadPool {
            idle: Mutex::new(conns),
            returned: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Connection>> {
        // The guarded Vec is always valid, so a poisoned lock is safe to reuse.
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes a connection, waiting until one is returned if all are in use.
    pub(crate) fn get(&self) -> PooledConn<'_> {
        let mut idle = self.lock();
        loop {
            if let Some(conn) = idle.pop() {
                return PooledConn {
                    pool: self,
                    conn: Some(conn),
                };
            }
            idle = self
                .returned
                .wait(idle)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    pub(crate) fn idle_count(&self) -> usize {
        self.lock().len()
    }
}

pub(crate) struct PooledConn<'a> {
    pool: &'a ReadPool,
    conn: Option<Connection>,
}

impl Deref for PooledConn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("pooled connection is present until the guard drops")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        // Never hand out a connection with a read transaction still open: it
        // would pin an old snapshot and stop checkpoints from completing.
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK");
        }
        self.pool.lock().push(conn);
        self.pool.returned.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn returns_connections_and_closes_open_transactions() {
        let pool = ReadPool::new(vec![Connection::open_in_memory().unwrap()]);
        {
            let conn = pool.get();
            conn.execute_batch("BEGIN").unwrap();
            assert!(!conn.is_autocommit());
            assert_eq!(pool.idle_count(), 0);
        }
        assert_eq!(pool.idle_count(), 1);
        assert!(pool.get().is_autocommit());
    }

    #[test]
    fn waiters_block_until_a_connection_returns() {
        let pool = Arc::new(ReadPool::new(vec![
            Connection::open_in_memory().unwrap(),
            Connection::open_in_memory().unwrap(),
        ]));
        let in_use = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (pool, in_use, max_seen) = (pool.clone(), in_use.clone(), max_seen.clone());
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        let conn = pool.get();
                        let now = in_use.fetch_add(1, Ordering::SeqCst) + 1;
                        max_seen.fetch_max(now, Ordering::SeqCst);
                        let one: i64 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
                        assert_eq!(one, 1);
                        in_use.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
        assert_eq!(pool.idle_count(), 2);
    }
}
