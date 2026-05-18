/// Lycan learning layer. Per-context bandit memory, weight updates with safety
/// rails, reward shaping, change detection, delayed-feedback fusion, CVaR
/// scoring, corruption-robust UCB, and conformal prediction sets.

use std::collections::{HashMap, VecDeque};

use crate::change_detection::AdwinDetector;
use crate::feature_schema::ContextSpec;
use crate::linucb::LinUcbState;
use crate::meta_bandit::{CandidateId, MetaBandit};

// ── Learning config ──

#[derive(Debug, Clone)]
pub struct LearningConfig {
    pub algorithm: Algorithm,
    pub decay: DecayConfig,
    pub safety: SafetyConfig,
    pub window: WindowConfig,
    pub change_detection: ChangeDetectionConfig,
    pub reward_policy: Option<RewardPolicy>,
    pub learning_rate: f64,
    pub delayed_feedback: DelayedFeedbackConfig,
    pub risk_sensitive: RiskSensitiveConfig,
    pub corruption_robust: CorruptionRobustConfig,
    pub conformal: ConformalConfig,
    pub pareto: ParetoConfig,
    /// Context type declaration. Discrete (string contextKey) by default.
    /// Features (feature vector) enables the LinUcb candidate in the meta-bandit.
    pub context_spec: ContextSpec,
    pub refusal: RefusalConfig,
    pub action_space: ActionSpace,
    pub shared_state: SharedStateConfig,
}

/// Action-space declaration (Phase 3A).
///
/// Discrete (default) — the K options in the capsule YAML are the K choices.
/// Continuous — the K options are buckets over a continuous range; the
/// decide response surfaces both the chosen bucket index AND the bucket
/// midpoint (as `chosenAction`) so a downstream caller can apply the
/// chosen value directly without a separate lookup.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionSpace {
    Discrete,
    Continuous { range: [f64; 2], buckets: usize },
}

impl Default for ActionSpace {
    fn default() -> Self { ActionSpace::Discrete }
}

impl ActionSpace {
    /// For a continuous action space with K buckets over [lo, hi], return
    /// the midpoint of bucket `i` (0-indexed). Returns None for the
    /// Discrete variant or an out-of-bounds index.
    pub fn bucket_midpoint(&self, i: usize) -> Option<f64> {
        match self {
            ActionSpace::Discrete => None,
            ActionSpace::Continuous { range, buckets } => {
                if *buckets == 0 || i >= *buckets { return None; }
                let lo = range[0];
                let hi = range[1];
                let width = (hi - lo) / (*buckets as f64);
                Some(lo + width * (i as f64 + 0.5))
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RefusalConfig {
    pub enabled: bool,
    pub coverage: f64,
    pub max_interval_width: f64,
    pub ood_threshold: f64,
}

impl Default for RefusalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            coverage: 0.95,
            max_interval_width: 0.5,
            ood_threshold: 0.8,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Algorithm {
    SimpleWeighted,
    EpsilonGreedy { epsilon: f64 },
    Ucb1,
    /// Gaussian Thompson sampling on the posterior mean reward.
    /// Vanilla Beta-Bernoulli would require binary rewards; Lycan rewards
    /// are continuous so we use a Normal posterior with sample variance.
    ThompsonSampling,
    Softmax { temperature: f64 },
}

#[derive(Debug, Clone)]
pub struct DecayConfig {
    pub enabled: bool,
    /// Half-life applied per-feedback (count-based, not wall-clock).
    /// At half_life=N, stats from N feedbacks ago weigh half as much.
    /// Wall-clock half-life is also supported via half_life_seconds for
    /// long-lived capsules with irregular traffic.
    pub half_life_feedbacks: f64,
    pub half_life_seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Greedy,
    Weighted,
    EpsilonGreedy,
}

#[derive(Debug, Clone)]
pub struct SafetyConfig {
    pub max_weight_delta_per_feedback: f64,
    pub min_exploration: f64,
    pub freeze_learning: bool,
    pub reward_clip: f64,
    pub trimmed_fraction: f64,
    pub snapshot_on_feedback: bool,
    pub journal_on_feedback: bool,
    pub selection_mode: SelectionMode,
    pub selection_epsilon: f64,
    /// Geometric forgetting factor for candidate-bucket OptionState
    /// (Beta α/β decayed toward (1,1) prior; UCB tries/total_reward
    /// decayed proportionally). 1.0 = no decay (legacy behavior).
    /// 0.999 ≈ 700-event half-life.
    pub option_state_forgetting: f64,
    /// ADWIN `delta` for the capsule-level change detector
    /// (`WarmupState::detector`). Smaller = stricter (wider Hoeffding
    /// bound, slower to fire). Tuned so the capsule-level detector
    /// fires AFTER the per-context detector on narrow drift, since
    /// operators expect per-context drift to be flagged first.
    /// Default `0.0005` was chosen from synthetic characterization in
    /// `tests/change_detection_characterization.rs` and is "best
    /// available" — real workloads may need adjustment.
    pub capsule_adwin_delta: f64,
    /// ADWIN `delta` for per-(node_id, context_key) detectors in
    /// `StrategyMemory::context_detectors`. Looser than the capsule-
    /// level value so a single bucket going bad fires first. Default
    /// `0.002` (carried forward from the previous single-delta
    /// regime; characterization confirmed it stays inside the 5%
    /// false-positive bar on synthetic Gaussian streams).
    pub context_adwin_delta: f64,
}

#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub enabled: bool,
    /// Keep only the last N rewards per option. Used for windowed mean,
    /// trimmed mean, and change detection.
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeDetectionMethod {
    PageHinkley,
    ModelSurprise,
}

#[derive(Debug, Clone)]
pub struct ChangeDetectionConfig {
    pub enabled: bool,
    pub threshold: f64,
    pub min_drift: f64,
    pub exploration_boost: f64,
    pub boost_duration: u32,
    pub method: ChangeDetectionMethod,
    pub surprise_k_sigma: f64,
    pub surprise_fraction_threshold: f64,
}

#[derive(Debug, Clone)]
pub struct DelayedFeedbackConfig {
    pub enabled: bool,
    pub signals: Vec<DelayedSignalSpec>,
}

#[derive(Debug, Clone)]
pub struct DelayedSignalSpec {
    pub name: String,
    pub noise_variance: f64,
    pub bias: f64,
}

#[derive(Debug, Clone)]
pub struct RiskSensitiveConfig {
    pub enabled: bool,
    pub alpha: f64,
    pub blend: f64,
}

#[derive(Debug, Clone)]
pub struct CorruptionRobustConfig {
    pub enabled: bool,
    pub budget: f64,
}

#[derive(Debug, Clone)]
pub struct ConformalConfig {
    pub enabled: bool,
    pub coverage: f64,
    pub calibration_size: usize,
}

#[derive(Debug, Clone)]
pub struct ParetoConfig {
    pub enabled: bool,
    pub objectives: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RewardPolicy {
    pub weights: HashMap<String, f64>,
}

/// Score kind for shared-state LinUCB scoring. UCB is the default and
/// what the math-layer tests have been validated against. LinTS is
/// supported as a Cholesky-sampled variant from `LinUcbSharedState`;
/// it falls back to the posterior mean on Cholesky failure (still
/// well-typed but non-Thompson for that call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedStateScoreKind {
    Ucb,
    LinTs,
}

impl Default for SharedStateScoreKind {
    fn default() -> Self { SharedStateScoreKind::Ucb }
}

/// Capsule-level configuration for the shared-state LinUCB strategy.
///
/// When `enabled = true`, `do_decide` routes selection through
/// `SharedStateOptionStrategy` instead of the per-option LinUcb path,
/// and `do_feedback` calls `apply_feedback` against the shared theta
/// vector.
///
/// `option_features` carries the action-feature vectors keyed by the
/// option name as it appears in the capsule's `(choice ...)` node.
/// `d_option` must match every entry's length; `d_context` must match
/// the encoded length of the capsule's `contextSpec`.
///
/// Numerical stability: `LinUcbSharedState` already clamps the
/// exploration bonus at `10 * alpha` and falls back to the posterior
/// mean on non-finite intermediate values. See linucb.rs for the
/// math-layer guards.
#[derive(Debug, Clone)]
pub struct SharedStateConfig {
    pub enabled: bool,
    pub d_context: usize,
    pub d_option: usize,
    pub lambda: f64,
    pub alpha: f64,
    pub score_kind: SharedStateScoreKind,
    /// Option name → action-feature vector. Order matches the capsule's
    /// `options[]`. Empty when `enabled = false`.
    pub option_features: std::collections::BTreeMap<String, Vec<f64>>,
}

impl Default for SharedStateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            d_context: 0,
            d_option: 0,
            lambda: 1.0,
            alpha: 1.0,
            score_kind: SharedStateScoreKind::Ucb,
            option_features: std::collections::BTreeMap::new(),
        }
    }
}

/// Default ADWIN `delta` for the capsule-level detector. Chosen from
/// synthetic characterization in
/// `tests/change_detection_characterization.rs` — best available, not
/// a production calibration. See `Syntra/docs/known-issues.md`.
pub fn default_capsule_adwin_delta() -> f64 { 0.0005 }

/// Default ADWIN `delta` for per-(node_id, context_key) detectors.
/// Looser than the capsule-level value so narrow drift in a single
/// context bucket is flagged first.
pub fn default_context_adwin_delta() -> f64 { 0.002 }

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            algorithm: Algorithm::SimpleWeighted,
            decay: DecayConfig {
                enabled: false,
                half_life_feedbacks: 200.0,
                half_life_seconds: 604800.0,
            },
            safety: SafetyConfig {
                max_weight_delta_per_feedback: 0.15,
                min_exploration: 0.02,
                freeze_learning: false,
                reward_clip: 2.0,
                trimmed_fraction: 0.0,
                snapshot_on_feedback: true,
                journal_on_feedback: true,
                selection_mode: SelectionMode::Greedy,
                selection_epsilon: 0.10,
                option_state_forgetting: 0.999,
                capsule_adwin_delta: default_capsule_adwin_delta(),
                context_adwin_delta: default_context_adwin_delta(),
            },
            window: WindowConfig { enabled: false, size: 100 },
            change_detection: ChangeDetectionConfig {
                enabled: false,
                threshold: 5.0,
                min_drift: 0.05,
                exploration_boost: 0.25,
                boost_duration: 50,
                method: ChangeDetectionMethod::PageHinkley,
                surprise_k_sigma: 2.5,
                surprise_fraction_threshold: 0.30,
            },
            reward_policy: None,
            learning_rate: 0.05,
            delayed_feedback: DelayedFeedbackConfig { enabled: false, signals: vec![] },
            risk_sensitive: RiskSensitiveConfig { enabled: false, alpha: 0.10, blend: 0.3 },
            corruption_robust: CorruptionRobustConfig { enabled: false, budget: 0.0 },
            conformal: ConformalConfig { enabled: false, coverage: 0.90, calibration_size: 100 },
            pareto: ParetoConfig { enabled: false, objectives: vec![] },
            context_spec: ContextSpec::default(),
            refusal: RefusalConfig::default(),
            action_space: ActionSpace::default(),
            shared_state: SharedStateConfig::default(),
        }
    }
}

impl LearningConfig {
    pub fn from_json(json: &serde_json::Value) -> Self {
        let mut cfg = Self::default();

        if let Some(mode) = json.get("mode").and_then(|v| v.as_str()) {
            if mode == "highThroughput" {
                cfg.safety.snapshot_on_feedback = false;
                cfg.safety.journal_on_feedback = false;
            } else if mode == "highAssurance" {
                cfg.safety.snapshot_on_feedback = true;
                cfg.safety.journal_on_feedback = true;
                cfg.safety.reward_clip = 1.0;
            }
        }

        if let Some(alg) = json.get("algorithm").and_then(|v| v.as_str()) {
            cfg.algorithm = match alg {
                "epsilonGreedy" => {
                    let eps = json.get("epsilon").and_then(|v| v.as_f64()).unwrap_or(0.1);
                    Algorithm::EpsilonGreedy { epsilon: eps }
                }
                "ucb1" => Algorithm::Ucb1,
                "thompsonSampling" | "thompson" => Algorithm::ThompsonSampling,
                "softmax" => {
                    let temp = json.get("temperature").and_then(|v| v.as_f64()).unwrap_or(1.0);
                    Algorithm::Softmax { temperature: temp }
                }
                _ => Algorithm::SimpleWeighted,
            };
        }

        if let Some(lr) = json.get("learningRate").and_then(|v| v.as_f64()) {
            cfg.learning_rate = lr.clamp(0.0001, 0.5);
        }

        if let Some(d) = json.get("decay") {
            cfg.decay.enabled = d.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.decay.half_life_feedbacks = d
                .get("halfLifeFeedbacks").and_then(|v| v.as_f64()).unwrap_or(200.0);
            cfg.decay.half_life_seconds = d
                .get("halfLifeSeconds").and_then(|v| v.as_f64()).unwrap_or(604800.0);
        }

        if let Some(s) = json.get("safety") {
            cfg.safety.max_weight_delta_per_feedback = s
                .get("maxWeightDeltaPerFeedback").and_then(|v| v.as_f64()).unwrap_or(0.15);
            cfg.safety.min_exploration = s
                .get("minExploration").and_then(|v| v.as_f64()).unwrap_or(0.02);
            cfg.safety.freeze_learning = s
                .get("freezeLearning").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.safety.reward_clip = s
                .get("rewardClip").and_then(|v| v.as_f64()).unwrap_or(2.0);
            cfg.safety.trimmed_fraction = s
                .get("trimmedFraction").and_then(|v| v.as_f64()).unwrap_or(0.0).clamp(0.0, 0.49);
            cfg.safety.snapshot_on_feedback = s
                .get("snapshotOnFeedback").and_then(|v| v.as_bool()).unwrap_or(true);
            cfg.safety.journal_on_feedback = s
                .get("journalOnFeedback").and_then(|v| v.as_bool()).unwrap_or(true);
            cfg.safety.selection_mode = match s.get("selectionMode").and_then(|v| v.as_str()) {
                Some("weighted") => SelectionMode::Weighted,
                Some("epsilonGreedy") => SelectionMode::EpsilonGreedy,
                _ => SelectionMode::Greedy,
            };
            cfg.safety.selection_epsilon = s
                .get("selectionEpsilon").and_then(|v| v.as_f64()).unwrap_or(0.10)
                .clamp(0.0, 0.5);
            cfg.safety.option_state_forgetting = s
                .get("optionStateForgetting").and_then(|v| v.as_f64()).unwrap_or(0.999)
                .clamp(0.0, 1.0);

            // ADWIN deltas. `adwinDelta` (legacy single-delta key) is
            // accepted as a fallback for both layers so older configs
            // that pre-date the two-layer split still load; if the
            // explicit keys are present they take precedence.
            let legacy = s.get("adwinDelta").and_then(|v| v.as_f64());
            cfg.safety.capsule_adwin_delta = s
                .get("capsuleAdwinDelta").and_then(|v| v.as_f64())
                .or(legacy)
                .unwrap_or(default_capsule_adwin_delta())
                .clamp(1e-9, 0.5);
            cfg.safety.context_adwin_delta = s
                .get("contextAdwinDelta").and_then(|v| v.as_f64())
                .or(legacy)
                .unwrap_or(default_context_adwin_delta())
                .clamp(1e-9, 0.5);
        }

        if let Some(w) = json.get("window") {
            cfg.window.enabled = w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.window.size = w
                .get("size").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            if cfg.window.size == 0 { cfg.window.size = 1; }
        }

        if let Some(c) = json.get("changeDetection") {
            cfg.change_detection.enabled = c
                .get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.change_detection.threshold = c
                .get("threshold").and_then(|v| v.as_f64()).unwrap_or(5.0);
            cfg.change_detection.min_drift = c
                .get("minDrift").and_then(|v| v.as_f64()).unwrap_or(0.05);
            cfg.change_detection.exploration_boost = c
                .get("explorationBoost").and_then(|v| v.as_f64()).unwrap_or(0.25);
            cfg.change_detection.boost_duration = c
                .get("boostDuration").and_then(|v| v.as_u64()).unwrap_or(50) as u32;
            cfg.change_detection.method = match c.get("method").and_then(|v| v.as_str()) {
                Some("modelSurprise") => ChangeDetectionMethod::ModelSurprise,
                _ => ChangeDetectionMethod::PageHinkley,
            };
            cfg.change_detection.surprise_k_sigma = c
                .get("surpriseKSigma").and_then(|v| v.as_f64()).unwrap_or(2.5);
            cfg.change_detection.surprise_fraction_threshold = c
                .get("surpriseFractionThreshold").and_then(|v| v.as_f64()).unwrap_or(0.30);
        }

        if let Some(df) = json.get("delayedFeedback") {
            cfg.delayed_feedback.enabled = df.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            if let Some(sigs) = df.get("signals").and_then(|v| v.as_array()) {
                cfg.delayed_feedback.signals = sigs.iter().filter_map(|s| {
                    let name = s.get("name").and_then(|v| v.as_str())?.to_string();
                    let noise_variance = s.get("noiseVariance").and_then(|v| v.as_f64()).unwrap_or(1.0).max(1e-6);
                    let bias = s.get("bias").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    Some(DelayedSignalSpec { name, noise_variance, bias })
                }).collect();
            }
        }

        if let Some(r) = json.get("riskSensitive") {
            cfg.risk_sensitive.enabled = r.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.risk_sensitive.alpha = r.get("alpha").and_then(|v| v.as_f64()).unwrap_or(0.10).clamp(0.01, 0.99);
            cfg.risk_sensitive.blend = r.get("blend").and_then(|v| v.as_f64()).unwrap_or(0.30).clamp(0.0, 1.0);
        }

        if let Some(cr) = json.get("corruptionRobust") {
            cfg.corruption_robust.enabled = cr.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.corruption_robust.budget = cr.get("budget").and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);
        }

        if let Some(cf) = json.get("conformal") {
            cfg.conformal.enabled = cf.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            cfg.conformal.coverage = cf.get("coverage").and_then(|v| v.as_f64()).unwrap_or(0.90).clamp(0.50, 0.999);
            cfg.conformal.calibration_size = cf.get("calibrationSize").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            if cfg.conformal.calibration_size < 10 { cfg.conformal.calibration_size = 10; }
        }

        if let Some(p) = json.get("pareto") {
            cfg.pareto.enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            if let Some(objs) = p.get("objectives").and_then(|v| v.as_array()) {
                cfg.pareto.objectives = objs.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
        }

        if let Some(rp) = json.get("rewardPolicy").and_then(|v| v.as_object()) {
            let mut weights = HashMap::new();
            for (k, v) in rp {
                if let Some(f) = v.as_f64() { weights.insert(k.clone(), f); }
            }
            if !weights.is_empty() { cfg.reward_policy = Some(RewardPolicy { weights }); }
        }

        if let Some(cs) = json.get("contextSpec") {
            if let Ok(spec) = serde_json::from_value::<ContextSpec>(cs.clone()) {
                cfg.context_spec = spec;
            }
        }

        if let Some(asp) = json.get("actionSpace") {
            if let Ok(parsed) = serde_json::from_value::<ActionSpace>(asp.clone()) {
                cfg.action_space = parsed;
            }
        }

        if let Some(rf) = json.get("refusal") {
            let parsed: RefusalConfig = serde_json::from_value(rf.clone())
                .unwrap_or_default();
            cfg.refusal = RefusalConfig {
                enabled: parsed.enabled,
                coverage: parsed.coverage.clamp(0.50, 0.999),
                max_interval_width: parsed.max_interval_width.max(0.0),
                ood_threshold: parsed.ood_threshold.clamp(0.0, 10.0),
            };
        }

        if let Some(ss) = json.get("sharedState").and_then(|v| v.as_object()) {
            let enabled = ss.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            if enabled {
                let d_context = ss.get("dContext").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let d_option = ss.get("dOption").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let lambda = ss.get("lambda").and_then(|v| v.as_f64()).unwrap_or(1.0).max(1e-9);
                let alpha = ss.get("alpha").and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.0);
                let score_kind = match ss.get("scoreKind").and_then(|v| v.as_str()) {
                    Some("lin_ts") | Some("linTs") | Some("LinTs") => SharedStateScoreKind::LinTs,
                    _ => SharedStateScoreKind::Ucb,
                };
                let mut option_features = std::collections::BTreeMap::new();
                if let Some(of) = ss.get("optionFeatures").and_then(|v| v.as_object()) {
                    for (k, v) in of {
                        if let Some(arr) = v.as_array() {
                            let vec: Vec<f64> = arr.iter().filter_map(|x| x.as_f64()).collect();
                            // Silently drop entries whose dimension mismatches; the
                            // validation gate runs at install time via the wrapper's
                            // `validate()` call from the server's install handler.
                            if vec.len() == d_option {
                                option_features.insert(k.clone(), vec);
                            }
                        }
                    }
                }
                cfg.shared_state = SharedStateConfig {
                    enabled: true,
                    d_context,
                    d_option,
                    lambda,
                    alpha,
                    score_kind,
                    option_features,
                };
            }
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
            "learningRate": self.learning_rate,
            "decay": {
                "enabled": self.decay.enabled,
                "halfLifeFeedbacks": self.decay.half_life_feedbacks,
                "halfLifeSeconds": self.decay.half_life_seconds
            },
            "safety": {
                "maxWeightDeltaPerFeedback": self.safety.max_weight_delta_per_feedback,
                "minExploration": self.safety.min_exploration,
                "freezeLearning": self.safety.freeze_learning,
                "rewardClip": self.safety.reward_clip,
                "trimmedFraction": self.safety.trimmed_fraction,
                "snapshotOnFeedback": self.safety.snapshot_on_feedback,
                "journalOnFeedback": self.safety.journal_on_feedback,
                "selectionMode": match self.safety.selection_mode {
                    SelectionMode::Greedy => "greedy",
                    SelectionMode::Weighted => "weighted",
                    SelectionMode::EpsilonGreedy => "epsilonGreedy",
                },
                "selectionEpsilon": self.safety.selection_epsilon,
                "optionStateForgetting": self.safety.option_state_forgetting,
                "capsuleAdwinDelta": self.safety.capsule_adwin_delta,
                "contextAdwinDelta": self.safety.context_adwin_delta
            },
            "window": {
                "enabled": self.window.enabled,
                "size": self.window.size
            },
            "changeDetection": {
                "enabled": self.change_detection.enabled,
                "threshold": self.change_detection.threshold,
                "minDrift": self.change_detection.min_drift,
                "explorationBoost": self.change_detection.exploration_boost,
                "boostDuration": self.change_detection.boost_duration,
                "method": match self.change_detection.method {
                    ChangeDetectionMethod::PageHinkley => "pageHinkley",
                    ChangeDetectionMethod::ModelSurprise => "modelSurprise",
                },
                "surpriseKSigma": self.change_detection.surprise_k_sigma,
                "surpriseFractionThreshold": self.change_detection.surprise_fraction_threshold,
            },
            "delayedFeedback": {
                "enabled": self.delayed_feedback.enabled,
                "signals": self.delayed_feedback.signals.iter().map(|s| serde_json::json!({
                    "name": s.name,
                    "noiseVariance": s.noise_variance,
                    "bias": s.bias,
                })).collect::<Vec<_>>(),
            },
            "riskSensitive": {
                "enabled": self.risk_sensitive.enabled,
                "alpha": self.risk_sensitive.alpha,
                "blend": self.risk_sensitive.blend,
            },
            "corruptionRobust": {
                "enabled": self.corruption_robust.enabled,
                "budget": self.corruption_robust.budget,
            },
            "conformal": {
                "enabled": self.conformal.enabled,
                "coverage": self.conformal.coverage,
                "calibrationSize": self.conformal.calibration_size,
            },
            "pareto": {
                "enabled": self.pareto.enabled,
                "objectives": self.pareto.objectives,
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
        j["contextSpec"] = serde_json::to_value(&self.context_spec)
            .unwrap_or(serde_json::Value::Null);
        j["refusal"] = serde_json::to_value(&self.refusal)
            .unwrap_or(serde_json::Value::Null);
        j["actionSpace"] = serde_json::to_value(&self.action_space)
            .unwrap_or(serde_json::Value::Null);
        let mut option_features = serde_json::Map::new();
        for (k, v) in &self.shared_state.option_features {
            option_features.insert(k.clone(), serde_json::json!(v));
        }
        j["sharedState"] = serde_json::json!({
            "enabled": self.shared_state.enabled,
            "dContext": self.shared_state.d_context,
            "dOption": self.shared_state.d_option,
            "lambda": self.shared_state.lambda,
            "alpha": self.shared_state.alpha,
            "scoreKind": match self.shared_state.score_kind {
                SharedStateScoreKind::Ucb => "ucb",
                SharedStateScoreKind::LinTs => "lin_ts",
            },
            "optionFeatures": serde_json::Value::Object(option_features),
        });
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
    pub effective_tries: f64,
    pub window: VecDeque<f64>,
    pub ph_cumsum: f64,
    pub ph_min: f64,
    pub change_boost_remaining: u32,
    pub change_points: u32,
    /// Posterior on latent true reward, fused across multi-signal feedback.
    /// posterior_mean defaults to 0, posterior_var to 1.0 (uninformative prior).
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
        if self.tries == 0 { 0.0 } else { self.reward_sum / self.tries as f64 }
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
        if self.window.is_empty() { return self.reward_mean(); }
        let s: f64 = self.window.iter().sum();
        s / self.window.len() as f64
    }

    /// Trimmed mean of windowed rewards: drops `frac` from each tail.
    pub fn reward_mean_trimmed(&self, frac: f64) -> f64 {
        if self.window.is_empty() { return self.reward_mean(); }
        if frac <= 0.0 { return self.reward_mean_windowed(); }
        let mut sorted: Vec<f64> = self.window.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let trim = ((sorted.len() as f64) * frac).floor() as usize;
        let lo = trim;
        let hi = sorted.len().saturating_sub(trim);
        if hi <= lo { return self.reward_mean_windowed(); }
        let slice = &sorted[lo..hi];
        slice.iter().sum::<f64>() / slice.len() as f64
    }

    pub fn reward_variance(&self) -> f64 {
        if self.tries < 2 { 0.0 }
        else {
            let mean = self.reward_mean();
            ((self.reward_sq_sum / self.tries as f64) - mean * mean).max(0.0)
        }
    }

    /// Sample-variance from the window if available, else cumulative variance.
    pub fn reward_variance_recent(&self) -> f64 {
        if self.window.len() < 2 { return self.reward_variance(); }
        let mean = self.reward_mean_windowed();
        let n = self.window.len() as f64;
        let var = self.window.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        var.max(1e-6)
    }

    pub fn objective_mean(&self, name: &str) -> f64 {
        let count = *self.objective_counts.get(name).unwrap_or(&0);
        if count == 0 { return 0.0; }
        self.objective_rewards.get(name).copied().unwrap_or(0.0) / count as f64
    }

    pub fn record_objective(&mut self, name: &str, value: f64) {
        *self.objective_rewards.entry(name.to_string()).or_insert(0.0) += value;
        *self.objective_counts.entry(name.to_string()).or_insert(0) += 1;
    }

    /// CVaR_alpha (lower tail). Mean of the worst alpha-fraction of windowed rewards.
    /// Falls back to the windowed mean if the window is too small to compute.
    pub fn reward_cvar(&self, alpha: f64) -> f64 {
        if self.window.is_empty() { return self.reward_mean(); }
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
            // Persistence-only fields. `serialize_bucket` (the legacy
            // canonical-persistence path) also injects these, overriding the
            // rounded `effectiveTries` above with the unrounded value. Direct
            // callers of `to_json` followed by `from_json` (e.g.
            // `hierarchical_state` persistence) get a faithful round-trip
            // with two-decimal precision loss on `effectiveTries` only.
            "rewardSum": self.reward_sum,
            "rewardSqSum": self.reward_sq_sum,
            "window": window,
            "phCumsum": self.ph_cumsum,
            "phMin": self.ph_min,
            "changeBoostRemaining": self.change_boost_remaining,
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Self {
        let window: VecDeque<f64> = j.get("window")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
            .unwrap_or_default();
        let mut signal_counts = HashMap::new();
        if let Some(obj) = j.get("signalCounts").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(n) = v.as_u64() { signal_counts.insert(k.clone(), n); }
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
            effective_tries: j.get("effectiveTries").and_then(|v| v.as_f64())
                .unwrap_or_else(|| j.get("tries").and_then(|v| v.as_u64()).unwrap_or(0) as f64),
            window,
            ph_cumsum: j.get("phCumsum").and_then(|v| v.as_f64()).unwrap_or(0.0),
            ph_min: j.get("phMin").and_then(|v| v.as_f64()).unwrap_or(0.0),
            change_boost_remaining: j.get("changeBoostRemaining").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            change_points: j.get("changePoints").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            posterior_mean: j.get("posteriorMean").and_then(|v| v.as_f64()).unwrap_or(0.0),
            posterior_var: j.get("posteriorVar").and_then(|v| v.as_f64()).unwrap_or(1.0),
            signal_counts,
            surprise_recent: j.get("surpriseRecent").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            objective_rewards: j.get("objectiveRewards").and_then(|v| v.as_object())
                .map(|o| o.iter().filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f))).collect())
                .unwrap_or_default(),
            objective_counts: j.get("objectiveCounts").and_then(|v| v.as_object())
                .map(|o| o.iter().filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n))).collect())
                .unwrap_or_default(),
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
    /// Per-candidate per-context buckets. Each candidate algorithm maintains
    /// independent state. Keyed by (candidate, context_key).
    pub candidate_contexts: HashMap<(CandidateId, String), ContextBucket>,
    /// Per-(node_id) meta-bandit. None until first Active decision against this node.
    pub meta_bandit: Option<MetaBandit>,
    /// Per-context change detectors. Independent from the capsule-level detector
    /// in WarmupState — detects narrow drift within a single context without
    /// triggering a full re-warmup.
    pub context_detectors: HashMap<String, AdwinDetector>,
    /// Per-(node_id) OOD detector for discrete contexts. Created on first use.
    pub discrete_ood: Option<crate::ood::DiscreteOodDetector>,
    /// Per-(node_id) OOD detector for feature-vector contexts. Created lazily
    /// with the dimension from the capsule's context spec.
    pub feature_ood: Option<crate::ood::FeatureOodDetector>,
}

#[derive(Debug, Clone)]
pub enum OptionState {
    Weighted { weight: f64 },
    BetaBernoulli { alpha: f64, beta: f64 },
    /// `tries` is a soft count: increments by 1 per pull but decays under
    /// geometric forgetting alongside `total_reward`, so their ratio reflects
    /// recent-weighted mean.
    Ucb { tries: f64, total_reward: f64 },
    LinUcb { state: LinUcbState },
}

impl OptionState {
    pub fn weighted(w: f64) -> Self { Self::Weighted { weight: w } }
    pub fn beta(alpha: f64, beta: f64) -> Self { Self::BetaBernoulli { alpha, beta } }
    pub fn ucb_initial() -> Self { Self::Ucb { tries: 0.0, total_reward: 0.0 } }
    pub fn linucb_initial(d: usize, lambda: f64) -> Self {
        Self::LinUcb { state: LinUcbState::new(d, lambda) }
    }

    pub fn as_visible_weight(&self) -> f64 {
        match self {
            Self::Weighted { weight } => *weight,
            Self::BetaBernoulli { alpha, beta } => alpha / (alpha + beta).max(1e-9),
            Self::Ucb { tries, total_reward } => {
                if *tries < 1e-9 { 0.5 } else { (total_reward / *tries).clamp(0.0, 1.0) }
            }
            Self::LinUcb { state } => {
                // L2 norm of theta — rough "how strongly does this option
                // respond to features" indicator. Real selection uses
                // ucb_score on actual feature vectors.
                let theta = state.theta();
                theta.iter().map(|v| v * v).sum::<f64>().sqrt()
            }
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Weighted { weight } => serde_json::json!({ "kind": "weighted", "weight": weight }),
            Self::BetaBernoulli { alpha, beta } => serde_json::json!({
                "kind": "betaBernoulli", "alpha": alpha, "beta": beta,
            }),
            Self::Ucb { tries, total_reward } => serde_json::json!({
                "kind": "ucb", "tries": tries, "totalReward": total_reward,
            }),
            Self::LinUcb { state } => serde_json::json!({
                "kind": "linucb",
                "d": state.d,
                "lambda": state.lambda,
                "aInv": state.a_inv,
                "a": state.a,
                "b": state.b,
                "sinceLastRebuild": state.since_last_rebuild,
            }),
        }
    }

    pub fn from_json(j: &serde_json::Value) -> Option<Self> {
        let kind = j.get("kind")?.as_str()?;
        match kind {
            "weighted" => Some(Self::Weighted { weight: j.get("weight")?.as_f64()? }),
            "betaBernoulli" => Some(Self::BetaBernoulli {
                alpha: j.get("alpha")?.as_f64()?,
                beta: j.get("beta")?.as_f64()?,
            }),
            "ucb" => Some(Self::Ucb {
                tries: j.get("tries").and_then(|v| v.as_f64()).unwrap_or(0.0),
                total_reward: j.get("totalReward")?.as_f64()?,
            }),
            "linucb" => {
                let d = j.get("d")?.as_u64()? as usize;
                let lambda = j.get("lambda")?.as_f64()?;
                let a_inv: Vec<Vec<f64>> =
                    serde_json::from_value(j.get("aInv")?.clone()).ok()?;
                let a: Vec<Vec<f64>> =
                    serde_json::from_value(j.get("a")?.clone()).ok()?;
                let b: Vec<f64> =
                    serde_json::from_value(j.get("b")?.clone()).ok()?;
                let since_last_rebuild = j.get("sinceLastRebuild")
                    .and_then(|v| v.as_u64()).unwrap_or(0);
                Some(Self::LinUcb {
                    state: LinUcbState { a_inv, a, b, d, lambda, since_last_rebuild },
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextBucket {
    pub weights: Vec<f64>,
    pub stats: Vec<OptionStats>,
    pub updated_at: u64,
    pub option_states: Vec<OptionState>,
    /// Split-conformal calibrator over absolute residuals between predicted
    /// (option-state visible weight or LinUcb θ·x) and observed reward. Drives
    /// prediction-interval widths used by /decide refusal semantics (Phase E).
    pub conformity_calibrator: crate::conformal::ConformalCalibrator,
}

// ── Capsule memory (sidecar) ──

#[derive(Debug, Clone)]
pub struct CapsuleMemory {
    pub strategies: HashMap<u32, StrategyMemory>,
    pub version: u32,
    /// 3E: per-capsule rolling windows for `FeatureType::TimeSeries`
    /// features. Keyed by feature name. The runtime pushes a new value on
    /// every `/decide` whose `features` includes a Number for this name,
    /// and reads the window via `ContextSpec::encode_with_windows` to
    /// produce the encoded aggregation features.
    pub time_series_windows: HashMap<String, crate::feature_schema::TimeSeriesWindow>,
    /// Item 2: shared-state LinUCB state for capsules whose
    /// `learning.json::sharedState.enabled` is true. None for legacy
    /// per-option-LinUCB capsules.
    pub shared_state: Option<crate::shared_state_strategy::SharedStateOptionStrategy>,
}

impl Default for CapsuleMemory {
    fn default() -> Self { Self {
        strategies: HashMap::new(),
        version: 2,
        time_series_windows: HashMap::new(),
        shared_state: None,
    } }
}

impl CapsuleMemory {
    pub fn to_json(&self) -> serde_json::Value {
        let mut strats = serde_json::Map::new();
        for (nid, sm) in &self.strategies {
            let mut contexts = serde_json::Map::new();
            for (ctx_key, bucket) in &sm.contexts {
                contexts.insert(ctx_key.clone(), serialize_bucket(bucket));
            }
            let mut candidate_ctx_json = serde_json::Map::new();
            for ((cid, ctx_key), bucket) in &sm.candidate_contexts {
                let combined_key = format!("{}|{}", candidate_id_str(*cid), ctx_key);
                candidate_ctx_json.insert(combined_key, serialize_bucket(bucket));
            }
            let meta_bandit_json = sm.meta_bandit.as_ref().map(serialize_meta_bandit);
            let mut context_detectors_json = serde_json::Map::new();
            for (ctx_key, detector) in &sm.context_detectors {
                context_detectors_json.insert(ctx_key.clone(), serialize_adwin(detector));
            }
            let discrete_ood_json = sm.discrete_ood.as_ref()
                .and_then(|d| serde_json::to_value(d).ok());
            let feature_ood_json = sm.feature_ood.as_ref()
                .and_then(|d| serde_json::to_value(d).ok());
            strats.insert(nid.to_string(), serde_json::json!({
                "nodeId": nid,
                "nOptions": sm.n_options,
                "contexts": contexts,
                "candidateContexts": candidate_ctx_json,
                "metaBandit": meta_bandit_json,
                "contextDetectors": context_detectors_json,
                "discreteOod": discrete_ood_json,
                "featureOod": feature_ood_json,
            }));
        }
        let mut ts_json = serde_json::Map::new();
        for (name, win) in &self.time_series_windows {
            ts_json.insert(name.clone(), win.serialize());
        }
        let shared_state_json = self.shared_state.as_ref().map(|s| s.to_json());
        serde_json::json!({
            "version": 7,
            "strategies": strats,
            "timeSeriesWindows": ts_json,
            "sharedState": shared_state_json,
        })
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
                        contexts.insert(ctx_key.clone(), parse_bucket(bucket_json));
                    }
                }
                let mut candidate_contexts = HashMap::new();
                if let Some(cc_map) = sm_json.get("candidateContexts").and_then(|v| v.as_object()) {
                    for (combined_key, bucket_json) in cc_map {
                        let Some((cid_str, ctx_key)) = combined_key.split_once('|') else { continue; };
                        let Some(candidate) = candidate_id_from_str(cid_str) else { continue; };
                        candidate_contexts.insert((candidate, ctx_key.to_string()), parse_bucket(bucket_json));
                    }
                }
                let meta_bandit = sm_json.get("metaBandit").and_then(parse_meta_bandit);
                let mut context_detectors = HashMap::new();
                if let Some(cd_map) = sm_json.get("contextDetectors").and_then(|v| v.as_object()) {
                    for (ctx_key, detector_json) in cd_map {
                        if let Some(detector) = parse_adwin(detector_json) {
                            context_detectors.insert(ctx_key.clone(), detector);
                        }
                    }
                }
                let discrete_ood = sm_json.get("discreteOod")
                    .and_then(|v| serde_json::from_value::<crate::ood::DiscreteOodDetector>(v.clone()).ok());
                let feature_ood = sm_json.get("featureOod")
                    .and_then(|v| serde_json::from_value::<crate::ood::FeatureOodDetector>(v.clone()).ok());
                mem.strategies.insert(nid, StrategyMemory {
                    node_id: nid, n_options, contexts, candidate_contexts, meta_bandit, context_detectors,
                    discrete_ood, feature_ood,
                });
            }
        }
        // 3E: load per-capsule time-series windows.
        if let Some(ts_map) = j.get("timeSeriesWindows").and_then(|v| v.as_object()) {
            for (name, win_json) in ts_map {
                if let Ok(win) = crate::feature_schema::TimeSeriesWindow::deserialize(win_json) {
                    mem.time_series_windows.insert(name.clone(), win);
                }
            }
        }
        // Item 2: load shared-state LinUCB if present. Absent / null on legacy
        // capsules is fine — they keep using the per-option LinUcb path.
        if let Some(ss_json) = j.get("sharedState") {
            if !ss_json.is_null() {
                if let Ok(ss) = crate::shared_state_strategy::SharedStateOptionStrategy::from_json(ss_json) {
                    mem.shared_state = Some(ss);
                }
            }
        }
        mem
    }

    /// Get or create a context bucket for a strategy, initializing from graph weights.
    pub fn get_or_init_context(&mut self, node_id: u32, context_key: &str, graph_weights: &[f64], n_options: usize) -> &mut ContextBucket {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id, n_options, contexts: HashMap::new(), candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.contexts.entry(context_key.to_string()).or_insert_with(|| {
            let weights = if graph_weights.len() >= n_options {
                graph_weights[..n_options].to_vec()
            } else {
                vec![1.0 / n_options as f64; n_options]
            };
            let option_states: Vec<OptionState> = weights.iter().map(|w| OptionState::weighted(*w)).collect();
            ContextBucket {
                weights,
                stats: (0..n_options).map(|_| OptionStats::default()).collect(),
                updated_at: 0,
                conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
                option_states,
            }
        })
    }

    #[allow(dead_code)]
    pub fn list_contexts(&self, node_id: u32) -> Vec<String> {
        self.strategies.get(&node_id)
            .map(|sm| sm.contexts.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_or_init_candidate_context(
        &mut self,
        node_id: u32,
        context_key: &str,
        candidate: CandidateId,
        graph_weights: &[f64],
        n_options: usize,
    ) -> &mut ContextBucket {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.candidate_contexts
            .entry((candidate, context_key.to_string()))
            .or_insert_with(|| {
                let weights = if graph_weights.len() >= n_options {
                    graph_weights[..n_options].to_vec()
                } else {
                    vec![1.0 / n_options as f64; n_options]
                };
                let option_states = make_option_states_for_candidate(candidate, &weights);
                ContextBucket {
                    weights,
                    stats: (0..n_options).map(|_| OptionStats::default()).collect(),
                    updated_at: 0,
                    conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
                    option_states,
                }
            })
    }

    /// Drop all candidate state for a given (node_id, context_key) pair.
    /// Used when the meta-bandit re-warms after a regime change.
    pub fn reset_candidate_contexts(&mut self, node_id: u32, context_key: &str) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            sm.candidate_contexts.retain(|(_, ck), _| ck != context_key);
        }
    }

    pub fn get_or_init_meta_bandit(
        &mut self,
        node_id: u32,
        n_options: usize,
        candidates: &[CandidateId],
    ) -> &mut MetaBandit {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        let cands = candidates.to_vec();
        sm.meta_bandit.get_or_insert_with(|| MetaBandit::new_with_candidates(&cands))
    }

    pub fn meta_bandit_for(&self, node_id: u32) -> Option<&MetaBandit> {
        self.strategies.get(&node_id).and_then(|sm| sm.meta_bandit.as_ref())
    }

    pub fn reset_meta_bandit(&mut self, node_id: u32) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            if let Some(mb) = sm.meta_bandit.as_mut() {
                mb.reset();
            }
        }
    }

    pub fn get_or_init_context_detector(
        &mut self,
        node_id: u32,
        context_key: &str,
    ) -> &mut AdwinDetector {
        self.get_or_init_context_detector_with_delta(
            node_id,
            context_key,
            default_context_adwin_delta(),
        )
    }

    /// Like `get_or_init_context_detector` but accepts an explicit
    /// ADWIN delta. The delta is only applied on first insertion for a
    /// `(node_id, context_key)` pair — subsequent calls return the
    /// existing detector unchanged, so changing
    /// `SafetyConfig.context_adwin_delta` after a capsule has seen
    /// traffic does NOT retune the live detector. Persisted detectors
    /// rebuilt via `restore_state` keep their stored delta.
    pub fn get_or_init_context_detector_with_delta(
        &mut self,
        node_id: u32,
        context_key: &str,
        context_adwin_delta: f64,
    ) -> &mut AdwinDetector {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options: 0,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.context_detectors
            .entry(context_key.to_string())
            .or_insert_with(|| AdwinDetector::new(context_adwin_delta, 1000))
    }

    pub fn get_or_init_discrete_ood(&mut self, node_id: u32) -> &mut crate::ood::DiscreteOodDetector {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options: 0,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        sm.discrete_ood.get_or_insert_with(crate::ood::DiscreteOodDetector::new)
    }

    pub fn get_or_init_feature_ood(&mut self, node_id: u32, d: usize) -> &mut crate::ood::FeatureOodDetector {
        let sm = self.strategies.entry(node_id).or_insert_with(|| StrategyMemory {
            node_id,
            n_options: 0,
            contexts: HashMap::new(),
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        let existing_dim = sm.feature_ood.as_ref().map(|f| f.d);
        if existing_dim == Some(d) {
            sm.feature_ood.as_mut().unwrap()
        } else {
            sm.feature_ood = Some(crate::ood::FeatureOodDetector::new(d));
            sm.feature_ood.as_mut().unwrap()
        }
    }

    pub fn discrete_ood_for(&self, node_id: u32) -> Option<&crate::ood::DiscreteOodDetector> {
        self.strategies.get(&node_id).and_then(|sm| sm.discrete_ood.as_ref())
    }

    pub fn feature_ood_for(&self, node_id: u32) -> Option<&crate::ood::FeatureOodDetector> {
        self.strategies.get(&node_id).and_then(|sm| sm.feature_ood.as_ref())
    }

    pub fn reset_ood_detectors(&mut self, node_id: u32) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            if let Some(d) = sm.discrete_ood.as_mut() { d.reset(); }
            if let Some(d) = sm.feature_ood.as_mut() { d.reset(); }
        }
    }

    pub fn reset_context_detector(&mut self, node_id: u32, context_key: &str) {
        if let Some(sm) = self.strategies.get_mut(&node_id) {
            if let Some(d) = sm.context_detectors.get_mut(context_key) {
                d.reset();
            }
        }
    }

    /// List all candidates that have state for a given (node_id, context_key).
    pub fn candidates_for_context(&self, node_id: u32, context_key: &str) -> Vec<CandidateId> {
        self.strategies
            .get(&node_id)
            .map(|sm| {
                sm.candidate_contexts
                    .keys()
                    .filter(|(_, ck)| ck == context_key)
                    .map(|(cid, _)| *cid)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn make_option_states_for_candidate(candidate: CandidateId, weights: &[f64]) -> Vec<OptionState> {
    match candidate {
        CandidateId::Thompson => weights.iter().map(|_| OptionState::beta(1.0, 1.0)).collect(),
        CandidateId::Ucb => weights.iter().map(|_| OptionState::ucb_initial()).collect(),
        CandidateId::Weighted | CandidateId::EpsilonGreedy | CandidateId::Greedy => {
            weights.iter().map(|w| OptionState::weighted(*w)).collect()
        }
        // LinUcb and LinTs need a feature dimension to size their state.
        // We don't have d here; the caller (do_decide / do_feedback)
        // upgrades these to LinUcb states via `ensure_linucb_states(bucket, d)`
        // once d is known. LinTs reuses the same LinUcb state shape — the
        // math is identical for the update path; only the score function
        // differs (Thompson-sampling θ vs UCB optimistic bonus).
        CandidateId::LinUcb | CandidateId::LinTs => {
            weights.iter().map(|w| OptionState::weighted(*w)).collect()
        }
    }
}

/// Upgrade a bucket's option_states to LinUcb variants at the given feature
/// dimension. Idempotent: existing LinUcb states at the same d are kept;
/// states at a different d (or non-LinUcb states) are replaced with fresh
/// LinUcb states.
pub fn ensure_linucb_states(bucket: &mut ContextBucket, d: usize, lambda: f64) {
    for state in bucket.option_states.iter_mut() {
        let needs_replace = match state {
            OptionState::LinUcb { state: ls } => ls.d != d,
            _ => true,
        };
        if needs_replace {
            *state = OptionState::linucb_initial(d, lambda);
        }
    }
}

fn candidate_id_str(cid: CandidateId) -> &'static str {
    cid.as_str()
}

fn candidate_id_from_str(s: &str) -> Option<CandidateId> {
    CandidateId::from_str(s)
}

fn serialize_meta_bandit(mb: &MetaBandit) -> serde_json::Value {
    let candidates: Vec<serde_json::Value> = mb.candidates.iter().map(|c| {
        serde_json::json!({
            "id": candidate_id_str(c.id),
            "trials": c.trials,
            "cumulativeReward": c.cumulative_reward,
        })
    }).collect();
    serde_json::json!({
        "candidates": candidates,
        "totalRounds": mb.total_rounds,
        "explorationDecay": mb.exploration_decay,
        "minExploration": mb.min_exploration,
        "forgettingFactor": mb.forgetting_factor,
    })
}

fn serialize_adwin(d: &AdwinDetector) -> serde_json::Value {
    serde_json::json!({
        "window": d.window_snapshot(),
        "delta": d.delta(),
        "maxSize": d.max_size(),
        "minSubwindow": d.min_subwindow(),
    })
}

fn parse_adwin(j: &serde_json::Value) -> Option<AdwinDetector> {
    let window: Vec<f64> = j.get("window")?.as_array()?
        .iter().filter_map(|v| v.as_f64()).collect();
    let delta = j.get("delta")?.as_f64()?;
    let max_size = j.get("maxSize")?.as_u64()? as usize;
    let min_subwindow = j.get("minSubwindow").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
    Some(AdwinDetector::restore_state(window, delta, max_size, min_subwindow))
}

fn parse_meta_bandit(j: &serde_json::Value) -> Option<MetaBandit> {
    let candidates_json = j.get("candidates")?.as_array()?;
    let ids: Vec<CandidateId> = candidates_json.iter()
        .filter_map(|c| c.get("id")?.as_str().and_then(candidate_id_from_str))
        .collect();
    if ids.is_empty() {
        return None;
    }
    let mut mb = MetaBandit::new_with_candidates(&ids);
    for c_json in candidates_json {
        let id_str = match c_json.get("id").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let id = match candidate_id_from_str(id_str) {
            Some(i) => i,
            None => continue,
        };
        let trials = c_json.get("trials").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cumulative_reward = c_json.get("cumulativeReward").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if let Some(c) = mb.candidates.iter_mut().find(|c| c.id == id) {
            c.trials = trials;
            c.cumulative_reward = cumulative_reward;
        }
    }
    mb.total_rounds = j.get("totalRounds").and_then(|v| v.as_u64()).unwrap_or(0);
    mb.exploration_decay = j.get("explorationDecay").and_then(|v| v.as_f64()).unwrap_or(5.0);
    mb.min_exploration = j.get("minExploration").and_then(|v| v.as_f64()).unwrap_or(0.05);
    mb.forgetting_factor = j.get("forgettingFactor").and_then(|v| v.as_f64()).unwrap_or(0.999);
    Some(mb)
}

fn serialize_bucket(bucket: &ContextBucket) -> serde_json::Value {
    let stats_json: Vec<serde_json::Value> = bucket.stats.iter().map(|s| {
        let mut sj = s.to_json();
        if let Some(m) = sj.as_object_mut() {
            m.insert("rewardSum".into(), serde_json::json!(s.reward_sum));
            m.insert("rewardSqSum".into(), serde_json::json!(s.reward_sq_sum));
            m.insert("effectiveTries".into(), serde_json::json!(s.effective_tries));
            let win: Vec<f64> = s.window.iter().copied().collect();
            m.insert("window".into(), serde_json::json!(win));
            m.insert("phCumsum".into(), serde_json::json!(s.ph_cumsum));
            m.insert("phMin".into(), serde_json::json!(s.ph_min));
            m.insert("changeBoostRemaining".into(), serde_json::json!(s.change_boost_remaining));
            m.insert("changePoints".into(), serde_json::json!(s.change_points));
        }
        sj
    }).collect();
    let option_states_json: Vec<serde_json::Value> =
        bucket.option_states.iter().map(OptionState::to_json).collect();
    let calibrator_json = serde_json::json!({
        "residuals": bucket.conformity_calibrator.residuals_snapshot(),
        "maxSize": bucket.conformity_calibrator.max_size(),
        "minSamples": bucket.conformity_calibrator.min_samples(),
    });
    serde_json::json!({
        "weights": bucket.weights,
        "stats": stats_json,
        "updatedAt": bucket.updated_at,
        "conformityCalibrator": calibrator_json,
        "optionStates": option_states_json,
    })
}

fn parse_bucket(bucket_json: &serde_json::Value) -> ContextBucket {
    let weights: Vec<f64> = bucket_json.get("weights")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
        .unwrap_or_default();
    let stats: Vec<OptionStats> = bucket_json.get("stats")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(OptionStats::from_json).collect())
        .unwrap_or_default();
    let updated_at = bucket_json.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
    let conformity_calibrator = if let Some(cal_json) = bucket_json.get("conformityCalibrator") {
        parse_conformal(cal_json).unwrap_or_else(crate::conformal::ConformalCalibrator::default_config)
    } else {
        // Legacy path: read raw residuals array left by v5 sidecars.
        let residuals: Vec<f64> = bucket_json.get("conformityScores")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
            .unwrap_or_default();
        crate::conformal::ConformalCalibrator::restore_state(residuals, 500, 30)
    };
    let option_states: Vec<OptionState> = bucket_json.get("optionStates")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(OptionState::from_json).collect::<Vec<_>>())
        .filter(|v| v.len() == weights.len())
        .unwrap_or_else(|| weights.iter().map(|w| OptionState::weighted(*w)).collect());
    ContextBucket {
        weights, stats, updated_at, conformity_calibrator, option_states,
    }
}

fn parse_conformal(j: &serde_json::Value) -> Option<crate::conformal::ConformalCalibrator> {
    let residuals: Vec<f64> = j.get("residuals")?.as_array()?
        .iter().filter_map(|v| v.as_f64()).collect();
    let max_size = j.get("maxSize").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
    let min_samples = j.get("minSamples").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
    Some(crate::conformal::ConformalCalibrator::restore_state(residuals, max_size, min_samples))
}

// ── Selection algorithms ──

fn now_secs() -> u64 {
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

fn stats_score(s: &OptionStats, config: &LearningConfig) -> f64 {
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
    // Gaussian approximation: Beta(α, β) ≈ Normal(α/(α+β), α·β / ((α+β)²·(α+β+1))).
    // Accurate enough for Thompson sampling once α+β ≳ 10. For very early
    // rounds (α=β=1) the approximation is wide and forces exploration, which
    // is the desired behavior.
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

// ── Feedback application ──

/// Apply per-feedback exponential decay to all option stats in a bucket.
/// This is the "sliding window with forgetting" mechanism wired into feedback.
/// Operates on cumulative reward_sum, reward_sq_sum, effective_tries.
fn apply_feedback_decay(bucket: &mut ContextBucket, config: &LearningConfig) {
    // Option-state forgetting (Phase C3 parameter) applies to OptionStats
    // accumulators too. Without this, `tries`/`successes`/`failures` /
    // `reward_sum` grow unbounded over long deployments and the values
    // become uninformative ("tries: 1,000,000" tells the operator nothing
    // about recent behavior). Apply unconditionally — the parameter has a
    // sensible default (0.999, i.e. ~7000-feedback half-life).
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
    // Always feed the split-conformal calibrator from non-LinUcb feedback so
    // Phase E refusal logic has data even when the legacy `conformal.enabled`
    // gate is off. The LinUcb path skips this (the calibrator is updated from
    // server.rs once the feature vector is in scope).
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

/// Apply feedback to a context bucket with safety rails.
///
/// Pipeline:
///   1. Clip reward to safety.reward_clip.
///   2. Apply per-feedback decay to cumulative stats (if decay.enabled).
///   3. Update stats (tries, sums, window, effective_tries).
///   4. Run change-point detection on the chosen option.
///   5. Weight update with configurable learning_rate, clamping, normalization,
///      and min_exploration floor.
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

    // 5. Weight update.
    let learning_rate = config.learning_rate.clamp(0.0001, 0.5);
    let raw_delta = clipped * learning_rate;
    let max_delta = config.safety.max_weight_delta_per_feedback;
    let delta = raw_delta.clamp(-max_delta, max_delta);

    for j in 0..n {
        if j == option {
            bucket.weights[j] = (bucket.weights[j] + delta).clamp(0.01, 0.99);
        } else if n > 1 {
            bucket.weights[j] = (bucket.weights[j] - delta / (n - 1) as f64).clamp(0.01, 0.99);
        }
    }

    // Allocate min_exploration of the probability mass uniformly across options,
    // and assign the remaining (1 - min_exploration) by current relative weights.
    // This guarantees the floor post-normalization, unlike clamping-then-normalizing
    // which can push small weights back below the floor.
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

    // Apply geometric forgetting to ALL option states before the chosen-option
    // update. Unchosen options fade alongside the chosen one so relative
    // comparisons remain meaningful. Weighted is left alone — its scalar
    // weight already reflects a current estimate, not an accumulator.
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
                    // LinUCB state isn't updated through scalar-only feedback;
                    // its update path (D3) requires the feature vector. Per-context
                    // ADWIN handles drift detection for feature-based capsules.
                }
            }
        }
    }

    let learning_rate = config.learning_rate.clamp(0.0001, 0.5);
    let n = bucket.option_states.len();
    match &mut bucket.option_states[option] {
        OptionState::Weighted { weight } => {
            *weight = (*weight + clipped * learning_rate).clamp(0.01, 0.99);
        }
        OptionState::BetaBernoulli { alpha, beta } => {
            // Treat reward as a sample of a Bernoulli outcome:
            // reward > 0 → success (alpha += 1), reward ≤ 0 → failure (beta += 1).
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
            // No-op without a feature vector. The D3 feedback path calls
            // LinUcbState::update directly with (x, reward).
        }
    }
    // For Weighted variant, push complementary updates so weights sum-balance.
    if matches!(bucket.option_states[option], OptionState::Weighted { .. }) && n > 1 {
        let delta = clipped * learning_rate;
        let per_other = delta / (n - 1) as f64;
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

pub(crate) fn rand_f64() -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_nanos().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    c.hash(&mut h);
    (h.finish() % 1_000_000) as f64 / 1_000_000.0
}

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
pub fn pareto_frontier(bucket: &ContextBucket, config: &LearningConfig, n_options: usize) -> Vec<usize> {
    if !config.pareto.enabled || config.pareto.objectives.is_empty() {
        return (0..n_options).collect();
    }
    let scores: Vec<Vec<f64>> = (0..n_options).map(|i| {
        let s = bucket.stats.get(i);
        config.pareto.objectives.iter().map(|obj| {
            s.map(|st| st.objective_mean(obj)).unwrap_or(f64::NEG_INFINITY)
        }).collect()
    }).collect();

    let mut frontier = Vec::new();
    for i in 0..n_options {
        let mut dominated = false;
        for j in 0..n_options {
            if i == j { continue; }
            let strictly_better = scores[j].iter().zip(&scores[i]).any(|(a, b)| a > b);
            let no_worse = scores[j].iter().zip(&scores[i]).all(|(a, b)| a >= b);
            if no_worse && strictly_better { dominated = true; break; }
        }
        if !dominated { frontier.push(i); }
    }
    if frontier.is_empty() { (0..n_options).collect() } else { frontier }
}

pub fn select_pareto(bucket: &ContextBucket, config: &LearningConfig, n_options: usize) -> (usize, String) {
    let frontier = pareto_frontier(bucket, config, n_options);
    if frontier.is_empty() { return (0, "empty frontier".into()); }
    if frontier.len() == 1 { return (frontier[0], "single non-dominated option".into()); }
    // Pick from the frontier by current weight (so learned preferences still
    // bias choice among the Pareto-equal options).
    let mut best = frontier[0];
    let mut best_w = bucket.weights.get(best).copied().unwrap_or(0.0);
    for &i in &frontier[1..] {
        let w = bucket.weights.get(i).copied().unwrap_or(0.0);
        if w > best_w { best_w = w; best = i; }
    }
    (best, format!("pareto frontier ({} options)", frontier.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_stats_to_from_json_is_self_round_tripping() {
        // Regression test for the persistence gap surfaced during Item 2:
        // direct callers of `OptionStats::to_json` (without the
        // `serialize_bucket` field-injection wrapper) must round-trip
        // through `from_json` without losing reward_sum / reward_sq_sum /
        // window / Page-Hinkley state.
        let mut s = OptionStats::default();
        s.tries = 7;
        s.successes = 4;
        s.failures = 3;
        s.reward_sum = 3.25;
        s.reward_sq_sum = 1.875;
        s.last_reward = 0.75;
        s.last_updated = 42_000;
        s.effective_tries = 6.5;
        s.window = std::collections::VecDeque::from(vec![1.0, 0.5, 0.25]);
        s.ph_cumsum = 0.1;
        s.ph_min = -0.2;
        s.change_boost_remaining = 3;
        s.change_points = 1;
        s.posterior_mean = 0.4;
        s.posterior_var = 0.8;

        let round = OptionStats::from_json(&s.to_json());
        assert_eq!(round.tries, s.tries);
        assert!((round.reward_sum - s.reward_sum).abs() < 1e-9, "reward_sum lost across round-trip");
        assert!((round.reward_sq_sum - s.reward_sq_sum).abs() < 1e-9, "reward_sq_sum lost across round-trip");
        assert_eq!(round.window.len(), s.window.len(), "window dropped across round-trip");
        for (a, b) in round.window.iter().zip(s.window.iter()) {
            assert!((a - b).abs() < 1e-9);
        }
        assert!((round.ph_cumsum - s.ph_cumsum).abs() < 1e-9);
        assert!((round.ph_min - s.ph_min).abs() < 1e-9);
        assert_eq!(round.change_boost_remaining, s.change_boost_remaining);
        // effective_tries goes through 2-decimal rounding in to_json — this is
        // the one documented precision loss. Tolerance reflects that.
        assert!((round.effective_tries - s.effective_tries).abs() < 0.01);
    }

    fn make_bucket(n: usize) -> ContextBucket {
        let w = 1.0 / n as f64;
        ContextBucket {
            weights: vec![w; n],
            stats: (0..n).map(|_| OptionStats::default()).collect(),
            updated_at: 0,
            conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
            option_states: (0..n).map(|_| OptionState::weighted(w)).collect(),
        }
    }

    #[test]
    fn reward_clipping_bounds_extreme_input() {
        let mut cfg = LearningConfig::default();
        cfg.safety.reward_clip = 1.0;
        let mut b = make_bucket(3);
        apply_feedback(&mut b, 0, 100.0, &cfg).unwrap();
        assert!(b.stats[0].reward_sum <= 1.0 + 1e-9);
        assert!(b.stats[0].last_reward <= 1.0 + 1e-9);
    }

    #[test]
    fn configurable_learning_rate_affects_weight_delta() {
        let mut cfg_low = LearningConfig::default();
        cfg_low.learning_rate = 0.005;
        let mut cfg_high = LearningConfig::default();
        cfg_high.learning_rate = 0.1;

        let mut b_low = make_bucket(3);
        let mut b_high = make_bucket(3);
        apply_feedback(&mut b_low, 0, 1.0, &cfg_low).unwrap();
        apply_feedback(&mut b_high, 0, 1.0, &cfg_high).unwrap();

        // Higher learning rate ⇒ chosen option gets larger weight after one feedback.
        assert!(b_high.weights[0] > b_low.weights[0]);
    }

    #[test]
    fn decay_shrinks_effective_tries_over_feedbacks() {
        let mut cfg = LearningConfig::default();
        cfg.decay.enabled = true;
        cfg.decay.half_life_feedbacks = 10.0;
        let mut b = make_bucket(2);
        for _ in 0..10 { apply_feedback(&mut b, 0, 1.0, &cfg).unwrap(); }
        let eff_after_10 = b.stats[0].effective_tries;
        for _ in 0..10 { apply_feedback(&mut b, 1, 1.0, &cfg).unwrap(); }
        // After 10 more feedbacks on option 1 with half-life=10, option 0's
        // effective_tries should be ~halved.
        assert!(b.stats[0].effective_tries < eff_after_10 * 0.6);
    }

    #[test]
    fn windowed_stats_track_only_recent_rewards() {
        let mut cfg = LearningConfig::default();
        cfg.window.enabled = true;
        cfg.window.size = 5;
        let mut b = make_bucket(2);
        // 10 positive rewards
        for _ in 0..10 { apply_feedback(&mut b, 0, 1.0, &cfg).unwrap(); }
        // 3 negative rewards
        for _ in 0..3 { apply_feedback(&mut b, 0, -1.0, &cfg).unwrap(); }
        // Window has the last 5: [1,1,-1,-1,-1]
        assert_eq!(b.stats[0].window.len(), 5);
        let win_mean = b.stats[0].reward_mean_windowed();
        assert!(win_mean < 0.0);  // recent rewards dominate
        let all_time_mean = b.stats[0].reward_mean();
        assert!(all_time_mean > 0.3);  // cumulative still positive
    }

    #[test]
    fn trimmed_mean_drops_extremes() {
        let mut s = OptionStats::default();
        // Use an asymmetric outlier pattern so trimming has an unambiguous effect.
        // 4 stable values around 0.2 plus one extreme high outlier.
        for v in [0.1, 0.2, 0.3, 0.4, 50.0] {
            s.window.push_back(v);
            s.tries += 1;
            s.reward_sum += v;
        }
        let plain = s.reward_mean_windowed();
        // Trim 20% from each tail → drop 1 value each side, keep middle 3.
        let trimmed = s.reward_mean_trimmed(0.2);
        // Trimmed mean should be far from the outlier-inflated plain mean.
        assert!(trimmed < plain - 1.0, "plain={plain} trimmed={trimmed}");
        // Middle three of [0.1, 0.2, 0.3, 0.4, 50.0] are [0.2, 0.3, 0.4] → mean 0.3.
        assert!((trimmed - 0.3).abs() < 0.01, "expected ~0.3, got {trimmed}");
    }

    #[test]
    fn change_detection_fires_on_regime_shift() {
        let mut cfg = LearningConfig::default();
        cfg.change_detection.enabled = true;
        cfg.change_detection.threshold = 1.0;
        cfg.change_detection.min_drift = 0.0;
        cfg.window.enabled = true;
        cfg.window.size = 20;
        let mut b = make_bucket(2);
        // Establish a stable positive regime
        for _ in 0..30 { apply_feedback(&mut b, 0, 1.0, &cfg).unwrap(); }
        let cp_before = b.stats[0].change_points;
        // Sudden negative regime
        for _ in 0..30 { apply_feedback(&mut b, 0, -1.0, &cfg).unwrap(); }
        let cp_after = b.stats[0].change_points;
        assert!(cp_after > cp_before, "expected change point to fire on regime shift");
    }

    #[test]
    fn min_exploration_floor_enforced() {
        let mut cfg = LearningConfig::default();
        cfg.safety.min_exploration = 0.10;
        let mut b = make_bucket(5);
        // Hammer option 0 with positive rewards.
        for _ in 0..200 { apply_feedback(&mut b, 0, 1.0, &cfg).unwrap(); }
        let min_w = b.weights.iter().cloned().fold(f64::INFINITY, f64::min);
        // With 5 options and min_exploration=0.10, floor per-option is 0.10/5 = 0.02.
        assert!(min_w >= 0.02 - 1e-6, "min weight {min_w} below floor");
    }

    #[test]
    fn thompson_picks_higher_mean_in_expectation() {
        let mut b = make_bucket(2);
        let cfg = LearningConfig {
            algorithm: Algorithm::ThompsonSampling,
            ..Default::default()
        };
        // Option 1 clearly better.
        for _ in 0..50 { apply_feedback(&mut b, 0, -0.5, &cfg).unwrap(); }
        for _ in 0..50 { apply_feedback(&mut b, 1, 0.8, &cfg).unwrap(); }
        let mut picks = [0usize, 0usize];
        for _ in 0..200 {
            let (idx, _) = select_option(&b, &cfg, 2);
            picks[idx] += 1;
        }
        assert!(picks[1] > picks[0] + 50, "thompson should favor better arm");
    }

    #[test]
    fn freeze_learning_blocks_updates() {
        let mut cfg = LearningConfig::default();
        cfg.safety.freeze_learning = true;
        let mut b = make_bucket(3);
        let before = b.weights.clone();
        let res = apply_feedback(&mut b, 0, 1.0, &cfg);
        assert!(res.is_err());
        assert_eq!(b.weights, before);
    }

    #[test]
    fn thompson_beta_favors_higher_success_rate() {
        let mut cfg = LearningConfig::default();
        cfg.algorithm = Algorithm::ThompsonSampling;
        let mut b = make_bucket(2);
        // 60 wins / 40 losses on arm 0; 40 wins / 60 losses on arm 1.
        for _ in 0..60 { apply_feedback(&mut b, 0, 1.0, &cfg).unwrap(); }
        for _ in 0..40 { apply_feedback(&mut b, 0, 0.0, &cfg).unwrap(); }
        for _ in 0..40 { apply_feedback(&mut b, 1, 1.0, &cfg).unwrap(); }
        for _ in 0..60 { apply_feedback(&mut b, 1, 0.0, &cfg).unwrap(); }
        // option_states should now be BetaBernoulli with α₀≈61, β₀≈41 and α₁≈41, β₁≈61
        assert!(matches!(b.option_states[0], OptionState::BetaBernoulli { .. }));
        let mut picks = [0u32, 0u32];
        for _ in 0..1000 {
            let (i, _) = select_option(&b, &cfg, 2);
            picks[i] += 1;
        }
        assert!(picks[0] > picks[1] + 200,
                "thompson-beta should favor arm 0; got picks={picks:?}");
    }

    #[test]
    fn select_option_does_not_mutate_bucket_weights() {
        // Selection is read-only on the bucket. The runtime decide path copies
        // bucket weights into the graph node for sampling; select_option's
        // output is advisory and must not modify the bucket's stored state.
        let mut cfg = LearningConfig::default();
        cfg.algorithm = Algorithm::Ucb1;
        cfg.corruption_robust.enabled = true;
        cfg.corruption_robust.budget = 10.0;
        let mut b = make_bucket(5);
        for _ in 0..30 { apply_feedback(&mut b, 0, 0.8, &cfg).unwrap(); }
        for _ in 0..10 { apply_feedback(&mut b, 1, 0.5, &cfg).unwrap(); }
        let weights_before = b.weights.clone();
        for _ in 0..100 {
            let (_idx, _r) = select_option(&b, &cfg, 5);
        }
        assert_eq!(weights_before, b.weights, "select_option must be read-only");
    }

    #[test]
    fn pareto_frontier_keeps_non_dominated_options() {
        let mut cfg = LearningConfig::default();
        cfg.pareto.enabled = true;
        cfg.pareto.objectives = vec!["latency".into(), "cost".into()];
        let mut b = make_bucket(4);
        // Option 0: low latency, high cost
        b.stats[0].record_objective("latency", 0.9);
        b.stats[0].record_objective("cost", 0.2);
        // Option 1: high latency, low cost
        b.stats[1].record_objective("latency", 0.3);
        b.stats[1].record_objective("cost", 0.9);
        // Option 2: dominated (worse on both)
        b.stats[2].record_objective("latency", 0.2);
        b.stats[2].record_objective("cost", 0.1);
        // Option 3: balanced, non-dominated
        b.stats[3].record_objective("latency", 0.6);
        b.stats[3].record_objective("cost", 0.6);

        let frontier = pareto_frontier(&b, &cfg, 4);
        assert!(frontier.contains(&0));
        assert!(frontier.contains(&1));
        assert!(frontier.contains(&3));
        assert!(!frontier.contains(&2), "option 2 is dominated; got frontier={frontier:?}");
    }

    #[test]
    fn cvar_is_average_of_lower_tail() {
        let mut s = OptionStats::default();
        for v in [-2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0] {
            s.window.push_back(v);
            s.tries += 1;
            s.reward_sum += v;
        }
        // Worst 20% of 10 values → [-2.0, -1.0], CVaR_0.20 = -1.5.
        let cvar = s.reward_cvar(0.20);
        assert!((cvar - (-1.5)).abs() < 1e-9, "got {cvar}");
        // Worst 50% → first 5 values, mean = (-2 -1 + 0 + 1 + 2)/5 = 0.0.
        let cvar50 = s.reward_cvar(0.50);
        assert!((cvar50 - 0.0).abs() < 1e-9, "got {cvar50}");
    }

    #[test]
    fn risk_sensitive_blend_prefers_lower_variance_option() {
        let mut b = make_bucket(2);
        let mut cfg = LearningConfig::default();
        cfg.window.enabled = true;
        cfg.window.size = 50;
        cfg.algorithm = Algorithm::EpsilonGreedy { epsilon: 0.0 };

        // Option 0: rewards {1, 1, 1, 1, 1, ..., -3} — mean ~0.6, awful tail.
        // Option 1: stable rewards of 0.5.
        for _ in 0..18 { apply_feedback(&mut b, 0, 1.0, &cfg).unwrap(); }
        for _ in 0..2  { apply_feedback(&mut b, 0, -2.0, &cfg).unwrap(); }
        for _ in 0..20 { apply_feedback(&mut b, 1, 0.5, &cfg).unwrap(); }

        // With pure-mean scoring, option 0 wins.
        let (mean_choice, _) = select_option(&b, &cfg, 2);
        // With risk-sensitive scoring, the lower-tail-averaged option 1 wins.
        cfg.risk_sensitive.enabled = true;
        cfg.risk_sensitive.alpha = 0.20;
        cfg.risk_sensitive.blend = 0.7;
        let (risk_choice, _) = select_option(&b, &cfg, 2);
        assert_ne!(mean_choice, risk_choice, "risk score should flip preference");
        assert_eq!(risk_choice, 1);
    }

    #[test]
    fn gkt_bonus_inflates_ucb_for_under_explored_arms() {
        let mut b = make_bucket(2);
        let mut cfg = LearningConfig::default();
        cfg.algorithm = Algorithm::Ucb1;
        for _ in 0..500 { apply_feedback(&mut b, 0, 0.5, &cfg).unwrap(); }
        for _ in 0..50  { apply_feedback(&mut b, 1, 0.5, &cfg).unwrap(); }

        let mut picks = [0usize, 0usize];
        for _ in 0..400 {
            let (idx, _) = select_option(&b, &cfg, 2);
            picks[idx] += 1;
        }
        let baseline_arm1 = picks[1];

        cfg.corruption_robust.enabled = true;
        cfg.corruption_robust.budget = 100.0;
        let mut picks2 = [0usize, 0usize];
        for _ in 0..400 {
            let (idx, _) = select_option(&b, &cfg, 2);
            picks2[idx] += 1;
        }
        assert!(picks2[1] >= baseline_arm1,
            "GKT should increase pulls on under-explored arm: baseline={baseline_arm1} with_gkt={}", picks2[1]);
        assert!(picks2[1] > picks2[0],
            "with budget=100, arm 1 should dominate; got {picks2:?}");
    }

    #[test]
    fn conformal_prediction_set_shrinks_with_data() {
        let mut b = make_bucket(3);
        let mut cfg = LearningConfig::default();
        cfg.conformal.enabled = true;
        cfg.conformal.coverage = 0.90;
        cfg.conformal.calibration_size = 50;
        cfg.window.enabled = true;
        cfg.window.size = 50;

        // No data → set covers everything.
        let initial = compute_prediction_set(&b, &cfg, 3);
        assert_eq!(initial.len(), 3);

        // Option 0 clearly better, low residuals after warmup.
        for _ in 0..30 { apply_feedback(&mut b, 0, 0.8, &cfg).unwrap(); }
        for _ in 0..15 { apply_feedback(&mut b, 1, -0.2, &cfg).unwrap(); }
        for _ in 0..15 { apply_feedback(&mut b, 2, -0.5, &cfg).unwrap(); }

        let set = compute_prediction_set(&b, &cfg, 3);
        assert!(set.contains(&0), "best option must be in the set");
        assert!(set.len() < 3, "set should shrink once data is informative; got {set:?}");
    }

    #[test]
    fn delayed_feedback_fuses_signals_into_posterior() {
        let mut cfg = LearningConfig::default();
        cfg.window.enabled = true;
        cfg.window.size = 50;
        cfg.delayed_feedback.enabled = true;
        cfg.delayed_feedback.signals = vec![
            DelayedSignalSpec { name: "surrogate".into(), noise_variance: 1.0, bias: 0.0 },
            DelayedSignalSpec { name: "final".into(), noise_variance: 0.05, bias: 0.0 },
        ];

        let mut b = make_bucket(2);
        // Noisy surrogate signals around 0.4, then a few high-confidence finals at 0.9.
        for _ in 0..5 { apply_feedback_signal(&mut b, 0, 0.4, "surrogate", &cfg).unwrap(); }
        let post_after_surrogate = b.stats[0].posterior_mean;
        for _ in 0..3 { apply_feedback_signal(&mut b, 0, 0.9, "final", &cfg).unwrap(); }
        let post_after_final = b.stats[0].posterior_mean;
        // Final signals (low noise) should pull the posterior strongly toward 0.9.
        assert!(post_after_final > post_after_surrogate + 0.2,
            "posterior {post_after_surrogate}→{post_after_final} did not move toward final");
        // Signal counts must reflect both kinds.
        assert_eq!(*b.stats[0].signal_counts.get("surrogate").unwrap_or(&0), 5);
        assert_eq!(*b.stats[0].signal_counts.get("final").unwrap_or(&0), 3);
    }

    #[test]
    fn model_surprise_detection_fires_on_distribution_shift() {
        let mut cfg = LearningConfig::default();
        cfg.window.enabled = true;
        cfg.window.size = 30;
        cfg.change_detection.enabled = true;
        cfg.change_detection.method = ChangeDetectionMethod::ModelSurprise;
        cfg.change_detection.surprise_k_sigma = 1.5;
        cfg.change_detection.surprise_fraction_threshold = 0.25;
        let mut b = make_bucket(2);
        // Stable narrow distribution.
        for i in 0..30 { apply_feedback(&mut b, 0, 0.5 + (i as f64) * 0.001, &cfg).unwrap(); }
        let cp_before = b.stats[0].change_points;
        // Sudden shift far from the established mean.
        for _ in 0..20 { apply_feedback(&mut b, 0, -1.5, &cfg).unwrap(); }
        assert!(b.stats[0].change_points > cp_before,
            "model-surprise should fire on distribution shift");
    }
}

#[cfg(test)]
mod candidate_context_tests {
    use super::*;
    use crate::meta_bandit::CandidateId;

    #[test]
    fn candidate_context_initialized_with_correct_option_state() {
        let mut mem = CapsuleMemory::default();
        let weights = vec![0.25, 0.25, 0.25, 0.25];

        let thompson_bucket = mem.get_or_init_candidate_context(
            42, "default", CandidateId::Thompson, &weights, 4,
        );
        for state in &thompson_bucket.option_states {
            assert!(matches!(state, OptionState::BetaBernoulli { alpha, beta }
                if (*alpha - 1.0).abs() < 1e-9 && (*beta - 1.0).abs() < 1e-9));
        }

        let ucb_bucket = mem.get_or_init_candidate_context(
            42, "default", CandidateId::Ucb, &weights, 4,
        );
        for state in &ucb_bucket.option_states {
            assert!(matches!(state, OptionState::Ucb { tries, .. } if tries.abs() < 1e-9));
        }

        let weighted_bucket = mem.get_or_init_candidate_context(
            42, "default", CandidateId::Weighted, &weights, 4,
        );
        for state in &weighted_bucket.option_states {
            assert!(matches!(state, OptionState::Weighted { .. }));
        }
    }

    #[test]
    fn candidate_contexts_are_independent() {
        let mut mem = CapsuleMemory::default();
        let weights = vec![0.5, 0.5];

        {
            let b = mem.get_or_init_candidate_context(7, "ctx", CandidateId::Thompson, &weights, 2);
            b.updated_at = 12345;
        }
        let ucb_b = mem.get_or_init_candidate_context(7, "ctx", CandidateId::Ucb, &weights, 2);
        assert_eq!(ucb_b.updated_at, 0);
    }

    #[test]
    fn legacy_get_or_init_context_unchanged() {
        let mut mem = CapsuleMemory::default();
        let weights = vec![0.5, 0.5];

        let bucket = mem.get_or_init_context(99, "ctx", &weights, 2);
        bucket.updated_at = 7;

        let sm = mem.strategies.get(&99).unwrap();
        assert!(sm.candidate_contexts.is_empty());
        assert_eq!(sm.contexts.get("ctx").unwrap().updated_at, 7);
    }

    #[test]
    fn serialization_roundtrip_preserves_candidate_contexts() {
        let mut mem = CapsuleMemory::default();
        let weights = vec![0.3, 0.7];

        {
            let b = mem.get_or_init_candidate_context(1, "ctx_a", CandidateId::Thompson, &weights, 2);
            b.updated_at = 1000;
        }
        {
            let b = mem.get_or_init_candidate_context(1, "ctx_b", CandidateId::Ucb, &weights, 2);
            b.updated_at = 2000;
        }

        let json = mem.to_json();
        let mem2 = CapsuleMemory::from_json(&json);

        let sm = mem2.strategies.get(&1).unwrap();
        let t_b = sm.candidate_contexts
            .get(&(CandidateId::Thompson, "ctx_a".to_string()))
            .unwrap();
        assert_eq!(t_b.updated_at, 1000);
        let u_b = sm.candidate_contexts
            .get(&(CandidateId::Ucb, "ctx_b".to_string()))
            .unwrap();
        assert_eq!(u_b.updated_at, 2000);
    }

    #[test]
    fn reset_candidate_contexts_clears_only_matching_context_key() {
        let mut mem = CapsuleMemory::default();
        let weights = vec![0.5, 0.5];

        mem.get_or_init_candidate_context(5, "keep", CandidateId::Thompson, &weights, 2);
        mem.get_or_init_candidate_context(5, "keep", CandidateId::Ucb, &weights, 2);
        mem.get_or_init_candidate_context(5, "drop", CandidateId::Thompson, &weights, 2);
        mem.get_or_init_candidate_context(5, "drop", CandidateId::Ucb, &weights, 2);

        mem.reset_candidate_contexts(5, "drop");

        let candidates = mem.candidates_for_context(5, "keep");
        assert_eq!(candidates.len(), 2);
        let candidates = mem.candidates_for_context(5, "drop");
        assert!(candidates.is_empty());
    }

    #[test]
    fn meta_bandit_roundtrip_through_json() {
        let mut mem = CapsuleMemory::default();
        {
            let mb = mem.get_or_init_meta_bandit(42, 3, &CandidateId::discrete_only());
            mb.forgetting_factor = 1.0; // disable decay for exact-equality assertions
            mb.record(CandidateId::Thompson, 0.8);
            mb.record(CandidateId::Ucb, 0.5);
            mb.forgetting_factor = 0.95;
        }

        let json = mem.to_json();
        let mem2 = CapsuleMemory::from_json(&json);

        let mb2 = mem2.meta_bandit_for(42).expect("meta-bandit should roundtrip");
        assert_eq!(mb2.total_rounds, 2);
        let thompson = mb2.candidates.iter().find(|c| c.id == CandidateId::Thompson).unwrap();
        assert!((thompson.trials - 1.0).abs() < 1e-9);
        assert!((thompson.cumulative_reward - 0.8).abs() < 1e-9);
        let ucb = mb2.candidates.iter().find(|c| c.id == CandidateId::Ucb).unwrap();
        assert!((ucb.trials - 1.0).abs() < 1e-9);
        assert!((ucb.cumulative_reward - 0.5).abs() < 1e-9);
        assert!((mb2.forgetting_factor - 0.95).abs() < 1e-9);
    }

    #[test]
    fn v3_memory_json_loads_without_meta_bandit() {
        // v3 capsules wrote candidateContexts but not metaBandit.
        let v3_json: serde_json::Value = serde_json::json!({
            "version": 3,
            "strategies": {
                "5": {
                    "nodeId": 5,
                    "nOptions": 2,
                    "contexts": {},
                    "candidateContexts": {},
                }
            }
        });
        let mem = CapsuleMemory::from_json(&v3_json);
        let sm = mem.strategies.get(&5).unwrap();
        assert!(sm.meta_bandit.is_none());
    }

    #[test]
    fn context_detector_created_on_demand() {
        let mut mem = CapsuleMemory::default();
        let d = mem.get_or_init_context_detector(5, "merchant_a");
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn context_detectors_are_independent() {
        let mut mem = CapsuleMemory::default();
        {
            let d = mem.get_or_init_context_detector(5, "merchant_a");
            for _ in 0..30 { d.add(0.5); }
        }
        let d_b = mem.get_or_init_context_detector(5, "merchant_b");
        assert_eq!(d_b.len(), 0, "merchant_b detector is independent of merchant_a");
    }

    #[test]
    fn context_detector_roundtrip_through_json() {
        let mut mem = CapsuleMemory::default();
        {
            let d = mem.get_or_init_context_detector(1, "ctx");
            for i in 0..20 { d.add(i as f64 / 20.0); }
        }
        let json = mem.to_json();
        let mem2 = CapsuleMemory::from_json(&json);
        let sm = mem2.strategies.get(&1).unwrap();
        let restored = sm.context_detectors.get("ctx").unwrap();
        assert_eq!(restored.len(), 20);
    }

    #[test]
    fn reset_context_detector_clears_window() {
        let mut mem = CapsuleMemory::default();
        {
            let d = mem.get_or_init_context_detector(1, "ctx");
            for _ in 0..30 { d.add(0.5); }
        }
        mem.reset_context_detector(1, "ctx");
        let d = mem.get_or_init_context_detector(1, "ctx");
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn v4_memory_loads_without_context_detectors() {
        let v4_json: serde_json::Value = serde_json::json!({
            "version": 4,
            "strategies": {
                "42": {
                    "nodeId": 42,
                    "nOptions": 2,
                    "contexts": {},
                    "candidateContexts": {},
                    "metaBandit": null
                }
            }
        });
        let mem = CapsuleMemory::from_json(&v4_json);
        let sm = mem.strategies.get(&42).unwrap();
        assert!(sm.context_detectors.is_empty());
    }

    #[test]
    fn beta_posterior_decays_toward_prior() {
        let mut state = OptionState::BetaBernoulli { alpha: 100.0, beta: 50.0 };
        let forgetting = 0.9;
        if let OptionState::BetaBernoulli { alpha, beta } = &mut state {
            *alpha = 1.0 + (*alpha - 1.0) * forgetting;
            *beta = 1.0 + (*beta - 1.0) * forgetting;
        }
        if let OptionState::BetaBernoulli { alpha, beta } = state {
            assert!((alpha - 90.1).abs() < 1e-6, "got alpha={alpha}");
            assert!((beta - 45.1).abs() < 1e-6, "got beta={beta}");
        } else {
            panic!("expected BetaBernoulli");
        }
    }

    #[test]
    fn ucb_state_decays_proportionally() {
        let mut state = OptionState::Ucb { tries: 100.0, total_reward: 75.0 };
        let forgetting = 0.9;
        if let OptionState::Ucb { tries, total_reward } = &mut state {
            *tries *= forgetting;
            *total_reward *= forgetting;
        }
        if let OptionState::Ucb { tries, total_reward } = state {
            assert!((tries - 90.0).abs() < 1e-6);
            assert!((total_reward - 67.5).abs() < 1e-6);
            assert!((total_reward / tries - 0.75).abs() < 1e-9);
        } else {
            panic!("expected Ucb");
        }
    }

    #[test]
    fn forgetting_factor_one_means_no_decay_in_option_state() {
        let alpha_before: f64 = 50.0;
        let alpha_after: f64 = 1.0 + (alpha_before - 1.0) * 1.0;
        assert!((alpha_after - alpha_before).abs() < 1e-9);
    }

    #[test]
    fn ucb_state_serializes_with_f64_tries() {
        let state = OptionState::Ucb { tries: 12.5, total_reward: 8.7 };
        let json = state.to_json();
        assert_eq!(json.get("tries").and_then(|v| v.as_f64()), Some(12.5));
        let restored = OptionState::from_json(&json).unwrap();
        if let OptionState::Ucb { tries, total_reward } = restored {
            assert!((tries - 12.5).abs() < 1e-9);
            assert!((total_reward - 8.7).abs() < 1e-9);
        } else {
            panic!("expected Ucb");
        }
    }

    #[test]
    fn ucb_state_loads_legacy_integer_tries() {
        let legacy_json = serde_json::json!({
            "kind": "ucb",
            "tries": 42,
            "totalReward": 30.5
        });
        let state = OptionState::from_json(&legacy_json).unwrap();
        if let OptionState::Ucb { tries, total_reward } = state {
            assert!((tries - 42.0).abs() < 1e-9);
            assert!((total_reward - 30.5).abs() < 1e-9);
        } else {
            panic!("expected Ucb");
        }
    }

    #[test]
    fn v2_memory_json_loads_without_candidate_contexts() {
        let v2_json: serde_json::Value = serde_json::json!({
            "version": 2,
            "strategies": {
                "42": {
                    "nodeId": 42,
                    "nOptions": 2,
                    "contexts": {
                        "default": {
                            "weights": [0.5, 0.5],
                            "stats": [],
                            "optionStates": [],
                            "conformityScores": [],
                            "updatedAt": 0
                        }
                    }
                }
            }
        });
        let mem = CapsuleMemory::from_json(&v2_json);
        let sm = mem.strategies.get(&42).unwrap();
        assert_eq!(sm.contexts.len(), 1);
        assert!(sm.candidate_contexts.is_empty());
    }

    fn fresh_bucket(n: usize) -> ContextBucket {
        let w = 1.0 / n as f64;
        ContextBucket {
            weights: vec![w; n],
            stats: (0..n).map(|_| OptionStats::default()).collect(),
            updated_at: 0,
            conformity_calibrator: crate::conformal::ConformalCalibrator::default_config(),
            option_states: (0..n).map(|_| OptionState::weighted(w)).collect(),
        }
    }

    #[test]
    fn apply_feedback_populates_calibrator() {
        let mut bucket = fresh_bucket(2);
        let cfg = LearningConfig::default();
        for _ in 0..50 {
            apply_feedback(&mut bucket, 0, 0.7, &cfg).unwrap();
        }
        assert!(bucket.conformity_calibrator.len() >= 30,
            "calibrator should have populated ≥30 residuals, got {}",
            bucket.conformity_calibrator.len());
        assert!(bucket.conformity_calibrator.quantile(0.05).is_some());
    }

    #[test]
    fn bucket_roundtrip_preserves_calibrator() {
        let mut bucket = fresh_bucket(2);
        for i in 0..40 {
            bucket.conformity_calibrator.record(0.5, 0.5 + (i as f64) * 0.01);
        }
        let q_before = bucket.conformity_calibrator.quantile(0.05).unwrap();

        let mut mem = CapsuleMemory::default();
        let mut contexts = HashMap::new();
        contexts.insert("ctx".to_string(), bucket);
        mem.strategies.insert(1, StrategyMemory {
            node_id: 1,
            n_options: 2,
            contexts,
            candidate_contexts: HashMap::new(),
            meta_bandit: None,
            context_detectors: HashMap::new(),
            discrete_ood: None,
            feature_ood: None,
        });
        let json = mem.to_json();
        assert_eq!(json.get("version").and_then(|v| v.as_u64()), Some(7));

        let mem2 = CapsuleMemory::from_json(&json);
        let restored = mem2.strategies.get(&1).unwrap()
            .contexts.get("ctx").unwrap();
        let q_after = restored.conformity_calibrator.quantile(0.05).unwrap();
        assert!((q_before - q_after).abs() < 1e-9,
            "quantile drift across roundtrip: before={q_before} after={q_after}");
    }

    #[test]
    fn legacy_conformity_scores_array_loads_into_calibrator() {
        let v5_json: serde_json::Value = serde_json::json!({
            "version": 5,
            "strategies": {
                "1": {
                    "nodeId": 1,
                    "nOptions": 2,
                    "contexts": {
                        "ctx": {
                            "weights": [0.5, 0.5],
                            "stats": [],
                            "optionStates": [],
                            "conformityScores": [
                                0.1, 0.2, 0.3, 0.4, 0.5,
                                0.1, 0.2, 0.3, 0.4, 0.5,
                                0.1, 0.2, 0.3, 0.4, 0.5,
                                0.1, 0.2, 0.3, 0.4, 0.5,
                                0.1, 0.2, 0.3, 0.4, 0.5,
                                0.1, 0.2, 0.3, 0.4, 0.5
                            ],
                            "updatedAt": 0
                        }
                    }
                }
            }
        });
        let mem = CapsuleMemory::from_json(&v5_json);
        let bucket = mem.strategies.get(&1).unwrap().contexts.get("ctx").unwrap();
        assert_eq!(bucket.conformity_calibrator.len(), 30);
        assert!(bucket.conformity_calibrator.quantile(0.05).is_some());
    }

    #[test]
    fn ood_state_roundtrips_through_memory_json() {
        let mut mem = CapsuleMemory::default();

        // Populate discrete detector.
        let d_det = mem.get_or_init_discrete_ood(7);
        for _ in 0..60 {
            d_det.record("known");
        }
        let score_known_before = mem.discrete_ood_for(7).unwrap().score("known");
        let score_unknown_before = mem.discrete_ood_for(7).unwrap().score("never_seen");

        // Populate feature detector.
        let f_det = mem.get_or_init_feature_ood(7, 3);
        let mut s: u64 = 11;
        for _ in 0..200 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r1 = (s >> 32) as f64 / (u32::MAX as f64 + 1.0) - 0.5;
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r2 = (s >> 32) as f64 / (u32::MAX as f64 + 1.0) - 0.5;
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r3 = (s >> 32) as f64 / (u32::MAX as f64 + 1.0) - 0.5;
            f_det.record(&[r1, r2, r3]);
        }
        f_det.rebuild_cov_inv();
        let far_score_before = mem.feature_ood_for(7).unwrap().score(&[10.0, 10.0, 10.0]);

        let json = mem.to_json();
        assert_eq!(json.get("version").and_then(|v| v.as_u64()), Some(7));
        let restored = CapsuleMemory::from_json(&json);

        assert!((restored.discrete_ood_for(7).unwrap().score("known") - score_known_before).abs() < 1e-9);
        assert!((restored.discrete_ood_for(7).unwrap().score("never_seen") - score_unknown_before).abs() < 1e-9);
        assert!((restored.feature_ood_for(7).unwrap().score(&[10.0, 10.0, 10.0]) - far_score_before).abs() < 1e-9);
    }

    #[test]
    fn refusal_config_defaults_to_disabled() {
        let cfg = RefusalConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.coverage, 0.95);
        assert_eq!(cfg.max_interval_width, 0.5);
        assert_eq!(cfg.ood_threshold, 0.8);
    }

    #[test]
    fn refusal_config_roundtrips() {
        let cfg = RefusalConfig {
            enabled: true,
            coverage: 0.99,
            max_interval_width: 0.2,
            ood_threshold: 0.5,
        };
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["maxIntervalWidth"].as_f64(), Some(0.2));
        assert_eq!(json["oodThreshold"].as_f64(), Some(0.5));
        let restored: RefusalConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, restored);
    }

    #[test]
    fn learning_config_loads_refusal_block() {
        let json = serde_json::json!({
            "refusal": {
                "enabled": true,
                "coverage": 0.99,
                "maxIntervalWidth": 0.15,
                "oodThreshold": 0.6
            }
        });
        let cfg = LearningConfig::from_json(&json);
        assert!(cfg.refusal.enabled);
        assert_eq!(cfg.refusal.coverage, 0.99);
        assert_eq!(cfg.refusal.max_interval_width, 0.15);
        assert_eq!(cfg.refusal.ood_threshold, 0.6);
    }

    #[test]
    fn action_space_discrete_returns_no_midpoint() {
        let a = ActionSpace::Discrete;
        assert!(a.bucket_midpoint(0).is_none());
        assert!(a.bucket_midpoint(5).is_none());
    }

    #[test]
    fn action_space_continuous_midpoint_math() {
        let a = ActionSpace::Continuous { range: [0.0, 1.0], buckets: 5 };
        // Bucket width = 0.2; midpoints at 0.1, 0.3, 0.5, 0.7, 0.9.
        assert!((a.bucket_midpoint(0).unwrap() - 0.1).abs() < 1e-9);
        assert!((a.bucket_midpoint(2).unwrap() - 0.5).abs() < 1e-9);
        assert!((a.bucket_midpoint(4).unwrap() - 0.9).abs() < 1e-9);
        assert!(a.bucket_midpoint(5).is_none()); // out of bounds
    }

    #[test]
    fn action_space_continuous_negative_range() {
        let a = ActionSpace::Continuous { range: [-100.0, 100.0], buckets: 4 };
        // Width = 50; midpoints at -75, -25, +25, +75.
        assert!((a.bucket_midpoint(0).unwrap() - (-75.0)).abs() < 1e-9);
        assert!((a.bucket_midpoint(3).unwrap() - 75.0).abs() < 1e-9);
    }

    #[test]
    fn learning_config_roundtrips_action_space() {
        let json = serde_json::json!({
            "actionSpace": {"type": "continuous", "range": [0.0, 50.0], "buckets": 10}
        });
        let cfg = LearningConfig::from_json(&json);
        match cfg.action_space {
            ActionSpace::Continuous { range, buckets } => {
                assert_eq!(range, [0.0, 50.0]);
                assert_eq!(buckets, 10);
            }
            _ => panic!("expected Continuous action space"),
        }
        // Roundtrip through to_json.
        let out = cfg.to_json();
        let reparsed = LearningConfig::from_json(&out);
        assert_eq!(reparsed.action_space, cfg.action_space);
    }

    #[test]
    fn learning_config_without_refusal_uses_default() {
        let cfg = LearningConfig::from_json(&serde_json::json!({}));
        assert_eq!(cfg.refusal, RefusalConfig::default());
    }

    #[test]
    fn v6_memory_loads_without_ood_fields() {
        // v6 sidecar has no discreteOod / featureOod keys.
        let v6_json: serde_json::Value = serde_json::json!({
            "version": 6,
            "strategies": {
                "3": {
                    "nodeId": 3,
                    "nOptions": 2,
                    "contexts": {},
                    "candidateContexts": {},
                    "metaBandit": null,
                    "contextDetectors": {}
                }
            }
        });
        let mem = CapsuleMemory::from_json(&v6_json);
        let sm = mem.strategies.get(&3).unwrap();
        assert!(sm.discrete_ood.is_none());
        assert!(sm.feature_ood.is_none());
    }
}
