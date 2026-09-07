use std::collections::HashMap;

use crate::change_detection::AdwinDetector;
use crate::meta_bandit::{CandidateId, MetaBandit};

use super::config::default_context_adwin_delta;
use super::memory::{ContextBucket, OptionState, StrategyMemory};
use super::stats::OptionStats;

// ── Capsule memory (sidecar) ──

#[derive(Debug, Clone)]
pub struct CapsuleMemory {
    pub strategies: HashMap<u32, StrategyMemory>,
    pub version: u32,
    /// Per-capsule rolling windows for `FeatureType::TimeSeries` features,
    /// keyed by feature name.
    pub time_series_windows: HashMap<String, crate::feature_schema::TimeSeriesWindow>,
    /// Shared-state LinUCB state when `sharedState.enabled`; `None` for
    /// legacy per-option-LinUCB capsules.
    pub shared_state: Option<crate::shared_state_strategy::SharedStateOptionStrategy>,
}

impl Default for CapsuleMemory {
    fn default() -> Self { Self {
        strategies: HashMap::new(),
        version: 2,
        time_series_windows: HashMap::new(),
        shared_state: None,
    } }
}

impl CapsuleMemory {
    pub fn to_json(&self) -> serde_json::Value {
        let mut strats = serde_json::Map::new();
        for (nid, sm) in &self.strategies {
            let mut contexts = serde_json::Map::new();
            for (ctx_key, bucket) in &sm.contexts {
                contexts.insert(ctx_key.clone(), serialize_bucket(bucket));
            }
            let mut candidate_ctx_json = serde_json::Map::new();
            for ((cid, ctx_key), bucket) in &sm.candidate_contexts {
                let combined_key = format!("{}|{}", candidate_id_str(*cid), ctx_key);
                candidate_ctx_json.insert(combined_key, serialize_bucket(bucket));
            }
            let meta_bandit_json = sm.meta_bandit.as_ref().map(serialize_meta_bandit);
            let mut context_detectors_json = serde_json::Map::new();
            for (ctx_key, detector) in &sm.context_detectors {
                context_detectors_json.insert(ctx_key.clone(), serialize_adwin(detector));
            }
            let discrete_ood_json = sm.discrete_ood.as_ref()
                .and_then(|d| serde_json::to_value(d).ok());
            let feature_ood_json = sm.feature_ood.as_ref()
                .and_then(|d| serde_json::to_value(d).ok());
            strats.insert(nid.to_string(), serde_json::json!({
                "nodeId": nid,
                "nOptions": sm.n_options,
                "contexts": contexts,
                "candidateContexts": candidate_ctx_json,
                "metaBandit": meta_bandit_json,
                "contextDetectors": context_detectors_json,
                "discreteOod": discrete_ood_json,
                "featureOod": feature_ood_json,
            }));
        }
        let mut ts_json = serde_json::Map::new();
        for (name, win) in &self.time_series_windows {
            ts_json.insert(name.clone(), win.serialize());
        }
        let shared_state_json = self.shared_state.as_ref().map(|s| s.to_json());
        serde_json::json!({
            "version": 7,
            "strategies": strats,
            "timeSeriesWindows": ts_json,
            "sharedState": shared_state_json,
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Self {
        let mut mem = Self::default();
        mem.version = j.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        if let Some(strats) = j.get("strategies").and_then(|v| v.as_object()) {
            for (nid_str, sm_json) in strats {
                let nid: u32 = nid_str.parse().unwrap_or(0);
                let n_options = sm_json.get("nOptions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let mut contexts = HashMap::new();
                if let Some(ctx_map) = sm_json.get("contexts").and_then(|v| v.as_object()) {
                    for (ctx_key, bucket_json) in ctx_map {
                        contexts.insert(ctx_key.clone(), parse_bucket(bucket_json));
                    }
                }
                let mut candidate_contexts = HashMap::new();
                if let Some(cc_map) = sm_json.get("candidateContexts").and_then(|v| v.as_object()) {
                    for (combined_key, bucket_json) in cc_map {
                        let Some((cid_str, ctx_key)) = combined_key.split_once('|') else { continue; };
                        let Some(candidate) = candidate_id_from_str(cid_str) else { continue; };
                        candidate_contexts.insert((candidate, ctx_key.to_string()), parse_bucket(bucket_json));
                    }
                }
                let meta_bandit = sm_json.get("metaBandit").and_then(parse_meta_bandit);
                let mut context_detectors = HashMap::new();
                if let Some(cd_map) = sm_json.get("contextDetectors").and_then(|v| v.as_object()) {
                    for (ctx_key, detector_json) in cd_map {
                        if let Some(detector) = parse_adwin(detector_json) {
                            context_detectors.insert(ctx_key.clone(), detector);
                        }
                    }
                }
                let discrete_ood = sm_json.get("discreteOod")
                    .and_then(|v| serde_json::from_value::<crate::ood::DiscreteOodDetector>(v.clone()).ok());
                let feature_ood = sm_json.get("featureOod")
                    .and_then(|v| serde_json::from_value::<crate::ood::FeatureOodDetector>(v.clone()).ok());
                mem.strategies.insert(nid, StrategyMemory {
                    node_id: nid, n_options, contexts, candidate_contexts, meta_bandit, context_detectors,
                    discrete_ood, feature_ood,
                });
            }
        }
        if let Some(ts_map) = j.get("timeSeriesWindows").and_then(|v| v.as_object()) {
            for (name, win_json) in ts_map {
                if let Ok(win) = crate::feature_schema::TimeSeriesWindow::deserialize(win_json) {
                    mem.time_series_windows.insert(name.clone(), win);
                }
            }
        }
        if let Some(ss_json) = j.get("sharedState") {
            if !ss_json.is_null() {
                if let Ok(ss) = crate::shared_state_strategy::SharedStateOptionStrategy::from_json(ss_json) {
                    mem.shared_state = Some(ss);
                }
            }
        }
        mem
    }

    /// Get or create a context bucket for a strategy, initializing from graph weights.
    pub fn get_or_init_context(&mut self, node_id: u32, context_key: &str, graph_weights: &[f64], n_options: usize) -> &mut ContextBucket {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id, n_options, contexts: HashMap::new(), candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.contexts.entry(context_key.to_string()).or_insert_with(|| {
            let weights = if graph_weights.len() >= n_options {
                graph_weights[..n_options].to_vec()
            } else {
                vec![1.0 / n_options as f64; n_options]
            };
            let option_states: Vec<OptionState> = weights.iter().map(|w| OptionState::weighted(*w)).collect();
            ContextBucket {
                weights,
                stats: (0..n_options).map(|_| OptionStats::default()).collect(),
                updated_at: 0,
                conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
                option_states,
            }
        })
    }

    #[allow(dead_code)]
    pub fn list_contexts(&self, node_id: u32) -> Vec<String> {
        self.strategies.get(&node_id)
            .map(|sm| sm.contexts.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_or_init_candidate_context(
        &mut self,
        node_id: u32,
        context_key: &str,
        candidate: CandidateId,
        graph_weights: &[f64],
        n_options: usize,
    ) -> &mut ContextBucket {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.candidate_contexts
            .entry((candidate, context_key.to_string()))
            .or_insert_with(|| {
                let weights = if graph_weights.len() >= n_options {
                    graph_weights[..n_options].to_vec()
                } else {
                    vec![1.0 / n_options as f64; n_options]
                };
                let option_states = make_option_states_for_candidate(candidate, &weights);
                ContextBucket {
                    weights,
                    stats: (0..n_options).map(|_| OptionStats::default()).collect(),
                    updated_at: 0,
                    conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
                    option_states,
                }
            })
    }

    /// Drop all candidate state for a given (node_id, context_key) pair.
    /// Used when the meta-bandit re-warms after a regime change.
    pub fn reset_candidate_contexts(&mut self, node_id: u32, context_key: &str) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            sm.candidate_contexts.retain(|(_, ck), _| ck != context_key);
        }
    }

    pub fn get_or_init_meta_bandit(
        &mut self,
        node_id: u32,
        n_options: usize,
        candidates: &[CandidateId],
    ) -> &mut MetaBandit {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        let cands = candidates.to_vec();
        sm.meta_bandit.get_or_insert_with(|| MetaBandit::new_with_candidates(&cands))
    }

    pub fn meta_bandit_for(&self, node_id: u32) -> Option<&MetaBandit> {
        self.strategies.get(&node_id).and_then(|sm| sm.meta_bandit.as_ref())
    }

    pub fn reset_meta_bandit(&mut self, node_id: u32) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            if let Some(mb) = sm.meta_bandit.as_mut() {
                mb.reset();
            }
        }
    }

    pub fn get_or_init_context_detector(
        &mut self,
        node_id: u32,
        context_key: &str,
    ) -> &mut AdwinDetector {
        self.get_or_init_context_detector_with_delta(
            node_id,
            context_key,
            default_context_adwin_delta(),
        )
    }

    /// Like `get_or_init_context_detector` but accepts an explicit delta.
    /// Delta applies only on first insertion; live detectors are not retuned.
    pub fn get_or_init_context_detector_with_delta(
        &mut self,
        node_id: u32,
        context_key: &str,
        context_adwin_delta: f64,
    ) -> &mut AdwinDetector {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options: 0,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.context_detectors
            .entry(context_key.to_string())
            .or_insert_with(|| AdwinDetector::new(context_adwin_delta, 1000))
    }

    pub fn get_or_init_discrete_ood(&mut self, node_id: u32) -> &mut crate::ood::DiscreteOodDetector {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options: 0,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.discrete_ood.get_or_insert_with(crate::ood::DiscreteOodDetector::new)
    }

    pub fn get_or_init_feature_ood(&mut self, node_id: u32, d: usize) -> &mut crate::ood::FeatureOodDetector {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options: 0,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        let existing_dim = sm.feature_ood.as_ref().map(|f| f.d);
        if existing_dim == Some(d) {
            sm.feature_ood.as_mut().unwrap()
        } else {
            sm.feature_ood = Some(crate::ood::FeatureOodDetector::new(d));
            sm.feature_ood.as_mut().unwrap()
        }
    }

    pub fn discrete_ood_for(&self, node_id: u32) -> Option<&crate::ood::DiscreteOodDetector> {
        self.strategies.get(&node_id).and_then(|sm| sm.discrete_ood.as_ref())
    }

    pub fn feature_ood_for(&self, node_id: u32) -> Option<&crate::ood::FeatureOodDetector> {
        self.strategies.get(&node_id).and_then(|sm| sm.feature_ood.as_ref())
    }

    pub fn reset_ood_detectors(&mut self, node_id: u32) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            if let Some(d) = sm.discrete_ood.as_mut() { d.reset(); }
            if let Some(d) = sm.feature_ood.as_mut() { d.reset(); }
        }
    }

    pub fn reset_context_detector(&mut self, node_id: u32, context_key: &str) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            if let Some(d) = sm.context_detectors.get_mut(context_key) {
                d.reset();
            }
        }
    }

    /// List all candidates that have state for a given (node_id, context_key).
    pub fn candidates_for_context(&self, node_id: u32, context_key: &str) -> Vec<CandidateId> {
        self.strategies
            .get(&node_id)
            .map(|sm| {
                sm.candidate_contexts
                    .keys()
                    .filter(|(_, ck)| ck == context_key)
                    .map(|(cid, _)| *cid)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn make_option_states_for_candidate(candidate: CandidateId, weights: &[f64]) -> Vec<OptionState> {
    match candidate {
        CandidateId::Thompson => weights.iter().map(|_| OptionState::beta(1.0, 1.0)).collect(),
        CandidateId::Ucb => weights.iter().map(|_| OptionState::ucb_initial()).collect(),
        CandidateId::Weighted | CandidateId::EpsilonGreedy | CandidateId::Greedy => {
            weights.iter().map(|w| OptionState::weighted(*w)).collect()
        }
        // The caller upgrades these to LinUcb states via `ensure_linucb_states`
        // once the feature dimension is known. LinTs reuses LinUcb's state.
        CandidateId::LinUcb | CandidateId::LinTs => {
            weights.iter().map(|w| OptionState::weighted(*w)).collect()
        }
    }
}

/// Upgrade a bucket's option_states to LinUcb variants at dimension `d`.
/// Idempotent — states at the same `d` are kept, others are replaced.
pub fn ensure_linucb_states(bucket: &mut ContextBucket, d: usize, lambda: f64) {
    for state in bucket.option_states.iter_mut() {
        let needs_replace = match state {
            OptionState::LinUcb { state: ls } => ls.d != d,
            _ => true,
        };
        if needs_replace {
            *state = OptionState::linucb_initial(d, lambda);
        }
    }
}

fn candidate_id_str(cid: CandidateId) -> &'static str {
    cid.as_str()
}

fn candidate_id_from_str(s: &str) -> Option<CandidateId> {
    CandidateId::from_str(s)
}

fn serialize_meta_bandit(mb: &MetaBandit) -> serde_json::Value {
    let candidates: Vec<serde_json::Value> = mb.candidates.iter().map(|c| {
        serde_json::json!({
            "id": candidate_id_str(c.id),
            "trials": c.trials,
            "cumulativeReward": c.cumulative_reward,
        })
    }).collect();
    serde_json::json!({
        "candidates": candidates,
        "totalRounds": mb.total_rounds,
        "explorationDecay": mb.exploration_decay,
        "minExploration": mb.min_exploration,
        "forgettingFactor": mb.forgetting_factor,
    })
}

fn serialize_adwin(d: &AdwinDetector) -> serde_json::Value {
    serde_json::json!({
        "window": d.window_snapshot(),
        "delta": d.delta(),
        "maxSize": d.max_size(),
        "minSubwindow": d.min_subwindow(),
    })
}

fn parse_adwin(j: &serde_json::Value) -> Option<AdwinDetector> {
    let window: Vec<f64> = j.get("window")?.as_array()?
        .iter().filter_map(|v| v.as_f64()).collect();
    let delta = j.get("delta")?.as_f64()?;
    let max_size = j.get("maxSize")?.as_u64()? as usize;
    let min_subwindow = j.get("minSubwindow").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    Some(AdwinDetector::restore_state(window, delta, max_size, min_subwindow))
}

fn parse_meta_bandit(j: &serde_json::Value) -> Option<MetaBandit> {
    let candidates_json = j.get("candidates")?.as_array()?;
    let ids: Vec<CandidateId> = candidates_json.iter()
        .filter_map(|c| c.get("id")?.as_str().and_then(candidate_id_from_str))
        .collect();
    if ids.is_empty() {
        return None;
    }
    let mut mb = MetaBandit::new_with_candidates(&ids);
    for c_json in candidates_json {
        let id_str = match c_json.get("id").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let id = match candidate_id_from_str(id_str) {
            Some(i) => i,
            None => continue,
        };
        let trials = c_json.get("trials").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cumulative_reward = c_json.get("cumulativeReward").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if let Some(c) = mb.candidates.iter_mut().find(|c| c.id == id) {
            c.trials = trials;
            c.cumulative_reward = cumulative_reward;
        }
    }
    mb.total_rounds = j.get("totalRounds").and_then(|v| v.as_u64()).unwrap_or(0);
    mb.exploration_decay = j.get("explorationDecay").and_then(|v| v.as_f64()).unwrap_or(5.0);
    mb.min_exploration = j.get("minExploration").and_then(|v| v.as_f64()).unwrap_or(0.05);
    mb.forgetting_factor = j.get("forgettingFactor").and_then(|v| v.as_f64()).unwrap_or(0.999);
    Some(mb)
}

fn serialize_bucket(bucket: &ContextBucket) -> serde_json::Value {
    let stats_json: Vec<serde_json::Value> = bucket.stats.iter().map(|s| {
        let mut sj = s.to_json();
        if let Some(m) = sj.as_object_mut() {
            m.insert("rewardSum".into(), serde_json::json!(s.reward_sum));
            m.insert("rewardSqSum".into(), serde_json::json!(s.reward_sq_sum));
            m.insert("effectiveTries".into(), serde_json::json!(s.effective_tries));
            let win: Vec<f64> = s.window.iter().copied().collect();
            m.insert("window".into(), serde_json::json!(win));
            m.insert("phCumsum".into(), serde_json::json!(s.ph_cumsum));
            m.insert("phMin".into(), serde_json::json!(s.ph_min));
            m.insert("changeBoostRemaining".into(), serde_json::json!(s.change_boost_remaining));
            m.insert("changePoints".into(), serde_json::json!(s.change_points));
        }
        sj
    }).collect();
    let option_states_json: Vec<serde_json::Value> =
        bucket.option_states.iter().map(OptionState::to_json).collect();
    let calibrator_json = serde_json::json!({
        "residuals": bucket.conformity_calibrator.residuals_snapshot(),
        "maxSize": bucket.conformity_calibrator.max_size(),
        "minSamples": bucket.conformity_calibrator.min_samples(),
    });
    serde_json::json!({
        "weights": bucket.weights,
        "stats": stats_json,
        "updatedAt": bucket.updated_at,
        "conformityCalibrator": calibrator_json,
        "optionStates": option_states_json,
    })
}

fn parse_bucket(bucket_json: &serde_json::Value) -> ContextBucket {
    let weights: Vec<f64> = bucket_json.get("weights")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
        .unwrap_or_default();
    let stats: Vec<OptionStats> = bucket_json.get("stats")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(OptionStats::from_json).collect())
        .unwrap_or_default();
    let updated_at = bucket_json.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
    let conformity_calibrator = if let Some(cal_json) = bucket_json.get("conformityCalibrator") {
        parse_conformal(cal_json).unwrap_or_else(crate::conformal::ConformalCalibrator::default_config)
    } else {
        // Legacy path: read raw residuals array left by v5 sidecars.
        let residuals: Vec<f64> = bucket_json.get("conformityScores")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
            .unwrap_or_default();
        crate::conformal::ConformalCalibrator::restore_state(residuals, 500, 30)
    };
    let option_states: Vec<OptionState> = bucket_json.get("optionStates")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(OptionState::from_json).collect::<Vec<_>>())
        .filter(|v| v.len() == weights.len())
        .unwrap_or_else(|| weights.iter().map(|w| OptionState::weighted(*w)).collect());
    ContextBucket {
        weights, stats, updated_at, conformity_calibrator, option_states,
    }
}

fn parse_conformal(j: &serde_json::Value) -> Option<crate::conformal::ConformalCalibrator> {
    let residuals: Vec<f64> = j.get("residuals")?.as_array()?
        .iter().filter_map(|v| v.as_f64()).collect();
    let max_size = j.get("maxSize").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
    let min_samples = j.get("minSamples").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
    Some(crate::conformal::ConformalCalibrator::restore_state(residuals, max_size, min_samples))
}
