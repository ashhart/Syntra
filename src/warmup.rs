use serde::{Deserialize, Serialize};
use crate::reward_characterization::{RewardShape, PickedAlgorithm, characterize, pick_algorithm};
use crate::change_detection::{AdwinDetector, ChangeDetected};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CapsuleLifecycle {
    Warmup { samples_collected: usize, target: usize },
    Active { algorithm: PickedAlgorithm, characterization: RewardShape },
    Frozen { algorithm: PickedAlgorithm, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FeedbackOutcome {
    /// Still in warmup, more samples needed.
    Collecting { collected: usize, target: usize },
    /// Warmup just finished, capsule moved to Active.
    WarmupComplete { algorithm: PickedAlgorithm, characterization: RewardShape },
    /// Active state, nothing notable.
    ActiveStable,
    /// Active state, change detector fired. Capsule moved back to Warmup.
    ChangeDetected {
        change: ChangeDetected,
        previous_algorithm: PickedAlgorithm,
    },
    /// Capsule is frozen, feedback ignored for lifecycle purposes.
    FrozenIgnored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupState {
    pub lifecycle: CapsuleLifecycle,
    pub collected_rewards: Vec<f64>,
    pub detector: AdwinDetector,
    pub target_samples: usize,
}

impl WarmupState {
    pub fn new(target_samples: usize) -> Self {
        Self::with_capsule_delta(
            target_samples,
            AdwinDetector::capsule_level_config().delta(),
        )
    }

    /// `WarmupState` with an explicit capsule-level ADWIN delta.
    pub fn with_capsule_delta(target_samples: usize, capsule_adwin_delta: f64) -> Self {
        Self {
            lifecycle: CapsuleLifecycle::Warmup {
                samples_collected: 0,
                target: target_samples,
            },
            collected_rewards: Vec::with_capacity(target_samples),
            detector: AdwinDetector::new(capsule_adwin_delta, 1000),
            target_samples,
        }
    }

    pub fn is_warmup(&self) -> bool {
        matches!(self.lifecycle, CapsuleLifecycle::Warmup { .. })
    }

    pub fn is_active(&self) -> bool {
        matches!(self.lifecycle, CapsuleLifecycle::Active { .. })
    }

    pub fn is_frozen(&self) -> bool {
        matches!(self.lifecycle, CapsuleLifecycle::Frozen { .. })
    }

    pub fn current_algorithm(&self) -> Option<&PickedAlgorithm> {
        match &self.lifecycle {
            CapsuleLifecycle::Active { algorithm, .. } => Some(algorithm),
            CapsuleLifecycle::Frozen { algorithm, .. } => Some(algorithm),
            CapsuleLifecycle::Warmup { .. } => None,
        }
    }

    /// Record a feedback reward. Returns a FeedbackOutcome describing what happened.
    pub fn record_feedback(&mut self, reward: f64) -> FeedbackOutcome {
        match &mut self.lifecycle {
            CapsuleLifecycle::Warmup { samples_collected, target } => {
                self.collected_rewards.push(reward);
                *samples_collected += 1;
                let collected = *samples_collected;
                let target = *target;

                if collected >= target {
                    let shape = characterize(&self.collected_rewards);
                    let algorithm = pick_algorithm(&shape);
                    // Seed the detector with warmup samples before Active.
                    for r in &self.collected_rewards {
                        let _ = self.detector.add(*r);
                    }
                    self.lifecycle = CapsuleLifecycle::Active {
                        algorithm: algorithm.clone(),
                        characterization: shape.clone(),
                    };
                    FeedbackOutcome::WarmupComplete {
                        algorithm,
                        characterization: shape,
                    }
                } else {
                    FeedbackOutcome::Collecting { collected, target }
                }
            }
            CapsuleLifecycle::Active { algorithm, .. } => {
                let previous_algorithm = algorithm.clone();
                if let Some(change) = self.detector.add(reward) {
                    // Change detected. Transition back to Warmup with fresh state.
                    self.detector.reset();
                    self.collected_rewards.clear();
                    self.lifecycle = CapsuleLifecycle::Warmup {
                        samples_collected: 0,
                        target: self.target_samples,
                    };
                    FeedbackOutcome::ChangeDetected {
                        change,
                        previous_algorithm,
                    }
                } else {
                    FeedbackOutcome::ActiveStable
                }
            }
            CapsuleLifecycle::Frozen { .. } => FeedbackOutcome::FrozenIgnored,
        }
    }

    pub fn freeze(&mut self, reason: String) {
        if let Some(algo) = self.current_algorithm().cloned() {
            self.lifecycle = CapsuleLifecycle::Frozen { algorithm: algo, reason };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_starts_in_warmup_state() {
        let w = WarmupState::new(30);
        assert!(w.is_warmup());
        assert!(!w.is_active());
        assert!(w.current_algorithm().is_none());
    }

    #[test]
    fn warmup_collects_then_transitions() {
        let mut w = WarmupState::new(30);
        for i in 0..29 {
            let outcome = w.record_feedback(if i % 2 == 0 { 1.0 } else { 0.0 });
            assert!(matches!(outcome, FeedbackOutcome::Collecting { .. }));
            assert!(w.is_warmup());
        }
        let outcome = w.record_feedback(1.0);
        assert!(matches!(outcome, FeedbackOutcome::WarmupComplete { .. }));
        assert!(w.is_active());
        assert!(matches!(w.current_algorithm(), Some(PickedAlgorithm::Thompson { .. })));
    }

    #[test]
    fn continuous_reward_picks_weighted_or_ucb() {
        let mut w = WarmupState::new(30);
        for i in 0..30 {
            w.record_feedback(0.3 + (i as f64) * 0.01);
        }
        assert!(w.is_active());
        let algo = w.current_algorithm().unwrap();
        assert!(matches!(algo, PickedAlgorithm::Weighted { .. } | PickedAlgorithm::UCB { .. }));
    }

    #[test]
    fn active_stable_when_no_change() {
        let mut w = WarmupState::new(30);
        for _ in 0..30 {
            w.record_feedback(0.5);
        }
        assert!(w.is_active());
        // Feed a stable stream that matches what we trained on
        for _ in 0..50 {
            let outcome = w.record_feedback(0.5);
            assert!(matches!(outcome, FeedbackOutcome::ActiveStable));
        }
        assert!(w.is_active());
    }

    #[test]
    fn change_detection_reverts_to_warmup() {
        let mut w = WarmupState::new(30);
        // Warmup with Bernoulli(0.1)
        for i in 0..30 {
            w.record_feedback(if (i * 7919) % 10 < 1 { 1.0 } else { 0.0 });
        }
        assert!(w.is_active());
        // Now drop in Bernoulli(0.9)
        let mut reverted = false;
        for i in 0..200 {
            let r = if (i * 7919) % 10 < 9 { 1.0 } else { 0.0 };
            let outcome = w.record_feedback(r);
            if matches!(outcome, FeedbackOutcome::ChangeDetected { .. }) {
                reverted = true;
                break;
            }
        }
        assert!(reverted, "change should be detected after regime shift");
        assert!(w.is_warmup(), "should be back in warmup after change");
    }

    #[test]
    fn frozen_capsule_ignores_feedback() {
        let mut w = WarmupState::new(5);
        for _ in 0..5 {
            w.record_feedback(1.0);
        }
        w.freeze("manual".to_string());
        let outcome = w.record_feedback(1.0);
        assert!(matches!(outcome, FeedbackOutcome::FrozenIgnored));
    }

    #[test]
    fn detector_resets_on_change() {
        let mut w = WarmupState::new(30);
        for _ in 0..30 {
            w.record_feedback(0.1);
        }
        for i in 0..200 {
            let r = if (i * 7919) % 10 < 9 { 1.0 } else { 0.0 };
            if matches!(w.record_feedback(r), FeedbackOutcome::ChangeDetected { .. }) {
                break;
            }
        }
        // After change, we're back in Warmup. The detector should be empty.
        assert_eq!(w.detector.len(), 0);
        assert!(w.collected_rewards.is_empty());
    }
}
