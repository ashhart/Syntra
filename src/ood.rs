use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// OOD detector for discrete (string-keyed) contexts.
///
/// Tracks how many times each contextKey has been seen and when it was
/// last seen. A novel key is OOD with maximal score. A key that hasn't
/// been seen in a long time becomes increasingly OOD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscreteOodDetector {
    pub seen: HashMap<String, (u64, u64)>,
    pub total_rounds: u64,
    pub staleness_threshold: u64,
    pub min_warmup_rounds: u64,
}

impl DiscreteOodDetector {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
            total_rounds: 0,
            staleness_threshold: 1000,
            min_warmup_rounds: 50,
        }
    }

    pub fn record(&mut self, context_key: &str) {
        self.total_rounds += 1;
        let entry = self.seen.entry(context_key.to_string()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = self.total_rounds;
    }

    /// OOD score for `context_key`, computed BEFORE record() is called for
    /// the current request. 0.0 = clearly in-distribution; 1.0 = unseen or
    /// fully stale; intermediate values = staleness ratio.
    pub fn score(&self, context_key: &str) -> f64 {
        if self.total_rounds < self.min_warmup_rounds {
            return 0.0;
        }
        match self.seen.get(context_key) {
            None => 1.0,
            Some((_count, last_seen)) => {
                let staleness = self.total_rounds.saturating_sub(*last_seen);
                if staleness >= self.staleness_threshold {
                    1.0
                } else {
                    staleness as f64 / self.staleness_threshold as f64
                }
            }
        }
    }

    pub fn is_ood(&self, context_key: &str, threshold: f64) -> bool {
        self.score(context_key) >= threshold
    }

    pub fn reset(&mut self) {
        self.seen.clear();
        self.total_rounds = 0;
    }
}

impl Default for DiscreteOodDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// OOD detector for feature-vector contexts. Welford running mean+covariance;
/// score is Mahalanobis² normalized by `d + 3·sqrt(2d)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureOodDetector {
    pub mean: Vec<f64>,
    pub m2: Vec<Vec<f64>>,
    pub d: usize,
    pub n: u64,
    pub cov_inv: Vec<Vec<f64>>,
    pub since_last_rebuild: u64,
    pub regularization: f64,
    pub min_warmup_rounds: u64,
}

impl FeatureOodDetector {
    pub fn new(d: usize) -> Self {
        let mut cov_inv = vec![vec![0.0; d]; d];
        for i in 0..d {
            cov_inv[i][i] = 1.0;
        }
        Self {
            mean: vec![0.0; d],
            m2: vec![vec![0.0; d]; d],
            d,
            n: 0,
            cov_inv,
            since_last_rebuild: 0,
            regularization: 1e-4,
            min_warmup_rounds: 50,
        }
    }

    /// Welford-style online update for mean and M2 (sum of squared deviations).
    pub fn record(&mut self, x: &[f64]) {
        if x.len() != self.d {
            return;
        }
        self.n += 1;
        let n_f = self.n as f64;
        let mut delta = vec![0.0; self.d];
        for i in 0..self.d {
            delta[i] = x[i] - self.mean[i];
            self.mean[i] += delta[i] / n_f;
        }
        for i in 0..self.d {
            let dy = x[i] - self.mean[i];
            for j in 0..self.d {
                self.m2[i][j] += delta[j] * dy;
            }
        }
        self.since_last_rebuild += 1;
    }

    /// Rebuild Σ⁻¹ from M2 / (n-1) + λI. O(d³). Falls back to existing
    /// cov_inv on inversion failure.
    pub fn rebuild_cov_inv(&mut self) {
        if self.n < 2 {
            return;
        }
        let denom = (self.n - 1) as f64;
        let mut cov = vec![vec![0.0; self.d]; self.d];
        for i in 0..self.d {
            for j in 0..self.d {
                cov[i][j] = self.m2[i][j] / denom;
                if i == j {
                    cov[i][j] += self.regularization;
                }
            }
        }
        if let Some(inv) = crate::linucb::gauss_jordan_invert(&cov) {
            self.cov_inv = inv;
        }
        self.since_last_rebuild = 0;
    }

    pub fn mahalanobis_sq(&self, x: &[f64]) -> f64 {
        if x.len() != self.d {
            return 0.0;
        }
        let mut centered = vec![0.0; self.d];
        for i in 0..self.d {
            centered[i] = x[i] - self.mean[i];
        }
        let cov_inv_centered = crate::linucb::matvec(&self.cov_inv, &centered);
        let d_sq = crate::linucb::dot(&centered, &cov_inv_centered);
        d_sq.max(0.0)
    }

    /// OOD score: Mahalanobis² normalized by the chi-squared 99% rule
    /// (d + 3·sqrt(2d)). Capped at 10.0.
    pub fn score(&self, x: &[f64]) -> f64 {
        if self.n < self.min_warmup_rounds {
            return 0.0;
        }
        let d_sq = self.mahalanobis_sq(x);
        let chi_sq_99 = self.d as f64 + 3.0 * (2.0 * self.d as f64).sqrt();
        if chi_sq_99 <= 0.0 {
            return 0.0;
        }
        (d_sq / chi_sq_99).min(10.0)
    }

    pub fn is_ood(&self, x: &[f64], threshold: f64) -> bool {
        self.score(x) >= threshold
    }

    pub fn rebuild_due(&self, threshold: u64) -> bool {
        self.since_last_rebuild >= threshold
    }

    pub fn reset(&mut self) {
        self.mean = vec![0.0; self.d];
        self.m2 = vec![vec![0.0; self.d]; self.d];
        self.n = 0;
        self.cov_inv = vec![vec![0.0; self.d]; self.d];
        for i in 0..self.d {
            self.cov_inv[i][i] = 1.0;
        }
        self.since_last_rebuild = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Discrete OOD ──

    #[test]
    fn unseen_key_after_warmup_is_ood() {
        let mut det = DiscreteOodDetector::new();
        for _ in 0..50 {
            det.record("known");
        }
        assert_eq!(det.score("unknown"), 1.0);
        assert!(det.is_ood("unknown", 0.5));
    }

    #[test]
    fn during_warmup_nothing_is_ood() {
        let mut det = DiscreteOodDetector::new();
        for _ in 0..10 {
            det.record("a");
        }
        assert_eq!(det.score("b"), 0.0);
        assert!(!det.is_ood("b", 0.5));
    }

    #[test]
    fn freshly_seen_key_scores_zero() {
        let mut det = DiscreteOodDetector::new();
        for _ in 0..100 {
            det.record("a");
        }
        let score = det.score("a");
        assert!(score < 0.01, "got score = {}", score);
    }

    #[test]
    fn stale_key_scores_high() {
        let mut det = DiscreteOodDetector::new();
        det.staleness_threshold = 100;
        det.record("a");
        for _ in 0..200 {
            det.record("b");
        }
        assert_eq!(det.score("a"), 1.0);
    }

    #[test]
    fn discrete_reset_clears() {
        let mut det = DiscreteOodDetector::new();
        for _ in 0..100 {
            det.record("a");
        }
        det.reset();
        assert_eq!(det.score("a"), 0.0);
    }

    // ── Feature OOD ──

    fn rand_f64(s: &mut u64) -> f64 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*s >> 32) as f64 / (u32::MAX as f64 + 1.0)
    }

    #[test]
    fn feature_detector_in_distribution_low_score() {
        let mut det = FeatureOodDetector::new(3);
        let mut s = 42u64;
        for _ in 0..200 {
            let x = vec![
                rand_f64(&mut s) - 0.5,
                rand_f64(&mut s) - 0.5,
                rand_f64(&mut s) - 0.5,
            ];
            det.record(&x);
        }
        det.rebuild_cov_inv();
        let test_vec = vec![0.1, -0.1, 0.05];
        let score = det.score(&test_vec);
        assert!(score < 1.0, "in-distribution vector scored {} (expected < 1.0)", score);
    }

    #[test]
    fn feature_detector_far_vector_high_score() {
        let mut det = FeatureOodDetector::new(3);
        let mut s = 42u64;
        for _ in 0..200 {
            let x = vec![
                rand_f64(&mut s) - 0.5,
                rand_f64(&mut s) - 0.5,
                rand_f64(&mut s) - 0.5,
            ];
            det.record(&x);
        }
        det.rebuild_cov_inv();
        let far_vec = vec![10.0, 10.0, 10.0];
        let score = det.score(&far_vec);
        assert!(score > 5.0, "far vector scored {} (expected > 5.0)", score);
    }

    #[test]
    fn feature_detector_during_warmup_scores_zero() {
        let mut det = FeatureOodDetector::new(3);
        for _ in 0..10 {
            det.record(&[0.0, 0.0, 0.0]);
        }
        assert_eq!(det.score(&[10.0, 10.0, 10.0]), 0.0);
    }

    #[test]
    fn welford_mean_matches_arithmetic_mean() {
        let mut det = FeatureOodDetector::new(2);
        for x in &[[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]] {
            det.record(x);
        }
        assert!((det.mean[0] - 4.0).abs() < 1e-9);
        assert!((det.mean[1] - 5.0).abs() < 1e-9);
    }

    #[test]
    fn rebuild_due_flags_correctly() {
        let mut det = FeatureOodDetector::new(2);
        assert!(!det.rebuild_due(100));
        for _ in 0..100 {
            det.record(&[0.5, 0.5]);
        }
        assert!(det.rebuild_due(100));
        det.rebuild_cov_inv();
        assert!(!det.rebuild_due(100));
    }

    #[test]
    fn feature_reset_clears() {
        let mut det = FeatureOodDetector::new(2);
        for _ in 0..100 {
            det.record(&[1.0, 1.0]);
        }
        det.rebuild_cov_inv();
        det.reset();
        assert_eq!(det.n, 0);
        assert_eq!(det.mean, vec![0.0, 0.0]);
        assert_eq!(det.score(&[100.0, 100.0]), 0.0);
    }

    #[test]
    fn mahalanobis_uses_covariance() {
        // Anisotropic data: x_0 has wide variance, x_1 narrow. Two vectors
        // equidistant in Euclidean terms should produce very different
        // Mahalanobis distances.
        let mut det = FeatureOodDetector::new(2);
        let mut s = 7u64;
        for _ in 0..500 {
            let x = vec![
                (rand_f64(&mut s) - 0.5) * 10.0,
                (rand_f64(&mut s) - 0.5) * 0.1,
            ];
            det.record(&x);
        }
        det.rebuild_cov_inv();
        let along_x0 = det.mahalanobis_sq(&[5.0, 0.0]);
        let along_x1 = det.mahalanobis_sq(&[0.0, 5.0]);
        assert!(
            along_x1 > along_x0 * 100.0,
            "narrow-axis ({}) should dwarf wide-axis ({})",
            along_x1, along_x0,
        );
    }
}
