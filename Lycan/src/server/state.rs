use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::store::LycanStore;
use crate::auth_tokens::TokenStore;
use crate::rate_limit::RateLimiter;

use super::metrics::Metrics;

/// Per-runtime lock manager. The global map lock is only held to retrieve
/// or create a scoped mutex — never during request execution.
pub(super) struct CapsuleLockManager {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl CapsuleLockManager {
    pub(super) fn new() -> Self {
        Self { locks: Mutex::new(HashMap::new()) }
    }

    pub(super) fn get(&self, tenant: &str, job: &str, capsule: &str) -> Arc<Mutex<()>> {
        let key = format!("{tenant}/{job}/{capsule}");
        let mut map = self.locks.lock().unwrap();
        map.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
    }
}

/// Shared server state — no global mutex around the store.
pub(super) struct SharedState {
    pub(super) store: LycanStore,
    pub(super) admin_key: Option<String>,
    pub(super) service_name: String,
    pub(super) locks: CapsuleLockManager,
    pub(super) metrics: Metrics,
    pub(super) tokens: Mutex<TokenStore>,
    pub(super) rate_limiter: RateLimiter,
}

pub(super) type State = Arc<SharedState>;
