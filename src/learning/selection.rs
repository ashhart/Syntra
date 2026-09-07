use super::config::{Algorithm, LearningConfig};
use super::memory::{ContextBucket, OptionState};
use super::rng::rand_f64;
use super::stats::OptionStats;

// ── Selection algorithms ──

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs()
}

/// Effective epsilon for an option, accounting for change-triggered exploration boost.
fn effective_epsilon(base_eps: f64, min_exploration: f64, bucket: &ContextBucket) -> f64 {
    let boost_active = bucket.stats.iter().any(|s| s.change_boost_remaining > 0);
    let mut eps = base_eps.max(min_exploration);
    if boost_active {
        // Use the largest configured boost across options as the effective floor.
        // Bound to 0.5 so we never go fully random.
        let boost: f64 = bucket.stats.iter()
            .filter(|s| s.change_boost_remaining > 0)
            .map(|s| (s.change_boost_remaining as f64) / 50.0)
            .fold(0.0, f64::max)
            .min(1.0) * 0.25;
        eps = (eps + boost).min(0.5);
    }
    eps
}

/// Select an option using the configured algorithm. Returns (chosen_index, reason).
pub fn select_option(
    bucket: &ContextBucket,
    config: &LearningConfig,
    n_options: usize,
) -> (usize, String) {
    if n_options == 0 { return (0, "no options".into()); }
    if n_options == 1 { return (0, "single option".into()); }

    match &config.algorithm {
        Algorithm::SimpleWeighted => select_weighted(&bucket.weights, n_options),
        Algorithm::EpsilonGreedy { epsilon } => {
            let eps = effective_epsilon(*epsilon, config.safety.min_exploration, bucket);
            select_epsilon_greedy(&bucket.weights, &bucket.stats, n_options, eps, config)
        }
        Algorithm::Ucb1 => select_ucb1(&bucket.stats, n_options, config),
        Algorithm::ThompsonSampling => {
            let has_beta = bucket.option_states.iter().any(|s| matches!(s, OptionState::BetaBernoulli { .. }));
            if has_beta {
                select_thompson_beta(&bucket.option_states, n_options)
            } else {
                select_thompson_gaussian(&bucket.stats, n_options, config)
            }
        }
        Algorithm::Softmax { temperature } => select_softmax(&bucket.stats, n_options, *temperature, config),
    }
}

fn select_weighted(weights: &[f64], n: usize) -> (usize, String) {
    let sum: f64 = weights.iter().take(n).sum();
    if sum <= 0.0 { return (0, "zero weights, defaulting".into()); }
    let r: f64 = rand_f64() * sum;
    let mut cumulative = 0.0;
    for i in 0..n {
        cumulative += weights.get(i).copied().unwrap_or(0.0);
        if r < cumulative { return (i, "weighted selection".into()); }
    }
    (n - 1, "weighted selection (rounding)".into())
}

pub(crate) fn stats_score(s: &OptionStats, config: &LearningConfig) -> f64 {
    if s.tries == 0 { return f64::NEG_INFINITY; }
    let mean = if config.safety.trimmed_fraction > 0.0 && !s.window.is_empty() {
        s.reward_mean_trimmed(config.safety.trimmed_fraction)
    } else if config.window.enabled && !s.window.is_empty() {
        s.reward_mean_windowed()
    } else if config.decay.enabled {
        s.reward_mean_decayed()
    } else {
        s.reward_mean()
    };
    if config.risk_sensitive.enabled && !s.window.is_empty() {
        let cvar = s.reward_cvar(config.risk_sensitive.alpha);
        let b = config.risk_sensitive.blend;
        (1.0 - b) * mean + b * cvar
    } else {
        mean
    }
}

fn select_epsilon_greedy(
    weights: &[f64],
    stats: &[OptionStats],
    n: usize,
    epsilon: f64,
    config: &LearningConfig,
) -> (usize, String) {
    if rand_f64() < epsilon {
        let idx = (rand_f64() * n as f64) as usize;
        return (idx.min(n - 1), format!("epsilon-greedy explore (eps={epsilon:.3})"));
    }
    // Exploit: pick option with highest scored mean reward, falling back to weight
    let mut best = 0;
    let mut best_score = f64::NEG_INFINITY;
    for i in 0..n {
        let score = if stats.get(i).map(|s| s.tries > 0).unwrap_or(false) {
            stats_score(&stats[i], config)
        } else {
            weights.get(i).copied().unwrap_or(0.0)
        };
        if score > best_score { best_score = score; best = i; }
    }
    (best, "epsilon-greedy exploit".into())
}

fn select_ucb1(stats: &[OptionStats], n: usize, config: &LearningConfig) -> (usize, String) {
    let total_tries: u64 = stats.iter().take(n).map(|s| s.tries).sum();
    if total_tries == 0 { return (0, "ucb1: no data, trying first".into()); }

    for i in 0..n {
        if stats.get(i).map(|s| s.tries == 0).unwrap_or(true) {
            return (i, format!("ucb1: untried option {i}"));
        }
    }

    let log_total = (total_tries as f64).ln().max(1.0);
    let mut best = 0;
    let mut best_ucb = f64::NEG_INFINITY;
    let gkt_budget = if config.corruption_robust.enabled {
        config.corruption_robust.budget
    } else { 0.0 };
    for i in 0..n {
        if let Some(s) = stats.get(i) {
            let mean = stats_score(s, config);
            let denom = if config.decay.enabled && s.effective_tries > 0.5 {
                s.effective_tries
            } else {
                s.tries as f64
            };
            let exploration = (2.0 * log_total / denom.max(1.0)).sqrt();
            let corruption_bonus = if gkt_budget > 0.0 { gkt_budget / denom.max(1.0) } else { 0.0 };
            let ucb = mean + exploration + corruption_bonus;
            if ucb > best_ucb { best_ucb = ucb; best = i; }
        }
    }
    (best, format!("ucb1: best upper bound {best_ucb:.4}"))
}

/// Box-Muller transform: convert two uniform [0,1) samples to one standard Normal sample.
fn standard_normal() -> f64 {
    let u1 = rand_f64().max(1e-9);
    let u2 = rand_f64();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Gaussian Thompson sampling: posterior on mean reward is N(empirical_mean, var/n).
/// Sample one value per option, pick argmax. Works for continuous rewards.
fn sample_beta(alpha: f64, beta: f64) -> f64 {
    // Gaussian approximation Beta(α, β) ≈ N(α/(α+β), αβ/((α+β)²(α+β+1))).
    let s = alpha + beta;
    let mean = alpha / s.max(1e-9);
    let var = (alpha * beta) / (s * s * (s + 1.0)).max(1e-9);
    let sample = mean + standard_normal() * var.sqrt();
    sample.clamp(0.0, 1.0)
}

fn select_thompson_beta(states: &[OptionState], n: usize) -> (usize, String) {
    let mut best = 0;
    let mut best_sample = f64::NEG_INFINITY;
    for i in 0..n.min(states.len()) {
        let s = match &states[i] {
            OptionState::BetaBernoulli { alpha, beta } => sample_beta(*alpha, *beta),
            other => other.as_visible_weight(),
        };
        if s > best_sample { best_sample = s; best = i; }
    }
    (best, format!("thompson-beta: best sample {best_sample:.4}"))
}

fn select_thompson_gaussian(stats: &[OptionStats], n: usize, config: &LearningConfig) -> (usize, String) {
    // First-pass: try any untried option (matches UCB1's optimism-on-no-data behavior).
    for i in 0..n {
        if stats.get(i).map(|s| s.tries == 0).unwrap_or(true) {
            return (i, format!("thompson: untried option {i}"));
        }
    }

    let mut best = 0;
    let mut best_sample = f64::NEG_INFINITY;
    for i in 0..n {
        if let Some(s) = stats.get(i) {
            let mean = stats_score(s, config);
            // Use windowed variance when window is enabled; cumulative otherwise.
            let var = if config.window.enabled && s.window.len() >= 2 {
                s.reward_variance_recent()
            } else {
                s.reward_variance().max(1e-6)
            };
            let denom = if config.decay.enabled && s.effective_tries > 0.5 {
                s.effective_tries
            } else {
                s.tries as f64
            };
            let posterior_std = (var / denom.max(1.0)).sqrt().max(1e-4);
            let sample = mean + standard_normal() * posterior_std;
            if sample > best_sample { best_sample = sample; best = i; }
        }
    }
    (best, format!("thompson: best sample {best_sample:.4}"))
}

fn select_softmax(stats: &[OptionStats], n: usize, temperature: f64, config: &LearningConfig) -> (usize, String) {
    let temp = temperature.max(0.01);
    let scores: Vec<f64> = (0..n).map(|i| {
        if stats.get(i).map(|s| s.tries == 0).unwrap_or(true) {
            0.0
        } else {
            stats_score(&stats[i], config) / temp
        }
    }).collect();
    let max_s = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = scores.iter().map(|s| (s - max_s).exp()).collect();
    let sum: f64 = exps.iter().sum();
    if sum <= 0.0 { return (0, "softmax: degenerate".into()); }
    let r = rand_f64() * sum;
    let mut cum = 0.0;
    for (i, e) in exps.iter().enumerate() {
        cum += e;
        if r < cum { return (i, format!("softmax (temp={temp:.2})")); }
    }
    (n - 1, "softmax (rounding)".into())
}
