/// Lycan Learning Layer v1
///
/// Contextual strategy memory with bandit algorithms, confidence tracking,
/// reward shaping, and safety rails. Sidecar memory separate from graph code.
///
/// The LLM creates capsules. Lycan runs them cheaply. This layer makes
/// learning contextual, measurable, and safe.

use std::collections::HashMap;

// ── Learning config ──

#[derive(Debug, Clone)]
pub struct LearningConfig {
    pub algorithm: Algorithm,
    pub decay: DecayConfig,
    pub safety: SafetyConfig,
    pub reward_policy: Option<RewardPolicy>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Algorithm {
    SimpleWeighted,
    EpsilonGreedy { epsilon: f64 },
    Ucb1,
    ThompsonSampling,
    Softmax { temperature: f64 },
}

#[derive(Debug, Clone)]
pub struct DecayConfig {
    pub enabled: bool,
    pub half_life_seconds: f64,
}

#[derive(Debug, Clone)]
pub struct SafetyConfig {
    pub max_weight_delta_per_feedback: f64,
    pub min_exploration: f64,
    pub freeze_learning: bool,
}

#[derive(Debug, Clone)]
pub struct RewardPolicy {
    pub weights: HashMap<String, f64>,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::SimpleWeighted,
            decay: DecayConfig { enabled: false, half_life_seconds: 604800.0 },
            safety: SafetyConfig {
                max_weight_delta_per_feedback: 0.15,
                min_exploration: 0.02,
                freeze_learning: false,
            },
            reward_policy: None,
        }
    }
}

impl LearningConfig {
    pub fn from_json(json: &serde_json::Value) -> Self {
        let mut cfg = Self::default();

        if let Some(alg) = json.get("algorithm").and_then(|v| v.as_str()) {
            cfg.algorithm = match alg {
                "epsilonGreedy" => {
                    let eps = json.get("epsilon").and_then(|v| v.as_f64()).unwrap_or(0.1);
                    Algorithm::EpsilonGreedy { epsilon: eps }
                }
                "ucb1" => Algorithm::Ucb1,
                "thompsonSampling" => Algorithm::ThompsonSampling,
                "softmax" => {
                    let temp = json.get("temperature").and_then(|v| v.as_f64()).unwrap_or(1.0);
                    Algorithm::Softmax { temperature: temp }
                }
                _ => Algorithm::SimpleWeighted,
            };
        }

        if let Some(d) = json.get("decay") {
            cfg.decay.enabled = d.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.decay.half_life_seconds = d.get("halfLifeSeconds").and_then(|v| v.as_f64()).unwrap_or(604800.0);
        }

        if let Some(s) = json.get("safety") {
            cfg.safety.max_weight_delta_per_feedback = s.get("maxWeightDeltaPerFeedback").and_then(|v| v.as_f64()).unwrap_or(0.15);
            cfg.safety.min_exploration = s.get("minExploration").and_then(|v| v.as_f64()).unwrap_or(0.02);
            cfg.safety.freeze_learning = s.get("freezeLearning").and_then(|v| v.as_bool()).unwrap_or(false);
        }

        if let Some(rp) = json.get("rewardPolicy").and_then(|v| v.as_object()) {
            let mut weights = HashMap::new();
            for (k, v) in rp {
                if let Some(f) = v.as_f64() { weights.insert(k.clone(), f); }
            }
            if !weights.is_empty() { cfg.reward_policy = Some(RewardPolicy { weights }); }
        }

        cfg
    }

    pub fn to_json(&self) -> serde_json::Value {
        let alg_str = match &self.algorithm {
            Algorithm::SimpleWeighted => "simpleWeighted",
            Algorithm::EpsilonGreedy { .. } => "epsilonGreedy",
            Algorithm::Ucb1 => "ucb1",
            Algorithm::ThompsonSampling => "thompsonSampling",
            Algorithm::Softmax { .. } => "softmax",
        };
        let mut j = serde_json::json!({
            "algorithm": alg_str,
            "decay": {
                "enabled": self.decay.enabled,
                "halfLifeSeconds": self.decay.half_life_seconds
            },
            "safety": {
                "maxWeightDeltaPerFeedback": self.safety.max_weight_delta_per_feedback,
                "minExploration": self.safety.min_exploration,
                "freezeLearning": self.safety.freeze_learning
            }
        });
        match &self.algorithm {
            Algorithm::EpsilonGreedy { epsilon } => { j["epsilon"] = serde_json::json!(epsilon); }
            Algorithm::Softmax { temperature } => { j["temperature"] = serde_json::json!(temperature); }
            _ => {}
        }
        if let Some(ref rp) = self.reward_policy {
            j["rewardPolicy"] = serde_json::json!(rp.weights);
        }
        j
    }
}

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
}

impl Default for OptionStats {
    fn default() -> Self {
        Self { tries: 0, successes: 0, failures: 0, reward_sum: 0.0, reward_sq_sum: 0.0, last_reward: 0.0, last_updated: 0 }
    }
}

impl OptionStats {
    pub fn reward_mean(&self) -> f64 {
        if self.tries == 0 { 0.0 } else { self.reward_sum / self.tries as f64 }
    }

    pub fn reward_variance(&self) -> f64 {
        if self.tries < 2 { 0.0 }
        else {
            let mean = self.reward_mean();
            (self.reward_sq_sum / self.tries as f64) - mean * mean
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tries": self.tries,
            "successes": self.successes,
            "failures": self.failures,
            "rewardMean": (self.reward_mean() * 10000.0).round() / 10000.0,
            "rewardVariance": (self.reward_variance() * 10000.0).round() / 10000.0,
            "lastReward": self.last_reward,
            "lastUpdated": self.last_updated,
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Self {
        Self {
            tries: j.get("tries").and_then(|v| v.as_u64()).unwrap_or(0),
            successes: j.get("successes").and_then(|v| v.as_u64()).unwrap_or(0),
            failures: j.get("failures").and_then(|v| v.as_u64()).unwrap_or(0),
            reward_sum: j.get("rewardSum").and_then(|v| v.as_f64()).unwrap_or(0.0),
            reward_sq_sum: j.get("rewardSqSum").and_then(|v| v.as_f64()).unwrap_or(0.0),
            last_reward: j.get("lastReward").and_then(|v| v.as_f64()).unwrap_or(0.0),
            last_updated: j.get("lastUpdated").and_then(|v| v.as_u64()).unwrap_or(0),
        }
    }
}

// ── Strategy memory ──

#[derive(Debug, Clone)]
pub struct StrategyMemory {
    #[allow(dead_code)]
    pub node_id: u32,
    pub n_options: usize,
    pub contexts: HashMap<String, ContextBucket>,
}

#[derive(Debug, Clone)]
pub struct ContextBucket {
    pub weights: Vec<f64>,
    pub stats: Vec<OptionStats>,
    pub updated_at: u64,
}

// ── Capsule memory (sidecar) ──

#[derive(Debug, Clone)]
pub struct CapsuleMemory {
    pub strategies: HashMap<u32, StrategyMemory>,
    pub version: u32,
}

impl Default for CapsuleMemory {
    fn default() -> Self { Self { strategies: HashMap::new(), version: 1 } }
}

impl CapsuleMemory {
    pub fn to_json(&self) -> serde_json::Value {
        let mut strats = serde_json::Map::new();
        for (nid, sm) in &self.strategies {
            let mut contexts = serde_json::Map::new();
            for (ctx_key, bucket) in &sm.contexts {
                let stats_json: Vec<serde_json::Value> = bucket.stats.iter().map(|s| {
                    let mut sj = s.to_json();
                    sj.as_object_mut().map(|m| {
                        m.insert("rewardSum".into(), serde_json::json!(s.reward_sum));
                        m.insert("rewardSqSum".into(), serde_json::json!(s.reward_sq_sum));
                    });
                    sj
                }).collect();
                contexts.insert(ctx_key.clone(), serde_json::json!({
                    "weights": bucket.weights,
                    "stats": stats_json,
                    "updatedAt": bucket.updated_at,
                }));
            }
            strats.insert(nid.to_string(), serde_json::json!({
                "nodeId": nid,
                "nOptions": sm.n_options,
                "contexts": contexts,
            }));
        }
        serde_json::json!({ "version": self.version, "strategies": strats })
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
                        let weights: Vec<f64> = bucket_json.get("weights")
                            .and_then(|v| v.as_array())
                            .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
                            .unwrap_or_default();
                        let stats: Vec<OptionStats> = bucket_json.get("stats")
                            .and_then(|v| v.as_array())
                            .map(|a| a.iter().map(OptionStats::from_json).collect())
                            .unwrap_or_default();
                        let updated_at = bucket_json.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
                        contexts.insert(ctx_key.clone(), ContextBucket { weights, stats, updated_at });
                    }
                }
                mem.strategies.insert(nid, StrategyMemory { node_id: nid, n_options, contexts });
            }
        }
        mem
    }

    /// Get or create a context bucket for a strategy, initializing from graph weights.
    pub fn get_or_init_context(&mut self, node_id: u32, context_key: &str, graph_weights: &[f64], n_options: usize) -> &mut ContextBucket {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id, n_options, contexts: HashMap::new(),
        });
        sm.contexts.entry(context_key.to_string()).or_insert_with(|| {
            let weights = if graph_weights.len() >= n_options {
                graph_weights[..n_options].to_vec()
            } else {
                vec![1.0 / n_options as f64; n_options]
            };
            ContextBucket {
                weights,
                stats: (0..n_options).map(|_| OptionStats::default()).collect(),
                updated_at: 0,
            }
        })
    }

    #[allow(dead_code)]
    pub fn list_contexts(&self, node_id: u32) -> Vec<String> {
        self.strategies.get(&node_id)
            .map(|sm| sm.contexts.keys().cloned().collect())
            .unwrap_or_default()
    }
}

// ── Selection algorithms ──

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs()
}

/// Select an option using the configured algorithm. Returns (chosen_index, reason).
#[allow(dead_code)]
pub fn select_option(
    bucket: &ContextBucket,
    config: &LearningConfig,
    n_options: usize,
) -> (usize, String) {
    if n_options == 0 { return (0, "no options".into()); }
    if n_options == 1 { return (0, "single option".into()); }

    match &config.algorithm {
        Algorithm::SimpleWeighted => select_weighted(&bucket.weights, n_options),
        Algorithm::EpsilonGreedy { epsilon } => select_epsilon_greedy(&bucket.weights, &bucket.stats, n_options, *epsilon, config.safety.min_exploration),
        Algorithm::Ucb1 => select_ucb1(&bucket.stats, n_options),
        Algorithm::ThompsonSampling | Algorithm::Softmax { .. } => {
            // Fallback to weighted for unimplemented algorithms
            select_weighted(&bucket.weights, n_options)
        }
    }
}

#[allow(dead_code)]
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

#[allow(dead_code)]
fn select_epsilon_greedy(weights: &[f64], stats: &[OptionStats], n: usize, epsilon: f64, min_exploration: f64) -> (usize, String) {
    let eps = epsilon.max(min_exploration);
    if rand_f64() < eps {
        let idx = (rand_f64() * n as f64) as usize;
        (idx.min(n - 1), format!("epsilon-greedy explore (eps={eps:.2})"))
    } else {
        // Exploit: pick option with highest mean reward, falling back to weight
        let mut best = 0;
        let mut best_score = f64::NEG_INFINITY;
        for i in 0..n {
            let score = if stats.get(i).map(|s| s.tries > 0).unwrap_or(false) {
                stats[i].reward_mean()
            } else {
                weights.get(i).copied().unwrap_or(0.0)
            };
            if score > best_score { best_score = score; best = i; }
        }
        (best, "epsilon-greedy exploit".into())
    }
}

#[allow(dead_code)]
fn select_ucb1(stats: &[OptionStats], n: usize) -> (usize, String) {
    let total_tries: u64 = stats.iter().take(n).map(|s| s.tries).sum();
    if total_tries == 0 { return (0, "ucb1: no data, trying first".into()); }

    // Try untried options first
    for i in 0..n {
        if stats.get(i).map(|s| s.tries == 0).unwrap_or(true) {
            return (i, format!("ucb1: untried option {i}"));
        }
    }

    let log_total = (total_tries as f64).ln();
    let mut best = 0;
    let mut best_ucb = f64::NEG_INFINITY;
    for i in 0..n {
        if let Some(s) = stats.get(i) {
            let mean = s.reward_mean();
            let exploration = (2.0 * log_total / s.tries as f64).sqrt();
            let ucb = mean + exploration;
            if ucb > best_ucb { best_ucb = ucb; best = i; }
        }
    }
    (best, format!("ucb1: best upper bound {best_ucb:.4}"))
}

/// Apply feedback to a context bucket with safety rails.
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

    // Update stats
    if let Some(s) = bucket.stats.get_mut(option) {
        s.tries += 1;
        if reward > 0.0 { s.successes += 1; } else if reward < 0.0 { s.failures += 1; }
        s.reward_sum += reward;
        s.reward_sq_sum += reward * reward;
        s.last_reward = reward;
        s.last_updated = now_secs();
    }

    // Update weights with safety clamping
    let learning_rate = 0.05;
    let raw_delta = reward * learning_rate;
    let max_delta = config.safety.max_weight_delta_per_feedback;
    let delta = raw_delta.clamp(-max_delta, max_delta);

    for j in 0..n {
        if j == option {
            bucket.weights[j] = (bucket.weights[j] + delta).clamp(0.01, 0.99);
        } else if n > 1 {
            bucket.weights[j] = (bucket.weights[j] - delta / (n - 1) as f64).clamp(0.01, 0.99);
        }
    }

    // Enforce min exploration
    let min_w = config.safety.min_exploration / n as f64;
    for w in &mut bucket.weights {
        if *w < min_w { *w = min_w; }
    }

    // Normalize
    let sum: f64 = bucket.weights.iter().sum();
    if sum > 0.0 {
        for w in &mut bucket.weights { *w /= sum; }
    }

    bucket.updated_at = now_secs();
    Ok(())
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

/// Apply decay to stats based on time since last update.
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
        if factor >= 0.99 { continue; } // negligible decay
        s.reward_sum *= factor;
        s.reward_sq_sum *= factor;
        // Decay tries toward recent activity
        let decayed_tries = (s.tries as f64 * factor).round() as u64;
        s.tries = decayed_tries.max(1);
        s.successes = (s.successes as f64 * factor).round() as u64;
        s.failures = (s.failures as f64 * factor).round() as u64;
    }
}

// Simple pseudo-random for selection (no external dependency)
#[allow(dead_code)]
fn rand_f64() -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    (h.finish() % 10000) as f64 / 10000.0
}
