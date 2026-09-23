//! In-memory capsule runtimes.
//!
//! A runtime holds everything a decide call needs so the hot path never
//! reads a file: the engine (spec + learned model) behind an `RwLock`, the
//! verified feature program, and the parsed execution policy. Runtimes load
//! lazily on first use (latest model snapshot, then replay of later rewards)
//! and are replaced whenever the spec, program or policy changes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;

use crate::capabilities::CapValue;
use crate::context::{ExecutionContext, ExecutionPolicy, SelectionMode};
use crate::decision::{ActionSpec, DecisionSpec, Engine};
use crate::eventstore::{CapsuleKey, EventStore, ModelSnapshot};
use crate::graph::{NeuralGraph, OpCode};
use crate::store::{Store, sha256_hex};

/// What a feature program published for one request.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProgramOutput {
    /// `features.<name>` values, as a JSON object (namespace `d`).
    pub derived: Value,
    /// Action ids marked `exclude.<id>`.
    pub excluded: Vec<String>,
    /// Action ids marked `only.<id>`; `None` when none were.
    pub only: Option<Vec<String>>,
    /// `reason`, if published.
    pub reason: Option<String>,
}

/// A verified, compiled feature program.
pub struct FeatureProgram {
    graph: NeuralGraph,
    pub sha256: String,
}

impl FeatureProgram {
    /// Decode and verify a `.lyc` graph for use as a feature program.
    /// Programs with in-graph learning nodes are refused: on the server the
    /// learner is the capsule's engine, and a second learner inside the
    /// graph would silently diverge from the logged propensities.
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let graph = NeuralGraph::from_bytes(bytes).map_err(|e| format!("invalid program: {e}"))?;
        crate::verifier::verify(&graph).map_err(|e| format!("program failed verification: {e}"))?;
        if let Some(node) = graph.nodes.iter().find(|n| {
            matches!(
                n.op,
                OpCode::AdaptiveChoice | OpCode::Strategy | OpCode::Feedback
            )
        }) {
            return Err(format!(
                "program contains a {:?} node (#{}): feature programs compute features and \
                 eligibility; the capsule's spec declares the actions and Syntra learns the choice. \
                 See docs/design/v2-decision-core.md, \"Feature program\".",
                node.op, node.id
            ));
        }
        Ok(FeatureProgram {
            graph,
            sha256: sha256_hex(bytes),
        })
    }

    /// Run the program against one request under the capsule's policy.
    pub fn run(
        &self,
        input: &Value,
        policy: &ExecutionPolicy,
        data_dir: &std::path::Path,
    ) -> Result<ProgramOutput, String> {
        if (policy.allow_file_read || policy.allow_file_write) && !data_dir.exists() {
            let _ = std::fs::create_dir_all(data_dir);
        }
        let published = crate::context::new_published_buffer();
        let ctx = ExecutionContext {
            policy: Some(policy.clone()),
            input: Some(CapValue::from_json(input)),
            working_dir: Some(data_dir.to_path_buf()),
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.0,
            published: Some(published.clone()),
        };
        let mut executor =
            crate::graph_executor::GraphExecutor::new_with_context(self.graph.clone(), ctx);
        executor.run().map_err(|e| e.to_string())?;
        let map = published.borrow();
        let mut out = ProgramOutput {
            derived: Value::Object(serde_json::Map::new()),
            ..Default::default()
        };
        let mut only = Vec::new();
        for (key, value) in map.iter() {
            if let Some(name) = key.strip_prefix("features.") {
                if !name.is_empty() && !value.is_null() {
                    out.derived[name] = value.clone();
                }
            } else if let Some(id) = key.strip_prefix("exclude.") {
                if value.as_bool() == Some(true) {
                    out.excluded.push(id.to_string());
                }
            } else if let Some(id) = key.strip_prefix("only.") {
                if value.as_bool() == Some(true) {
                    only.push(id.to_string());
                }
            } else if key == "reason" {
                out.reason = value.as_str().map(String::from);
            }
        }
        if !only.is_empty() {
            out.only = Some(only);
        }
        Ok(out)
    }
}

/// One capsule, loaded.
pub struct CapsuleRuntime {
    pub key: CapsuleKey,
    pub engine: RwLock<Engine>,
    pub program: Option<FeatureProgram>,
    /// The parsed policy, or the reason it could not be used (the program
    /// then runs deny-all).
    pub policy: ExecutionPolicy,
    pub policy_error: Option<String>,
    pub data_dir: PathBuf,
    /// Serializes reward application so the model sees rewards in the same
    /// order as their sequence numbers (replay after a restart reproduces
    /// the live model exactly).
    pub reward_lock: Mutex<()>,
    /// Sequence number of the last reward applied to the model.
    pub reward_watermark: AtomicI64,
    /// Updates applied since the last snapshot.
    pub since_snapshot: AtomicU64,
    /// Counter feeding per-decision seeds for capsules with a fixed seed.
    pub seed_counter: AtomicU64,
}

impl CapsuleRuntime {
    /// Seed for the next decision: fully determined by the spec's seed and
    /// a counter when the spec fixes one, otherwise fresh OS randomness.
    pub fn next_seed(&self, spec: &DecisionSpec) -> u64 {
        match spec.seed {
            Some(base) => {
                let n = self.seed_counter.fetch_add(1, Ordering::Relaxed);
                let mut rng =
                    crate::decision::SplitMix64::new(base ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                rng.next_u64()
            }
            None => crate::decision::random_seed(),
        }
    }

    pub fn spec(&self) -> DecisionSpec {
        self.engine.read().unwrap().spec().clone()
    }
}

/// Cache of loaded runtimes.
#[derive(Default)]
pub struct RuntimeCache {
    map: RwLock<HashMap<(String, String, String), Arc<CapsuleRuntime>>>,
}

/// Why a runtime could not be produced.
#[derive(Debug)]
pub enum LoadError {
    NotFound,
    Invalid(String),
}

impl RuntimeCache {
    /// The loaded runtime, loading it on first use.
    pub fn get(
        &self,
        store: &Store,
        events: &dyn EventStore,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<Arc<CapsuleRuntime>, LoadError> {
        let k = (tenant.to_string(), job.to_string(), capsule.to_string());
        if let Some(rt) = self.map.read().unwrap().get(&k) {
            return Ok(rt.clone());
        }
        // Load outside the map lock; two racing loads produce equal
        // runtimes and the first insert wins.
        let rt = Arc::new(load_runtime(store, events, tenant, job, capsule)?);
        let mut map = self.map.write().unwrap();
        Ok(map.entry(k).or_insert(rt).clone())
    }

    /// Drop a cached runtime so the next request reloads it.
    pub fn invalidate(&self, tenant: &str, job: &str, capsule: &str) {
        self.map.write().unwrap().remove(&(
            tenant.to_string(),
            job.to_string(),
            capsule.to_string(),
        ));
    }

    /// Drop every runtime under a tenant (and optionally one job).
    pub fn invalidate_prefix(&self, tenant: &str, job: Option<&str>) {
        self.map
            .write()
            .unwrap()
            .retain(|(t, j, _), _| !(t == tenant && job.is_none_or(|job| j == job)));
    }

    pub fn loaded(&self) -> Vec<Arc<CapsuleRuntime>> {
        self.map.read().unwrap().values().cloned().collect()
    }
}

fn load_runtime(
    store: &Store,
    events: &dyn EventStore,
    tenant: &str,
    job: &str,
    capsule: &str,
) -> Result<CapsuleRuntime, LoadError> {
    let spec = store
        .load_spec(tenant, job, capsule)
        .map_err(LoadError::Invalid)?
        .ok_or(LoadError::NotFound)?;
    let key =
        CapsuleKey::new(tenant, job, capsule).map_err(|e| LoadError::Invalid(e.to_string()))?;
    let program = match store
        .load_program(tenant, job, capsule)
        .map_err(LoadError::Invalid)?
    {
        Some(bytes) => Some(FeatureProgram::load(&bytes).map_err(LoadError::Invalid)?),
        None => None,
    };
    let (policy, policy_error) = match store.load_execution_policy(tenant, job, capsule) {
        Ok(p) => (p, None),
        Err(e) => {
            tracing::error!(tenant, job, capsule, error = %e, "invalid policy; the feature program runs deny-all");
            (ExecutionPolicy::deny_all(), Some(e))
        }
    };
    let (engine, watermark) = restore_engine(events, &key, spec).map_err(LoadError::Invalid)?;
    Ok(CapsuleRuntime {
        data_dir: store
            .data_dir(tenant, job, capsule)
            .map_err(LoadError::Invalid)?,
        key,
        engine: RwLock::new(engine),
        program,
        policy,
        policy_error,
        reward_lock: Mutex::new(()),
        reward_watermark: AtomicI64::new(watermark),
        since_snapshot: AtomicU64::new(0),
        seed_counter: AtomicU64::new(0),
    })
}

/// Latest snapshot plus replay of every learned reward after its watermark.
/// Returns the engine and the watermark (sequence of the last reward
/// applied).
pub fn restore_engine(
    events: &dyn EventStore,
    key: &CapsuleKey,
    spec: DecisionSpec,
) -> Result<(Engine, i64), String> {
    let (mut engine, mut watermark) = match events
        .load_latest_model(key)
        .map_err(|e| e.to_string())?
    {
        Some(ModelSnapshot {
            state, reward_seq, ..
        }) => match Engine::restore(spec.clone(), &state) {
            Ok(engine) => (engine, reward_seq),
            Err(e) => {
                // A snapshot that no longer fits the spec (for example after
                // a change to learner.bits) is rebuilt from the full log.
                tracing::warn!(error = %e, "model snapshot does not fit the spec; replaying all rewards");
                (Engine::new(spec.clone())?, 0)
            }
        },
        None => (Engine::new(spec.clone())?, 0),
    };
    loop {
        let rows = events
            .rewards_since(key, watermark, 10_000)
            .map_err(|e| e.to_string())?;
        if rows.is_empty() {
            break;
        }
        for (reward, decision) in &rows {
            watermark = reward.seq;
            if !reward_was_learned(reward.detail.as_deref()) {
                continue;
            }
            let (context, derived, action) = decision_learning_inputs(decision)?;
            let probability = decision.probability.unwrap_or(1.0);
            engine.learn(&context, &derived, &action, reward.value, probability)?;
        }
    }
    Ok((engine, watermark))
}

/// Rewards recorded while a capsule was frozen carry `"learned": false` in
/// their detail and are skipped on replay, so the rebuilt model matches the
/// live one.
pub fn reward_was_learned(detail: Option<&str>) -> bool {
    detail
        .and_then(|d| serde_json::from_str::<Value>(d).ok())
        .and_then(|v| v.get("learned").and_then(|l| l.as_bool()))
        .unwrap_or(true)
}

/// The context, derived features and chosen action of a logged decision.
pub fn decision_learning_inputs(
    decision: &crate::eventstore::DecisionRecord,
) -> Result<(Value, Value, ActionSpec), String> {
    let context: Value = serde_json::from_str(&decision.context)
        .map_err(|e| format!("decision {}: invalid context: {e}", decision.id))?;
    let derived: Value = serde_json::from_str(&decision.derived)
        .map_err(|e| format!("decision {}: invalid derived features: {e}", decision.id))?;
    let actions: Vec<ActionSpec> = serde_json::from_str(&decision.actions)
        .map_err(|e| format!("decision {}: invalid actions: {e}", decision.id))?;
    let action = actions
        .get(decision.chosen_index as usize)
        .cloned()
        .ok_or_else(|| format!("decision {}: chosen index out of range", decision.id))?;
    Ok((context, derived, action))
}
