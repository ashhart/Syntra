//! Shared server state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::auth_tokens::TokenStore;
use crate::eventstore::{CapsuleKey, DecisionRecord, EventStore, ModelSnapshot};
use crate::rate_limit::RateLimiter;
use crate::store::{Store, now_ms};

use super::http::Response;
use super::metrics::Metrics;
use super::runtime::{CapsuleRuntime, LoadError, RuntimeCache};
use super::writer::DecisionWriter;

/// Model snapshots kept per capsule; older ones are pruned.
const SNAPSHOTS_KEPT: usize = 3;

/// Per-capsule mutex for administrative changes (install, spec, policy,
/// delete) so two edits never interleave. The decide path never takes it.
#[derive(Default)]
pub struct CapsuleLocks {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl CapsuleLocks {
    pub fn get(&self, tenant: &str, job: &str, capsule: &str) -> Arc<Mutex<()>> {
        let key = format!("{tenant}/{job}/{capsule}");
        self.locks.lock().unwrap().entry(key).or_default().clone()
    }
}

pub struct SharedState {
    pub store: Store,
    pub events: Arc<dyn EventStore>,
    pub runtimes: RuntimeCache,
    pub writer: DecisionWriter,
    pub admin_key: Option<String>,
    pub service_name: String,
    pub tokens: Mutex<TokenStore>,
    pub rate_limiter: RateLimiter,
    pub metrics: Metrics,
    pub locks: CapsuleLocks,
    pub started_at: std::time::Instant,
    /// `/metrics` without a credential (see `ServerConfig`).
    pub metrics_public: bool,
    /// Set while the background thread takes periodic model snapshots;
    /// otherwise the reward that crosses `snapshotEvery` takes it inline.
    pub background_snapshots: std::sync::atomic::AtomicBool,
}

pub type State = Arc<SharedState>;

/// A model snapshot and how many updates it covers since the last one.
struct Capture {
    snapshot: ModelSnapshot,
    since: u64,
}

impl SharedState {
    /// The loaded runtime for a capsule, as an HTTP error when it cannot be
    /// used.
    pub fn runtime(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<Arc<CapsuleRuntime>, Response> {
        if let Some(rt) = self.runtimes.cached(tenant, job, capsule) {
            return Ok(rt);
        }
        // Loading rebuilds the model from the event log. A runtime this one
        // replaces (after a spec, policy or program change) may have applied
        // rewards that are still queued; commit them first so the rebuilt
        // model includes them.
        if !self.writer.flush(std::time::Duration::from_secs(10)) {
            return Err(
                Response::error(503, "event log is backlogged; retry shortly")
                    .with_header("retry-after", "1"),
            );
        }
        self.runtimes
            .get(&self.store, &*self.events, tenant, job, capsule)
            .map_err(|e| match e {
                LoadError::NotFound => Response::error(
                    404,
                    &format!(
                        "capsule {tenant}/{job}/{capsule} not found (create it with PUT .../spec)"
                    ),
                ),
                LoadError::Invalid(msg) => {
                    tracing::error!(tenant, job, capsule, error = %msg, "capsule failed to load");
                    Response::error(500, &format!("capsule failed to load: {msg}"))
                }
            })
    }

    /// A decision by id: queued decisions first, then the event store.
    pub fn find_decision(
        &self,
        key: &CapsuleKey,
        id: &str,
    ) -> Result<Option<DecisionRecord>, Response> {
        if let Some(d) = self.writer.pending(key, id) {
            return Ok(Some(d));
        }
        self.events
            .get_decision(key, id)
            .map_err(|e| Response::error(500, &format!("reading decision: {e}")))
    }

    /// Append an audit event. Failures are logged, never surfaced to the
    /// caller: the audited action has already happened.
    pub fn audit(&self, key: &CapsuleKey, event: &str, detail: Value) {
        if let Err(e) = self
            .events
            .append_audit(key, now_ms(), event, &detail.to_string())
        {
            tracing::error!(capsule = %key, event, error = %e, "audit append failed");
        }
    }

    /// Persist the runtime's model with its reward watermark. Callers hold
    /// the runtime's reward lock. Every reward the model has applied is
    /// committed first (flush), so the watermark (the capsule's last
    /// committed reward) covers exactly what the snapshot contains.
    /// Snapshot the model now. Call with `rt.reward_lock` held, so no
    /// reward lands between the log watermark and the weights.
    pub fn snapshot(&self, rt: &CapsuleRuntime) {
        if let Some(c) = self.capture(rt) {
            self.persist(rt, c);
        }
    }

    /// Snapshot off the request path: the reward lock is held only to flush
    /// the log and copy the (sparse) weights; the write happens after.
    pub fn snapshot_background(&self, rt: &CapsuleRuntime) {
        // Most of the queue drains before the lock is taken.
        self.writer.flush(std::time::Duration::from_secs(10));
        let captured = {
            let _order = rt.reward_lock.lock().unwrap();
            self.capture(rt)
        };
        if let Some(c) = captured {
            self.persist(rt, c);
        }
    }

    /// A consistent copy of the model and its log watermark (reward lock
    /// held by the caller).
    fn capture(&self, rt: &CapsuleRuntime) -> Option<Capture> {
        if !self.writer.flush(std::time::Duration::from_secs(10)) {
            tracing::warn!(capsule = %rt.key, "event log did not drain; snapshot postponed");
            return None;
        }
        let watermark = match self.events.stats(&rt.key) {
            Ok(s) => s.last_reward_seq.unwrap_or(0),
            Err(e) => {
                tracing::warn!(capsule = %rt.key, error = %e, "cannot read reward watermark; snapshot postponed");
                return None;
            }
        };
        rt.reward_watermark
            .store(watermark, std::sync::atomic::Ordering::SeqCst);
        let since = rt.since_snapshot.load(std::sync::atomic::Ordering::SeqCst);
        let (version, state) = {
            let engine = rt.engine.read().unwrap();
            (engine.model_version(), engine.snapshot())
        };
        Some(Capture {
            snapshot: ModelSnapshot {
                key: rt.key.clone(),
                version,
                reward_seq: watermark,
                ts_ms: now_ms(),
                state,
            },
            since,
        })
    }

    fn persist(&self, rt: &CapsuleRuntime, c: Capture) {
        match self.events.save_model(&c.snapshot) {
            Ok(()) => {
                let _ = rt.since_snapshot.fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| Some(n.saturating_sub(c.since)),
                );
                if let Err(e) = self.events.prune_models(&rt.key, SNAPSHOTS_KEPT) {
                    tracing::warn!(error = %e, "pruning old model snapshots failed");
                }
            }
            Err(e) => tracing::error!(
                capsule = %rt.key,
                error = %e, "saving model snapshot failed; will retry at the next interval"
            ),
        }
    }

    /// Flush the decision log and snapshot every model with unsaved updates.
    pub fn shutdown(&self) {
        self.writer.shutdown();
        for rt in self.runtimes.loaded() {
            rt.save_deferred(&self.store);
            if rt.since_snapshot.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                let _order = rt.reward_lock.lock().unwrap();
                self.snapshot(&rt);
            }
        }
    }
}
