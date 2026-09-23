//! Write-behind event log.
//!
//! Neither `/decide` nor `/reward` waits on disk. Records go into one
//! bounded FIFO queue and a `pending` index, and one writer thread commits
//! the queue to the event store in batches (at most `MAX_BATCH` records or
//! `MAX_DELAY` per transaction). Within a batch, decisions are inserted
//! before rewards and each kind keeps queue order, so a reward is never
//! written ahead of its decision and rewards receive sequence numbers in
//! the order the model applied them.
//!
//! A crash can lose at most the uncommitted tail (bounded by `MAX_DELAY`).
//! The model is only ever snapshotted after a flush, so a snapshot never
//! contains a reward that the log could still lose.
//!
//! When the queue is full the request fails with 503 rather than serving a
//! decision whose log entry would be dropped: a decision without its
//! propensity record would silently bias off-policy evaluation.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::eventstore::{DecisionRecord, EventStore, InsertOutcome, RewardOutcome, RewardRecord};

/// Records per transaction.
pub const MAX_BATCH: usize = 512;
/// Longest a queued record waits before its batch commits.
pub const MAX_DELAY: Duration = Duration::from_millis(2);
/// Queue capacity; beyond it requests answer 503.
pub const QUEUE_CAPACITY: usize = 65_536;

enum Msg {
    Decision(Box<DecisionRecord>),
    Reward(Box<RewardRecord>),
    Shutdown,
}

fn decision_key(r: &DecisionRecord) -> String {
    format!("{}\x00{}", r.key, r.id)
}

fn reward_key(r: &RewardRecord) -> String {
    format!("{}\x00{}", r.key, r.idempotency_key)
}

#[derive(Default)]
struct Pending {
    decisions: HashMap<String, DecisionRecord>,
    reward_keys: HashSet<String>,
}

#[derive(Default)]
struct Progress {
    /// Records accepted into the queue.
    enqueued: u64,
    /// Records whose batch has been committed (or failed permanently).
    settled: u64,
}

/// Counters exported through `/metrics`.
#[derive(Default)]
pub struct WriterStats {
    pub committed: AtomicU64,
    pub rewards_committed: AtomicU64,
    pub batches: AtomicU64,
    pub rejected_full: AtomicU64,
    pub failed: AtomicU64,
    /// Rewards applied to a model but refused by the store at commit
    /// (should stay zero; nonzero means the model and log diverged).
    pub rewards_refused_after_apply: AtomicU64,
}

pub struct DecisionWriter {
    tx: SyncSender<Msg>,
    pending: Arc<Mutex<Pending>>,
    progress: Arc<(Mutex<Progress>, Condvar)>,
    pub stats: Arc<WriterStats>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DecisionWriter {
    pub fn start(events: Arc<dyn EventStore>) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE_CAPACITY);
        let pending = Arc::new(Mutex::new(Pending::default()));
        let progress = Arc::new((Mutex::new(Progress::default()), Condvar::new()));
        let stats = Arc::new(WriterStats::default());
        let handle = {
            let pending = pending.clone();
            let progress = progress.clone();
            let stats = stats.clone();
            std::thread::Builder::new()
                .name("syntra-event-writer".into())
                .spawn(move || writer_loop(rx, events, pending, progress, stats))
                .expect("spawn event writer")
        };
        DecisionWriter {
            tx,
            pending,
            progress,
            stats,
            handle: Mutex::new(Some(handle)),
        }
    }

    fn send(&self, msg: Msg, undo: impl FnOnce(&mut Pending)) -> Result<(), String> {
        // Counted before the send. The queue is FIFO, so a flush's target
        // then covers every record ahead of the caller's own, and `settled`
        // cannot reach it before that record commits. (Counting after the
        // send let a record sent earlier but counted later be mistaken for
        // the caller's.) A record that fails to send settles at once.
        let (lock, cvar) = &*self.progress;
        lock.lock().unwrap().enqueued += 1;
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(e) => {
                lock.lock().unwrap().settled += 1;
                cvar.notify_all();
                undo(&mut self.pending.lock().unwrap());
                match e {
                    TrySendError::Full(_) => {
                        self.stats.rejected_full.fetch_add(1, Ordering::Relaxed);
                        Err("event log is backlogged; retry shortly".into())
                    }
                    TrySendError::Disconnected(_) => Err("event log writer stopped".into()),
                }
            }
        }
    }

    /// Queue a decision record.
    pub fn enqueue(&self, record: DecisionRecord) -> Result<(), String> {
        // Indexed before sending: the writer removes the entry after commit.
        let key = decision_key(&record);
        self.pending
            .lock()
            .unwrap()
            .decisions
            .insert(key.clone(), record.clone());
        self.send(Msg::Decision(Box::new(record)), |p| {
            p.decisions.remove(&key);
        })
    }

    /// Queue a reward record. Returns false (and queues nothing) when a
    /// reward with the same idempotency key is already queued.
    pub fn enqueue_reward(&self, record: RewardRecord) -> Result<bool, String> {
        let key = reward_key(&record);
        if !self.pending.lock().unwrap().reward_keys.insert(key.clone()) {
            return Ok(false);
        }
        self.send(Msg::Reward(Box::new(record)), |p| {
            p.reward_keys.remove(&key);
        })?;
        Ok(true)
    }

    /// True while a reward with this idempotency key is queued (accepted
    /// but not yet committed).
    pub fn reward_queued(
        &self,
        key: &crate::eventstore::CapsuleKey,
        idempotency_key: &str,
    ) -> bool {
        self.pending
            .lock()
            .unwrap()
            .reward_keys
            .contains(&format!("{key}\x00{idempotency_key}"))
    }

    /// A decision that is queued but not yet committed.
    pub fn pending(&self, key: &crate::eventstore::CapsuleKey, id: &str) -> Option<DecisionRecord> {
        self.pending
            .lock()
            .unwrap()
            .decisions
            .get(&format!("{key}\x00{id}"))
            .cloned()
    }

    /// Wait until every record queued before this call is committed, or
    /// `timeout` passes. Returns true when the queue drained in time.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (lock, cvar) = &*self.progress;
        let deadline = Instant::now() + timeout;
        let mut p = lock.lock().unwrap();
        let target = p.enqueued;
        while p.settled < target {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            p = cvar.wait_timeout(p, deadline - now).unwrap().0;
        }
        true
    }

    /// Records waiting to be committed.
    pub fn backlog(&self) -> usize {
        let p = self.pending.lock().unwrap();
        p.decisions.len() + p.reward_keys.len()
    }

    /// Commit everything queued, then stop the writer thread.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

/// Sort a message into its batch; true on `Shutdown`.
fn take(msg: Msg, decisions: &mut Vec<DecisionRecord>, rewards: &mut Vec<RewardRecord>) -> bool {
    match msg {
        Msg::Decision(r) => decisions.push(*r),
        Msg::Reward(r) => rewards.push(*r),
        Msg::Shutdown => return true,
    }
    false
}

fn writer_loop(
    rx: Receiver<Msg>,
    events: Arc<dyn EventStore>,
    pending: Arc<Mutex<Pending>>,
    progress: Arc<(Mutex<Progress>, Condvar)>,
    stats: Arc<WriterStats>,
) {
    let mut decisions: Vec<DecisionRecord> = Vec::with_capacity(MAX_BATCH);
    let mut rewards: Vec<RewardRecord> = Vec::new();
    let mut shutting_down = false;
    while !shutting_down {
        // Block for the first record, then gather more until the batch is
        // full or MAX_DELAY has passed since the first one.
        match rx.recv() {
            Ok(msg) => shutting_down = take(msg, &mut decisions, &mut rewards),
            Err(_) => shutting_down = true,
        }
        let started = Instant::now();
        while !shutting_down && decisions.len() + rewards.len() < MAX_BATCH {
            let left = MAX_DELAY.saturating_sub(started.elapsed());
            match rx.recv_timeout(left) {
                Ok(msg) => shutting_down = take(msg, &mut decisions, &mut rewards),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => shutting_down = true,
            }
        }
        if shutting_down {
            while let Ok(msg) = rx.try_recv() {
                take(msg, &mut decisions, &mut rewards);
            }
        }
        let n = decisions.len() + rewards.len();
        if n == 0 {
            continue;
        }
        for chunk in decisions.chunks(MAX_BATCH) {
            commit_decisions(&*events, chunk, &stats);
        }
        for chunk in rewards.chunks(MAX_BATCH) {
            commit_rewards(&*events, chunk, &stats);
        }
        {
            let mut p = pending.lock().unwrap();
            for r in &decisions {
                p.decisions.remove(&decision_key(r));
            }
            for r in &rewards {
                p.reward_keys.remove(&reward_key(r));
            }
        }
        let (lock, cvar) = &*progress;
        lock.lock().unwrap().settled += n as u64;
        cvar.notify_all();
        decisions.clear();
        rewards.clear();
    }
}

/// Retry transient failures (a busy database, a full disk an operator
/// frees) with backoff for about five seconds before giving up on a batch.
fn with_retry<T, E: std::fmt::Display>(
    what: &str,
    n: usize,
    mut f: impl FnMut() -> Result<T, E>,
) -> Option<T> {
    let mut delay = Duration::from_millis(5);
    for attempt in 0..=10 {
        match f() {
            Ok(v) => return Some(v),
            Err(e) if attempt < 10 => {
                tracing::warn!(error = %e, records = n, attempt, "{what} commit failed; retrying");
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(1000));
            }
            Err(e) => tracing::error!(error = %e, records = n, "{what} batch lost after retries"),
        }
    }
    None
}

fn commit_decisions(events: &dyn EventStore, chunk: &[DecisionRecord], stats: &WriterStats) {
    match with_retry("decision", chunk.len(), || events.insert_decisions(chunk)) {
        Some(outcomes) => {
            for (o, r) in outcomes.iter().zip(chunk) {
                if let InsertOutcome::Invalid(msg) = o {
                    tracing::error!(decision = %r.id, error = %msg, "decision refused by the event store");
                }
            }
            stats
                .committed
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            stats.batches.fetch_add(1, Ordering::Relaxed);
        }
        None => {
            stats
                .failed
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
}

fn commit_rewards(events: &dyn EventStore, chunk: &[RewardRecord], stats: &WriterStats) {
    match with_retry("reward", chunk.len(), || events.insert_rewards(chunk)) {
        Some(outcomes) => {
            for (o, r) in outcomes.iter().zip(chunk) {
                if !matches!(o, RewardOutcome::Applied(_)) {
                    stats
                        .rewards_refused_after_apply
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        decision = %r.decision_id, outcome = ?o,
                        "reward was applied to the model but refused by the event store"
                    );
                }
            }
            stats
                .rewards_committed
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            stats.batches.fetch_add(1, Ordering::Relaxed);
        }
        None => {
            stats
                .failed
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
}
