use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Split-conformal calibration over reward residuals.
///
/// Maintains a sliding window of absolute residuals (|observed - predicted|).
/// On query, returns the empirical (1-alpha) quantile, which is the
/// half-width of a prediction interval with coverage at least 1-alpha
/// under exchangeability assumptions.
///
/// Reference: Vovk, Gammerman, Shafer (2005) "Algorithmic Learning in a
/// Random World"; split-conformal variant per Lei et al. (2018).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformalCalibrator {
    residuals: VecDeque<f64>,
    max_size: usize,
    min_samples: usize,
}

impl ConformalCalibrator {
    pub fn new(max_size: usize, min_samples: usize) -> Self {
        Self {
            residuals: VecDeque::with_capacity(max_size),
            max_size,
            min_samples,
        }
    }

    pub fn default_config() -> Self {
        Self::new(500, 30)
    }

    pub fn len(&self) -> usize {
        self.residuals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.residuals.is_empty()
    }

    /// Record a new (predicted, observed) pair.
    pub fn record(&mut self, predicted: f64, observed: f64) {
        let residual = (observed - predicted).abs();
        if !residual.is_finite() {
            return;
        }
        self.residuals.push_back(residual);
        while self.residuals.len() > self.max_size {
            self.residuals.pop_front();
        }
    }

    /// Empirical (1-alpha) quantile of the residual window. This is the
    /// half-width of the (1-alpha) prediction interval.
    ///
    /// Returns None if not enough samples to be meaningful — caller should
    /// treat this as "infinite uncertainty" and refuse.
    pub fn quantile(&self, alpha: f64) -> Option<f64> {
        if self.residuals.len() < self.min_samples {
            return None;
        }
        let coverage = (1.0 - alpha).clamp(0.0, 1.0);
        let mut sorted: Vec<f64> = self.residuals.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len() as f64;
        // Split-conformal correction: use ceil((n+1) * (1-alpha)) / n index.
        // Finite-sample valid coverage at the cost of slightly conservative intervals.
        let idx = (((n + 1.0) * coverage).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        Some(sorted[idx])
    }

    /// Prediction interval width (2 × quantile). None when undefined.
    pub fn interval_width(&self, alpha: f64) -> Option<f64> {
        self.quantile(alpha).map(|q| 2.0 * q)
    }

    pub fn reset(&mut self) {
        self.residuals.clear();
    }

    pub fn residuals_snapshot(&self) -> Vec<f64> {
        self.residuals.iter().copied().collect()
    }

    pub fn max_size(&self) -> usize {
        self.max_size
    }

    pub fn min_samples(&self) -> usize {
        self.min_samples
    }

    pub fn restore_state(residuals: Vec<f64>, max_size: usize, min_samples: usize) -> Self {
        let mut c = Self::new(max_size, min_samples);
        for r in residuals.into_iter().take(max_size) {
            if r.is_finite() {
                c.residuals.push_back(r);
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_calibrator_returns_none() {
        let c = ConformalCalibrator::default_config();
        assert!(c.quantile(0.05).is_none());
        assert!(c.interval_width(0.05).is_none());
    }

    #[test]
    fn under_min_samples_returns_none() {
        let mut c = ConformalCalibrator::new(500, 30);
        for i in 0..29 {
            c.record(0.5, 0.5 + (i as f64) * 0.01);
        }
        assert!(c.quantile(0.05).is_none());
    }

    #[test]
    fn quantile_at_threshold_returns_value() {
        let mut c = ConformalCalibrator::new(500, 30);
        for i in 0..50 {
            c.record(0.5, 0.5 + (i as f64) * 0.01);
        }
        let q = c.quantile(0.05).unwrap();
        // 95th percentile of 0..0.49 (step 0.01) via split-conformal:
        // ceil(51 * 0.95) = 49th index (0-indexed 48) ≈ 0.48
        assert!((q - 0.48).abs() < 0.01, "got q = {}", q);
    }

    #[test]
    fn interval_width_is_double_quantile() {
        let mut c = ConformalCalibrator::new(500, 30);
        for i in 0..50 {
            c.record(0.5, 0.5 + (i as f64) * 0.01);
        }
        let q = c.quantile(0.05).unwrap();
        let w = c.interval_width(0.05).unwrap();
        assert!((w - 2.0 * q).abs() < 1e-9);
    }

    #[test]
    fn higher_coverage_gives_wider_interval() {
        let mut c = ConformalCalibrator::new(500, 30);
        for i in 0..100 {
            c.record(0.5, 0.5 + (i as f64) * 0.005);
        }
        let q90 = c.quantile(0.1).unwrap();
        let q95 = c.quantile(0.05).unwrap();
        let q99 = c.quantile(0.01).unwrap();
        assert!(q90 <= q95, "90% should be tighter than or equal to 95%");
        assert!(q95 <= q99, "95% should be tighter than or equal to 99%");
        assert!(q90 < q99, "90% should be strictly tighter than 99%");
    }

    #[test]
    fn perfect_predictions_give_zero_quantile() {
        let mut c = ConformalCalibrator::new(500, 30);
        for _ in 0..50 {
            c.record(0.5, 0.5);
        }
        let q = c.quantile(0.05).unwrap();
        assert_eq!(q, 0.0);
    }

    #[test]
    fn noisy_predictions_give_wide_quantile() {
        let mut c = ConformalCalibrator::new(500, 30);
        for i in 0..100 {
            let observed = if i % 2 == 0 { 0.8 } else { 0.5 };
            c.record(0.5, observed);
        }
        let q = c.quantile(0.05).unwrap();
        // 95th percentile of residuals {0.0, 0.3, 0.0, 0.3, ...} is 0.3
        assert!((q - 0.3).abs() < 0.05, "got q = {}", q);
    }

    #[test]
    fn window_cap_drops_oldest() {
        let mut c = ConformalCalibrator::new(10, 5);
        for i in 0..20 {
            c.record(0.0, i as f64);
        }
        assert_eq!(c.len(), 10);
        let snap = c.residuals_snapshot();
        assert!(snap.iter().all(|r| *r >= 10.0), "got {:?}", snap);
    }

    #[test]
    fn reset_clears_residuals() {
        let mut c = ConformalCalibrator::new(500, 30);
        for _ in 0..50 {
            c.record(0.5, 0.7);
        }
        c.reset();
        assert!(c.is_empty());
        assert!(c.quantile(0.05).is_none());
    }

    #[test]
    fn restore_state_preserves_residuals() {
        let mut c = ConformalCalibrator::new(500, 30);
        for i in 0..40 {
            c.record(0.5, 0.5 + (i as f64) * 0.01);
        }
        let snap = c.residuals_snapshot();
        let c2 = ConformalCalibrator::restore_state(snap, 500, 30);
        let q1 = c.quantile(0.05).unwrap();
        let q2 = c2.quantile(0.05).unwrap();
        assert!((q1 - q2).abs() < 1e-9);
    }

    #[test]
    fn nan_observation_does_not_crash() {
        let mut c = ConformalCalibrator::new(500, 30);
        for _ in 0..30 {
            c.record(0.5, 0.5);
        }
        c.record(0.5, f64::NAN);
        assert_eq!(c.len(), 30);
    }
}
