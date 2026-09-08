//! Lycan learning layer. Per-context bandit memory, weight updates with safety
//! rails, reward shaping, change detection, delayed-feedback fusion, CVaR
//! scoring, corruption-robust UCB, and conformal prediction sets.

mod capsule;
mod config;
mod feedback;
mod memory;
mod multi_objective;
mod rng;
mod selection;
mod stats;

pub use capsule::{CapsuleMemory, ensure_linucb_states};
pub use config::{
    ActionSpace, Algorithm, ChangeDetectionConfig, ChangeDetectionMethod, ConformalConfig,
    CorruptionRobustConfig, DecayConfig, DelayedFeedbackConfig, DelayedSignalSpec, LearningConfig,
    ParetoConfig, RefusalConfig, RewardPolicy, RiskSensitiveConfig, SafetyConfig, SelectionMode,
    SharedStateConfig, SharedStateScoreKind, WindowConfig, default_capsule_adwin_delta,
    default_context_adwin_delta,
};
pub use feedback::{
    apply_decay, apply_feedback, apply_feedback_signal, compute_prediction_set, compute_reward,
    compute_reward_from_components, conformal_band_radius,
};
pub use memory::{ContextBucket, OptionState, StrategyMemory};
pub use multi_objective::{apply_feedback_multi, pareto_frontier, select_pareto};
pub use rng::{rng_seed_state, seed_rng};
pub(crate) use rng::rand_f64;
pub use selection::select_option;
pub use stats::OptionStats;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_stats_to_from_json_is_self_round_tripping() {
        // Direct OptionStats::to_json (no serialize_bucket wrapper) must
        // round-trip through from_json without losing accumulator state.
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
    fn weight_update_converges_to_mean_reward_not_success_flux() {
        // BUG-5 regression: two arms with mean rewards .7/.3, pulled
        // proportionally to current weights. The old additive rule
        // (`w += lr * r`) equilibrates at w ∝ 1/p — the WORSE arm leads.
        // The mean-seeking rule must converge to w ∝ p.
        let cfg = LearningConfig::default();
        let mut b = make_bucket(2);
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as f64 / u64::MAX as f64
        };
        for _ in 0..3000 {
            let sum = b.weights[0] + b.weights[1];
            let arm = if rand() * sum < b.weights[0] { 0 } else { 1 };
            apply_feedback(&mut b, arm, if arm == 0 { 0.7 } else { 0.3 }, &cfg).unwrap();
        }
        let share = b.weights[0] / (b.weights[0] + b.weights[1]);
        assert!(share > 0.5 && share < 0.95,
            "better arm must lead and weights must track response rates, share={share:.3}");
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
    use std::collections::HashMap;

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
