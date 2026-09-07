use std::collections::HashMap;

use crate::change_detection::AdwinDetector;
use crate::linucb::LinUcbState;
use crate::meta_bandit::{CandidateId, MetaBandit};

use super::stats::OptionStats;

// ── Strategy memory ──

#[derive(Debug, Clone)]
pub struct StrategyMemory {
    #[allow(dead_code)]
    pub node_id: u32,
    pub n_options: usize,
    pub contexts: HashMap<String, ContextBucket>,
    /// Per-candidate per-context buckets, keyed by (candidate, context_key).
    pub candidate_contexts: HashMap<(CandidateId, String), ContextBucket>,
    /// Per-(node_id) meta-bandit; `None` until first Active decision.
    pub meta_bandit: Option<MetaBandit>,
    /// Per-context ADWIN detectors, independent from the capsule-level one.
    pub context_detectors: HashMap<String, AdwinDetector>,
    pub discrete_ood: Option<crate::ood::DiscreteOodDetector>,
    pub feature_ood: Option<crate::ood::FeatureOodDetector>,
}

#[derive(Debug, Clone)]
pub enum OptionState {
    Weighted { weight: f64 },
    BetaBernoulli { alpha: f64, beta: f64 },
    /// `tries` is a soft count decayed under geometric forgetting alongside
    /// `total_reward`, so the ratio reflects the recent-weighted mean.
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
                // L2 norm of theta as a rough activity indicator; real
                // selection uses `ucb_score` on actual feature vectors.
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
    /// Split-conformal calibrator over |predicted − observed| residuals.
    /// Drives prediction-interval widths used by /decide refusal semantics.
    pub conformity_calibrator: crate::conformal::ConformalCalibrator,
}
