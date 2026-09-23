//! Default rewards.
//!
//! When a spec sets `reward.default`, a decision that has no reward
//! `reward.waitSeconds` after it was made receives that reward, so feedback
//! that only reports successes (a click, a purchase) still teaches the model
//! what a miss looks like. This is how Azure Personalizer treats missing
//! rewards.
//!
//! One thread sweeps every loaded capsule once a second. It first waits for
//! the write-behind queue, so every acknowledged reward is visible, then
//! applies the default through the normal reward path. The idempotency key
//! is the decision id under `rewards: "first"` (so a real reward arriving
//! later is a duplicate and ignored) and `<id>:default` under `"sum"` (a
//! late real reward then adds to it). Sweeps are idempotent: after a restart
//! the sweep re-reads the last [`SWEEP_HORIZON_MS`] and skips decisions that
//! already have a reward.
//!
//! Decisions logged after their wait has already passed (a local-evaluation
//! upload delayed longer than the wait) are not swept.
//!
//! The same thread snapshots models that have taken `snapshotEvery`
//! updates (so no reward request pays for the write), and drops deferred
//! decisions (Personalizer `deferActivation`) that were never activated,
//! once a minute.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;

use crate::decision::spec::RewardAggregation;
use crate::store::now_ms;

use super::runtime::CapsuleRuntime;
use super::state::State;

/// Time between sweeps.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(1);
/// How far back the first sweep after loading a capsule looks.
pub const SWEEP_HORIZON_MS: i64 = 24 * 3600 * 1000;
/// Decisions read per query.
const BATCH: usize = 1000;
/// Sorts after every valid decision id (they use `A-Z a-z 0-9 _ . : -`).
const AFTER_ALL_IDS: &str = "~";

pub struct Sweeper {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sweeper {
    pub fn start(state: State) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        state.background_snapshots.store(true, Ordering::Relaxed);
        let handle = std::thread::Builder::new()
            .name("syntra-default-rewards".into())
            .spawn(move || {
                let mut ticks: u64 = 0;
                while !flag.load(Ordering::Relaxed) {
                    std::thread::park_timeout(SWEEP_INTERVAL);
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    sweep_all(&state);
                    snapshot_due(&state);
                    ticks += 1;
                    if ticks.is_multiple_of(60) {
                        for rt in state.runtimes.loaded() {
                            let n = rt.expire_deferred();
                            if n > 0 {
                                tracing::info!(capsule = %rt.key, expired = n, "dropped deferred decisions never activated");
                            }
                        }
                    }
                }
            })
            .expect("spawn default-reward sweeper");
        Sweeper {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop after the current sweep.
    /// Stop after the current sweep. Models are snapshotted inline again
    /// (shutdown snapshots every model that has unsaved updates).
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}

/// Snapshot every loaded model that has taken `snapshotEvery` updates since
/// its last snapshot, so no request pays for it.
fn snapshot_due(state: &State) {
    for rt in state.runtimes.loaded() {
        let due = rt.since_snapshot.load(Ordering::SeqCst) >= rt.spec().snapshot_every;
        if due {
            state.snapshot_background(&rt);
        }
    }
}

/// Sweep every loaded capsule whose spec sets a default reward. Returns the
/// number of default rewards applied.
pub fn sweep_all(state: &State) -> usize {
    let runtimes: Vec<_> = state
        .runtimes
        .loaded()
        .into_iter()
        .filter(|rt| rt.spec().reward.default.is_some())
        .collect();
    if runtimes.is_empty() {
        return 0;
    }
    // Make every acknowledged reward visible to the query below.
    state.writer.flush(Duration::from_secs(1));
    let now = now_ms();
    runtimes.iter().map(|rt| sweep(state, rt, now)).sum()
}

fn sweep(state: &State, rt: &CapsuleRuntime, now: i64) -> usize {
    let spec = rt.spec();
    let Some(default) = spec.reward.default else {
        return 0;
    };
    let until = now - spec.reward.wait_seconds as i64 * 1000;
    let mut cursor = rt.sweep_cursor.lock().unwrap();
    let mut after = cursor
        .clone()
        .unwrap_or((until - SWEEP_HORIZON_MS, String::new()));
    let mut applied = 0;
    loop {
        let batch = match state.events.unrewarded_decisions(
            &rt.key,
            Some((after.0, &after.1)),
            until,
            BATCH,
        ) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(capsule = %rt.key, error = %e, "default-reward sweep failed");
                *cursor = Some(after);
                return applied;
            }
        };
        for (ts, id) in &batch {
            let key = match spec.rewards {
                RewardAggregation::First => None,
                RewardAggregation::Sum => Some(format!("{id}:default")),
            };
            let detail = Some(json!({ "default": true }));
            match super::reward::apply_reward(state, rt, id, default, key, detail) {
                Ok(v) => {
                    if v["applied"] == true {
                        applied += 1;
                    }
                }
                Err(resp) if resp.status == 503 => {
                    // Backlogged: resume from here on the next sweep.
                    *cursor = Some(after);
                    state
                        .metrics
                        .default_rewards
                        .fetch_add(applied as u64, Ordering::Relaxed);
                    return applied;
                }
                Err(resp) => {
                    tracing::warn!(
                        capsule = %rt.key, decision = %id, status = resp.status,
                        "default reward not applied"
                    );
                }
            }
            after = (*ts, id.clone());
        }
        if batch.len() < BATCH {
            break;
        }
    }
    *cursor = Some((until, AFTER_ALL_IDS.to_string()));
    state
        .metrics
        .default_rewards
        .fetch_add(applied as u64, Ordering::Relaxed);
    applied
}
