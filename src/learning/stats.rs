use std::collections::{HashMap, VecDeque};

// ── Option stats ──

#[derive(Debug, Clone)]
pub struct OptionStats {
    pub tries: u64,
    pub successes: u64,
    pub failures: u64,
    pub reward_sum: f64,
    pub reward_sq_sum: f64,
    pub last_reward: f64,
    pub last_updated: u64,
    pub effective_tries: f64,
    pub window: VecDeque<f64>,
    pub ph_cumsum: f64,
    pub ph_min: f64,
    pub change_boost_remaining: u32,
    pub change_points: u32,
    /// Posterior on latent true reward, fused across multi-signal feedback.
    pub posterior_mean: f64,
    pub posterior_var: f64,
    pub signal_counts: HashMap<String, u64>,
    pub surprise_recent: u32,
    pub objective_rewards: HashMap<String, f64>,
    pub objective_counts: HashMap<String, u64>,
}

impl Default for OptionStats {
    fn default() -> Self {
        Self {
            tries: 0,
            successes: 0,
            failures: 0,
            reward_sum: 0.0,
            reward_sq_sum: 0.0,
            last_reward: 0.0,
            last_updated: 0,
            effective_tries: 0.0,
            window: VecDeque::new(),
            ph_cumsum: 0.0,
            ph_min: 0.0,
            change_boost_remaining: 0,
            change_points: 0,
            posterior_mean: 0.0,
            posterior_var: 1.0,
            signal_counts: HashMap::new(),
            surprise_recent: 0,
            objective_rewards: HashMap::new(),
            objective_counts: HashMap::new(),
        }
    }
}

impl OptionStats {
    /// Cumulative reward mean (all-time, no decay).
    pub fn reward_mean(&self) -> f64 {
        if self.tries == 0 {
            0.0
        } else {
            self.reward_sum / self.tries as f64
        }
    }

    /// Decayed (effective) reward mean. Falls back to plain mean if no decay applied.
    pub fn reward_mean_decayed(&self) -> f64 {
        if self.effective_tries < 1e-9 {
            self.reward_mean()
        } else {
            self.reward_sum / self.effective_tries
        }
    }

    /// Windowed reward mean (last N rewards).
    pub fn reward_mean_windowed(&self) -> f64 {
        if self.window.is_empty() {
            return self.reward_mean();
        }
        let s: f64 = self.window.iter().sum();
        s / self.window.len() as f64
    }

    /// Trimmed mean of windowed rewards: drops `frac` from each tail.
    pub fn reward_mean_trimmed(&self, frac: f64) -> f64 {
        if self.window.is_empty() {
            return self.reward_mean();
        }
        if frac <= 0.0 {
            return self.reward_mean_windowed();
        }
        let mut sorted: Vec<f64> = self.window.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let trim = ((sorted.len() as f64) * frac).floor() as usize;
        let lo = trim;
        let hi = sorted.len().saturating_sub(trim);
        if hi <= lo {
            return self.reward_mean_windowed();
        }
        let slice = &sorted[lo..hi];
        slice.iter().sum::<f64>() / slice.len() as f64
    }

    pub fn reward_variance(&self) -> f64 {
        if self.tries < 2 {
            0.0
        } else {
            let mean = self.reward_mean();
            ((self.reward_sq_sum / self.tries as f64) - mean * mean).max(0.0)
        }
    }

    /// Sample-variance from the window if available, else cumulative variance.
    pub fn reward_variance_recent(&self) -> f64 {
        if self.window.len() < 2 {
            return self.reward_variance();
        }
        let mean = self.reward_mean_windowed();
        let n = self.window.len() as f64;
        let var = self.window.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        var.max(1e-6)
    }

    pub fn objective_mean(&self, name: &str) -> f64 {
        let count = *self.objective_counts.get(name).unwrap_or(&0);
        if count == 0 {
            return 0.0;
        }
        self.objective_rewards.get(name).copied().unwrap_or(0.0) / count as f64
    }

    pub fn record_objective(&mut self, name: &str, value: f64) {
        *self
            .objective_rewards
            .entry(name.to_string())
            .or_insert(0.0) += value;
        *self.objective_counts.entry(name.to_string()).or_insert(0) += 1;
    }

    /// CVaR_alpha (lower tail). Mean of the worst alpha-fraction of windowed rewards.
    /// Falls back to the windowed mean if the window is too small to compute.
    pub fn reward_cvar(&self, alpha: f64) -> f64 {
        if self.window.is_empty() {
            return self.reward_mean();
        }
        let a = alpha.clamp(0.01, 0.99);
        let mut sorted: Vec<f64> = self.window.iter().copied().collect();
        sorted.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        let k = (((n as f64) * a).ceil() as usize).max(1).min(n);
        let tail = &sorted[..k];
        tail.iter().sum::<f64>() / tail.len() as f64
    }

    pub fn to_json(&self) -> serde_json::Value {
        let window: Vec<f64> = self.window.iter().copied().collect();
        serde_json::json!({
            "tries": self.tries,
            "successes": self.successes,
            "failures": self.failures,
            "rewardMean": (self.reward_mean() * 10000.0).round() / 10000.0,
            "rewardMeanWindowed": (self.reward_mean_windowed() * 10000.0).round() / 10000.0,
            "rewardVariance": (self.reward_variance() * 10000.0).round() / 10000.0,
            "lastReward": self.last_reward,
            "lastUpdated": self.last_updated,
            "effectiveTries": (self.effective_tries * 100.0).round() / 100.0,
            "windowFill": self.window.len(),
            "changePoints": self.change_points,
            "changeBoostActive": self.change_boost_remaining > 0,
            "posteriorMean": (self.posterior_mean * 10000.0).round() / 10000.0,
            "posteriorVar": (self.posterior_var * 10000.0).round() / 10000.0,
            "signalCounts": self.signal_counts,
            "surpriseRecent": self.surprise_recent,
            "objectiveRewards": self.objective_rewards,
            "objectiveCounts": self.objective_counts,
            // Persistence-only fields; `serialize_bucket` overrides
            // rounded fields with unrounded values for the canonical path.
            "rewardSum": self.reward_sum,
            "rewardSqSum": self.reward_sq_sum,
            "window": window,
            "phCumsum": self.ph_cumsum,
            "phMin": self.ph_min,
            "changeBoostRemaining": self.change_boost_remaining,
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Self {
        let window: VecDeque<f64> = j
            .get("window")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
            .unwrap_or_default();
        let mut signal_counts = HashMap::new();
        if let Some(obj) = j.get("signalCounts").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(n) = v.as_u64() {
                    signal_counts.insert(k.clone(), n);
                }
            }
        }
        Self {
            tries: j.get("tries").and_then(|v| v.as_u64()).unwrap_or(0),
            successes: j.get("successes").and_then(|v| v.as_u64()).unwrap_or(0),
            failures: j.get("failures").and_then(|v| v.as_u64()).unwrap_or(0),
            reward_sum: j.get("rewardSum").and_then(|v| v.as_f64()).unwrap_or(0.0),
            reward_sq_sum: j.get("rewardSqSum").and_then(|v| v.as_f64()).unwrap_or(0.0),
            last_reward: j.get("lastReward").and_then(|v| v.as_f64()).unwrap_or(0.0),
            last_updated: j.get("lastUpdated").and_then(|v| v.as_u64()).unwrap_or(0),
            effective_tries: j
                .get("effectiveTries")
                .and_then(|v| v.as_f64())
                .unwrap_or_else(|| j.get("tries").and_then(|v| v.as_u64()).unwrap_or(0) as f64),
            window,
            ph_cumsum: j.get("phCumsum").and_then(|v| v.as_f64()).unwrap_or(0.0),
            ph_min: j.get("phMin").and_then(|v| v.as_f64()).unwrap_or(0.0),
            change_boost_remaining: j
                .get("changeBoostRemaining")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            change_points: j.get("changePoints").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            posterior_mean: j
                .get("posteriorMean")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            posterior_var: j
                .get("posteriorVar")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0),
            signal_counts,
            surprise_recent: j
                .get("surpriseRecent")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            objective_rewards: j
                .get("objectiveRewards")
                .and_then(|v| v.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                        .collect()
                })
                .unwrap_or_default(),
            objective_counts: j
                .get("objectiveCounts")
                .and_then(|v| v.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}
