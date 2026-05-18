use tracing::error;

use crate::graph::{Contract, GraphNode, NeuralGraph, OpCode, Operand};
use crate::learning::CapsuleMemory;
use crate::store::sha256_hex;
use crate::warmup::WarmupState;

use super::errors::{Resp, err_json, json_resp};
use super::helpers::audit_event_json;
use super::state::State;

/// Resolve the bandit overlay for one strategy/choice node, if any. Returns
/// `(weights, context_key, candidate_id_str)` for the meta-bandit leader's
/// most-recently-updated candidate-context bucket. Returns `None` when the
/// capsule is not in Active state, no meta-bandit exists for this node,
/// the meta-bandit has no leader yet, or no candidate-context bucket
/// matches the leader.
///
/// Factored out of both `do_report` and `inspect_graph_json` so the two
/// endpoints can't drift apart and the overlay decision is unit-testable
/// without standing up a `LycanStore` + `State`.
pub(super) fn bandit_overlay_for_node(
    warmup_state: &WarmupState,
    memory: &CapsuleMemory,
    node: &GraphNode,
) -> Option<(Vec<f64>, String, &'static str)> {
    if !warmup_state.is_active() { return None; }
    if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { return None; }
    let sm = memory.strategies.get(&node.id)?;
    let leader = sm.meta_bandit.as_ref()?.current_leader()?;
    let (ctx_key, bucket) = sm.candidate_contexts.iter()
        .filter(|((cid, _), _)| *cid == leader)
        .max_by_key(|(_, b)| b.updated_at)
        .map(|((_, k), b)| (k.clone(), b))?;
    Some((bucket.weights.clone(), ctx_key, leader.as_str()))
}

/// Number of usable options for a strategy/choice node, accounting for
/// the `WithinTolerance` contract which reserves the last weight slot.
fn n_options_for(node: &GraphNode) -> usize {
    if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
        node.weights.len() - 1
    } else {
        node.weights.len()
    }
}

/// Build one `/report` strategy entry. Pure — no I/O.
///
/// `liveSource` and `graphWeights` are always present:
/// - `liveSource` is null when the overlay is absent, in which case
///   `weights == graphWeights`.
/// - `liveSource` carries the leader's `CandidateId::as_str()` when the
///   overlay is present, in which case `weights` is the overlay and
///   `graphWeights` reflects the on-graph (likely frozen-uniform)
///   committed values.
pub(super) fn build_strategy_report_entry(
    node: &GraphNode,
    ng: &NeuralGraph,
    overlay: Option<&(Vec<f64>, String, &'static str)>,
) -> serde_json::Value {
    let n_options = n_options_for(node);
    let weights_for_report: Vec<f64> = match overlay {
        Some((w, _, _)) => w.clone(),
        None => node.weights.iter().take(n_options).copied().collect(),
    };
    let graph_weights_rounded: Vec<f64> = node.weights.iter().take(n_options)
        .map(|w| (w * 10000.0).round() / 10000.0)
        .collect();

    let mut options = Vec::new();
    for i in 0..n_options {
        let (tries, total_ns, correct) = if let Some(slot) = node.state_slot {
            let base = slot as usize + i * 3;
            if base + 2 < ng.state.len() {
                (ng.state[base] as u64, ng.state[base + 1], ng.state[base + 2] as u64)
            } else { (0, 0.0, 0) }
        } else { (0, 0.0, 0) };
        let avg_ms = if tries > 0 { (total_ns / tries as f64) / 1_000_000.0 } else { 0.0 };
        options.push(serde_json::json!({
            "option": i,
            "tries": tries,
            "correct": correct,
            "avg_ms": (avg_ms * 1000.0).round() / 1000.0,
            "weight": (weights_for_report.get(i).copied().unwrap_or(0.0) * 10000.0).round() / 10000.0,
            "graphWeight": graph_weights_rounded.get(i).copied().unwrap_or(0.0),
        }));
    }
    let live_source: serde_json::Value = match overlay {
        Some((_, _, leader_id)) => serde_json::json!(leader_id),
        None => serde_json::Value::Null,
    };
    let mut strat = serde_json::json!({
        "node_id": node.id,
        "activations": node.activation_count,
        "n_options": n_options,
        "options": options,
        "graphWeights": graph_weights_rounded,
        "liveSource": live_source,
    });
    if let Some((_, ctx_key, leader_id)) = overlay {
        if let Some(obj) = strat.as_object_mut() {
            obj.insert("weightsSource".into(),
                       serde_json::json!("meta_bandit_leader"));
            obj.insert("leaderCandidate".into(),
                       serde_json::json!(leader_id));
            obj.insert("contextKey".into(), serde_json::json!(ctx_key));
        }
    }
    strat
}

/// Build one `/inspect` node entry. Pure — no I/O.
///
/// Same `liveSource` / `graphWeights` semantics as the /report entry.
pub(super) fn build_inspect_node_entry(
    node: &GraphNode,
    overlay: Option<&(Vec<f64>, String, &'static str)>,
) -> serde_json::Value {
    let n_options = n_options_for(node);
    let operand_refs: Vec<u32> = node.operands.iter().filter_map(|operand| {
        match operand {
            Operand::NodeRef(id) => Some(*id),
            _ => None,
        }
    }).collect();
    let weights_out: Vec<f64> = match overlay {
        Some((w, _, _)) => w.iter().take(n_options)
            .map(|w| (w * 10000.0).round() / 10000.0).collect(),
        None => node.weights.iter().take(n_options)
            .map(|w| (w * 10000.0).round() / 10000.0).collect(),
    };
    let graph_weights_out: Vec<f64> = node.weights.iter().take(n_options)
        .map(|w| (w * 10000.0).round() / 10000.0).collect();
    let live_source: serde_json::Value = match overlay {
        Some((_, _, leader_id)) => serde_json::json!(leader_id),
        None => serde_json::Value::Null,
    };
    let mut node_json = serde_json::json!({
        "id": node.id,
        "op": format!("{:?}", node.op),
        "weightKind": format!("{:?}", node.weight_kind),
        "contract": format!("{:?}", node.contract),
        "objective": format!("{:?}", node.objective),
        "activationCount": node.activation_count,
        "operandCount": node.operands.len(),
        "operandRefs": operand_refs,
        "weights": weights_out,
        "graphWeights": graph_weights_out,
        "liveSource": live_source,
        "stateSlot": node.state_slot,
    });
    if let Some((_, ctx_key, leader_id)) = overlay {
        if let Some(obj) = node_json.as_object_mut() {
            obj.insert("weightsSource".into(),
                       serde_json::json!("meta_bandit_leader"));
            obj.insert("leaderCandidate".into(),
                       serde_json::json!(leader_id));
            obj.insert("contextKey".into(), serde_json::json!(ctx_key));
        }
    }
    node_json
}

pub(super) fn do_chaos(state: &State, tenant: &str, job: &str, capsule: &str) -> Resp {
    let learning_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    let memory = match state.store.load_memory_in_job(tenant, job, capsule) {
        Ok(m) => m,
        Err(_) => crate::learning::CapsuleMemory::default(),
    };

    let mut per_strategy = Vec::new();
    let mut global_change_points: u32 = 0;
    let mut global_active_boosts: u32 = 0;
    let mut global_max_set_width: usize = 0;
    let mut global_max_entropy: f64 = 0.0;
    let mut global_max_posterior_var: f64 = 0.0;
    let mut total_buckets: u32 = 0;

    for (nid, sm) in &memory.strategies {
        for (ctx_key, bucket) in &sm.contexts {
            total_buckets += 1;
            let n = bucket.weights.len();
            if n == 0 { continue; }

            // Weight entropy (high entropy = uncertain/exploring; low = concentrated).
            let entropy: f64 = bucket.weights.iter()
                .filter(|w| **w > 1e-9)
                .map(|w| -w * w.ln())
                .sum::<f64>() / (n as f64).ln().max(1.0);

            let change_points: u32 = bucket.stats.iter().map(|s| s.change_points).sum();
            let active_boosts: u32 = bucket.stats.iter()
                .filter(|s| s.change_boost_remaining > 0).count() as u32;
            let max_posterior_var: f64 = bucket.stats.iter()
                .map(|s| s.posterior_var).fold(0.0_f64, f64::max);

            let pset = crate::learning::compute_prediction_set(bucket, &learning_cfg, n);

            global_change_points += change_points;
            global_active_boosts += active_boosts;
            global_max_set_width = global_max_set_width.max(pset.len());
            global_max_entropy = global_max_entropy.max(entropy);
            global_max_posterior_var = global_max_posterior_var.max(max_posterior_var);

            per_strategy.push(serde_json::json!({
                "nodeId": nid,
                "contextKey": ctx_key,
                "weightEntropy": (entropy * 10000.0).round() / 10000.0,
                "changePoints": change_points,
                "activeExplorationBoosts": active_boosts,
                "maxPosteriorVar": (max_posterior_var * 10000.0).round() / 10000.0,
                "predictionSetWidth": pset.len(),
                "nOptions": n,
            }));
        }
    }

    // Composite chaos score in [0, 1]: 1 = highly chaotic. Each component is a
    // bounded indicator that something is unstable; we average them.
    let composite = if total_buckets == 0 {
        0.0
    } else {
        let entropy_term = global_max_entropy.clamp(0.0, 1.0);
        let boost_term = if global_active_boosts > 0 { 1.0 } else { 0.0 };
        let change_term = (global_change_points as f64 / 10.0).min(1.0);
        let post_term = (global_max_posterior_var / 0.5).min(1.0);
        let width_term = if global_max_set_width > 1 {
            ((global_max_set_width - 1) as f64 / 4.0).min(1.0)
        } else { 0.0 };
        (entropy_term + boost_term + change_term + post_term + width_term) / 5.0
    };

    json_resp(200, &serde_json::json!({
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "compositeChaosScore": (composite * 10000.0).round() / 10000.0,
        "components": {
            "maxWeightEntropy": (global_max_entropy * 10000.0).round() / 10000.0,
            "totalChangePoints": global_change_points,
            "activeExplorationBoosts": global_active_boosts,
            "maxPosteriorVar": (global_max_posterior_var * 10000.0).round() / 10000.0,
            "maxPredictionSetWidth": global_max_set_width,
            "totalContextBuckets": total_buckets,
        },
        "perStrategy": per_strategy,
    }).to_string())
}

pub(super) fn do_evaluate(state: &State, tenant: &str, job: &str, capsule: &str, body: &str) -> Resp {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
    };
    let alt_cfg = match json.get("learningConfig") {
        Some(c) => crate::learning::LearningConfig::from_json(c),
        None => return json_resp(400, &err_json("learningConfig is required")),
    };
    let current_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    let memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();

    // Replay the existing memory through both configs and compare scoring.
    // This is a one-shot surrogate-index OPE: we don't re-run decisions,
    // we just re-score the existing belief state under the alternate config
    // and compare top-of-distribution rankings.
    let mut alt_top: u64 = 0;
    let mut cur_top: u64 = 0;
    let mut agreements: u64 = 0;
    let mut buckets: u64 = 0;

    for (_nid, sm) in &memory.strategies {
        for (_ctx, bucket) in &sm.contexts {
            let n = bucket.weights.len();
            if n < 2 { continue; }
            buckets += 1;
            let (cur_choice, _) = crate::learning::select_option(bucket, &current_cfg, n);
            let (alt_choice, _) = crate::learning::select_option(bucket, &alt_cfg, n);
            cur_top += cur_choice as u64;
            alt_top += alt_choice as u64;
            if cur_choice == alt_choice { agreements += 1; }
        }
    }

    let agreement_rate = if buckets == 0 { 0.0 } else { agreements as f64 / buckets as f64 };

    json_resp(200, &serde_json::json!({
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "evaluatedBuckets": buckets,
        "agreementRate": (agreement_rate * 10000.0).round() / 10000.0,
        "currentChoiceSum": cur_top,
        "alternateChoiceSum": alt_top,
        "currentConfig": current_cfg.to_json(),
        "alternateConfig": alt_cfg.to_json(),
        "note": "agreementRate = fraction of context buckets where the alternate config picks the same option as current. Higher = configs converge; lower = configs diverge meaningfully.",
    }).to_string())
}

pub(super) fn do_evolve(state: &State, tenant: &str, job: &str, capsule: &str, body: &str) -> Resp {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
    };

    if json.get("agentCommand").is_some() || json.get("agent_command").is_some() {
        return json_resp(400, &err_json("agent-command is not allowed over HTTP — use CLI for agent mode"));
    }

    let proposal = match json.get("proposal") {
        Some(p) => p.to_string(),
        None => return json_resp(400, &err_json("proposal field required for server evolution")),
    };
    let dry_run = json.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(false);
    let min_improvement = json.get("minImprovement").and_then(|v| v.as_f64()).unwrap_or(0.05);

    let graph_path = match state.store.graph_path_in_job(tenant, job, capsule) {
        Ok(p) => p,
        Err(e) => return json_resp(404, &err_json(&e)),
    };

    if !graph_path.exists() {
        return json_resp(404, &err_json("capsule not found"));
    }

    let graph_path_str = graph_path.to_string_lossy().to_string();
    let tmp_proposal = format!("/tmp/lycan_evolve_server_{}_{:?}.json",
        std::process::id(), std::thread::current().id());
    if std::fs::write(&tmp_proposal, &proposal).is_err() {
        return json_resp(500, &err_json("cannot write temp proposal"));
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

    let config = crate::evolution_loop::EvolutionConfig {
        iterations: 1,
        budget_ms: 30000,
        min_improvement,
        dry_run,
        agent_command: None,
        proposal_path: Some(tmp_proposal.clone()),
        json_output: false,
        policy,
    };

    let result = crate::evolution_loop::run_evolution(&graph_path_str, &config);
    let _ = std::fs::remove_file(&tmp_proposal);

    match result {
        Ok(r) => {
            let outcomes: Vec<serde_json::Value> = r.outcomes.iter().map(|o| serde_json::json!({
                "accepted": o.accepted,
                "reason": o.reason,
                "proposal": o.proposal_name,
                "target": o.target_strategy,
                "beforeHash": o.before_hash,
            })).collect();

            state.store.append_audit_in_job(tenant, job, capsule,
                &audit_event_json("evolve", tenant, job, capsule, serde_json::json!({
                    "accepted": r.proposals_accepted,
                    "rejected": r.proposals_rejected,
                    "dryRun": dry_run,
                }))).ok();

            json_resp(200, &serde_json::json!({
                "ok": true,
                "tenant": tenant,
                "job": job,
                "capsule": capsule,
                "accepted": r.proposals_accepted,
                "rejected": r.proposals_rejected,
                "outcomes": outcomes,
            }).to_string())
        }
        Err(e) => json_resp(500, &err_json(&e)),
    }
}

pub(super) fn do_report(state: &State, tenant: &str, job: &str, capsule: &str) -> Resp {
    let data = match state.store.load_graph_in_job(tenant, job, capsule) {
        Ok(d) => d,
        Err(e) => return json_resp(404, &err_json(&e)),
    };
    let ng = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => return json_resp(500, &err_json(&e)),
    };

    // When the capsule is Active, the on-graph weights are frozen-uniform
    // (the bandit's chosen option is overlaid as a one-hot at decide time).
    // For /report to be useful — operators rely on it to see what was
    // learned — we overlay the meta-bandit leader's candidate-context
    // bucket weights here, falling back to the on-graph weights only when
    // the leader has no bucket for the canonical context (e.g. still warm).
    let warmup_state = state.store
        .load_warmup_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| crate::warmup::WarmupState::new(30));
    let memory = state.store.load_memory_in_job(tenant, job, capsule)
        .unwrap_or_default();

    let mut strategies = Vec::new();
    for node in &ng.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        let overlay = bandit_overlay_for_node(&warmup_state, &memory, node);
        strategies.push(build_strategy_report_entry(node, &ng, overlay.as_ref()));
    }

    // Surface the lifecycle + post-warmup algorithm + meta-bandit summary.
    // /report previously omitted these, which forced operators inspecting a
    // capsule from the CLI to round-trip through /memory + the on-disk
    // warmup.json. The dashboard already does that round-trip; surfacing
    // the same fields here removes friction for everyone else.
    let warmup_json: serde_json::Value = match &warmup_state.lifecycle {
        crate::warmup::CapsuleLifecycle::Warmup { samples_collected, target } => {
            serde_json::json!({
                "state": "warmup",
                "collected": samples_collected,
                "target": target,
            })
        }
        crate::warmup::CapsuleLifecycle::Active { characterization, .. } => {
            serde_json::json!({
                "state": "active",
                "characterization": format!("{characterization:?}"),
            })
        }
        crate::warmup::CapsuleLifecycle::Frozen { reason, .. } => {
            serde_json::json!({
                "state": "frozen",
                "reason": reason,
            })
        }
    };
    let algorithm_json: serde_json::Value = match warmup_state.current_algorithm() {
        Some(alg) => serde_json::Value::String(format!("{alg:?}")),
        None => serde_json::Value::Null,
    };

    // Meta-bandit summary: emit per strategy node, keyed by node id, with
    // totalRounds, currentLeader, and per-candidate trials + mean reward.
    let mut meta_by_node = serde_json::Map::new();
    for (nid, sm) in &memory.strategies {
        let mb = match &sm.meta_bandit { Some(m) => m, None => continue };
        let leader = mb.current_leader().map(|c| c.as_str().to_string());
        let candidates: Vec<serde_json::Value> = mb.candidates.iter().map(|c| {
            serde_json::json!({
                "id": c.id.as_str(),
                "trials": (c.trials * 100.0).round() / 100.0,
                "meanReward": (c.mean_reward() * 10000.0).round() / 10000.0,
                "cumulativeReward": (c.cumulative_reward * 10000.0).round() / 10000.0,
            })
        }).collect();
        meta_by_node.insert(nid.to_string(), serde_json::json!({
            "totalRounds": mb.total_rounds,
            "currentLeader": leader,
            "candidates": candidates,
        }));
    }

    json_resp(200, &serde_json::json!({
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "hash": sha256_hex(&data),
        "strategies": strategies,
        "warmup": warmup_json,
        "algorithm": algorithm_json,
        "metaBandit": serde_json::Value::Object(meta_by_node),
    }).to_string())
}

// ── Helpers ──

pub(super) fn inspect_graph_json(
    tenant: &str,
    job: &str,
    capsule: &str,
    data: &[u8],
    graph: &NeuralGraph,
    state: &State,
) -> String {
    // Same overlay as /report: in Active state, the on-graph weights are
    // frozen-uniform; surface the meta-bandit leader's bucket weights so
    // the admin console shows what the bandit actually learned.
    let warmup_state = state.store
        .load_warmup_state_in_job(tenant, job, capsule)
        .unwrap_or_else(|| crate::warmup::WarmupState::new(30));
    let memory = state.store.load_memory_in_job(tenant, job, capsule)
        .unwrap_or_default();
    let nodes: Vec<serde_json::Value> = graph.nodes.iter().map(|node| {
        let overlay = bandit_overlay_for_node(&warmup_state, &memory, node);
        build_inspect_node_entry(node, overlay.as_ref())
    }).collect();

    let edges: Vec<serde_json::Value> = graph.edges.iter().map(|edge| {
        serde_json::json!({
            "from": edge.from,
            "to": edge.to,
            "weight": (edge.weight * 10000.0).round() / 10000.0,
            "gated": edge.gate.is_some(),
        })
    }).collect();

    serde_json::json!({
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "hash": sha256_hex(data),
        "nodes": graph.nodes.len(),
        "edges": graph.edges.len(),
        "entry": graph.entry,
        "journal": graph.journal.len(),
        "stateSize": graph.state.len(),
        "nodeList": nodes,
        "edgeList": edges,
    }).to_string()
}

#[cfg(test)]
mod tests {
    //! Regression tests for B-1: disambiguating live-overlay weights from
    //! the on-graph (graphWeights) weights in /report and /inspect.
    //!
    //! These tests exercise the pure helpers
    //! `bandit_overlay_for_node`, `build_strategy_report_entry`, and
    //! `build_inspect_node_entry` directly — they don't stand up a
    //! `LycanStore` or HTTP server, because the JSON shape is what
    //! operators rely on and the helpers are the load-bearing piece.
    use super::*;
    use crate::graph::{Contract, GraphNode, NeuralGraph, Objective, OpCode, WeightKind};
    use crate::learning::{CapsuleMemory, ContextBucket, OptionStats, StrategyMemory};
    use crate::meta_bandit::{CandidateId, MetaBandit};
    use crate::warmup::WarmupState;
    use std::collections::HashMap;

    fn make_choice_node() -> GraphNode {
        GraphNode {
            id: 7,
            op: OpCode::AdaptiveChoice,
            operands: Vec::new(),
            // Distinct, easy-to-eyeball on-graph weights so we can tell
            // them apart from the bandit overlay below.
            weights: vec![0.5, 0.3, 0.2],
            bias: 0.0,
            activation_count: 0,
            state_slot: None,
            weight_kind: WeightKind::Strategy,
            annotation: None,
            contract: Contract::None,
            objective: Objective::None,
        }
    }

    fn make_graph_with_node(node: GraphNode) -> NeuralGraph {
        let mut ng = NeuralGraph::new();
        ng.nodes.push(node);
        ng
    }

    fn active_warmup_state() -> WarmupState {
        // Drive 30 feedbacks through a fresh `WarmupState::new(30)` so the
        // capsule transitions Warmup → Active. We don't care which
        // PickedAlgorithm `pick_algorithm` selects — the overlay logic
        // only checks `is_active()`.
        let mut w = WarmupState::new(30);
        for _ in 0..30 {
            w.record_feedback(0.5);
        }
        assert!(w.is_active(), "warmup must transition after 30 samples");
        w
    }

    fn bucket_with_weights(w: Vec<f64>) -> ContextBucket {
        let n = w.len();
        ContextBucket {
            weights: w,
            stats: (0..n).map(|_| OptionStats::default()).collect(),
            updated_at: 1_000_000,
            option_states: Vec::new(),
            conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
        }
    }

    fn memory_with_leader(node_id: u32, leader: CandidateId, bucket_weights: Vec<f64>) -> CapsuleMemory {
        let mut mb = MetaBandit::new_with_candidates(&CandidateId::discrete_only());
        // Record one strong reward for the desired leader so
        // `current_leader()` returns it deterministically.
        mb.record(leader, 0.95);
        // Sprinkle weaker rewards on the other candidates so they have
        // trials > 0 (otherwise they'd be excluded from leader voting,
        // which is fine — but a more realistic state is healthier).
        for cid in CandidateId::discrete_only().iter().filter(|c| **c != leader) {
            mb.record(*cid, 0.1);
        }
        assert_eq!(mb.current_leader(), Some(leader));

        let mut candidate_contexts = HashMap::new();
        candidate_contexts.insert(
            (leader, "ctx-A".to_string()),
            bucket_with_weights(bucket_weights),
        );
        let sm = StrategyMemory {
            node_id,
            n_options: 3,
            contexts: HashMap::new(),
            candidate_contexts,
            meta_bandit: Some(mb),
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        };

        let mut mem = CapsuleMemory::default();
        mem.strategies.insert(node_id, sm);
        mem
    }

    #[test]
    fn overlay_absent_in_warmup() {
        let node = make_choice_node();
        let warmup = WarmupState::new(30);  // freshly Warmup
        let memory = CapsuleMemory::default();

        let overlay = bandit_overlay_for_node(&warmup, &memory, &node);
        assert!(overlay.is_none(), "no overlay in Warmup");

        let entry = build_strategy_report_entry(&node, &make_graph_with_node(node.clone()), overlay.as_ref());
        assert_eq!(entry["liveSource"], serde_json::Value::Null);
        assert_eq!(entry["graphWeights"], serde_json::json!([0.5, 0.3, 0.2]));
        let opts = entry["options"].as_array().unwrap();
        // weight == graphWeight per option when no overlay.
        for opt in opts {
            assert_eq!(opt["weight"], opt["graphWeight"], "weight should match graphWeight in Warmup");
        }
        // Existing fields untouched.
        assert!(entry.get("weightsSource").is_none(),
                "weightsSource absent when overlay absent");
        assert!(entry.get("leaderCandidate").is_none(),
                "leaderCandidate absent when overlay absent");
    }

    #[test]
    fn overlay_present_in_active() {
        let node = make_choice_node();
        let warmup = active_warmup_state();
        // Bandit bucket diverges from on-graph: [0.1, 0.8, 0.1] vs [0.5, 0.3, 0.2].
        let memory = memory_with_leader(node.id, CandidateId::Thompson, vec![0.1, 0.8, 0.1]);

        let overlay = bandit_overlay_for_node(&warmup, &memory, &node);
        let overlay = overlay.expect("overlay should be present in Active with leader bucket");
        assert_eq!(overlay.2, "Thompson");
        assert_eq!(overlay.1, "ctx-A");

        let entry = build_strategy_report_entry(&node, &make_graph_with_node(node.clone()), Some(&overlay));
        assert_eq!(entry["liveSource"], serde_json::json!("Thompson"));
        assert_eq!(entry["graphWeights"], serde_json::json!([0.5, 0.3, 0.2]));
        // Existing legacy fields still present for back-compat.
        assert_eq!(entry["weightsSource"], serde_json::json!("meta_bandit_leader"));
        assert_eq!(entry["leaderCandidate"], serde_json::json!("Thompson"));
        assert_eq!(entry["contextKey"], serde_json::json!("ctx-A"));
        // Per-option: `weight` reflects the overlay, `graphWeight` reflects on-graph.
        let opts = entry["options"].as_array().unwrap();
        assert_eq!(opts[0]["weight"], serde_json::json!(0.1));
        assert_eq!(opts[0]["graphWeight"], serde_json::json!(0.5));
        assert_eq!(opts[1]["weight"], serde_json::json!(0.8));
        assert_eq!(opts[1]["graphWeight"], serde_json::json!(0.3));
        assert_eq!(opts[2]["weight"], serde_json::json!(0.1));
        assert_eq!(opts[2]["graphWeight"], serde_json::json!(0.2));
    }

    #[test]
    fn inspect_node_entry_shape_warmup_vs_active() {
        let node = make_choice_node();

        // Warmup: no overlay. weights == graphWeights, liveSource null.
        let warmup = WarmupState::new(30);
        let mem_empty = CapsuleMemory::default();
        let overlay_w = bandit_overlay_for_node(&warmup, &mem_empty, &node);
        let json_w = build_inspect_node_entry(&node, overlay_w.as_ref());
        assert_eq!(json_w["liveSource"], serde_json::Value::Null);
        assert_eq!(json_w["weights"], json_w["graphWeights"],
                   "weights must equal graphWeights when liveSource is null");
        assert_eq!(json_w["graphWeights"], serde_json::json!([0.5, 0.3, 0.2]));
        assert!(json_w.get("weightsSource").is_none());

        // Active with bandit overlay diverging from graph.
        let active = active_warmup_state();
        let mem_active = memory_with_leader(node.id, CandidateId::Ucb, vec![0.05, 0.05, 0.9]);
        let overlay_a = bandit_overlay_for_node(&active, &mem_active, &node);
        let overlay_a = overlay_a.expect("active overlay");
        let json_a = build_inspect_node_entry(&node, Some(&overlay_a));
        assert_eq!(json_a["liveSource"], serde_json::json!("Ucb"));
        assert_eq!(json_a["graphWeights"], serde_json::json!([0.5, 0.3, 0.2]));
        assert_eq!(json_a["weights"], serde_json::json!([0.05, 0.05, 0.9]));
        assert_ne!(json_a["weights"], json_a["graphWeights"],
                   "in Active with overlay, weights must diverge from graphWeights");
        // Back-compat legacy fields preserved.
        assert_eq!(json_a["weightsSource"], serde_json::json!("meta_bandit_leader"));
        assert_eq!(json_a["leaderCandidate"], serde_json::json!("Ucb"));
        assert_eq!(json_a["contextKey"], serde_json::json!("ctx-A"));
    }

    #[test]
    fn overlay_only_for_strategy_or_choice_nodes() {
        // A non-strategy node (e.g. a generic compute) must never get an
        // overlay even in Active state with a (mis)matched memory entry.
        let mut node = make_choice_node();
        node.op = OpCode::Add;
        let warmup = active_warmup_state();
        let memory = memory_with_leader(node.id, CandidateId::Thompson, vec![0.1, 0.8, 0.1]);
        assert!(bandit_overlay_for_node(&warmup, &memory, &node).is_none());
    }
}
