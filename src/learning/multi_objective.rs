use std::collections::HashMap;

use super::config::LearningConfig;
use super::feedback::apply_feedback;
use super::memory::ContextBucket;

pub fn apply_feedback_multi(
    bucket: &mut ContextBucket,
    option: usize,
    objective_rewards: &HashMap<String, f64>,
    config: &LearningConfig,
) -> Result<(), String> {
    if let Some(s) = bucket.stats.get_mut(option) {
        for (k, v) in objective_rewards {
            s.record_objective(k, *v);
        }
    }
    // Reduce to a scalar reward (mean of objective values) so the existing
    // weight-update + decay + change-detection + conformal path still runs.
    let scalar = if objective_rewards.is_empty() {
        0.0
    } else {
        objective_rewards.values().sum::<f64>() / objective_rewards.len() as f64
    };
    apply_feedback(bucket, option, scalar, config)
}

/// Returns indices that are Pareto-non-dominated across the configured
/// objectives. Maximization assumed. An option A dominates B iff A is
/// no worse on every objective and strictly better on at least one.
pub fn pareto_frontier(
    bucket: &ContextBucket,
    config: &LearningConfig,
    n_options: usize,
) -> Vec<usize> {
    if !config.pareto.enabled || config.pareto.objectives.is_empty() {
        return (0..n_options).collect();
    }
    let scores: Vec<Vec<f64>> = (0..n_options)
        .map(|i| {
            let s = bucket.stats.get(i);
            config
                .pareto
                .objectives
                .iter()
                .map(|obj| {
                    s.map(|st| st.objective_mean(obj))
                        .unwrap_or(f64::NEG_INFINITY)
                })
                .collect()
        })
        .collect();

    let mut frontier = Vec::new();
    for i in 0..n_options {
        let mut dominated = false;
        for j in 0..n_options {
            if i == j {
                continue;
            }
            let strictly_better = scores[j].iter().zip(&scores[i]).any(|(a, b)| a > b);
            let no_worse = scores[j].iter().zip(&scores[i]).all(|(a, b)| a >= b);
            if no_worse && strictly_better {
                dominated = true;
                break;
            }
        }
        if !dominated {
            frontier.push(i);
        }
    }
    if frontier.is_empty() {
        (0..n_options).collect()
    } else {
        frontier
    }
}

pub fn select_pareto(
    bucket: &ContextBucket,
    config: &LearningConfig,
    n_options: usize,
) -> (usize, String) {
    let frontier = pareto_frontier(bucket, config, n_options);
    if frontier.is_empty() {
        return (0, "empty frontier".into());
    }
    if frontier.len() == 1 {
        return (frontier[0], "single non-dominated option".into());
    }
    // Pick from the frontier by current weight (so learned preferences still
    // bias choice among the Pareto-equal options).
    let mut best = frontier[0];
    let mut best_w = bucket.weights.get(best).copied().unwrap_or(0.0);
    for &i in &frontier[1..] {
        let w = bucket.weights.get(i).copied().unwrap_or(0.0);
        if w > best_w {
            best_w = w;
            best = i;
        }
    }
    (
        best,
        format!("pareto frontier ({} options)", frontier.len()),
    )
}
