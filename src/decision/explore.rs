//! Exploration: turning predicted rewards into a probability mass function
//! (PMF) over the K eligible actions, and sampling from it.
//!
//! Predictions are normalized rewards (higher is better) and must be
//! finite. Every PMF produced here is renormalized so it sums to 1 within
//! floating-point rounding. After [`apply_floor`] every action has
//! probability at least `floor / K`, which is what keeps the decision logs
//! usable for off-policy evaluation.

use super::rng::SplitMix64;
use super::spec::{ExplorationKind, ExplorationSpec};

/// Index of the largest prediction; ties go to the lowest index.
pub fn argmax(predictions: &[f64]) -> usize {
    let mut best = 0;
    for (i, &p) in predictions.iter().enumerate().skip(1) {
        if p > predictions[best] {
            best = i;
        }
    }
    best
}

/// SquareCB exploration strength `gamma = gamma_scale * n^gamma_exponent`,
/// where `n` is the number of learner updates applied so far (so `n = 0`
/// gives `gamma = 0`, a uniform PMF, unless the exponent is 0).
pub fn gamma(gamma_scale: f64, gamma_exponent: f64, n: u64) -> f64 {
    gamma_scale * (n as f64).powf(gamma_exponent)
}

/// Inverse gap weighting (Foster and Rakhlin 2020, as in SquareCB). With
/// `b = argmax` (ties to the lowest index), every other action gets
/// `1 / (K + gamma * (r_b - r_a))` and `b` gets the rest, which is at least
/// `1 / K`. `gamma` must be finite and non-negative.
pub fn squarecb(predictions: &[f64], gamma: f64) -> Vec<f64> {
    debug_assert!(gamma.is_finite() && gamma >= 0.0, "gamma {gamma}");
    debug_assert!(predictions.iter().all(|p| p.is_finite()));
    if predictions.is_empty() {
        return Vec::new();
    }
    let k = predictions.len() as f64;
    let best = argmax(predictions);
    let mut pmf = vec![0.0; predictions.len()];
    let mut others = 0.0;
    for (a, &r) in predictions.iter().enumerate() {
        if a != best {
            let p = 1.0 / (k + gamma * (predictions[best] - r));
            pmf[a] = p;
            others += p;
        }
    }
    pmf[best] = 1.0 - others;
    finish(&mut pmf, 0.0);
    pmf
}

/// Epsilon-greedy: `1 - epsilon` on the argmax (ties to the lowest index)
/// plus `epsilon / K` on every action.
pub fn epsilon_greedy(predictions: &[f64], epsilon: f64) -> Vec<f64> {
    debug_assert!((0.0..=1.0).contains(&epsilon), "epsilon {epsilon}");
    if predictions.is_empty() {
        return Vec::new();
    }
    let best = argmax(predictions);
    let mut pmf = vec![epsilon / predictions.len() as f64; predictions.len()];
    let others: f64 = pmf
        .iter()
        .enumerate()
        .filter(|&(a, _)| a != best)
        .map(|(_, p)| p)
        .sum();
    pmf[best] = 1.0 - others;
    finish(&mut pmf, 0.0);
    pmf
}

/// Mix in the exploration floor: `p = (1 - floor) * p + floor / K`, then
/// renormalize. Afterwards every entry is at least `floor / K` exactly.
pub fn apply_floor(pmf: &mut [f64], floor: f64) {
    debug_assert!((0.0..=1.0).contains(&floor), "floor {floor}");
    if pmf.is_empty() {
        return;
    }
    let min = floor / pmf.len() as f64;
    for p in pmf.iter_mut() {
        *p = (1.0 - floor) * *p + min;
    }
    finish(pmf, min);
}

/// Baseline exploration: `1 - epsilon` on the caller's incumbent action
/// plus `epsilon / K` on every action. Panics if `baseline >= k`.
pub fn baseline_explore(k: usize, baseline: usize, epsilon: f64) -> Vec<f64> {
    assert!(
        baseline < k,
        "baseline index {baseline} out of range for {k} actions"
    );
    debug_assert!((0.0..=1.0).contains(&epsilon), "epsilon {epsilon}");
    let min = epsilon / k as f64;
    let mut pmf = vec![min; k];
    pmf[baseline] = 1.0 - min * (k - 1) as f64;
    finish(&mut pmf, min);
    pmf
}

/// The learner-mode PMF for `spec`: the configured exploration followed by
/// the floor. `n_updates` drives SquareCB's gamma schedule.
pub fn learner_pmf(spec: &ExplorationSpec, predictions: &[f64], n_updates: u64) -> Vec<f64> {
    let mut pmf = match spec.kind {
        ExplorationKind::SquareCb => squarecb(
            predictions,
            gamma(spec.gamma_scale, spec.gamma_exponent, n_updates),
        ),
        ExplorationKind::EpsilonGreedy => epsilon_greedy(predictions, spec.epsilon),
    };
    apply_floor(&mut pmf, spec.floor);
    pmf
}

/// Draw an index from `pmf` by inverting its CDF with one uniform draw.
/// Zero-probability entries are never returned. If rounding leaves the
/// cumulative sum just below the draw, the last positive entry is returned.
/// Panics on an empty PMF.
pub fn sample(pmf: &[f64], rng: &mut SplitMix64) -> usize {
    assert!(!pmf.is_empty(), "cannot sample from an empty PMF");
    let u = rng.next_f64();
    let mut cumulative = 0.0;
    for (i, &p) in pmf.iter().enumerate() {
        cumulative += p;
        if u < cumulative && p > 0.0 {
            return i;
        }
    }
    pmf.iter().rposition(|&p| p > 0.0).unwrap_or(pmf.len() - 1)
}

/// Renormalize to sum 1 (when the sum is positive and finite), then raise
/// any entry that rounding left below `min` back to `min`. The result sums
/// to 1 within a few ulps and honours the minimum exactly.
fn finish(pmf: &mut [f64], min: f64) {
    let sum: f64 = pmf.iter().sum();
    if sum > 0.0 && sum.is_finite() && sum != 1.0 {
        for p in pmf.iter_mut() {
            *p /= sum;
        }
    }
    for p in pmf.iter_mut() {
        if *p < min {
            *p = min;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    fn assert_valid(pmf: &[f64], floor: f64) {
        let k = pmf.len() as f64;
        let sum: f64 = pmf.iter().sum();
        assert!((sum - 1.0).abs() <= 1e-12, "sum {sum} for {pmf:?}");
        for &p in pmf {
            assert!(p.is_finite(), "{pmf:?}");
            assert!(p >= floor / k, "{p} below floor {} in {pmf:?}", floor / k);
            assert!(p <= 1.0);
        }
    }

    #[test]
    fn argmax_ties_go_to_the_lowest_index() {
        assert_eq!(argmax(&[0.1, 0.5, 0.5, 0.2]), 1);
        assert_eq!(argmax(&[0.3, 0.3]), 0);
        assert_eq!(argmax(&[-1.0]), 0);
    }

    #[test]
    fn squarecb_known_values() {
        // K = 2, gap 0.2, gamma 10: p_1 = 1 / (2 + 2) = 0.25.
        let pmf = squarecb(&[0.5, 0.3], 10.0);
        assert!(close(pmf[0], 0.75) && close(pmf[1], 0.25), "{pmf:?}");
        // K = 3 with a tie for the best: the lower index is b, the tied
        // action gets 1 / K, the worse one 1 / (3 + 100 * 0.5).
        let pmf = squarecb(&[0.2, 0.7, 0.7], 100.0);
        let low = 1.0 / 53.0;
        assert!(close(pmf[0], low) && close(pmf[2], 1.0 / 3.0), "{pmf:?}");
        assert!(close(pmf[1], 1.0 - low - 1.0 / 3.0), "{pmf:?}");
        // With the floor: (1 - 0.05) * 0.25 + 0.05 / 2.
        let mut pmf = squarecb(&[0.5, 0.3], 10.0);
        apply_floor(&mut pmf, 0.05);
        assert!(close(pmf[1], 0.2625) && close(pmf[0], 0.7375), "{pmf:?}");
    }

    #[test]
    fn gamma_schedule() {
        assert_eq!(gamma(10.0, 0.5, 0), 0.0);
        assert_eq!(gamma(10.0, 0.5, 100), 100.0);
        assert_eq!(gamma(10.0, 0.0, 0), 10.0);
        assert_eq!(gamma(2.0, 1.0, 7), 14.0);
    }

    #[test]
    fn untrained_squarecb_is_uniform() {
        let pmf = squarecb(&[0.9, 0.1, 0.4, 0.0], gamma(10.0, 0.5, 0));
        assert!(pmf.iter().all(|&p| close(p, 0.25)), "{pmf:?}");
    }

    #[test]
    fn single_action_gets_everything() {
        for mut pmf in [
            squarecb(&[0.3], 50.0),
            epsilon_greedy(&[0.3], 0.2),
            baseline_explore(1, 0, 0.1),
        ] {
            apply_floor(&mut pmf, 0.05);
            assert_eq!(pmf, vec![1.0]);
        }
        let spec = ExplorationSpec::default();
        assert_eq!(learner_pmf(&spec, &[0.7], 1000), vec![1.0]);
    }

    #[test]
    fn epsilon_greedy_values() {
        let pmf = epsilon_greedy(&[0.1, 0.9, 0.5, 0.9], 0.2);
        assert!(close(pmf[1], 0.8 + 0.05), "{pmf:?}");
        for a in [0, 2, 3] {
            assert!(close(pmf[a], 0.05), "{pmf:?}");
        }
        assert_eq!(epsilon_greedy(&[0.1, 0.9], 0.0), vec![0.0, 1.0]);
    }

    #[test]
    fn baseline_explore_values() {
        let pmf = baseline_explore(4, 2, 0.1);
        assert!(close(pmf[2], 0.9 + 0.025), "{pmf:?}");
        for a in [0, 1, 3] {
            assert!(close(pmf[a], 0.025), "{pmf:?}");
        }
        assert_valid(&pmf, 0.1);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn baseline_explore_rejects_bad_index() {
        baseline_explore(3, 3, 0.1);
    }

    #[test]
    fn empty_inputs_give_empty_pmfs() {
        assert!(squarecb(&[], 1.0).is_empty());
        assert!(epsilon_greedy(&[], 0.1).is_empty());
        let mut empty: Vec<f64> = Vec::new();
        apply_floor(&mut empty, 0.05);
        assert!(empty.is_empty());
    }

    /// Property test over many random prediction vectors, including ties,
    /// extreme gaps and the whole range of gamma and floor.
    #[test]
    fn invariants_hold_for_random_inputs() {
        let mut rng = SplitMix64::new(2024);
        for case in 0..6_000 {
            let k = 1 + (rng.next_u64() % 40) as usize;
            let predictions: Vec<f64> = (0..k)
                .map(|_| match rng.next_u64() % 4 {
                    0 => 0.5,                         // ties
                    1 => (rng.next_u64() % 3) as f64, // coarse values
                    2 => rng.next_f64() * 1e-9,       // tiny gaps
                    _ => rng.next_f64() * 2.0 - 0.5,  // outside [0, 1] too
                })
                .collect();
            let gamma = match case % 4 {
                0 => 0.0,
                1 => rng.next_f64() * 10.0,
                2 => rng.next_f64() * 1e4,
                _ => 1e12,
            };
            let floor = match case % 3 {
                0 => 0.05,
                1 => rng.next_f64(),
                _ => 1.0,
            };
            let epsilon = rng.next_f64();
            let best = argmax(&predictions);
            for mut pmf in [
                squarecb(&predictions, gamma),
                epsilon_greedy(&predictions, epsilon),
            ] {
                let before = pmf.clone();
                assert_valid(&pmf, 0.0);
                // The argmax is a most probable action.
                assert!(
                    pmf.iter().all(|&p| p <= pmf[best] + 1e-15),
                    "{predictions:?} {pmf:?}"
                );
                apply_floor(&mut pmf, floor);
                assert_valid(&pmf, floor);
                // Mixing keeps the ordering of probabilities.
                for a in 0..k {
                    for b in 0..k {
                        if before[a] > before[b] + 1e-9 {
                            assert!(pmf[a] >= pmf[b], "order changed: {before:?} -> {pmf:?}");
                        }
                    }
                }
            }
            // SquareCB gives more mass to better actions.
            let pmf = squarecb(&predictions, gamma);
            for a in 0..k {
                for b in 0..k {
                    if a != best && b != best && predictions[a] > predictions[b] {
                        assert!(pmf[a] >= pmf[b], "{predictions:?} {pmf:?}");
                    }
                }
            }
            let baseline = (rng.next_u64() % k as u64) as usize;
            let pmf = baseline_explore(k, baseline, epsilon);
            assert_valid(&pmf, epsilon);
        }
    }

    #[test]
    fn sampling_matches_the_pmf() {
        let pmf = [0.5, 0.0, 0.2, 0.25, 0.05];
        let mut rng = SplitMix64::new(77);
        let n = 200_000;
        let mut counts = [0usize; 5];
        for _ in 0..n {
            counts[sample(&pmf, &mut rng)] += 1;
        }
        assert_eq!(counts[1], 0, "zero-probability action drawn");
        // Pearson chi-square with 3 degrees of freedom (the zero cell
        // excluded); the 0.999 quantile is 16.27.
        let chi2: f64 = pmf
            .iter()
            .zip(counts)
            .filter(|(p, _)| **p > 0.0)
            .map(|(p, c)| {
                let expected = p * n as f64;
                (c as f64 - expected).powi(2) / expected
            })
            .sum();
        assert!(chi2 < 16.27, "chi2 {chi2}, counts {counts:?}");
    }

    #[test]
    fn sampling_falls_back_to_the_last_positive_entry() {
        // A PMF that sums to 0.2: draws in [0.2, 1) run past the end and
        // must land on the last positive entry, never on the zero one.
        let pmf = [0.1, 0.1, 0.0];
        let mut rng = SplitMix64::new(5);
        let mut counts = [0usize; 3];
        for _ in 0..10_000 {
            counts[sample(&pmf, &mut rng)] += 1;
        }
        assert_eq!(counts[2], 0);
        assert!(counts[1] > 8_000, "{counts:?}");
        assert!(counts[0] > 700, "{counts:?}");
        // All-zero input still returns an index in range.
        assert_eq!(sample(&[0.0, 0.0], &mut rng), 1);
    }

    #[test]
    #[should_panic(expected = "empty PMF")]
    fn sampling_rejects_empty_pmf() {
        sample(&[], &mut SplitMix64::new(1));
    }

    #[test]
    fn learner_pmf_uses_the_configured_kind() {
        let mut spec = ExplorationSpec::default();
        let preds = [0.2, 0.8, 0.5];
        let mut expected = squarecb(&preds, gamma(10.0, 0.5, 400));
        apply_floor(&mut expected, 0.05);
        assert_eq!(learner_pmf(&spec, &preds, 400), expected);
        spec.kind = ExplorationKind::EpsilonGreedy;
        spec.epsilon = 0.3;
        spec.floor = 0.1;
        let mut expected = epsilon_greedy(&preds, 0.3);
        apply_floor(&mut expected, 0.1);
        assert_eq!(learner_pmf(&spec, &preds, 400), expected);
    }
}
