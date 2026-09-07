use tracing::warn;

use crate::graph::{Contract, NeuralGraph, OpCode};

pub(super) fn audit_event_json(action: &str, tenant: &str, job: &str, capsule: &str, extra: serde_json::Value) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs();
    let mut m = serde_json::Map::new();
    m.insert("action".into(), serde_json::json!(action));
    m.insert("tenant".into(), serde_json::json!(tenant));
    m.insert("job".into(), serde_json::json!(job));
    m.insert("capsule".into(), serde_json::json!(capsule));
    m.insert("timestamp".into(), serde_json::json!(ts));
    if let serde_json::Value::Object(extra_map) = extra {
        for (k, v) in extra_map { m.insert(k, v); }
    }
    serde_json::Value::Object(m).to_string()
}

/// Emit a stderr warning if the installed `.lyc` payload contains
/// `OpCode::Strategy` nodes; those bypass Syntra's `/feedback`-driven learning.
pub(super) fn warn_if_strategy_nodes(tenant: &str, job: &str, capsule: &str, data: &[u8]) {
    let Ok(graph) = NeuralGraph::from_bytes(data) else { return };
    let count = graph.nodes.iter().filter(|n| matches!(n.op, OpCode::Strategy)).count();
    if count > 0 {
        warn!(
            tenant = %tenant,
            job = %job,
            capsule = %capsule,
            strategy_nodes = count,
            "capsule has (strategy ...) node — use (choice ...) for Syntra-driven adaptive choice. See docs/investigations/greedy-lock-2026-05.md"
        );
    }
}

/// Stable per-feature-vector hash for ADWIN bucketing on feature-context capsules.
/// Buckets each component to one decimal place so similar vectors share a key.
pub(super) fn stable_hash_features(v: &[f64]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let bucketed: Vec<i64> = v.iter().map(|x| (x * 10.0).round() as i64).collect();
    let mut h = DefaultHasher::new();
    bucketed.hash(&mut h);
    format!("f:{:x}", h.finish())
}

/// First AdaptiveChoice node in graph order, or `None` if absent.
pub(crate) fn primary_choice_node(graph: &NeuralGraph) -> Option<u32> {
    graph.nodes.iter().enumerate()
        .find(|(_, n)| matches!(n.op, OpCode::AdaptiveChoice))
        .map(|(idx, _)| idx as u32)
}

/// All AdaptiveChoice nodes in graph order as `(node_id, weights_len, contract)`.
pub(crate) fn all_choice_nodes(
    graph: &NeuralGraph,
) -> Vec<(u32, usize, Contract)> {
    graph.nodes.iter().enumerate()
        .filter(|(_, n)| matches!(n.op, OpCode::AdaptiveChoice))
        .map(|(idx, n)| (idx as u32, n.weights.len(), n.contract))
        .collect()
}

pub(super) fn flatten_strategy_weights(graph: &mut NeuralGraph) {
    for node in graph.nodes.iter_mut() {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        if n_options == 0 { continue; }
        let uniform = 1.0 / n_options as f64;
        for w in node.weights.iter_mut().take(n_options) {
            *w = uniform;
        }
    }
}

pub(super) fn apply_context_memory_to_graph(
    graph: &mut NeuralGraph,
    memory: &crate::learning::CapsuleMemory,
    context_key: &str,
    config: &crate::learning::LearningConfig,
    is_binary_reward: bool,
) -> std::collections::HashMap<u32, (usize, Vec<usize>, Option<f64>, Vec<f64>)> {
    let mut decisions: std::collections::HashMap<u32, (usize, Vec<usize>, Option<f64>, Vec<f64>)>
        = std::collections::HashMap::new();
    for node in &mut graph.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        let Some(strategy_memory) = memory.strategies.get(&node.id) else { continue; };
        let Some(bucket) = strategy_memory.contexts.get(context_key) else { continue; };

        let limit = n_options.min(bucket.weights.len()).min(node.weights.len());
        for i in 0..limit {
            node.weights[i] = bucket.weights[i];
        }

        if limit >= 2 {
            let (algorithm_choice, _reason) = crate::learning::select_option(bucket, config, limit);
            let pset = crate::learning::compute_prediction_set(bucket, config, limit);
            let band = crate::learning::conformal_band_radius(bucket, config);
            let posterior_means: Vec<f64> = (0..limit)
                .map(|i| bucket.stats.get(i).map(|s| s.posterior_mean).unwrap_or(0.0))
                .collect();

            let needs_override = matches!(
                config.algorithm,
                crate::learning::Algorithm::ThompsonSampling
                    | crate::learning::Algorithm::Ucb1
            );
            if needs_override && algorithm_choice < limit {
                // Binary rewards: hard greedy commit (textbook Thompson/UCB1).
                // Continuous rewards: soft nudge so weighted-bucket dynamics
                // still explore around the algorithm's pick, since premature
                // commitment is asymmetrically costly.
                if is_binary_reward {
                    let floor = (config.safety.min_exploration / limit as f64).max(0.0);
                    let chosen_w = (1.0 - floor * (limit - 1) as f64).max(floor);
                    for i in 0..limit {
                        node.weights[i] = if i == algorithm_choice { chosen_w } else { floor };
                    }
                } else {
                    let max_w = node.weights[..limit].iter().cloned().fold(0.0_f64, f64::max);
                    node.weights[algorithm_choice] = (max_w + 1e-3).min(1.0);
                    let sum: f64 = node.weights[..limit].iter().sum();
                    if sum > 0.0 {
                        for i in 0..limit { node.weights[i] /= sum; }
                    }
                }
            }

            decisions.insert(node.id, (algorithm_choice, pset, band, posterior_means));
        }
    }
    decisions
}

pub(super) fn extract_decisions(graph: &NeuralGraph) -> Vec<serde_json::Value> {
    use crate::graph::Objective;
    let mut decisions = Vec::new();
    for node in &graph.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        if node.activation_count == 0 { continue; }
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        let chosen = node.bias as usize;
        let confidence = node.weights.get(chosen).copied().unwrap_or(0.0);
        let objective = match node.objective {
            Objective::Speed => "speed", Objective::Accuracy => "accuracy",
            Objective::Reliability => "reliability", Objective::Cost => "cost",
            Objective::Risk => "risk", Objective::Confidence => "confidence",
            Objective::Reward => "reward", Objective::MultiObjective => "multi",
            Objective::None => "general",
        };
        let weights: Vec<f64> = node.weights[..n_options].to_vec();
        decisions.push(serde_json::json!({
            "node_id": node.id,
            "chosen_option": chosen,
            "confidence": (confidence * 10000.0).round() / 10000.0,
            "objective": objective,
            "weights": weights.iter().map(|w| (w * 10000.0).round() / 10000.0).collect::<Vec<f64>>(),
            "activations": node.activation_count,
        }));
    }
    decisions
}
