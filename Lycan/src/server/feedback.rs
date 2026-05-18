use tracing::{error, warn};

use crate::graph::{Contract, NeuralGraph, OpCode};

use super::errors::{Resp, err_json, json_resp};
use super::helpers::audit_event_json;
use super::state::State;

/// Hierarchical-capsule `/feedback` handler. Looks up the decision by
/// `decisionId`, extracts the recorded `path`, calls
/// `HierarchicalCapsuleState::apply_feedback` to propagate the
/// observed reward across every level, and persists the updated
/// state. Also records into warmup-state so `/report` shows
/// progression. Mirrors the flat `/feedback` path's reward-parsing
/// surface (`reward`, `components` + `rewardSpec`, or `outcome`).
fn do_feedback_hierarchical(
    state: &State,
    tenant: &str,
    job: &str,
    capsule: &str,
    body: &str,
) -> Resp {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
    };

    // Same reward-parsing surface as the flat path: scalar `reward`,
    // `components` (+ optional `rewardSpec`), or `outcome` (with the
    // capsule's reward_policy applied).
    let learning_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    let reward = if let Some(r) = json.get("reward").and_then(|v| v.as_f64()) {
        r
    } else if let Some(components) = json.get("components") {
        let inline_spec = json.get("rewardSpec");
        let on_disk_spec = state.store.load_reward_spec_in_job(tenant, job, capsule);
        let spec_ref = inline_spec.or(on_disk_spec.as_ref());
        match spec_ref {
            Some(spec) => crate::learning::compute_reward_from_components(spec, components),
            None => return json_resp(400, &err_json(
                "components provided but no rewardSpec available \
                 (install reward_spec.json or pass inline rewardSpec)"
            )),
        }
    } else if let Some(outcome) = json.get("outcome") {
        if let Some(ref rp) = learning_cfg.reward_policy {
            crate::learning::compute_reward(outcome, rp)
        } else {
            outcome.get("success").and_then(|v| v.as_bool())
                .map(|b| if b { 1.0 } else { -1.0 })
                .unwrap_or(0.0)
        }
    } else {
        return json_resp(400, &err_json("reward, components, or outcome is required"));
    };

    // Update warmup state for /report consistency. Hierarchical
    // capsules don't gate decisions on warmup, but operators reading
    // /report still expect to see lifecycle progression as feedback
    // arrives.
    let mut warmup_state = state.store
        .load_warmup_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| crate::warmup::WarmupState::with_capsule_delta(
            30, learning_cfg.safety.capsule_adwin_delta,
        ));
    let _ = warmup_state.record_feedback(reward);
    if let Err(e) = state.store.save_warmup_state_in_job(tenant, job, capsule, &warmup_state) {
        error!(tenant = %tenant, job = %job, capsule = %capsule, error = %e,
               "warmup save failed (hierarchical)");
    }

    // Look up the decision event by id. Hierarchical decisions carry
    // their recorded `path` in `decisions[0].path`; the flat path's
    // `chosen_option` field is not used here.
    let dec_id = match json.get("decisionId").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return json_resp(400, &err_json(
            "decisionId is required for hierarchical feedback"
        )),
    };
    let event_line = match state.store.find_decision_in_job(tenant, job, capsule, &dec_id) {
        Ok(Some(line)) => line,
        Ok(None) => return json_resp(404, &err_json(&format!("decisionId not found: {dec_id}"))),
        Err(e) => return json_resp(500, &err_json(&e)),
    };
    let ev: serde_json::Value = match serde_json::from_str(&event_line) {
        Ok(v) => v,
        Err(e) => return json_resp(500, &err_json(&format!("cannot parse decision event: {e}"))),
    };
    let dec_entry = ev.get("decisions")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let path: Vec<usize> = match dec_entry.get("path").and_then(|p| p.as_array()) {
        Some(arr) => arr.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect(),
        None => return json_resp(400, &err_json(
            "decision event has no `path` — expected a hierarchical decision"
        )),
    };
    if path.is_empty() {
        return json_resp(400, &err_json("decision event's `path` is empty"));
    }

    // Recover the per-level candidate ids from the decision event so
    // `apply_feedback` can credit the candidate that actually fired at
    // decide time rather than fall back to the greedy-leader proxy.
    // A length-mismatch (e.g. the spec drifted between decide and
    // feedback) is treated as missing data — the inner path falls
    // back to the proxy automatically.
    let per_level_candidates: Vec<crate::meta_bandit::CandidateId> = dec_entry
        .get("perLevelCandidateIds")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter()
            .filter_map(|v| v.as_str())
            .filter_map(crate::meta_bandit::CandidateId::from_str)
            .collect())
        .unwrap_or_default();

    // Load the hierarchical state, apply feedback along the path,
    // persist. path == chosen_per_level for v1 (the path *is* the
    // sequence of chosen indices at each level).
    let mut hier_state = match state.store.load_hierarchical_state_in_job(tenant, job, capsule) {
        Some(s) => s,
        None => return json_resp(500, &err_json(
            "no hierarchical_state on disk — /decide must run before /feedback"
        )),
    };
    let per_level_updates = hier_state.apply_feedback_with_candidates(
        &path, &path, &per_level_candidates, reward,
    );
    if per_level_updates.is_empty() {
        return json_resp(400, &err_json(&format!(
            "apply_feedback returned no updates — path {path:?} is invalid for the installed spec"
        )));
    }
    if let Err(e) = state.store.save_hierarchical_state_in_job(tenant, job, capsule, &hier_state) {
        error!(tenant = %tenant, job = %job, capsule = %capsule, error = %e,
               "save hierarchical state failed (feedback)");
        return json_resp(500, &err_json(&e));
    }

    // Audit + feedback log entries so the trail mirrors the flat path.
    state.store.append_audit_in_job(tenant, job, capsule,
        &audit_event_json("feedback", tenant, job, capsule, serde_json::json!({
            "kind": "hierarchical",
            "decisionId": dec_id,
            "path": path,
            "reward": reward,
            "levelsUpdated": per_level_updates.len(),
        }))).ok();
    state.store.append_feedback_log_in_job(tenant, job, capsule, &serde_json::json!({
        "tenant": tenant, "job": job, "capsule": capsule,
        "kind": "hierarchical",
        "decisionId": dec_id,
        "path": path,
        "reward": reward,
    }).to_string()).ok();

    state.metrics.record_request("feedback", tenant, job, capsule, "ok");
    json_resp(200, &serde_json::json!({
        "ok": true,
        "kind": "hierarchical",
        "decisionId": dec_id,
        "path": path,
        "reward": reward,
        "levelsUpdated": per_level_updates.len(),
    }).to_string())
}
pub(super) fn do_feedback(state: &State, tenant: &str, job: &str, capsule: &str, body: &str) -> Resp {
    // Hierarchical bandits (roadmap.md step 4). Same early-dispatch
    // pattern as `do_decide`: when the capsule has a
    // `hierarchical_spec.json` sidecar, route to the hierarchical
    // feedback helper. This bypasses the flat-AdaptiveChoice feedback
    // path entirely — `apply_feedback` propagates the observed reward
    // across every level of the recorded decision path.
    if state.store.load_hierarchical_spec_in_job(tenant, job, capsule).is_some() {
        return do_feedback_hierarchical(state, tenant, job, capsule, body);
    }

    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
    };

    let explicit_context_key = json.get("contextKey").and_then(|v| v.as_str()).map(|s| s.to_string());
    let mut context_key = explicit_context_key.clone().unwrap_or_else(|| "default".to_string());

    let learning_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    // 2C: when feedback comes with `components: {name: value}`, keep the
    // per-component map alongside the combined scalar so the bandit can
    // record per-objective Q estimates (used by Pareto selection and by
    // operators inspecting /memory).
    let mut component_values: Option<std::collections::HashMap<String, f64>> = None;
    let reward = if let Some(r) = json.get("reward").and_then(|v| v.as_f64()) {
        r
    } else if let Some(components) = json.get("components") {
        let inline_spec = json.get("rewardSpec");
        let on_disk_spec = state.store.load_reward_spec_in_job(tenant, job, capsule);
        let spec_ref = inline_spec.or(on_disk_spec.as_ref());
        let combined = match spec_ref {
            Some(spec) => crate::learning::compute_reward_from_components(spec, components),
            None => return json_resp(400, &err_json(
                "components provided but no rewardSpec available (install reward_spec.json or pass inline rewardSpec)"
            )),
        };
        // Extract per-component scalar values so apply_feedback_multi can
        // record them into the bucket's objective_rewards/counts maps.
        if let Some(obj) = components.as_object() {
            let mut map = std::collections::HashMap::new();
            for (k, v) in obj {
                if let Some(f) = v.as_f64() {
                    map.insert(k.clone(), f);
                }
            }
            if !map.is_empty() {
                component_values = Some(map);
            }
        }
        combined
    } else if let Some(outcome) = json.get("outcome") {
        if let Some(ref rp) = learning_cfg.reward_policy {
            crate::learning::compute_reward(outcome, rp)
        } else {
            outcome.get("success").and_then(|v| v.as_bool())
                .map(|b| if b { 1.0 } else { -1.0 })
                .unwrap_or(0.0)
        }
    } else {
        return json_resp(400, &err_json("reward, components, or outcome is required"));
    };

    let mut warmup_state = state.store
        .load_warmup_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| crate::warmup::WarmupState::with_capsule_delta(
            30, learning_cfg.safety.capsule_adwin_delta,
        ));
    let warmup_outcome = warmup_state.record_feedback(reward);
    if let Err(e) = state.store.save_warmup_state_in_job(tenant, job, capsule, &warmup_state) {
        error!(tenant = %tenant, job = %job, capsule = %capsule, error = %e, "warmup save failed");
    }
    let warmup_transitioned = matches!(warmup_outcome, crate::warmup::FeedbackOutcome::WarmupComplete { .. });
    let change_detected = matches!(warmup_outcome, crate::warmup::FeedbackOutcome::ChangeDetected { .. });
    match &warmup_outcome {
        crate::warmup::FeedbackOutcome::WarmupComplete { algorithm, characterization } => {
            state.store.append_audit_in_job(tenant, job, capsule,
                &audit_event_json("warmup_complete", tenant, job, capsule, serde_json::json!({
                    "event": "warmup_complete",
                    "algorithm": format!("{algorithm:?}"),
                    "characterization": format!("{characterization:?}"),
                }))).ok();
        }
        crate::warmup::FeedbackOutcome::ChangeDetected { change, previous_algorithm } => {
            state.store.append_audit_in_job(tenant, job, capsule,
                &audit_event_json("change_detected", tenant, job, capsule, serde_json::json!({
                    "event": "change_detected",
                    "previousAlgorithm": format!("{previous_algorithm:?}"),
                    "dropped": change.dropped,
                    "oldMean": (change.old_mean * 10000.0).round() / 10000.0,
                    "newMean": (change.new_mean * 10000.0).round() / 10000.0,
                    "note": "reverted to warmup",
                }))).ok();
        }
        _ => {}
    }

    let mut chosen_candidate: Option<crate::meta_bandit::CandidateId> = None;
    let mut feature_vector: Option<Vec<f64>> = None;

    // Support two modes: explicit strategyId+option, or decisionId lookup
    let (node_id, option) = if let Some(dec_id) = json.get("decisionId").and_then(|v| v.as_str()) {
        // Look up the decision event to find strategyId and selected option
        match state.store.find_decision_in_job(tenant, job, capsule, dec_id) {
            Ok(Some(event_line)) => {
                // Parse the decision event to extract strategy info
                match serde_json::from_str::<serde_json::Value>(&event_line) {
                    Ok(ev) => {
                        if explicit_context_key.is_none() {
                            if let Some(ev_context) = ev.get("contextKey").and_then(|v| v.as_str()) {
                                context_key = ev_context.to_string();
                            }
                        }
                        let was_refused = ev.get("refused")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if was_refused {
                            state.store.append_audit_in_job(tenant, job, capsule,
                                &audit_event_json("feedback_on_refused", tenant, job, capsule, serde_json::json!({
                                    "decisionId": dec_id,
                                    "reward": reward,
                                }))).ok();
                            return json_resp(200, &serde_json::json!({
                                "ok": true,
                                "noted": "feedback recorded against refused decision; bandit state unchanged",
                            }).to_string());
                        }
                        let decisions = ev.get("decisions").and_then(|d| d.as_array());
                        // 5C: feedback may target a specific decision in a
                        // multi-AdaptiveChoice graph via `decisionIndex`
                        // (default 0). Out-of-range falls back to 0 so
                        // existing single-decision callers stay unaffected.
                        let dec_idx = json.get("decisionIndex")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(0);
                        let chosen = decisions.and_then(|d| d.get(dec_idx).or_else(|| d.first()));
                        match chosen {
                            Some(dec) => {
                                let nid = dec.get("node_id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                                let opt = dec.get("chosen_option").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                chosen_candidate = dec.get("candidateId")
                                    .and_then(|v| v.as_str())
                                    .and_then(crate::meta_bandit::CandidateId::from_str);
                                feature_vector = dec.get("featureVector")
                                    .and_then(|v| serde_json::from_value::<Vec<f64>>(v.clone()).ok());
                                (nid, opt)
                            }
                            None => return json_resp(400, &err_json("decision event has no strategy decisions")),
                        }
                    }
                    Err(_) => return json_resp(500, &err_json("cannot parse decision event")),
                }
            }
            Ok(None) => return json_resp(404, &err_json(&format!("decisionId not found: {dec_id}"))),
            Err(e) => return json_resp(500, &err_json(&e)),
        }
    } else {
        // Explicit mode
        let nid = match json.get("strategyId").or(json.get("nodeId")).and_then(|v| v.as_u64()) {
            Some(id) => id as u32,
            None => return json_resp(400, &err_json("strategyId/nodeId or decisionId is required")),
        };
        let opt = match json.get("option").and_then(|v| v.as_u64()) {
            Some(o) => o as usize,
            None => return json_resp(400, &err_json("option is required when using strategyId")),
        };
        (nid, opt)
    };

    let skip_weight_mutation = chosen_candidate.is_some();

    let data = match state.store.load_graph_in_job(tenant, job, capsule) {
        Ok(d) => d,
        Err(e) => return json_resp(404, &err_json(&e)),
    };

    let mut ng = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => return json_resp(500, &err_json(&e)),
    };

    let node = match ng.nodes.get(node_id as usize) {
        Some(n) if matches!(n.op, OpCode::Strategy | OpCode::AdaptiveChoice) => n,
        _ => return json_resp(400, &err_json(&format!("node {node_id} is not a strategy node"))),
    };

    let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
        node.weights.len() - 1
    } else {
        node.weights.len()
    };

    if option >= n_options {
        return json_resp(400, &err_json(&format!("option {option} out of range ({n_options} options)")));
    }

    let before: Vec<f64> = ng.nodes[node_id as usize].weights[..n_options].to_vec();

    // Clip raw reward at the graph layer so the persisted graph weights respect
    // the same bound the memory sidecar uses. Configurable per-capsule.
    let clipped_reward = if learning_cfg.safety.reward_clip > 0.0 {
        reward.clamp(-learning_cfg.safety.reward_clip, learning_cfg.safety.reward_clip)
    } else {
        reward
    };
    let learning_rate = learning_cfg.learning_rate.clamp(0.0001, 0.5);
    let raw_delta = clipped_reward * learning_rate;
    let max_delta = learning_cfg.safety.max_weight_delta_per_feedback;
    let delta = raw_delta.clamp(-max_delta, max_delta);
    if !skip_weight_mutation {
        for j in 0..n_options {
            if j == option {
                ng.nodes[node_id as usize].weights[j] =
                    (ng.nodes[node_id as usize].weights[j] + delta).clamp(0.01, 0.99);
            } else if n_options > 1 {
                ng.nodes[node_id as usize].weights[j] =
                    (ng.nodes[node_id as usize].weights[j] - delta / (n_options - 1) as f64).clamp(0.01, 0.99);
            }
        }
        // Min-exploration floor at the graph layer so weights never collapse below
        // the configured exploration budget — matches sidecar behavior.
        let min_w = learning_cfg.safety.min_exploration / n_options as f64;
        for j in 0..n_options {
            if ng.nodes[node_id as usize].weights[j] < min_w {
                ng.nodes[node_id as usize].weights[j] = min_w;
            }
        }
        let sum: f64 = ng.nodes[node_id as usize].weights[..n_options].iter().sum();
        if sum > 0.0 {
            for j in 0..n_options { ng.nodes[node_id as usize].weights[j] /= sum; }
        }
    }

    if let Some(slot) = ng.nodes[node_id as usize].state_slot {
        let base = slot as usize + option * 3;
        if base + 2 < ng.state.len() {
            ng.state[base] += 1.0;
            if reward > 0.0 { ng.state[base + 2] += 1.0; }
        }
    }

    if learning_cfg.safety.journal_on_feedback {
        ng.journal.push(crate::graph::JournalEntry {
            run_number: ng.nodes.get(ng.entry as usize).map(|n| n.activation_count).unwrap_or(0),
            node_id,
            mutation: crate::graph::MutationKind::FeedbackReceived,
            reason: u32::MAX,
        });
    }

    let after: Vec<f64> = ng.nodes[node_id as usize].weights[..n_options].to_vec();
    let updated = ng.to_bytes();
    if learning_cfg.safety.snapshot_on_feedback {
        state.store.snapshot_in_job(tenant, job, capsule).ok();
    }
    state.store.save_graph_in_job(tenant, job, capsule, &updated).ok();

    state.store.append_audit_in_job(tenant, job, capsule,
        &audit_event_json("feedback", tenant, job, capsule, serde_json::json!({
            "nodeId": node_id, "option": option, "reward": reward,
            "components": component_values,
        }))).ok();
    state.store.append_feedback_log_in_job(tenant, job, capsule,
        &serde_json::json!({
            "tenant": tenant,
            "job": job,
            "capsule": capsule,
            "nodeId": node_id,
            "option": option,
            "reward": reward,
        }).to_string()).ok();

    let mut memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();

    if matches!(warmup_outcome, crate::warmup::FeedbackOutcome::ChangeDetected { .. }) {
        memory.reset_meta_bandit(node_id);
        memory.reset_candidate_contexts(node_id, &context_key);
        // The feedback that triggered the change is the last observation of the
        // *old* regime; routing it through the legacy bucket keeps the freshly
        // reset meta-bandit and candidate state at zero.
        chosen_candidate = None;
    }

    let graph_weights = after.clone();
    let signal_kind = json.get("signalKind").and_then(|v| v.as_str());
    let candidates_list_fb: Vec<crate::meta_bandit::CandidateId> =
        match &learning_cfg.context_spec {
            crate::feature_schema::ContextSpec::Discrete => {
                crate::meta_bandit::CandidateId::discrete_only().to_vec()
            }
            crate::feature_schema::ContextSpec::Features { .. } => {
                crate::meta_bandit::CandidateId::all().to_vec()
            }
        };
    let result = if let Some(candidate) = chosen_candidate {
        let is_lin_candidate = matches!(candidate,
            crate::meta_bandit::CandidateId::LinUcb
            | crate::meta_bandit::CandidateId::LinTs);
        if is_lin_candidate {
            // Item 2: shared-state LinUCB feedback path. When the capsule
            // opts in, route reward updates through `apply_feedback` on
            // the shared θ instead of the per-option LinUcb state. The
            // chosen-option NAME is recovered from the BTreeMap iteration
            // order of `option_features` (sorted by name), which is the
            // same order the decide path used to assign graph indices.
            if learning_cfg.shared_state.enabled {
                let option_names: Vec<String> = learning_cfg.shared_state
                    .option_features.keys().cloned().collect();
                let chosen_name = option_names.get(option).cloned();
                let r: Result<(), String> = match (chosen_name, feature_vector.as_ref()) {
                    (Some(name), Some(x)) => {
                        // Lazily initialise — feedback can land before a decide
                        // touched the strategy (e.g. installed-then-fed
                        // recovered state). Derive d_context from the encoded
                        // dimension so the bias term is accounted for, same
                        // as the decide path does.
                        if memory.shared_state.is_none() {
                            let encoded_d_context =
                                learning_cfg.context_spec.encoded_dimension();
                            let mut strat =
                                crate::shared_state_strategy::SharedStateOptionStrategy::new(
                                    encoded_d_context,
                                    learning_cfg.shared_state.d_option,
                                    learning_cfg.shared_state.lambda,
                                );
                            for (n, feats) in &learning_cfg.shared_state.option_features {
                                let _ = strat.register_option(n, feats.clone());
                            }
                            memory.shared_state = Some(strat);
                        }
                        memory.shared_state.as_mut().unwrap()
                            .apply_feedback(&name, x, reward)
                    }
                    (None, _) => Err(format!(
                        "shared-state feedback: option index {} out of range \
                         ({} option_features)",
                        option, learning_cfg.shared_state.option_features.len()
                    )),
                    (Some(_), None) => Err(
                        "shared-state feedback missing feature vector in decision log"
                            .to_string()
                    ),
                };
                let mb = memory.get_or_init_meta_bandit(node_id, n_options, &candidates_list_fb);
                mb.record(candidate, reward);
                r
            } else {
            // Both LinUcb and LinTs use the same underlying state update
            // (Sherman-Morrison on A, b += reward·x). Only the score
            // function at decide time differs.
            let d = learning_cfg.context_spec.encoded_dimension();
            let bucket = memory.get_or_init_candidate_context(
                node_id, &context_key, candidate, &graph_weights, n_options,
            );
            crate::learning::ensure_linucb_states(bucket, d, 1.0);
            let mut r: Result<(), String> = Ok(());
            if let Some(ref x) = feature_vector {
                let mut predicted: Option<f64> = None;
                if let Some(state) = bucket.option_states.get_mut(option) {
                    if let crate::learning::OptionState::LinUcb { state: ls } = state {
                        let theta = ls.theta();
                        predicted = Some(crate::linucb::dot(x, &theta));
                        ls.update(x, reward);
                        if ls.rebuild_due(1000) {
                            ls.rebuild_inverse();
                        }
                    }
                }
                if let Some(p) = predicted {
                    bucket.conformity_calibrator.record(p, reward);
                }
            } else {
                r = Err(format!(
                    "{} feedback missing feature vector in decision log",
                    candidate.as_str()
                ));
            }
            let mb = memory.get_or_init_meta_bandit(node_id, n_options, &candidates_list_fb);
            mb.record(candidate, reward);
            r
            }
        } else {
            let bucket = memory.get_or_init_candidate_context(
                node_id, &context_key, candidate, &graph_weights, n_options,
            );
            let r = if let Some(sk) = signal_kind {
                if learning_cfg.delayed_feedback.enabled {
                    crate::learning::apply_feedback_signal(bucket, option, reward, sk, &learning_cfg)
                } else {
                    crate::learning::apply_feedback(bucket, option, reward, &learning_cfg)
                }
            } else {
                crate::learning::apply_feedback(bucket, option, reward, &learning_cfg)
            };
            let mb = memory.get_or_init_meta_bandit(node_id, n_options, &candidates_list_fb);
            mb.record(candidate, reward);
            r
        }
    } else {
        let bucket = memory.get_or_init_context(node_id, &context_key, &graph_weights, n_options);
        if let Some(sk) = signal_kind {
            if learning_cfg.delayed_feedback.enabled {
                crate::learning::apply_feedback_signal(bucket, option, reward, sk, &learning_cfg)
            } else {
                crate::learning::apply_feedback(bucket, option, reward, &learning_cfg)
            }
        } else {
            crate::learning::apply_feedback(bucket, option, reward, &learning_cfg)
        }
    };
    if let Err(e) = result {
        warn!(tenant = %tenant, job = %job, capsule = %capsule, error = %e, "learning feedback returned warning");
    }

    // 2C: also record per-component values into the bucket's
    // objective_rewards / objective_counts so the bandit learns a per-
    // component Q estimate. The scalar `reward` already drove the main
    // option-state update via apply_feedback above; this side-channel
    // makes the per-component history available for Pareto selection
    // and audit/diagnostic inspection via /memory.
    if let Some(comps) = component_values.as_ref() {
        let bucket = if let Some(_candidate) = chosen_candidate {
            // The candidate-context bucket got the main update; mirror the
            // per-component values there too.
            memory.get_or_init_candidate_context(
                node_id, &context_key, _candidate, &graph_weights, n_options,
            )
        } else {
            memory.get_or_init_context(node_id, &context_key, &graph_weights, n_options)
        };
        if let Some(s) = bucket.stats.get_mut(option) {
            for (name, val) in comps {
                s.record_objective(name, *val);
            }
        }
    }

    // Per-(node_id, context_key) ADWIN. Independent from the capsule-level
    // detector that ran inside warmup_state.record_feedback above. We pass
    // the per-context delta from SafetyConfig so operators can tune the
    // two-layer ordering without recompiling.
    let context_change_detected = {
        let detector = memory.get_or_init_context_detector_with_delta(
            node_id, &context_key, learning_cfg.safety.context_adwin_delta,
        );
        detector.add(reward)
    };
    let context_change_detected_flag = context_change_detected.is_some();
    if let Some(ref change) = context_change_detected {
        memory.reset_candidate_contexts(node_id, &context_key);
        memory.reset_context_detector(node_id, &context_key);
        state.store.append_audit_in_job(tenant, job, capsule,
            &audit_event_json("context_change_detected", tenant, job, capsule, serde_json::json!({
                "event": "context_change_detected",
                "nodeId": node_id,
                "contextKey": context_key,
                "dropped": change.dropped,
                "oldMean": (change.old_mean * 10000.0).round() / 10000.0,
                "newMean": (change.new_mean * 10000.0).round() / 10000.0,
                "note": "reset only this context's candidate state; capsule stays in current lifecycle",
            }))).ok();
    }

    state.store.save_memory_in_job(tenant, job, capsule, &memory).ok();

    let warmup_lifecycle = match &warmup_state.lifecycle {
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

    json_resp(200, &serde_json::json!({
        "ok": true,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "nodeId": node_id,
        "option": option,
        "reward": reward,
        "before": before,
        "after": after,
        "contextKey": context_key,
        "warmupTransitioned": warmup_transitioned,
        "changeDetected": change_detected,
        "contextChangeDetected": context_change_detected_flag,
        "warmup": warmup_lifecycle,
    }).to_string())
}
