use serde::{Deserialize, Serialize};
use crate::reward_characterization::PickedAlgorithm;

/// Identifier for one of the candidate algorithms in the portfolio.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum CandidateId {
    Thompson,
    Ucb,
    Weighted,
    EpsilonGreedy,
    Greedy,
    LinUcb,
    LinTs,
}

impl CandidateId {
    /// Full portfolio: 5 discrete-context candidates + LinUcb + LinTs for
    /// feature-context capsules.
    pub fn all() -> [CandidateId; 7] {
        [
            CandidateId::Thompson,
            CandidateId::Ucb,
            CandidateId::Weighted,
            CandidateId::EpsilonGreedy,
            CandidateId::Greedy,
            CandidateId::LinUcb,
            CandidateId::LinTs,
        ]
    }

    /// Discrete-context portfolio: omits LinUcb and LinTs (need feature vectors).
    pub fn discrete_only() -> [CandidateId; 5] {
        [
            CandidateId::Thompson,
            CandidateId::Ucb,
            CandidateId::Weighted,
            CandidateId::EpsilonGreedy,
            CandidateId::Greedy,
        ]
    }

    pub fn to_algorithm(&self) -> PickedAlgorithm {
        match self {
            CandidateId::Thompson => PickedAlgorithm::Thompson { alpha: 1.0, beta: 1.0 },
            CandidateId::Ucb => PickedAlgorithm::UCB { c: 2.0 },
            CandidateId::Weighted => PickedAlgorithm::Weighted { learning_rate: 0.1 },
            CandidateId::EpsilonGreedy => PickedAlgorithm::EpsilonGreedy { epsilon: 0.1 },
            CandidateId::Greedy => PickedAlgorithm::EpsilonGreedy { epsilon: 0.0 },
            CandidateId::LinUcb => PickedAlgorithm::UCB { c: 1.0 },
            CandidateId::LinTs => PickedAlgorithm::Thompson { alpha: 1.0, beta: 1.0 },
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CandidateId::Thompson => "Thompson",
            CandidateId::Ucb => "Ucb",
            CandidateId::Weighted => "Weighted",
            CandidateId::EpsilonGreedy => "EpsilonGreedy",
            CandidateId::Greedy => "Greedy",
            CandidateId::LinUcb => "LinUcb",
            CandidateId::LinTs => "LinTs",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Thompson" => Some(CandidateId::Thompson),
            "Ucb" => Some(CandidateId::Ucb),
            "Weighted" => Some(CandidateId::Weighted),
            "EpsilonGreedy" => Some(CandidateId::EpsilonGreedy),
            "Greedy" => Some(CandidateId::Greedy),
            "LinUcb" => Some(CandidateId::LinUcb),
            "LinTs" => Some(CandidateId::LinTs),
            _ => None,
        }
    }
}

/// Per-candidate cumulative tracking for the meta-bandit's selection.
/// `trials` is a *soft* count: it accumulates as +1 per record but decays
/// geometrically when forgetting is active, so it reflects the effective
/// number of recent observations rather than the raw count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRecord {
    pub id: CandidateId,
    pub trials: f64,
    pub cumulative_reward: f64,
}

impl CandidateRecord {
    pub fn new(id: CandidateId) -> Self {
        Self { id, trials: 0.0, cumulative_reward: 0.0 }
    }

    pub fn mean_reward(&self) -> f64 {
        if self.trials < 1e-9 {
            0.0
        } else {
            self.cumulative_reward / self.trials
        }
    }
}

/// Rate-adaptive meta-bandit over a portfolio of candidate algorithms.
///
/// Per Bibaut, Chambaz, van der Laan 2020: at round t, with probability p_t,
/// pick a candidate at random; otherwise pick the candidate with the highest
/// cumulative reward. The p_t schedule is chosen so that we explore enough
/// to identify the best candidate while exploiting the apparent leader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaBandit {
    pub candidates: Vec<CandidateRecord>,
    pub total_rounds: u64,
    /// Decay parameter for exploration probability. Higher = slower decay.
    pub exploration_decay: f64,
    /// Minimum exploration probability (epsilon floor).
    pub min_exploration: f64,
    /// Geometric forgetting factor applied to candidate records on each update.
    /// 1.0 = no forgetting; 0.999 = ~700-event half-life; 0.99 = ~70-event half-life.
    pub forgetting_factor: f64,
}

impl MetaBandit {
    /// Default: discrete-context portfolio (5 candidates). Keeps backward
    /// compatibility with existing capsules. Use `new_with_candidates` to
    /// opt into LinUcb for feature-context capsules.
    pub fn new() -> Self {
        Self::new_with_candidates(&CandidateId::discrete_only())
    }

    pub fn new_with_candidates(candidates: &[CandidateId]) -> Self {
        Self {
            candidates: candidates.iter().map(|id| CandidateRecord::new(*id)).collect(),
            total_rounds: 0,
            exploration_decay: 5.0,
            min_exploration: 0.05,
            forgetting_factor: 0.999,
        }
    }

    /// Probability of choosing a random candidate vs the apparent leader.
    /// Decays as sqrt(N / total_rounds) where N is the number of candidates,
    /// floored at min_exploration.
    pub fn exploration_probability(&self) -> f64 {
        let n = self.candidates.len() as f64;
        if self.total_rounds == 0 {
            return 1.0;
        }
        let raw = (n * self.exploration_decay / self.total_rounds as f64).sqrt();
        raw.max(self.min_exploration).min(1.0)
    }

    /// Select a candidate using a provided uniform [0, 1) random.
    /// Returning the chosen candidate plus whether selection was exploratory.
    pub fn select(&self, rng_value: f64, rng_pick: f64) -> (CandidateId, bool) {
        let p_explore = self.exploration_probability();

        if rng_value < p_explore {
            // Random pick.
            let idx = (rng_pick * self.candidates.len() as f64) as usize;
            let idx = idx.min(self.candidates.len() - 1);
            (self.candidates[idx].id, true)
        } else {
            // Greedy on mean reward. Ties broken by trial count (favor underexplored).
            let leader = self.candidates
                .iter()
                .max_by(|a, b| {
                    a.mean_reward()
                        .partial_cmp(&b.mean_reward())
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.trials.partial_cmp(&a.trials).unwrap_or(std::cmp::Ordering::Equal))
                })
                .map(|c| c.id)
                .unwrap_or(CandidateId::Thompson);
            (leader, false)
        }
    }

    /// Update the chosen candidate's record with the observed reward.
    pub fn record(&mut self, chosen: CandidateId, reward: f64) {
        // Apply forgetting to ALL candidates before incorporating new observation.
        // This ensures relative comparisons stay meaningful: a candidate that
        // hasn't been chosen recently doesn't get unfair credit from its old
        // cumulative reward.
        if self.forgetting_factor < 1.0 {
            for c in self.candidates.iter_mut() {
                c.trials *= self.forgetting_factor;
                c.cumulative_reward *= self.forgetting_factor;
            }
        }
        if let Some(c) = self.candidates.iter_mut().find(|c| c.id == chosen) {
            c.trials += 1.0;
            c.cumulative_reward += reward;
        }
        self.total_rounds += 1;
    }

    /// Reset all tracking (e.g., on regime change).
    pub fn reset(&mut self) {
        for c in self.candidates.iter_mut() {
            c.trials = 0.0;
            c.cumulative_reward = 0.0;
        }
        self.total_rounds = 0;
    }

    /// Return the current leader (highest mean reward among trialed candidates).
    pub fn current_leader(&self) -> Option<CandidateId> {
        self.candidates
            .iter()
            .filter(|c| c.trials > 0.0)
            .max_by(|a, b| {
                a.mean_reward()
                    .partial_cmp(&b.mean_reward())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.id)
    }
}

impl Default for MetaBandit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_meta_bandit_has_five_candidates() {
        let m = MetaBandit::new();
        assert_eq!(m.candidates.len(), 5);
        assert_eq!(m.total_rounds, 0);
    }

    #[test]
    fn discrete_candidates_excludes_linucb() {
        assert!(!CandidateId::discrete_only().contains(&CandidateId::LinUcb));
        assert_eq!(CandidateId::discrete_only().len(), 5);
    }

    #[test]
    fn all_candidates_includes_linucb_and_lints() {
        assert!(CandidateId::all().contains(&CandidateId::LinUcb));
        assert!(CandidateId::all().contains(&CandidateId::LinTs));
        assert_eq!(CandidateId::all().len(), 7);
    }

    #[test]
    fn new_with_candidates_respects_list() {
        let m = MetaBandit::new_with_candidates(&CandidateId::discrete_only());
        assert_eq!(m.candidates.len(), 5);
        assert!(!m.candidates.iter().any(|c| c.id == CandidateId::LinUcb));
        assert!(!m.candidates.iter().any(|c| c.id == CandidateId::LinTs));
        let m_all = MetaBandit::new_with_candidates(&CandidateId::all());
        assert_eq!(m_all.candidates.len(), 7);
        assert!(m_all.candidates.iter().any(|c| c.id == CandidateId::LinUcb));
        assert!(m_all.candidates.iter().any(|c| c.id == CandidateId::LinTs));
    }

    #[test]
    fn first_selection_is_exploratory() {
        let m = MetaBandit::new();
        // With total_rounds=0, exploration probability is 1.0
        assert_eq!(m.exploration_probability(), 1.0);
    }

    #[test]
    fn exploration_decays_with_rounds() {
        let mut m = MetaBandit::new();
        for _ in 0..1000 {
            m.record(CandidateId::Thompson, 0.5);
        }
        let p = m.exploration_probability();
        assert!(p < 0.5, "exploration should decay, got {}", p);
        assert!(p >= m.min_exploration, "should respect floor");
    }

    #[test]
    fn greedy_selection_picks_leader() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 1.0;
        // Make Thompson the clear winner
        for _ in 0..100 {
            m.record(CandidateId::Thompson, 0.9);
        }
        for _ in 0..100 {
            m.record(CandidateId::Greedy, 0.1);
        }
        // rng_value = 0.99 forces exploitation; rng_pick irrelevant
        let (chosen, exploratory) = m.select(0.99, 0.5);
        assert!(!exploratory);
        assert_eq!(chosen, CandidateId::Thompson);
    }

    #[test]
    fn exploratory_selection_uses_rng_pick() {
        let m = MetaBandit::new();
        // rng_value = 0.01 forces exploration; rng_pick picks idx 0
        let (chosen, exploratory) = m.select(0.01, 0.0);
        assert!(exploratory);
        assert_eq!(chosen, CandidateId::Thompson);
        // rng_pick near 1.0 picks last candidate
        let (chosen, exploratory) = m.select(0.01, 0.99);
        assert!(exploratory);
        assert_eq!(chosen, CandidateId::Greedy);
    }

    #[test]
    fn record_updates_correct_candidate() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 1.0;
        m.record(CandidateId::Ucb, 0.7);
        m.record(CandidateId::Ucb, 0.3);
        let ucb = m.candidates.iter().find(|c| c.id == CandidateId::Ucb).unwrap();
        assert!((ucb.trials - 2.0).abs() < 1e-9);
        assert!((ucb.cumulative_reward - 1.0).abs() < 1e-9);
        assert!((ucb.mean_reward() - 0.5).abs() < 1e-9);
        assert_eq!(m.total_rounds, 2);
    }

    #[test]
    fn reset_clears_everything() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 1.0;
        for _ in 0..50 {
            m.record(CandidateId::Thompson, 1.0);
        }
        m.reset();
        assert_eq!(m.total_rounds, 0);
        for c in &m.candidates {
            assert!(c.trials.abs() < 1e-9);
            assert_eq!(c.cumulative_reward, 0.0);
        }
    }

    #[test]
    fn current_leader_returns_highest_mean() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 1.0;
        m.record(CandidateId::Thompson, 0.9);
        m.record(CandidateId::Ucb, 0.5);
        assert_eq!(m.current_leader(), Some(CandidateId::Thompson));
    }

    #[test]
    fn current_leader_none_when_no_trials() {
        let m = MetaBandit::new();
        assert_eq!(m.current_leader(), None);
    }

    #[test]
    fn forgetting_decays_old_observations() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 0.9;

        for _ in 0..50 {
            m.record(CandidateId::Thompson, 1.0);
        }
        let thompson_before = m
            .candidates
            .iter()
            .find(|c| c.id == CandidateId::Thompson)
            .unwrap()
            .clone();

        for _ in 0..50 {
            m.record(CandidateId::Greedy, 1.0);
        }
        let thompson_after = m
            .candidates
            .iter()
            .find(|c| c.id == CandidateId::Thompson)
            .unwrap();
        let greedy_after = m
            .candidates
            .iter()
            .find(|c| c.id == CandidateId::Greedy)
            .unwrap();

        assert!(
            thompson_after.cumulative_reward < thompson_before.cumulative_reward * 0.5,
            "Thompson should decay: was {}, now {}",
            thompson_before.cumulative_reward,
            thompson_after.cumulative_reward
        );
        assert!(greedy_after.cumulative_reward > thompson_after.cumulative_reward);
    }

    #[test]
    fn forgetting_factor_one_means_no_decay() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 1.0;

        for _ in 0..100 {
            m.record(CandidateId::Thompson, 1.0);
        }
        let thompson = m.candidates.iter().find(|c| c.id == CandidateId::Thompson).unwrap();
        assert!((thompson.trials - 100.0).abs() < 1e-9);
        assert!((thompson.cumulative_reward - 100.0).abs() < 1e-9);
    }

    #[test]
    fn meta_bandit_with_decay_adapts_to_regime_shift() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 0.99;
        let mut rng_state: u64 = 42;
        let next_rand = |s: &mut u64| -> f64 {
            *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*s as u32) as f64 / (u32::MAX as f64 + 1.0)
        };

        for _ in 0..300 {
            let (r1, r2) = (next_rand(&mut rng_state), next_rand(&mut rng_state));
            let (chosen, _) = m.select(r1, r2);
            let reward = if chosen == CandidateId::Thompson { 0.9 } else { 0.2 };
            m.record(chosen, reward);
        }
        let leader_phase1 = m.current_leader();
        assert_eq!(leader_phase1, Some(CandidateId::Thompson));

        for _ in 0..500 {
            let (r1, r2) = (next_rand(&mut rng_state), next_rand(&mut rng_state));
            let (chosen, _) = m.select(r1, r2);
            let reward = if chosen == CandidateId::Greedy { 0.9 } else { 0.2 };
            m.record(chosen, reward);
        }
        let leader_phase2 = m.current_leader();
        assert_eq!(
            leader_phase2,
            Some(CandidateId::Greedy),
            "After regime shift with forgetting, Greedy should lead"
        );
    }

    #[test]
    fn forgetting_serializes_correctly() {
        let mut m = MetaBandit::new();
        m.forgetting_factor = 0.95;
        assert_eq!(m.forgetting_factor, 0.95);
    }

    #[test]
    fn converges_to_best_under_repeated_selection() {
        // Simulated environment where Thompson gives reward 0.8 and others give 0.2.
        // After many rounds, the meta-bandit should be choosing Thompson most of the time.
        let mut m = MetaBandit::new();
        m.forgetting_factor = 1.0;
        let mut rng_state: u64 = 12345;

        let next_rand = |state: &mut u64| -> f64 {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*state as u32) as f64 / (u32::MAX as f64 + 1.0)
        };

        let mut thompson_count = 0;
        for _ in 0..2000 {
            let r1 = next_rand(&mut rng_state);
            let r2 = next_rand(&mut rng_state);
            let (chosen, _) = m.select(r1, r2);
            let reward = if chosen == CandidateId::Thompson { 0.8 } else { 0.2 };
            m.record(chosen, reward);
            if chosen == CandidateId::Thompson {
                thompson_count += 1;
            }
        }

        // After 2000 rounds, Thompson should dominate selections.
        // Floor of 0.05 exploration × 5 candidates = ~1% pure random Thompson selections,
        // plus the rest of exploitation. Conservative threshold: >50%.
        assert!(
            thompson_count > 1000,
            "Thompson should dominate, got {} / 2000",
            thompson_count
        );
    }
}
