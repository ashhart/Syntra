use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RewardShape {
    Binary { positive_rate: f64 },
    BoundedContinuous { min: f64, max: f64, mean: f64, std: f64 },
    Sparse { density: f64, nonzero_mean: f64, nonzero_std: f64 },
    Unknown { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PickedAlgorithm {
    Thompson { alpha: f64, beta: f64 },
    UCB { c: f64 },
    Weighted { learning_rate: f64 },
    EpsilonGreedy { epsilon: f64 },
}

const MIN_SAMPLES: usize = 30;
const BINARY_TOL: f64 = 1e-9;
const SPARSITY_THRESHOLD: f64 = 0.7;
const HIGH_VARIANCE_CV: f64 = 0.5;

pub fn characterize(rewards: &[f64]) -> RewardShape {
    if rewards.len() < MIN_SAMPLES {
        return RewardShape::Unknown {
            reason: format!("need {} samples, got {}", MIN_SAMPLES, rewards.len()),
        };
    }

    let all_binary = rewards.iter().all(|r| r.abs() < BINARY_TOL || (r - 1.0).abs() < BINARY_TOL);
    if all_binary {
        let pos = rewards.iter().filter(|r| **r > 0.5).count() as f64;
        return RewardShape::Binary { positive_rate: pos / rewards.len() as f64 };
    }

    let zeros = rewards.iter().filter(|r| r.abs() < BINARY_TOL).count();
    let zero_ratio = zeros as f64 / rewards.len() as f64;
    if zero_ratio > SPARSITY_THRESHOLD {
        let nz: Vec<f64> = rewards.iter().filter(|r| r.abs() >= BINARY_TOL).copied().collect();
        if nz.is_empty() {
            return RewardShape::Unknown { reason: "all zero".to_string() };
        }
        let mean = nz.iter().sum::<f64>() / nz.len() as f64;
        let var = nz.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / nz.len() as f64;
        return RewardShape::Sparse {
            density: 1.0 - zero_ratio,
            nonzero_mean: mean,
            nonzero_std: var.sqrt(),
        };
    }

    let min = rewards.iter().copied().fold(f64::INFINITY, f64::min);
    let max = rewards.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = rewards.iter().sum::<f64>() / rewards.len() as f64;
    let var = rewards.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rewards.len() as f64;
    RewardShape::BoundedContinuous { min, max, mean, std: var.sqrt() }
}

pub fn pick_algorithm(shape: &RewardShape) -> PickedAlgorithm {
    match shape {
        RewardShape::Binary { .. } => PickedAlgorithm::Thompson { alpha: 1.0, beta: 1.0 },
        RewardShape::Sparse { .. } => PickedAlgorithm::UCB { c: 3.0 },
        RewardShape::BoundedContinuous { mean, std, .. } => {
            let cv = if mean.abs() > 1e-9 { std / mean.abs() } else { f64::INFINITY };
            if cv > HIGH_VARIANCE_CV {
                PickedAlgorithm::UCB { c: 2.0 }
            } else {
                PickedAlgorithm::Weighted { learning_rate: 0.1 }
            }
        }
        RewardShape::Unknown { .. } => PickedAlgorithm::Weighted { learning_rate: 0.05 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detected() {
        let rewards: Vec<f64> = (0..50).map(|i| if i % 3 == 0 { 1.0 } else { 0.0 }).collect();
        let shape = characterize(&rewards);
        assert!(matches!(shape, RewardShape::Binary { .. }));
        assert!(matches!(pick_algorithm(&shape), PickedAlgorithm::Thompson { .. }));
    }

    #[test]
    fn continuous_detected() {
        let rewards: Vec<f64> = (0..50).map(|i| 0.5 + (i as f64) * 0.01).collect();
        let shape = characterize(&rewards);
        assert!(matches!(shape, RewardShape::BoundedContinuous { .. }));
    }

    #[test]
    fn sparse_detected() {
        let mut rewards = vec![0.0; 40];
        rewards.extend([1.5, 2.3, 0.8, 1.1, 1.9, 0.5, 2.7, 1.3, 1.7, 2.1]);
        let shape = characterize(&rewards);
        assert!(matches!(shape, RewardShape::Sparse { .. }));
        assert!(matches!(pick_algorithm(&shape), PickedAlgorithm::UCB { c } if c > 2.5));
    }

    #[test]
    fn under_min_samples_unknown() {
        let rewards = vec![0.5; 10];
        assert!(matches!(characterize(&rewards), RewardShape::Unknown { .. }));
    }
}
