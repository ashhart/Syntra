use std::collections::HashMap;

use crate::feature_schema::ContextSpec;

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

/// Action-space declaration. For `Continuous`, the K options are buckets
/// over a numeric range; the decide response surfaces both the bucket
/// index and the bucket midpoint as `chosenAction`.
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
    /// Gaussian Thompson sampling on the posterior mean reward (Lycan
    /// rewards are continuous so we use a Normal posterior).
    ThompsonSampling,
    Softmax { temperature: f64 },
}

#[derive(Debug, Clone)]
pub struct DecayConfig {
    pub enabled: bool,
    /// Half-life applied per-feedback (count-based, not wall-clock).
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
    /// Geometric forgetting factor for candidate-bucket OptionState.
    /// `1.0` disables decay; `0.999` ≈ 700-event half-life.
    pub option_state_forgetting: f64,
    /// ADWIN `delta` for the capsule-level change detector.
    /// Smaller is stricter; tuned to fire after per-context detectors.
    pub capsule_adwin_delta: f64,
    /// ADWIN `delta` for per-(node_id, context_key) detectors. Looser
    /// than the capsule-level value so a single bucket is flagged first.
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

/// Score kind for shared-state LinUCB. `LinTs` is Cholesky-sampled and
/// falls back to the posterior mean on Cholesky failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedStateScoreKind {
    Ucb,
    LinTs,
}

impl Default for SharedStateScoreKind {
    fn default() -> Self { SharedStateScoreKind::Ucb }
}

/// Capsule-level configuration for shared-state LinUCB. When enabled,
/// `do_decide`/`do_feedback` route through `SharedStateOptionStrategy`'s
/// shared theta instead of the per-option LinUcb path.
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

/// Default ADWIN `delta` for the capsule-level detector.
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

            // Legacy `adwinDelta` is accepted as a fallback for both layers.
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
