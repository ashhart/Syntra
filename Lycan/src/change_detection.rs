use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// ADWIN change detector. Maintains a window of recent observations and
/// detects mean shifts using Hoeffding's bound on subwindow means.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdwinDetector {
    /// Sliding window of recent observations.
    window: VecDeque<f64>,
    /// Confidence parameter. Lower = stricter (fewer false alarms, slower detection).
    delta: f64,
    /// Maximum window size to prevent unbounded growth.
    max_size: usize,
    /// Minimum subwindow size before checking splits.
    min_subwindow: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeDetected {
    /// How many observations were dropped from the front of the window.
    pub dropped: usize,
    /// Mean of the older subwindow that was dropped.
    pub old_mean: f64,
    /// Mean of the newer subwindow that remains.
    pub new_mean: f64,
}

impl AdwinDetector {
    pub fn new(delta: f64, max_size: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(max_size),
            delta,
            max_size,
            min_subwindow: 5,
        }
    }

    /// Default config for per-context detectors. Looser delta —
    /// smaller delta in the Hoeffding bound is MORE strict (the
    /// `check_for_change` math reads `ln(4n / delta)`, so smaller
    /// delta widens `epsilon` and slows detection). We keep this
    /// looser so it fires FIRST on single-context drift, which is
    /// what operators expect when only one bucket goes bad. Defaults
    /// chosen from synthetic characterization — see
    /// `tests/change_detection_characterization.rs` and the per-layer
    /// fields in `SafetyConfig`.
    pub fn default_config() -> Self {
        Self::new(0.002, 1000)
    }

    /// Default config for the capsule-level detector. Stricter (smaller)
    /// delta than the per-context default so per-context detectors fire
    /// first on narrow drift. The capsule-level detector is meant to
    /// catch *aggregate* shifts only.
    pub fn capsule_level_config() -> Self {
        Self::new(0.0005, 1000)
    }

    /// Rebuild a detector from persisted state. Used by deserialization paths
    /// in the memory sidecar.
    pub fn restore_state(
        window: Vec<f64>,
        delta: f64,
        max_size: usize,
        min_subwindow: usize,
    ) -> Self {
        let mut d = Self::new(delta, max_size);
        d.min_subwindow = min_subwindow;
        for v in window {
            d.window.push_back(v);
        }
        d
    }

    pub fn delta(&self) -> f64 { self.delta }
    pub fn max_size(&self) -> usize { self.max_size }
    pub fn min_subwindow(&self) -> usize { self.min_subwindow }
    pub fn window_snapshot(&self) -> Vec<f64> { self.window.iter().copied().collect() }

    pub fn len(&self) -> usize {
        self.window.len()
    }

    pub fn is_empty(&self) -> bool {
        self.window.is_empty()
    }

    /// Add an observation and check for change. Returns Some if change detected.
    pub fn add(&mut self, value: f64) -> Option<ChangeDetected> {
        self.window.push_back(value);
        if self.window.len() > self.max_size {
            self.window.pop_front();
        }
        self.check_for_change()
    }

    fn check_for_change(&mut self) -> Option<ChangeDetected> {
        let n = self.window.len();
        if n < 2 * self.min_subwindow {
            return None;
        }

        // Try every split point, looking for a significant mean difference.
        // Walk from oldest to newest; we want to drop the oldest data on change.
        for split in self.min_subwindow..=(n - self.min_subwindow) {
            let (old_slice, new_slice) = (
                self.window.iter().take(split).copied().collect::<Vec<_>>(),
                self.window.iter().skip(split).copied().collect::<Vec<_>>(),
            );

            let n_old = old_slice.len() as f64;
            let n_new = new_slice.len() as f64;
            let mean_old: f64 = old_slice.iter().sum::<f64>() / n_old;
            let mean_new: f64 = new_slice.iter().sum::<f64>() / n_new;

            // Hoeffding bound: epsilon = sqrt((1 / 2m) * ln(4n / delta))
            // where m is harmonic mean of subwindow sizes and n is total.
            let m = 1.0 / (1.0 / n_old + 1.0 / n_new);
            let epsilon = ((1.0 / (2.0 * m)) * (4.0 * n as f64 / self.delta).ln()).sqrt();

            if (mean_old - mean_new).abs() > epsilon {
                // Change detected. Drop old observations.
                for _ in 0..split {
                    self.window.pop_front();
                }
                return Some(ChangeDetected {
                    dropped: split,
                    old_mean: mean_old,
                    new_mean: mean_new,
                });
            }
        }
        None
    }

    /// Reset the detector entirely (e.g., on capsule re-warmup).
    pub fn reset(&mut self) {
        self.window.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_on_stationary_stream() {
        let mut d = AdwinDetector::new(0.002, 1000);
        // 200 samples from a stationary Bernoulli(0.3)
        let mut detected = None;
        for i in 0..200 {
            let v = if (i * 7919) % 10 < 3 { 1.0 } else { 0.0 };
            if let Some(c) = d.add(v) {
                detected = Some(c);
                break;
            }
        }
        assert!(detected.is_none(), "no change should be detected on stationary stream");
    }

    #[test]
    fn detects_clear_mean_shift() {
        let mut d = AdwinDetector::new(0.002, 1000);
        // 100 samples from Bernoulli(0.1), then 100 from Bernoulli(0.9)
        for i in 0..100 {
            d.add(if (i * 7919) % 10 < 1 { 1.0 } else { 0.0 });
        }
        let mut detected = None;
        for i in 0..100 {
            let v = if (i * 7919) % 10 < 9 { 1.0 } else { 0.0 };
            if let Some(c) = d.add(v) {
                detected = Some(c);
                break;
            }
        }
        let c = detected.expect("change should be detected after mean shift");
        assert!(c.old_mean < 0.3, "old mean should be low, got {}", c.old_mean);
        assert!(c.new_mean > 0.7, "new mean should be high, got {}", c.new_mean);
    }

    #[test]
    fn window_capped_at_max_size() {
        let mut d = AdwinDetector::new(0.002, 50);
        for _ in 0..200 {
            d.add(0.5);
        }
        assert!(d.len() <= 50);
    }

    #[test]
    fn reset_clears_window() {
        let mut d = AdwinDetector::new(0.002, 1000);
        for _ in 0..50 {
            d.add(0.5);
        }
        assert_eq!(d.len(), 50);
        d.reset();
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn detects_continuous_drift() {
        let mut d = AdwinDetector::new(0.002, 1000);
        // 100 samples around 0.2, then 100 samples around 0.8
        for i in 0..100 {
            let noise = ((i as f64 * 13.0).sin() + 1.0) / 20.0;
            d.add(0.2 + noise);
        }
        let mut detected = false;
        for i in 0..100 {
            let noise = ((i as f64 * 13.0).sin() + 1.0) / 20.0;
            if d.add(0.8 + noise).is_some() {
                detected = true;
                break;
            }
        }
        assert!(detected, "should detect shift from ~0.2 to ~0.8");
    }
}
