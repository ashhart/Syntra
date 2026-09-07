use tracing::{error, info};

use crate::store::sha256_hex;
use crate::graph::{Contract, NeuralGraph};
use crate::graph_executor::GraphExecutor;
use crate::context::ExecutionContext;
use crate::capabilities;
use crate::verifier;

use super::errors::{Resp, err_json, json_resp};
use super::helpers::{
    all_choice_nodes, apply_context_memory_to_graph, audit_event_json,
    extract_decisions, flatten_strategy_weights, primary_choice_node,
    stable_hash_features,
};
use super::state::State;

/// Hierarchical-capsule `/decide` handler. Selection runs through the
/// per-level meta-bandits in `HierarchicalCapsuleState`; the `.lyc` is
/// not executed. Decision-log entries carry `path`, `leafName`, and
/// `perLevelCandidateIds`.
fn do_decide_hierarchical(
    state: &State,
    tenant: &str,
    job: &str,
    capsule: &str,
    body: &str,
    learn: bool,
    spec: crate::hierarchical::HierarchicalSpec,
) -> Resp {
    // Lazily initialise the per-HierState bandit state on first use.
    let mut hier_state = state.store
        .load_hierarchical_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| {
            crate::hierarchical_state::HierarchicalCapsuleState::new(spec.clone())
        });

    // Two independent rand draws per level feed the meta-bandit's
    // (explore-vs-exploit, random-pick) selection.
    let decision = match hier_state.select_path(|| {
        (crate::learning::rand_f64(), crate::learning::rand_f64())
    }) {
        Some(d) => d,
        None => return json_resp(500, &err_json(
            "hierarchical select_path returned None — spec malformed?",
        )),
    };

    if let Err(e) = state.store.save_hierarchical_state_in_job(
        tenant, job, capsule, &hier_state,
    ) {
        error!(tenant = %tenant, job = %job, capsule = %capsule, error = %e,
               "save hierarchical state failed");
    }

    let decision_id = format!("dec_{}", sha256_hex(
        format!(
            "{}{}{}{}", tenant, job, capsule,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ).as_bytes(),
    ).get(..16).unwrap_or(""));

    // `/feedback` detects hierarchical decisions by the presence of `path`.
    let dec_entry = serde_json::json!({
        "node_id": 0,  // legacy parity — hierarchical has no graph node id
        "path": decision.path,
        "leafName": decision.leaf_name,
        "perLevelCandidateIds": decision.per_level_candidate_ids,
        "kind": "hierarchical",
    });
    let decisions_arr = serde_json::json!([dec_entry]);

    let decision_event = serde_json::json!({
        "id": decision_id,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "contextKey": "default",  // hierarchical v1 uses a single context bucket
        "algorithm": "hierarchical",
        "inputSha256": sha256_hex(body.as_bytes()).get(..16).unwrap_or(""),
        "learned": learn,
        "decisions": decisions_arr,
        "refused": false,
        "refusalReason": serde_json::Value::Null,
    });
    state.store.append_decision_log_in_job(
        tenant, job, capsule, &decision_event.to_string(),
    ).ok();

    let warmup_state = state.store
        .load_warmup_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| crate::warmup::WarmupState::new(30));
    let warmup_json: serde_json::Value = match &warmup_state.lifecycle {
        crate::warmup::CapsuleLifecycle::Warmup { samples_collected, target } => {
            serde_json::json!({
                "state": "warmup",
                "collected": samples_collected,
                "target": target,
            })
        }
        crate::warmup::CapsuleLifecycle::Active { .. } => {
            serde_json::json!({"state": "active"})
        }
        crate::warmup::CapsuleLifecycle::Frozen { reason, .. } => {
            serde_json::json!({"state": "frozen", "reason": reason})
        }
    };

    let response = serde_json::json!({
        "ok": true,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "decisionId": decision_id,
        "decisions": decisions_arr,
        "algorithm": "hierarchical",
        "warmup": warmup_json,
        "refused": false,
        "learned": learn,
    });
    state.metrics.record_request("decide", tenant, job, capsule, "ok");
    json_resp(200, &response.to_string())
}
pub(super) fn do_decide(state: &State, tenant: &str, job: &str, capsule: &str, body: &str, learn: bool) -> Resp {
    // Hierarchical capsules bypass the flat graph path entirely.
    if let Some(hier_spec) = state.store.load_hierarchical_spec_in_job(tenant, job, capsule) {
        return do_decide_hierarchical(state, tenant, job, capsule, body, learn, hier_spec);
    }

    let data = match state.store.load_graph_in_job(tenant, job, capsule) {
        Ok(d) => d,
        Err(e) => return json_resp(404, &err_json(&e)),
    };
    let graph_hash = sha256_hex(&data);
    let mut ng = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => return json_resp(500, &err_json(&e)),
    };
    if let Err(e) = verifier::verify(&ng) {
        return json_resp(500, &err_json(&format!("{e}")));
    }

    // Load capsule policy — fail closed
    let policy = match state.store.load_execution_policy_in_job(tenant, job, capsule) {
        Ok(p) => Some(p),
        Err(e) => {
            error!(tenant = %tenant, job = %job, capsule = %capsule, error = %e, "policy load failed — denying all");
            Some(crate::context::ExecutionPolicy {
                allow_stdout: false, allow_stdin: false,
                allow_file_read: false, allow_file_write: false, allow_network: false,
                file_root: None, allowed_hosts: vec![], deny_private_networks: true,
            })
        }
    };

    // Parse input and extract contextKey
    let body_json: Option<serde_json::Value> = if !body.trim().is_empty() {
        serde_json::from_str(body).ok()
    } else {
        None
    };
    let input = body_json.as_ref().map(|v| {
        // If there's an "input" field, use that; otherwise use the whole body
        if let Some(inp) = v.get("input") {
            capabilities::CapValue::from_json(inp)
        } else {
            capabilities::CapValue::from_json(v)
        }
    });

    // Load these before execution so context weights drive the live decision.
    let learning_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    let mut memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();

    let context_spec = learning_cfg.context_spec.clone();
    let (context_key, feature_vector): (String, Option<Vec<f64>>) = match &context_spec {
        crate::feature_schema::ContextSpec::Discrete => {
            let key = body_json.as_ref()
                .and_then(|j| j.get("contextKey"))
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            (key, None)
        }
        crate::feature_schema::ContextSpec::Features { .. } => {
            let features_json = match body_json.as_ref().and_then(|j| j.get("features")) {
                Some(v) => v,
                None => return json_resp(400, &err_json(
                    "capsule has feature-context schema; request body must include a 'features' object"
                )),
            };
            let features_obj = match features_json.as_object() {
                Some(o) => o,
                None => return json_resp(400, &err_json("'features' must be a JSON object")),
            };
            let mut values: std::collections::HashMap<String, crate::feature_schema::FeatureValue> =
                std::collections::HashMap::new();
            for (name, v) in features_obj {
                if let Some(fv) = crate::feature_schema::FeatureValue::from_json(v) {
                    values.insert(name.clone(), fv);
                }
            }
            // Push raw TimeSeries values onto the rolling windows before
            // encoding so `encode_with_windows` sees the updated history.
            if let crate::feature_schema::ContextSpec::Features { features } = &context_spec {
                for spec in features {
                    if let crate::feature_schema::FeatureType::TimeSeries { window_size, .. }
                        = &spec.feature_type
                    {
                        if let Some(crate::feature_schema::FeatureValue::Number(n))
                            = values.get(&spec.name)
                        {
                            let win = memory.time_series_windows
                                .entry(spec.name.clone())
                                .or_insert_with(|| {
                                    crate::feature_schema::TimeSeriesWindow::new(*window_size)
                                });
                            win.push(*n);
                        }
                    }
                }
            }
            let windows_ref: std::collections::HashMap<String, &crate::feature_schema::TimeSeriesWindow> =
                memory.time_series_windows.iter()
                    .map(|(k, v)| (k.clone(), v))
                    .collect();
            let vec = match context_spec.encode_with_windows(&values, &windows_ref) {
                Ok(v) => v,
                Err(e) => return json_resp(400, &err_json(&format!("feature encoding: {e}"))),
            };
            if let Err(e) = crate::linucb::validate_features(&vec, context_spec.encoded_dimension()) {
                return json_resp(400, &err_json(&format!("feature validation: {e}")));
            }
            let key = stable_hash_features(&vec);
            (key, Some(vec))
        }
    };
    let context_key: &str = &context_key;

    // OOD score-then-record on the first AdaptiveChoice; the current
    // request is reflected on the next call.
    let first_choice_node_id: Option<u32> = primary_choice_node(&ng);
    let ood_score: f64 = if let Some(nid) = first_choice_node_id {
        match &context_spec {
            crate::feature_schema::ContextSpec::Discrete => {
                let score = memory.discrete_ood_for(nid)
                    .map(|d| d.score(context_key))
                    .unwrap_or(0.0);
                memory.get_or_init_discrete_ood(nid).record(context_key);
                score
            }
            crate::feature_schema::ContextSpec::Features { .. } => {
                let x = match feature_vector.as_ref() {
                    Some(v) => v,
                    None => return json_resp(500, &err_json("feature path missing vector")),
                };
                let d = context_spec.encoded_dimension();
                let score = memory.feature_ood_for(nid)
                    .map(|det| det.score(x))
                    .unwrap_or(0.0);
                let det = memory.get_or_init_feature_ood(nid, d);
                det.record(x);
                if det.rebuild_due(100) {
                    det.rebuild_cov_inv();
                }
                score
            }
        }
    } else {
        0.0
    };

    let warmup_state = state.store
        .load_warmup_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| crate::warmup::WarmupState::new(30));
    let in_warmup = warmup_state.is_warmup();
    let in_active = warmup_state.is_active();

    let mut effective_selection_mode = if in_warmup {
        crate::context::SelectionMode::Weighted
    } else {
        match warmup_state.current_algorithm() {
            Some(crate::reward_characterization::PickedAlgorithm::Thompson { .. }) => {
                crate::context::SelectionMode::Weighted
            }
            Some(crate::reward_characterization::PickedAlgorithm::UCB { .. }) => {
                crate::context::SelectionMode::Greedy
            }
            Some(crate::reward_characterization::PickedAlgorithm::EpsilonGreedy { .. }) => {
                crate::context::SelectionMode::EpsilonGreedy
            }
            Some(crate::reward_characterization::PickedAlgorithm::Weighted { .. }) => {
                crate::context::SelectionMode::Weighted
            }
            None => match learning_cfg.safety.selection_mode {
                crate::learning::SelectionMode::Greedy => crate::context::SelectionMode::Greedy,
                crate::learning::SelectionMode::Weighted => crate::context::SelectionMode::Weighted,
                crate::learning::SelectionMode::EpsilonGreedy => crate::context::SelectionMode::EpsilonGreedy,
            },
        }
    };

    let mut chosen_candidate: Option<crate::meta_bandit::CandidateId> = None;
    // `chosen_candidate` mirrors the first AdaptiveChoice's candidate (used by
    // the legacy feedback path); per-node entries support multi-decision feedback.
    let mut per_node_candidates: std::collections::HashMap<u32, crate::meta_bandit::CandidateId>
        = std::collections::HashMap::new();
    // Per-node shared-state LinUCB/LinTs scored arrays, surfaced in the
    // /decide response as `sharedStateScores`.
    let mut shared_state_scored_per_node:
        std::collections::HashMap<u32, Vec<(String, f64)>> =
            std::collections::HashMap::new();

    // Binary rewards drive hard greedy commit; continuous rewards use a
    // softer nudge. See `apply_context_memory_to_graph`.
    let is_binary_reward = matches!(
        warmup_state.current_algorithm(),
        Some(crate::reward_characterization::PickedAlgorithm::Thompson { .. })
    );

    let bandit_decisions = if in_warmup {
        flatten_strategy_weights(&mut ng);
        std::collections::HashMap::new()
    } else {
        let bd = apply_context_memory_to_graph(&mut ng, &memory, context_key, &learning_cfg, is_binary_reward);

        if in_active {
            // Each AdaptiveChoice gets its own meta-bandit candidate. The
            // first one is mirrored into the legacy `chosen_candidate` field
            // for the legacy feedback path.
            let choice_nodes = all_choice_nodes(&ng);
            for (idx, (node_id, weights_len, contract)) in choice_nodes.into_iter().enumerate() {
                let is_first = idx == 0;
                let n_options = if contract == Contract::WithinTolerance && weights_len > 1 {
                    weights_len - 1
                } else {
                    weights_len
                };
                if n_options > 0 {
                    // Shared-state short-circuit: replace the meta-bandit +
                    // per-option LinUcb chain with a single θ over
                    // [x_context, x_option]. Recorded candidate stays LinUcb
                    // so feedback routing is unchanged.
                    if learning_cfg.shared_state.enabled && feature_vector.is_some() {
                        let encoded_d_context =
                            learning_cfg.context_spec.encoded_dimension();
                        if memory.shared_state.is_none() {
                            let mut strat =
                                crate::shared_state_strategy::SharedStateOptionStrategy::new(
                                    encoded_d_context,
                                    learning_cfg.shared_state.d_option,
                                    learning_cfg.shared_state.lambda,
                                );
                            for (name, feats) in &learning_cfg.shared_state.option_features {
                                let _ = strat.register_option(name, feats.clone());
                            }
                            memory.shared_state = Some(strat);
                        }
                        let strategy = memory.shared_state.as_ref().unwrap();
                        let x = feature_vector.as_ref().unwrap();
                        let alpha = learning_cfg.shared_state.alpha;
                        let kind = learning_cfg.shared_state.score_kind;
                        let mut rng_normal = || {
                            let u1 = crate::learning::rand_f64().clamp(1e-12, 1.0 - 1e-12);
                            let u2 = crate::learning::rand_f64().clamp(1e-12, 1.0 - 1e-12);
                            (-2.0 * u1.ln()).sqrt()
                                * (2.0 * std::f64::consts::PI * u2).cos()
                        };

                        // BTreeMap order is sorted by name, matching the
                        // operand order in capsule source.
                        let option_names: Vec<String> = learning_cfg.shared_state
                            .option_features.keys().cloned().collect();
                        let mut scored: Vec<(String, f64)> =
                            Vec::with_capacity(option_names.len());
                        let mut best_idx: usize = 0;
                        let mut best_score: f64 = f64::NEG_INFINITY;
                        for (i, name) in option_names.iter().enumerate().take(n_options) {
                            if let Some(opt_feats) = strategy.option_features.get(name) {
                                let score = match kind {
                                    crate::learning::SharedStateScoreKind::Ucb => {
                                        strategy.shared.shared_ucb_score(x, opt_feats, alpha).0
                                    }
                                    crate::learning::SharedStateScoreKind::LinTs => {
                                        strategy.shared.shared_lin_ts_score(
                                            x, opt_feats, alpha, &mut rng_normal,
                                        )
                                    }
                                };
                                scored.push((name.clone(), score));
                                if score > best_score {
                                    best_score = score;
                                    best_idx = i;
                                }
                            }
                        }

                        // One-hot graph weights on the chosen option.
                        for (i, w) in ng.nodes[node_id as usize].weights.iter_mut().enumerate() {
                            if i < n_options {
                                *w = if i == best_idx { 1.0 } else { 0.0 };
                            }
                        }

                        per_node_candidates.insert(node_id, crate::meta_bandit::CandidateId::LinUcb);
                        if is_first {
                            chosen_candidate = Some(crate::meta_bandit::CandidateId::LinUcb);
                            effective_selection_mode =
                                crate::context::SelectionMode::Greedy;
                        }
                        shared_state_scored_per_node.insert(node_id, scored);
                        continue;
                    }

                    let candidates_list: Vec<crate::meta_bandit::CandidateId> = match &context_spec {
                        crate::feature_schema::ContextSpec::Discrete => {
                            crate::meta_bandit::CandidateId::discrete_only().to_vec()
                        }
                        crate::feature_schema::ContextSpec::Features { .. } => {
                            crate::meta_bandit::CandidateId::all().to_vec()
                        }
                    };
                    let candidate = {
                        let mb = memory.get_or_init_meta_bandit(node_id, n_options, &candidates_list);
                        let r1 = crate::learning::rand_f64();
                        let r2 = crate::learning::rand_f64();
                        let (c, _) = mb.select(r1, r2);
                        c
                    };
                    per_node_candidates.insert(node_id, candidate);
                    if is_first {
                        chosen_candidate = Some(candidate);
                    }

                    let graph_weights: Vec<f64> =
                        ng.nodes[node_id as usize].weights[..n_options].to_vec();

                    let is_lin_candidate =
                        candidate == crate::meta_bandit::CandidateId::LinUcb
                        || candidate == crate::meta_bandit::CandidateId::LinTs;
                    if is_lin_candidate {
                        // LinUcb scores via UCB; LinTs samples θ̃ from N(μ, v²·A⁻¹).
                        let d = context_spec.encoded_dimension();
                        let x = feature_vector.clone().unwrap_or_default();
                        let bucket = memory.get_or_init_candidate_context(
                            node_id, context_key, candidate, &graph_weights, n_options,
                        );
                        crate::learning::ensure_linucb_states(bucket, d, 1.0);
                        let mut best_idx = 0;
                        let mut best_score = f64::NEG_INFINITY;
                        for (i, state) in bucket.option_states.iter().enumerate() {
                            if let crate::learning::OptionState::LinUcb { state: ls } = state {
                                let score = if candidate
                                    == crate::meta_bandit::CandidateId::LinUcb
                                {
                                    ls.ucb_score(&x, 1.0).0
                                } else {
                                    let r1 = crate::learning::rand_f64()
                                        .clamp(1e-12, 1.0 - 1e-12);
                                    let r2 = crate::learning::rand_f64()
                                        .clamp(1e-12, 1.0 - 1e-12);
                                    let mut box_muller = move || {
                                        let _ = (r1, r2);
                                        let u1 = crate::learning::rand_f64()
                                            .clamp(1e-12, 1.0 - 1e-12);
                                        let u2 = crate::learning::rand_f64()
                                            .clamp(1e-12, 1.0 - 1e-12);
                                        (-2.0 * u1.ln()).sqrt()
                                            * (2.0 * std::f64::consts::PI * u2).cos()
                                    };
                                    ls.lin_ts_score(&x, 0.1, &mut box_muller)
                                };
                                if score > best_score {
                                    best_score = score;
                                    best_idx = i;
                                }
                            }
                        }
                        for (i, w) in ng.nodes[node_id as usize].weights.iter_mut().enumerate() {
                            if i < n_options {
                                *w = if i == best_idx { 1.0 } else { 0.0 };
                            }
                        }
                    } else {
                        let new_weights: Vec<f64> = {
                            let bucket = memory.get_or_init_candidate_context(
                                node_id, context_key, candidate, &graph_weights, n_options,
                            );
                            bucket.weights.clone()
                        };
                        for (i, w) in new_weights.iter().enumerate() {
                            if i < ng.nodes[node_id as usize].weights.len() {
                                ng.nodes[node_id as usize].weights[i] = *w;
                            }
                        }
                    }

                    // Only the first AdaptiveChoice's candidate drives the
                    // executor selection_mode; later nodes overlay their own
                    // one-hot directly in graph weights.
                    if is_first {
                        effective_selection_mode = match candidate {
                            crate::meta_bandit::CandidateId::Thompson
                            | crate::meta_bandit::CandidateId::Weighted => {
                                crate::context::SelectionMode::Weighted
                            }
                            crate::meta_bandit::CandidateId::Ucb
                            | crate::meta_bandit::CandidateId::Greedy
                            | crate::meta_bandit::CandidateId::LinUcb
                            | crate::meta_bandit::CandidateId::LinTs => {
                                crate::context::SelectionMode::Greedy
                            }
                            crate::meta_bandit::CandidateId::EpsilonGreedy => {
                                crate::context::SelectionMode::EpsilonGreedy
                            }
                        };
                    }
                }
            }
        }
        bd
    };

    let working_dir = state.store.capsule_dir_in_job(tenant, job, capsule).ok();
    // Shared Rc lets us read `runtime.publish` output after the executor
    // (which consumes the ExecutionContext) returns.
    let published_buf = crate::context::new_published_buffer();
    let ctx = ExecutionContext {
        policy, input, working_dir,
        selection_mode: effective_selection_mode,
        selection_epsilon: learning_cfg.safety.selection_epsilon,
        published: Some(published_buf.clone()),
    };
    let mut executor = GraphExecutor::new_with_context(ng, ctx);
    let result = match executor.run() {
        Ok(val) => format!("{val}"),
        Err(e) => return json_resp(500, &err_json(&format!("{e}"))),
    };

    let stdout_lines = executor.stdout_buffer.clone();
    let graph = executor.into_graph();
    let decisions = extract_decisions(&graph);

    // Snapshot the `runtime.publish` buffer; every decision entry receives
    // the same map (per-decision attribution is not yet supported).
    let published_snapshot: serde_json::Map<String, serde_json::Value> = published_buf
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut enriched_decisions: Vec<serde_json::Value> = decisions.iter().map(|d| {
        let mut ed = d.clone();
        if let Some(nid) = d.get("node_id").and_then(|v| v.as_u64()) {
            let nid32 = nid as u32;
            if let Some(sm) = memory.strategies.get(&nid32) {
                if let Some(bucket) = sm.contexts.get(context_key) {
                    if let Some(m) = ed.as_object_mut() {
                        m.insert("contextKey".into(), serde_json::json!(context_key));
                        m.insert("contextWeights".into(), serde_json::json!(bucket.weights));
                        let stats: Vec<serde_json::Value> = bucket.stats.iter().map(|s| s.to_json()).collect();
                        m.insert("contextStats".into(), serde_json::json!(stats));
                        if let Some((chosen_by_algo, pset, band, posteriors)) = bandit_decisions.get(&nid32) {
                            m.insert("algorithmChose".into(), serde_json::json!(chosen_by_algo));
                            m.insert("predictionSet".into(), serde_json::json!(pset));
                            m.insert("setWidth".into(), serde_json::json!(pset.len()));
                            m.insert("posteriorMeans".into(), serde_json::json!(posteriors));
                            if let Some(r) = band {
                                m.insert("conformalBandRadius".into(), serde_json::json!(r));
                            }
                            if learning_cfg.conformal.enabled {
                                m.insert("coverage".into(), serde_json::json!(learning_cfg.conformal.coverage));
                            }
                        }
                    }
                }
            }
        }
        ed
    }).collect();

    // For Continuous action spaces, surface bucket midpoint as `chosenAction`.
    if !matches!(learning_cfg.action_space, crate::learning::ActionSpace::Discrete) {
        for ed in enriched_decisions.iter_mut() {
            let idx = match ed.get("chosen_option").and_then(|v| v.as_u64()) {
                Some(i) => i as usize,
                None => continue,
            };
            if let Some(mid) = learning_cfg.action_space.bucket_midpoint(idx) {
                if let Some(obj) = ed.as_object_mut() {
                    obj.insert("chosenAction".to_string(), serde_json::json!(mid));
                }
            }
        }
    }

    // Attach each AdaptiveChoice node's candidateId to its enriched entry.
    for ed in enriched_decisions.iter_mut() {
        let nid = ed.get("node_id").and_then(|v| v.as_u64()).map(|v| v as u32);
        let candidate = match nid.and_then(|n| per_node_candidates.get(&n)) {
            Some(c) => *c,
            None => continue,
        };
        if let Some(obj) = ed.as_object_mut() {
            obj.insert(
                "candidateId".to_string(),
                serde_json::Value::String(candidate.as_str().to_string()),
            );
            if matches!(candidate,
                crate::meta_bandit::CandidateId::LinUcb
                | crate::meta_bandit::CandidateId::LinTs)
            {
                if let Some(ref x) = feature_vector {
                    obj.insert(
                        "featureVector".to_string(),
                        serde_json::to_value(x).unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            // Surface shared-state per-option scores, sorted by score desc.
            if let Some(node_id) = nid {
                if let Some(scored) = shared_state_scored_per_node.get(&node_id) {
                    let mut sorted = scored.clone();
                    sorted.sort_by(|a, b| {
                        b.1.partial_cmp(&a.1)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let arr: Vec<serde_json::Value> = sorted
                        .into_iter()
                        .map(|(name, score)| {
                            serde_json::json!({
                                "option": name,
                                "score": score,
                            })
                        })
                        .collect();
                    obj.insert(
                        "sharedStateScores".to_string(),
                        serde_json::Value::Array(arr),
                    );
                }
            }
        }
    }

    // Every decision entry receives the same `published` map.
    if !published_snapshot.is_empty() {
        for ed in enriched_decisions.iter_mut() {
            if let Some(obj) = ed.as_object_mut() {
                obj.insert(
                    "published".to_string(),
                    serde_json::Value::Object(published_snapshot.clone()),
                );
            }
        }
    }

    // Add algorithm info
    let alg_str = match &learning_cfg.algorithm {
        crate::learning::Algorithm::SimpleWeighted => "simpleWeighted",
        crate::learning::Algorithm::EpsilonGreedy { .. } => "epsilonGreedy",
        crate::learning::Algorithm::Ucb1 => "ucb1",
        crate::learning::Algorithm::ThompsonSampling => "thompsonSampling",
        crate::learning::Algorithm::Softmax { .. } => "softmax",
    };

    let decision_id = format!("dec_{}", sha256_hex(
        format!("{}{}{}{}", tenant, job, capsule, std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        ).as_bytes()
    ).get(..16).unwrap_or(""));

    // Refusal evaluation runs before the decision log is written.
    let refusal_cfg = &learning_cfg.refusal;
    let alpha = (1.0 - refusal_cfg.coverage).clamp(0.0, 1.0);
    let interval_width: Option<f64> = first_choice_node_id.and_then(|nid| {
        let sm = memory.strategies.get(&nid)?;
        // Prefer the chosen bucket; pool across candidates when under-sampled.
        let chosen_iw = match chosen_candidate {
            Some(c) => sm.candidate_contexts
                .get(&(c, context_key.to_string()))
                .or_else(|| sm.contexts.get(context_key)),
            None => sm.contexts.get(context_key),
        }.and_then(|b| b.conformity_calibrator.interval_width(alpha));
        if chosen_iw.is_some() {
            return chosen_iw;
        }
        let mut pooled = Vec::new();
        for ((_, k), b) in &sm.candidate_contexts {
            if k == context_key {
                pooled.extend(b.conformity_calibrator.residuals_snapshot());
            }
        }
        if let Some(b) = sm.contexts.get(context_key) {
            pooled.extend(b.conformity_calibrator.residuals_snapshot());
        }
        if pooled.is_empty() {
            return None;
        }
        let max = pooled.len().max(30);
        let cal = crate::conformal::ConformalCalibrator::restore_state(pooled, max, 30);
        cal.interval_width(alpha)
    });

    // Refusal applies only when Active; warmup must collect baseline data.
    let refusal_reason: Option<&'static str> = if !refusal_cfg.enabled || !in_active {
        None
    } else if ood_score >= refusal_cfg.ood_threshold {
        Some("ood")
    } else {
        match interval_width {
            None => Some("insufficient_calibration_data"),
            Some(w) if w > refusal_cfg.max_interval_width => Some("interval_too_wide"),
            Some(_) => None,
        }
    };
    let refused = refusal_reason.is_some();
    if let Some(reason) = refusal_reason {
        state.metrics.record_refusal(tenant, job, capsule, reason);
    }

    let decision_event = serde_json::json!({
        "id": decision_id,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "contextKey": context_key,
        "algorithm": alg_str,
        "inputSha256": sha256_hex(body.as_bytes()).get(..16).unwrap_or(""),
        "graphHash": graph_hash.get(..16).unwrap_or(""),
        "learned": learn,
        "decisions": enriched_decisions,
        "refused": refused,
        "refusalReason": refusal_reason,
        "oodScore": ood_score,
        "intervalWidth": interval_width,
    });
    state.store.append_decision_log_in_job(tenant, job, capsule, &decision_event.to_string()).ok();

    state.store.save_memory_in_job(tenant, job, capsule, &memory).ok();

    if learn {
        let updated_bytes = graph.to_bytes();
        let after_hash = sha256_hex(&updated_bytes);
        if graph_hash != after_hash {
            state.store.snapshot_in_job(tenant, job, capsule).ok();
        }
        state.store.save_graph_in_job(tenant, job, capsule, &updated_bytes).ok();

        state.store.append_audit_in_job(tenant, job, capsule,
            &audit_event_json("decide", tenant, job, capsule, serde_json::json!({
                "decisionId": decision_id, "learned": true,
                "beforeHash": graph_hash, "afterHash": after_hash,
            }))).ok();
    } else {
        state.store.append_audit_in_job(tenant, job, capsule,
            &audit_event_json("decide", tenant, job, capsule, serde_json::json!({
                "decisionId": decision_id, "learned": false, "graphHash": graph_hash,
            }))).ok();
    }

    let warmup_info = match &warmup_state.lifecycle {
        crate::warmup::CapsuleLifecycle::Warmup { samples_collected, target } => {
            serde_json::json!({
                "state": "warmup",
                "collected": samples_collected,
                "target": target,
            })
        }
        crate::warmup::CapsuleLifecycle::Active { algorithm, .. } => {
            serde_json::json!({
                "state": "active",
                "algorithm": format!("{algorithm:?}"),
            })
        }
        crate::warmup::CapsuleLifecycle::Frozen { algorithm, reason } => {
            serde_json::json!({
                "state": "frozen",
                "algorithm": format!("{algorithm:?}"),
                "reason": reason,
            })
        }
    };

    if refused {
        let reason = refusal_reason.unwrap_or("?");
        info!(
            tenant = %tenant, job = %job, capsule = %capsule,
            decision_id = %decision_id, reason = %reason,
            ood_score = ood_score, interval_width = ?interval_width,
            "decision refused",
        );
        state.store.append_audit_in_job(tenant, job, capsule,
            &audit_event_json("decision_refused", tenant, job, capsule, serde_json::json!({
                "decisionId": decision_id,
                "reason": reason,
                "oodScore": ood_score,
                "intervalWidth": interval_width,
                "coverage": refusal_cfg.coverage,
            }))).ok();
    }

    let confidence_block = serde_json::json!({
        "oodScore": ood_score,
        "intervalWidth": interval_width,
        "coverage": refusal_cfg.coverage,
        "refused": refused,
        "refusalReason": refusal_reason,
    });

    if refused {
        json_resp(200, &serde_json::json!({
            "ok": true,
            "tenant": tenant,
            "job": job,
            "capsule": capsule,
            "decisionId": decision_id,
            "contextKey": context_key,
            "warmup": warmup_info,
            "decisions": [],
            "refused": true,
            "confidence": confidence_block,
            "oodScore": ood_score,
        }).to_string())
    } else {
        json_resp(200, &serde_json::json!({
            "ok": true,
            "tenant": tenant,
            "job": job,
            "capsule": capsule,
            "decisionId": decision_id,
            "contextKey": context_key,
            "algorithm": alg_str,
            "learned": learn,
            "warmup": warmup_info,
            "decisions": enriched_decisions,
            "result": result,
            "stdout": stdout_lines,
            "oodScore": ood_score,
            "refused": false,
            "confidence": confidence_block,
        }).to_string())
    }
}
