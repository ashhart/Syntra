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

/// Hierarchical-capsule `/decide` handler. The capsule's `.lyc` is not
/// executed in v1 — selection happens entirely through the per-level
/// meta-bandits in `HierarchicalCapsuleState`. The decision-log entry
/// carries `path`, `leafName`, and `perLevelCandidateIds` instead of
/// the flat `chosen_option`/`node_id` shape used by the meta-bandit
/// path; `/feedback` (step 4) uses those to call `apply_feedback` and
/// propagate the observed reward across every level of the tree.
fn do_decide_hierarchical(
    state: &State,
    tenant: &str,
    job: &str,
    capsule: &str,
    body: &str,
    learn: bool,
    spec: crate::hierarchical::HierarchicalSpec,
) -> Resp {
    // Read or lazily initialise the per-HierState bandit state. Freshly
    // installed hierarchical capsules don't have a state sidecar yet —
    // we construct an empty one and let `select_path` populate the
    // buckets on the first walk.
    let mut hier_state = state.store
        .load_hierarchical_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| {
            crate::hierarchical_state::HierarchicalCapsuleState::new(spec.clone())
        });

    // Walk the tree. select_path returns one decision per leaf path: the
    // path of indices, the resolved leaf name, and the per-level
    // CandidateId chosen at each step. We pass two independent
    // rand_f64 draws per level so the meta-bandit's
    // (explore-vs-exploit, random-pick) selection is honoured.
    let decision = match hier_state.select_path(|| {
        (crate::learning::rand_f64(), crate::learning::rand_f64())
    }) {
        Some(d) => d,
        None => return json_resp(500, &err_json(
            "hierarchical select_path returned None — spec malformed?",
        )),
    };

    // Persist the updated state (select_path allocates new buckets
    // lazily when it first visits a level).
    if let Err(e) = state.store.save_hierarchical_state_in_job(
        tenant, job, capsule, &hier_state,
    ) {
        error!(tenant = %tenant, job = %job, capsule = %capsule, error = %e,
               "save hierarchical state failed");
    }

    // Generate decisionId in the same format as the flat path.
    let decision_id = format!("dec_{}", sha256_hex(
        format!(
            "{}{}{}{}", tenant, job, capsule,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ).as_bytes(),
    ).get(..16).unwrap_or(""));

    // Decision-event shape mirrors the flat path's `decisions[]` array
    // entry, with hierarchical-specific fields. /feedback (step 4)
    // detects hierarchical decisions by the presence of `path`.
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

    // Response body: the leaf name is what the caller acts on; the path
    // is included for audit / debugging and is what /feedback expects.
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
    // Hierarchical bandits (roadmap.md step 3). When the capsule was
    // installed with a `hierarchical_options` declaration, its
    // `hierarchical_spec.json` sidecar is on disk and we route the
    // entire `/decide` through `do_decide_hierarchical` — bypassing the
    // flat AdaptiveChoice loop, the meta-bandit + LinUCB candidate
    // scoring, the shared-state branch, and the graph executor.
    // Hierarchical capsules pick their leaf option entirely outside
    // the graph in v1; the .lyc carries a single flat AdaptiveChoice
    // node for legacy compat and inspection only.
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

    // Load learning config and memory before execution so context-specific
    // weights can drive the live decision, not just decorate the response.
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
            // 3E: for each TimeSeries feature in the spec, push the raw
            // observed value onto the per-capsule rolling window in memory
            // BEFORE encoding. The encoded vector then draws aggregations
            // from the updated window via `encode_with_windows`.
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

    // OOD scoring: locate the first AdaptiveChoice node and run the
    // score-then-record cycle so the current request is reflected on the
    // very next call. Score 0 if no AdaptiveChoice node exists.
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
    // 5C: per-node candidate selections. The legacy `chosen_candidate` above
    // is kept for backward-compatibility (it remains the FIRST node's
    // candidate, which is what the existing decision_event[0] / feedback
    // path uses). Entries beyond [0] are surfaced in the decision log so
    // multi-decision feedback can target them by decisionIndex.
    let mut per_node_candidates: std::collections::HashMap<u32, crate::meta_bandit::CandidateId>
        = std::collections::HashMap::new();
    // Item 2 (shared-state LinUCB): per-node scored arrays from
    // `SharedStateOptionStrategy::shared_ucb_score`. Populated only for
    // capsules whose `learning.json::sharedState.enabled` is true. The
    // /decide response includes this as `sharedStateScores` on each
    // matching decision entry, so callers can verify generalisation to
    // unseen options at the API boundary.
    let mut shared_state_scored_per_node:
        std::collections::HashMap<u32, Vec<(String, f64)>> =
            std::collections::HashMap::new();

    let bandit_decisions = if in_warmup {
        flatten_strategy_weights(&mut ng);
        std::collections::HashMap::new()
    } else {
        let bd = apply_context_memory_to_graph(&mut ng, &memory, context_key, &learning_cfg);

        if in_active {
            // 5C: iterate every AdaptiveChoice node so each gets its own
            // meta-bandit candidate. The first node's chosen_candidate is
            // recorded in the legacy `chosen_candidate` field for feedback;
            // additional nodes get their candidateId stored in the
            // enriched decision log entry instead.
            let choice_nodes = all_choice_nodes(&ng);
            for (idx, (node_id, weights_len, contract)) in choice_nodes.into_iter().enumerate() {
                let is_first = idx == 0;
                let n_options = if contract == Contract::WithinTolerance && weights_len > 1 {
                    weights_len - 1
                } else {
                    weights_len
                };
                if n_options > 0 {
                    // ── Item 2: shared-state LinUCB short-circuit ────────────
                    // When the capsule's learning config opts into shared
                    // state, replace the entire meta-bandit + per-option
                    // LinUcb scoring chain with a single θ over
                    // [x_context, x_option]. The recorded candidate stays
                    // LinUcb so the existing feedback machinery routes
                    // correctly; the *actual* scoring goes through
                    // SharedStateOptionStrategy.
                    if learning_cfg.shared_state.enabled && feature_vector.is_some() {
                        // The actual encoded context length (including the
                        // bias term `encode_with_windows` appends) is the
                        // authority — the capsule's declared `dContext` is
                        // informational only. Resolving here keeps capsule
                        // authors from having to know about the bias.
                        let encoded_d_context =
                            learning_cfg.context_spec.encoded_dimension();
                        // Lazily initialise the shared state on first use.
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

                        // Score each option in BTreeMap iteration order
                        // (sorted by name — deterministic, matches the
                        // expected `(choice ...)` operand order in
                        // capsule source).
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
                        // Feature-vector path. LinUcb scores deterministically
                        // via UCB; LinTs samples θ̃ from N(μ, v²·A⁻¹).
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
                                        // One-shot Box-Muller: each LinTs decision
                                        // resamples per option from the same
                                        // posterior, so we don't need a multi-draw
                                        // generator. r1/r2 are mixed with i to
                                        // de-correlate across options.
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
                    // global executor selection_mode. Subsequent nodes overlay
                    // their own one-hot in the graph weights (the LinUcb/LinTs
                    // branches above do this directly), so the
                    // executor-level mode doesn't need to track them.
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
    // Build the per-decision `runtime.publish` buffer. We clone the `Rc`
    // into the ExecutionContext (moved into the executor) and keep the
    // original handle here so we can read the published values back after
    // `executor.run()` returns. `ExecutionContext` is moved into the
    // executor and there is no public accessor to retrieve it, so the
    // shared-Rc handoff is the boundary mechanism.
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

    // Snapshot the `runtime.publish` buffer for attachment to each
    // decision entry below. v1 behaviour: every decision entry receives
    // the same `published` map (multi-decision capsules will see redundant
    // copies — per-decision attribution can land in a future round if
    // multi-decision capsules need it).
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

    // 3A: when actionSpace is Continuous, surface the bucket midpoint as
    // `chosenAction` alongside `chosen_option`. Done before per-node
    // metadata pass below so it's available on every decision.
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

    // 5C: attach each AdaptiveChoice node's candidateId to its own
    // enriched_decision entry, keyed by node_id. The legacy
    // `chosen_candidate` (= first node's choice) is still used by the
    // feedback path; entries beyond [0] surface their per-node candidateId
    // so a future feedback caller can target them via decisionIndex.
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
            // Item 2: surface shared-state per-option scores when present.
            // Sorted by score descending so the API reader can scan the
                // top candidates without re-sorting. Lets callers verify the
            // generalisation property for unseen options at the API boundary.
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

    // Attach `runtime.publish` output. v1: every decision entry receives
    // the same `published` map. Per-decision attribution can land in a
    // future round if multi-decision capsules need it.
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

    // Refusal evaluation must happen before the decision log is written so
    // feedback can identify refused decisions later.
    let refusal_cfg = &learning_cfg.refusal;
    let alpha = (1.0 - refusal_cfg.coverage).clamp(0.0, 1.0);
    let interval_width: Option<f64> = first_choice_node_id.and_then(|nid| {
        let sm = memory.strategies.get(&nid)?;
        // Prefer the chosen bucket; fall back to pooling across all candidate
        // buckets for this context when the chosen bucket is under-sampled
        // (meta-bandit fragments residuals across N candidates).
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

    // Refusal only applies once the capsule is Active. During warmup the
    // bandit is supposed to be collecting baseline data — refusing every
    // request would starve the calibrator and create a cold-start deadlock.
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

    // Persist memory so OOD detectors, candidate-context init, and meta-bandit
    // structure survive across decides (record happens here regardless of /feedback).
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
