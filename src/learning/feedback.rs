use super::config::{
    Algorithm, ChangeDetectionConfig, ChangeDetectionMethod, ConformalConfig, DecayConfig,
    DelayedSignalSpec, LearningConfig, RewardPolicy,
};
use super::memory::{ContextBucket, OptionState};
use super::selection::{now_secs, stats_score};
use super::stats::OptionStats;

// ── Feedback application ──

/// Apply per-feedback exponential decay to all option stats in a bucket.
/// This is the "sliding window with forgetting" mechanism wired into feedback.
/// Operates on cumulative reward_sum, reward_sq_sum, effective_tries.
fn apply_feedback_decay(bucket: &mut ContextBucket, config: &LearningConfig) {
    // Apply option-state forgetting to OptionStats accumulators so counters
    // don't grow unbounded over long deployments.
    let f = config.safety.option_state_forgetting;
    if f < 1.0 && f > 0.0 {
        for s in bucket.stats.iter_mut() {
            s.reward_sum *= f;
            s.reward_sq_sum *= f;
            s.effective_tries *= f;
            let tries_f = (s.tries as f64) * f;
            let succ_f = (s.successes as f64) * f;
            let fail_f = (s.failures as f64) * f;
            s.tries = tries_f.round() as u64;
            s.successes = succ_f.round() as u64;
            s.failures = fail_f.round() as u64;
        }
    }

    // Legacy half-life decay (opt-in via config.decay.enabled).
    if !config.decay.enabled { return; }
    let h = config.decay.half_life_feedbacks;
    if h <= 0.0 { return; }
    let factor = (0.5_f64).powf(1.0 / h);
    for s in bucket.stats.iter_mut() {
        s.reward_sum *= factor;
        s.reward_sq_sum *= factor;
        s.effective_tries *= factor;
    }
}

fn check_change_point_page_hinkley(s: &mut OptionStats, reward: f64, config: &ChangeDetectionConfig) -> bool {
    if s.tries < 5 { return false; }
    let mean = if s.effective_tries > 1.0 { s.reward_sum / s.effective_tries } else { s.reward_mean() };
    let up_delta = reward - mean - config.min_drift;
    let down_delta = mean - reward - config.min_drift;
    s.ph_cumsum = (s.ph_cumsum + up_delta).max(0.0);
    s.ph_min = (s.ph_min + down_delta).max(0.0);
    if s.ph_cumsum > config.threshold || s.ph_min > config.threshold {
        s.change_points += 1;
        s.change_boost_remaining = config.boost_duration;
        s.ph_cumsum = 0.0;
        s.ph_min = 0.0;
        return true;
    }
    false
}

fn check_change_point_model_surprise(s: &mut OptionStats, reward: f64, config: &ChangeDetectionConfig) -> bool {
    if s.tries < 5 || s.window.len() < 4 { return false; }
    let mean = s.reward_mean_windowed();
    let var = s.reward_variance_recent().max(1e-4);
    let denom = (var / (s.window.len() as f64)).sqrt().max(1e-3);
    let z = ((reward - mean).abs()) / denom;
    let surprising = z > config.surprise_k_sigma;
    if surprising { s.surprise_recent = s.surprise_recent.saturating_add(1); }
    let win_len = s.window.len() as f64;
    let frac = (s.surprise_recent as f64).min(win_len) / win_len;
    if frac >= config.surprise_fraction_threshold {
        s.change_points += 1;
        s.change_boost_remaining = config.boost_duration;
        s.surprise_recent = 0;
        return true;
    }
    if !surprising && s.surprise_recent > 0 {
        s.surprise_recent -= 1;
    }
    false
}

fn check_change_point(s: &mut OptionStats, reward: f64, config: &ChangeDetectionConfig) -> bool {
    if !config.enabled { return false; }
    match config.method {
        ChangeDetectionMethod::PageHinkley => check_change_point_page_hinkley(s, reward, config),
        ChangeDetectionMethod::ModelSurprise => check_change_point_model_surprise(s, reward, config),
    }
}

fn fuse_signal(s: &mut OptionStats, observed: f64, signal: &DelayedSignalSpec) {
    let z = observed - signal.bias;
    let prior_mean = s.posterior_mean;
    let prior_var = s.posterior_var.max(1e-6);
    let obs_var = signal.noise_variance.max(1e-6);
    let post_var = 1.0 / (1.0 / prior_var + 1.0 / obs_var);
    let post_mean = post_var * (prior_mean / prior_var + z / obs_var);
    s.posterior_mean = post_mean;
    s.posterior_var = post_var;
    *s.signal_counts.entry(signal.name.clone()).or_insert(0) += 1;
}

fn update_conformal(bucket: &mut ContextBucket, option: usize, observed: f64, _config: &ConformalConfig) {
    // Always feed the calibrator from non-LinUcb feedback so refusal logic
    // has data regardless of the `conformal.enabled` gate. LinUcb updates
    // the calibrator separately once the feature vector is in scope.
    let predicted = match bucket.option_states.get(option) {
        Some(OptionState::LinUcb { .. }) => return,
        Some(state) => state.as_visible_weight(),
        None => return,
    };
    bucket.conformity_calibrator.record(predicted, observed);
}

pub fn conformal_band_radius(bucket: &ContextBucket, config: &LearningConfig) -> Option<f64> {
    if !config.conformal.enabled {
        return None;
    }
    // ConformalConfig.coverage is the desired coverage (e.g. 0.90); convert to alpha.
    let alpha = (1.0 - config.conformal.coverage).clamp(0.0, 1.0);
    bucket.conformity_calibrator.quantile(alpha)
}

pub fn compute_prediction_set(bucket: &ContextBucket, config: &LearningConfig, n_options: usize) -> Vec<usize> {
    let radius = match conformal_band_radius(bucket, config) {
        Some(r) => r,
        None => return (0..n_options).collect(),
    };
    let scores: Vec<f64> = (0..n_options).map(|i| {
        bucket.stats.get(i).map(|s| stats_score(s, config))
            .unwrap_or(f64::NEG_INFINITY)
    }).collect();
    let best = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    (0..n_options).filter(|&i| {
        let s = scores[i];
        s.is_finite() && (best - s) <= radius
    }).collect()
}

/// Apply feedback to a context bucket: clip, decay, update stats, detect
/// change, update weights with min-exploration floor.
pub fn apply_feedback(
    bucket: &mut ContextBucket,
    option: usize,
    reward: f64,
    config: &LearningConfig,
) -> Result<(), String> {
    if config.safety.freeze_learning {
        return Err("learning is frozen".into());
    }

    let n = bucket.weights.len();
    if option >= n {
        return Err(format!("option {option} out of range ({n} options)"));
    }

    // 1. Clip reward.
    let clipped = if config.safety.reward_clip > 0.0 {
        reward.clamp(-config.safety.reward_clip, config.safety.reward_clip)
    } else {
        reward
    };

    // 2. Decay all cumulative stats (per-feedback decay = sliding window).
    apply_feedback_decay(bucket, config);

    // 3. Update chosen option's stats.
    if let Some(s) = bucket.stats.get_mut(option) {
        s.tries += 1;
        if clipped > 0.0 { s.successes += 1; } else if clipped < 0.0 { s.failures += 1; }
        s.reward_sum += clipped;
        s.reward_sq_sum += clipped * clipped;
        s.last_reward = clipped;
        s.last_updated = now_secs();
        s.effective_tries += 1.0;

        if config.window.enabled {
            s.window.push_back(clipped);
            while s.window.len() > config.window.size { s.window.pop_front(); }
        }

        if s.change_boost_remaining > 0 { s.change_boost_remaining -= 1; }

        let _changed = check_change_point(s, clipped, &config.change_detection);

        if !config.delayed_feedback.enabled {
            s.posterior_mean = s.reward_mean_windowed();
            s.posterior_var = (s.reward_variance_recent() / (s.window.len().max(1) as f64)).max(1e-4);
        }
    }

    update_conformal(bucket, option, clipped, &config.conformal);

    // 5. Weight update. Mean-seeking toward the observed reward
    // (`w += lr * (r - w)`), matching `hierarchical_state.rs`. The old
    // additive rule (`w += lr * r`) let cumulative success flux — not
    // average reward — drive the weights, so with stochastic rewards a
    // luckier inferior option could lock in.
    let learning_rate = config.learning_rate.clamp(0.0001, 0.5);
    let max_delta = config.safety.max_weight_delta_per_feedback;
    let chosen_w = bucket.weights.get(option).copied().unwrap_or(0.0);
    let delta = ((clipped - chosen_w) * learning_rate).clamp(-max_delta, max_delta);

    for j in 0..n {
        if j == option {
            bucket.weights[j] = (bucket.weights[j] + delta).clamp(0.01, 0.99);
        } else if n > 1 {
            bucket.weights[j] = (bucket.weights[j] - delta / (n - 1) as f64).clamp(0.01, 0.99);
        }
    }

    // Allocate min_exploration uniformly, then distribute the remaining
    // mass by relative weight so the floor is preserved post-normalization.
    let min_w = config.safety.min_exploration / n as f64;
    let remaining = (1.0 - config.safety.min_exploration).max(0.0);
    let sum: f64 = bucket.weights.iter().sum();
    if sum > 0.0 {
        for w in &mut bucket.weights {
            let relative = *w / sum;
            *w = min_w + remaining * relative;
        }
    } else {
        let uniform = 1.0 / n as f64;
        for w in &mut bucket.weights { *w = uniform; }
    }

    update_option_states(bucket, option, clipped, config);

    bucket.updated_at = now_secs();
    Ok(())
}

fn update_option_states(bucket: &mut ContextBucket, option: usize, clipped: f64, config: &LearningConfig) {
    ensure_option_states_match_algorithm(bucket, config);
    if option >= bucket.option_states.len() { return; }

    // Forget across all states so unchosen options fade with the chosen one.
    // Weighted is skipped (scalar weight is already a current estimate).
    let forgetting = config.safety.option_state_forgetting;
    if forgetting < 1.0 {
        for state in bucket.option_states.iter_mut() {
            match state {
                OptionState::Weighted { .. } => {}
                OptionState::BetaBernoulli { alpha, beta } => {
                    *alpha = 1.0 + (*alpha - 1.0) * forgetting;
                    *beta = 1.0 + (*beta - 1.0) * forgetting;
                }
                OptionState::Ucb { tries, total_reward } => {
                    *tries *= forgetting;
                    *total_reward *= forgetting;
                }
                OptionState::LinUcb { .. } => {
                    // LinUcb requires the feature vector and is updated elsewhere.
                }
            }
        }
    }

    let learning_rate = config.learning_rate.clamp(0.0001, 0.5);
    let max_delta = config.safety.max_weight_delta_per_feedback;
    let n = bucket.option_states.len();
    let mut weighted_delta = 0.0f64;
    match &mut bucket.option_states[option] {
        OptionState::Weighted { weight } => {
            // Mean-seeking toward the observed reward (see apply_feedback).
            let d = ((clipped - *weight) * learning_rate).clamp(-max_delta, max_delta);
            *weight = (*weight + d).clamp(0.01, 0.99);
            weighted_delta = d;
        }
        OptionState::BetaBernoulli { alpha, beta } => {
            // Continuous rewards in (0, 1) contribute fractionally.
            if clipped >= 1.0 - 1e-9 {
                *alpha += 1.0;
            } else if clipped <= 1e-9 {
                *beta += 1.0;
            } else {
                let s = clipped.clamp(0.0, 1.0);
                *alpha += s;
                *beta += 1.0 - s;
            }
        }
        OptionState::Ucb { tries, total_reward } => {
            *tries += 1.0;
            *total_reward += clipped;
        }
        OptionState::LinUcb { .. } => {
            // No-op; the feature-vector feedback path calls LinUcbState::update.
        }
    }
    // For Weighted variant, push complementary updates so weights sum-balance.
    if matches!(bucket.option_states[option], OptionState::Weighted { .. }) && n > 1 {
        let per_other = weighted_delta / (n - 1) as f64;
        for j in 0..n {
            if j == option { continue; }
            if let OptionState::Weighted { weight } = &mut bucket.option_states[j] {
                *weight = (*weight - per_other).clamp(0.01, 0.99);
            }
        }
    }
}

fn ensure_option_states_match_algorithm(bucket: &mut ContextBucket, config: &LearningConfig) {
    let target_kind = match config.algorithm {
        Algorithm::ThompsonSampling => "betaBernoulli",
        Algorithm::Ucb1 => "ucb",
        _ => "weighted",
    };
    let mismatch = bucket.option_states.iter().any(|s| match (s, target_kind) {
        (OptionState::Weighted { .. }, "weighted") => false,
        (OptionState::BetaBernoulli { .. }, "betaBernoulli") => false,
        (OptionState::Ucb { .. }, "ucb") => false,
        _ => true,
    });
    if !mismatch { return; }
    let n = bucket.option_states.len();
    bucket.option_states = (0..n).map(|i| match target_kind {
        "betaBernoulli" => OptionState::beta(1.0, 1.0),
        "ucb" => OptionState::ucb_initial(),
        _ => OptionState::weighted(bucket.weights.get(i).copied().unwrap_or(1.0 / n.max(1) as f64)),
    }).collect();
}

pub fn apply_feedback_signal(
    bucket: &mut ContextBucket,
    option: usize,
    observed: f64,
    signal_name: &str,
    config: &LearningConfig,
) -> Result<(), String> {
    if config.safety.freeze_learning { return Err("learning is frozen".into()); }
    let n = bucket.weights.len();
    if option >= n {
        return Err(format!("option {option} out of range ({n} options)"));
    }
    let signal = config.delayed_feedback.signals.iter()
        .find(|s| s.name == signal_name)
        .cloned()
        .ok_or_else(|| format!("unknown signal '{signal_name}'"))?;
    let clipped = if config.safety.reward_clip > 0.0 {
        observed.clamp(-config.safety.reward_clip, config.safety.reward_clip)
    } else { observed };
    if let Some(s) = bucket.stats.get_mut(option) {
        fuse_signal(s, clipped, &signal);
    }
    // Drive the rest of the learning path off the posterior mean.
    let effective_reward = bucket.stats.get(option).map(|s| s.posterior_mean).unwrap_or(clipped);
    apply_feedback(bucket, option, effective_reward, config)
}

/// Compute reward from outcome using reward policy.
pub fn compute_reward(outcome: &serde_json::Value, policy: &RewardPolicy) -> f64 {
    let mut total = 0.0;
    for (key, weight) in &policy.weights {
        if let Some(val) = outcome.get(key) {
            let v = if let Some(b) = val.as_bool() { if b { 1.0 } else { 0.0 } }
                else { val.as_f64().unwrap_or(0.0) };
            total += v * weight;
        }
    }
    total
}

pub fn compute_reward_from_components(
    reward_spec: &serde_json::Value,
    components: &serde_json::Value,
) -> f64 {
    let arr = match reward_spec.get("components").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return 0.0,
    };
    let mut total = 0.0;
    for c in arr {
        let name = match c.get("name").and_then(|v| v.as_str()) { Some(s) => s, None => continue };
        let weight = c.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let raw = match components.get(name).and_then(|v| v.as_f64()) {
            Some(v) => v,
            None => continue,
        };
        let norm = match c.get("normalize").and_then(|v| v.as_str()).unwrap_or("") {
            "minmax" => {
                let range = c.get("range").and_then(|v| v.as_array());
                if let Some(r) = range {
                    let lo = r.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let hi = r.get(1).and_then(|v| v.as_f64()).unwrap_or(1.0);
                    if (hi - lo).abs() > 1e-9 { (raw - lo) / (hi - lo) } else { 0.0 }
                } else { raw }
            }
            "budget" => {
                let b = c.get("budget").and_then(|v| v.as_f64()).unwrap_or(1.0);
                if b.abs() > 1e-9 { raw / b } else { 0.0 }
            }
            _ => raw,
        };
        total += weight * norm.clamp(0.0, 1.0);
    }
    total
}

/// Wall-clock decay (legacy). Called by maintenance, not the feedback path.
#[allow(dead_code)]
pub fn apply_decay(bucket: &mut ContextBucket, config: &DecayConfig) {
    if !config.enabled { return; }
    let now = now_secs();
    let half_life = config.half_life_seconds;
    if half_life <= 0.0 { return; }

    for s in &mut bucket.stats {
        if s.last_updated == 0 || s.tries == 0 { continue; }
        let age = (now - s.last_updated) as f64;
        let factor = (0.5_f64).powf(age / half_life);
        if factor >= 0.99 { continue; }
        s.reward_sum *= factor;
        s.reward_sq_sum *= factor;
        s.effective_tries *= factor;
        let decayed_tries = (s.tries as f64 * factor).round() as u64;
        s.tries = decayed_tries.max(1);
        s.successes = (s.successes as f64 * factor).round() as u64;
        s.failures = (s.failures as f64 * factor).round() as u64;
    }
}
