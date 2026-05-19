//! Shared-state LinUCB option strategy — wraps `LinUcbSharedState` with a
//! named option set, per-option action-feature vectors, and JSON
//! (de)serialisation. Math layer lives in `linucb.rs`.

use crate::linucb::{LinUcbSharedState, validate_shared_features};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Score variant requested at decide time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKind {
    /// LinUCB: deterministic `μ + α·sqrt(xᵀA⁻¹x)`.
    Ucb,
    /// Linear Thompson Sampling: stochastic draw from `N(μ, v²·A⁻¹)`.
    /// The caller supplies the iid standard-normal RNG; `α` is reused as
    /// the LinTS scale `v` so the call signature is uniform.
    LinTs,
}

/// Outcome of [`SharedStateOptionStrategy::select`]: which option won,
/// its score, and the full list of `(option_name, score)` pairs in the
/// order they were scored.
#[derive(Debug, Clone)]
pub struct SelectedOption {
    pub name: String,
    pub score: f64,
    pub all_scores: Vec<(String, f64)>,
}

/// Shared-state LinUCB wrapper. Late-registered options inherit a
/// non-zero posterior mean from the shared θ.
#[derive(Debug, Clone)]
pub struct SharedStateOptionStrategy {
    /// Shared LinUCB state over `[x_context, x_option]`.
    pub shared: LinUcbSharedState,
    /// Option name → fixed action-feature vector of length `d_option`.
    pub option_features: HashMap<String, Vec<f64>>,
    pub d_context: usize,
    pub d_option: usize,
    pub lambda: f64,
}

impl SharedStateOptionStrategy {
    /// Initialise an empty strategy. No options are registered yet; call
    /// [`Self::register_option`] for each one before [`Self::select`].
    pub fn new(d_context: usize, d_option: usize, lambda: f64) -> Self {
        Self {
            shared: LinUcbSharedState::new(d_context + d_option, lambda),
            option_features: HashMap::new(),
            d_context,
            d_option,
            lambda,
        }
    }

    /// Register an option. Rejects wrong-length / non-finite vectors and
    /// empty names; overwrites if the name already exists.
    pub fn register_option(
        &mut self,
        name: &str,
        features: Vec<f64>,
    ) -> Result<(), String> {
        if features.len() != self.d_option {
            return Err(format!(
                "option '{}' has feature length {}, expected {}",
                name,
                features.len(),
                self.d_option
            ));
        }
        for (i, v) in features.iter().enumerate() {
            if !v.is_finite() {
                return Err(format!(
                    "option '{}' feature[{}] is non-finite: {}",
                    name, i, v
                ));
            }
        }
        if name.is_empty() {
            return Err("option name must not be empty".into());
        }
        self.option_features.insert(name.to_string(), features);
        Ok(())
    }

    /// True if an option with this name is registered.
    pub fn contains_option(&self, name: &str) -> bool {
        self.option_features.contains_key(name)
    }

    /// Number of registered options.
    pub fn n_options(&self) -> usize {
        self.option_features.len()
    }

    /// Score every registered option at the given context and return the
    /// highest-scoring option, its score, and every option's score in
    /// the iteration order of the underlying `HashMap`. The order is
    /// not guaranteed to be stable across calls; consumers that need a
    /// stable ordering should sort `all_scores` by name themselves.
    /// `α` is the LinUCB coefficient (for `Ucb`) or LinTS scale `v` (for
    /// `LinTs`). Errors when no options are registered or context invalid.
    pub fn select<F>(
        &self,
        x_context: &[f64],
        alpha: f64,
        score_kind: ScoreKind,
        mut rng_normal: F,
    ) -> Result<SelectedOption, String>
    where
        F: FnMut() -> f64,
    {
        if self.option_features.is_empty() {
            return Err("no options registered on this strategy".into());
        }
        if x_context.len() != self.d_context {
            return Err(format!(
                "context length {} does not match d_context {}",
                x_context.len(),
                self.d_context
            ));
        }
        for (i, v) in x_context.iter().enumerate() {
            if !v.is_finite() {
                return Err(format!(
                    "context feature[{}] is non-finite: {}",
                    i, v
                ));
            }
        }

        let mut all_scores: Vec<(String, f64)> =
            Vec::with_capacity(self.option_features.len());
        let mut best_idx: usize = 0;
        let mut best_score: f64 = f64::NEG_INFINITY;

        for (name, opt_vec) in &self.option_features {
            // Re-check at the math boundary so release builds stay safe.
            if validate_shared_features(
                x_context, opt_vec, self.d_context, self.d_option,
            ).is_err() {
                continue;
            }
            let score = match score_kind {
                ScoreKind::Ucb => {
                    let (s, _clamped) =
                        self.shared.shared_ucb_score(x_context, opt_vec, alpha);
                    s
                }
                ScoreKind::LinTs => {
                    self.shared.shared_lin_ts_score(
                        x_context, opt_vec, alpha, &mut rng_normal,
                    )
                }
            };
            let idx = all_scores.len();
            all_scores.push((name.clone(), score));
            if score > best_score {
                best_score = score;
                best_idx = idx;
            }
        }

        if all_scores.is_empty() {
            return Err("no scoreable options".into());
        }

        let (name, score) = all_scores[best_idx].clone();
        Ok(SelectedOption { name, score, all_scores })
    }

    /// Apply one reward against `(context, option)`. Sherman-Morrison
    /// update on `A_inv`; full rebuild every 1000 updates to clear drift.
    pub fn apply_feedback(
        &mut self,
        chosen_option_name: &str,
        x_context: &[f64],
        reward: f64,
    ) -> Result<(), String> {
        if !reward.is_finite() {
            return Err(format!("reward is non-finite: {}", reward));
        }
        let opt_vec = match self.option_features.get(chosen_option_name) {
            Some(v) => v.clone(),
            None => {
                return Err(format!(
                    "option '{}' is not registered",
                    chosen_option_name
                ));
            }
        };
        validate_shared_features(
            x_context, &opt_vec, self.d_context, self.d_option,
        )?;
        self.shared.shared_update(x_context, &opt_vec, reward);
        if self.shared.shared_rebuild_due(1000) {
            self.shared.shared_rebuild_inverse();
        }
        Ok(())
    }

    /// Posterior-mean reward `x · θ̂` for an option, or `None` if unregistered.
    pub fn posterior_mean(
        &self,
        option_name: &str,
        x_context: &[f64],
    ) -> Option<f64> {
        let opt_vec = self.option_features.get(option_name)?;
        if x_context.len() != self.d_context || opt_vec.len() != self.d_option {
            return None;
        }
        // α = 0.0 strips the exploration bonus; returned score is the
        // pure posterior mean.
        let (score, _) =
            self.shared.shared_ucb_score(x_context, opt_vec, 0.0);
        Some(score)
    }

    /// Verify per-option feature lengths and `shared.d_total` consistency.
    pub fn validate(&self) -> Result<(), String> {
        if self.shared.d_total != self.d_context + self.d_option {
            return Err(format!(
                "shared.d_total ({}) != d_context ({}) + d_option ({})",
                self.shared.d_total, self.d_context, self.d_option
            ));
        }
        for (name, vec) in &self.option_features {
            if vec.len() != self.d_option {
                return Err(format!(
                    "option '{}' has feature length {}, expected {}",
                    name, vec.len(), self.d_option
                ));
            }
            for (i, v) in vec.iter().enumerate() {
                if !v.is_finite() {
                    return Err(format!(
                        "option '{}' feature[{}] is non-finite: {}",
                        name, i, v
                    ));
                }
            }
        }
        Ok(())
    }

    /// Serialise to JSON; round-trips via [`Self::from_json`].
    pub fn to_json(&self) -> Value {
        let mut opt_obj = serde_json::Map::new();
        for (name, vec) in &self.option_features {
            opt_obj.insert(name.clone(), Value::Array(
                vec.iter().map(|v| {
                    serde_json::Number::from_f64(*v)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                }).collect()
            ));
        }
        json!({
            "dContext": self.d_context,
            "dOption":  self.d_option,
            "lambda":   self.lambda,
            "optionFeatures": Value::Object(opt_obj),
            "shared": serde_json::to_value(&self.shared)
                .unwrap_or(Value::Null),
        })
    }

    /// Inverse of [`Self::to_json`]. Returns `Err` with a human-readable
    /// reason on any structural problem.
    pub fn from_json(j: &Value) -> Result<Self, String> {
        let d_context = j.get("dContext")
            .and_then(|v| v.as_u64())
            .ok_or("missing/invalid dContext")? as usize;
        let d_option = j.get("dOption")
            .and_then(|v| v.as_u64())
            .ok_or("missing/invalid dOption")? as usize;
        let lambda = j.get("lambda")
            .and_then(|v| v.as_f64())
            .ok_or("missing/invalid lambda")?;
        let shared_v = j.get("shared")
            .ok_or("missing shared state")?;
        let shared: LinUcbSharedState = serde_json::from_value(shared_v.clone())
            .map_err(|e| format!("invalid shared state: {e}"))?;
        let opt_obj = j.get("optionFeatures")
            .and_then(|v| v.as_object())
            .ok_or("missing/invalid optionFeatures object")?;
        let mut option_features: HashMap<String, Vec<f64>> = HashMap::new();
        for (k, v) in opt_obj {
            let arr = v.as_array()
                .ok_or_else(|| format!("option '{}' features must be array", k))?;
            let mut vec = Vec::with_capacity(arr.len());
            for (i, x) in arr.iter().enumerate() {
                let f = x.as_f64().ok_or_else(|| format!(
                    "option '{}' feature[{}] not a number", k, i,
                ))?;
                vec.push(f);
            }
            option_features.insert(k.clone(), vec);
        }
        let me = Self { shared, option_features, d_context, d_option, lambda };
        me.validate()?;
        Ok(me)
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tests.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic uniform RNG for the simulation. Not cryptographically
    /// random; just a reproducible linear-congruential generator so the
    /// test is bit-exact across runs.
    struct DetRng(u64);
    impl DetRng {
        fn next_u01(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((self.0 >> 32) as f64 / u32::MAX as f64).max(1e-12);
            v.min(1.0 - 1e-12)
        }
        fn next_normal(&mut self) -> f64 {
            let u1 = self.next_u01();
            let u2 = self.next_u01();
            (-2.0 * u1.ln()).sqrt()
                * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }

    #[test]
    fn new_initialises_consistent_dimensions() {
        let s = SharedStateOptionStrategy::new(3, 2, 1.0);
        assert_eq!(s.d_context, 3);
        assert_eq!(s.d_option, 2);
        assert_eq!(s.shared.d_total, 5);
        assert_eq!(s.n_options(), 0);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn register_option_rejects_wrong_dimension() {
        let mut s = SharedStateOptionStrategy::new(1, 2, 1.0);
        // Right length.
        assert!(s.register_option("a", vec![0.1, 0.9]).is_ok());
        // Wrong length.
        assert!(s.register_option("b", vec![0.5]).is_err());
        // Non-finite.
        assert!(s.register_option("c", vec![f64::NAN, 0.0]).is_err());
        // Empty name.
        assert!(s.register_option("", vec![0.0, 0.0]).is_err());
        assert_eq!(s.n_options(), 1);
        assert!(s.contains_option("a"));
    }

    #[test]
    fn select_errors_on_no_options() {
        let s = SharedStateOptionStrategy::new(1, 2, 1.0);
        let mut rng = DetRng(1);
        let r = s.select(&[0.5], 1.0, ScoreKind::Ucb, || rng.next_normal());
        assert!(r.is_err());
    }

    #[test]
    fn select_returns_one_of_registered_options() {
        let mut s = SharedStateOptionStrategy::new(1, 2, 1.0);
        s.register_option("a", vec![0.1, 0.9]).unwrap();
        s.register_option("b", vec![0.9, 0.1]).unwrap();
        let mut rng = DetRng(7);
        let pick = s.select(&[0.5], 1.0, ScoreKind::Ucb, || rng.next_normal()).unwrap();
        assert!(pick.name == "a" || pick.name == "b");
        assert_eq!(pick.all_scores.len(), 2);
    }

    #[test]
    fn apply_feedback_changes_posterior_mean_for_chosen_option() {
        let mut s = SharedStateOptionStrategy::new(1, 2, 1.0);
        s.register_option("a", vec![1.0, 0.0]).unwrap();
        s.register_option("b", vec![0.0, 1.0]).unwrap();
        // Drive option "a" with positive reward at a single context.
        for _ in 0..30 {
            s.apply_feedback("a", &[1.0], 1.0).unwrap();
        }
        let ma = s.posterior_mean("a", &[1.0]).unwrap();
        let mb = s.posterior_mean("b", &[1.0]).unwrap();
        // After 30 positive rewards on "a", its posterior should clearly
        // beat "b"'s (which has zero observations at all).
        assert!(ma > mb, "ma={} mb={}", ma, mb);
        assert!(ma > 0.3, "ma={} too low", ma);
    }

    #[test]
    fn json_roundtrip_preserves_state() {
        let mut s = SharedStateOptionStrategy::new(1, 2, 1.0);
        s.register_option("a", vec![0.1, 0.9]).unwrap();
        s.register_option("b", vec![0.9, 0.1]).unwrap();
        for _ in 0..10 {
            s.apply_feedback("a", &[0.5], 0.7).unwrap();
            s.apply_feedback("b", &[0.5], 0.2).unwrap();
        }
        let v = s.to_json();
        let s2 = SharedStateOptionStrategy::from_json(&v).unwrap();
        assert_eq!(s2.d_context, s.d_context);
        assert_eq!(s2.d_option, s.d_option);
        assert!((s2.lambda - s.lambda).abs() < 1e-12);
        assert_eq!(s2.n_options(), s.n_options());
        // Posterior means should match bit-for-bit after a clean
        // roundtrip — no incremental updates, no Cholesky.
        for ctx in [-1.0, 0.0, 0.5, 1.0] {
            for name in ["a", "b"] {
                let m1 = s.posterior_mean(name, &[ctx]).unwrap();
                let m2 = s2.posterior_mean(name, &[ctx]).unwrap();
                assert!((m1 - m2).abs() < 1e-9, "name={} ctx={} m1={} m2={}",
                    name, ctx, m1, m2);
            }
        }
    }

    #[test]
    fn validate_catches_dimension_mismatch_in_serialised_blob() {
        // Construct a JSON blob whose dContext+dOption disagrees with
        // shared.dTotal. from_json must reject.
        let bad = json!({
            "dContext": 2,
            "dOption":  2,
            "lambda":   1.0,
            "optionFeatures": { "a": [0.0, 0.0] },
            // dTotal of 3 here, but dContext+dOption = 4 above.
            "shared": {
                "a": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                "a_inv": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                "b": [0.0, 0.0, 0.0],
                "d_total": 3,
                "lambda": 1.0,
                "since_last_rebuild": 0
            }
        });
        let r = SharedStateOptionStrategy::from_json(&bad);
        assert!(r.is_err(), "expected dimension-mismatch error");
    }

    /// The marquee test the user asked for: train on A/B/C/D, then
    /// introduce E and F. The shared LinUCB has learned how the option
    /// features (which span context-driven and context-free reward
    /// channels) map to rewards, so when E and F appear, their predicted
    /// reward at any context is already in the right ballpark — within
    /// roughly 30% of the true expected reward — even though no
    /// observation has ever been taken at E or F's option features.
    #[test]
    fn shared_state_strategy_generalises_to_unseen_options() {
        // d_context = 1: a scalar in [0, 1] that we'll vary at random.
        // d_option  = 2: each option carries a 2-vector action embedding.
        //
        // True reward function is LINEAR in [x_context, x_option]:
        //     r(ctx, x_opt) = w_c·ctx + w_0·x_opt[0] + w_1·x_opt[1] + ε
        // with w_c=0.10, w_0=0.40, w_1=0.60, ε ~ U(-0.025, 0.025).
        //
        // This is exactly the function-class shared-state LinUCB is
        // built to fit — the shared θ in R^3 is identifiable from
        // (context, option) pairs where the option features span
        // a 2-D subspace, and the corners A/B/C/D do span it.
        //
        // A non-linear truth (e.g. bilinear `ctx·x_opt[0]`) would
        // require feature engineering at the capsule layer (e.g.
        // emit `[ctx, x_opt0, x_opt1, ctx·x_opt0, ctx·x_opt1]`).
        // That's a real consideration for production capsules and is
        // flagged in `Syntra/docs/capsule-features/shared-state-linucb.md`.
        // The test here demonstrates the *core mechanism* (action-
        // embedding generalisation under the function class
        // LinUCB can represent).
        let mut s = SharedStateOptionStrategy::new(1, 2, 1.0);
        s.register_option("A", vec![0.1, 0.1]).unwrap();
        s.register_option("B", vec![0.1, 0.9]).unwrap();
        s.register_option("C", vec![0.9, 0.1]).unwrap();
        s.register_option("D", vec![0.9, 0.9]).unwrap();

        let mut rng = DetRng(2026);
        let w_c = 0.10_f64;
        let w_0 = 0.40_f64;
        let w_1 = 0.60_f64;
        let true_reward = |ctx: f64, opt: &[f64]| -> f64 {
            w_c * ctx + w_0 * opt[0] + w_1 * opt[1]
        };

        // 300 decide / observe / apply_feedback rounds.
        let n_rounds = 300;
        for _ in 0..n_rounds {
            let ctx = rng.next_u01();
            let x = [ctx];
            let pick = s.select(
                &x, 1.0, ScoreKind::Ucb, || rng.next_normal(),
            ).unwrap();
            // Simulate the true reward at the chosen option, with a
            // tiny noise so the regression isn't perfectly degenerate.
            let opt_vec = s.option_features.get(&pick.name).unwrap().clone();
            let mu = true_reward(ctx, &opt_vec);
            let noise = (rng.next_u01() - 0.5) * 0.05;
            let r = (mu + noise).clamp(-1.0, 1.0);
            s.apply_feedback(&pick.name, &x, r).unwrap();
        }

        // Now register E and F. They sit inside the convex hull of A–D.
        s.register_option("E", vec![0.5, 0.5]).unwrap();
        s.register_option("F", vec![0.3, 0.7]).unwrap();

        // Probe at three contexts. We DON'T train against E or F — the
        // posterior_mean estimates must come entirely from the shared θ.
        let probes = [0.0_f64, 0.5, 1.0];
        let unseen = ["E", "F"];
        // Print first, then assert. The print is structural: the test
        // output is consumed by the prep report.
        println!("# shared-state-strategy generalisation results (n_rounds={})", n_rounds);
        println!("# columns: option | context | posterior_mean | true_expected | |diff|");
        // Tolerance: 0.3 (30%). The truth is linear in [ctx, opt0,
        // opt1] and λ=1.0 induces a mild shrinkage on θ, so the
        // unbiased estimate is shifted slightly toward zero. Observed
        // worst-case relative error after 300 rounds is well under
        // 30%; 0.3 leaves headroom for RNG-seed drift. The strong
        // claim — "shared state gives a non-trivial prior for an
        // unseen option whose features lie in the convex hull of
        // training options" — is demonstrated below.
        let tol = 0.3;
        let mut max_rel: f64 = 0.0;
        for name in &unseen {
            let opt_vec = s.option_features.get(*name).unwrap().clone();
            for &ctx in &probes {
                let est = s.posterior_mean(name, &[ctx]).unwrap();
                let truth = true_reward(ctx, &opt_vec);
                let diff = (est - truth).abs();
                // Floor the denominator so probes where truth ≈ 0 don't
                // blow up the relative error.
                let denom = truth.abs().max(0.1);
                let rel = diff / denom;
                println!("{}\t{:.2}\t{:.4}\t{:.4}\t{:.4}",
                    name, ctx, est, truth, diff);
                if rel > max_rel { max_rel = rel; }
                assert!(
                    rel < tol,
                    "{} at ctx={}: est={} truth={} rel_err={} (tol={})",
                    name, ctx, est, truth, rel, tol
                );
            }
        }
        println!("# max relative error across unseen probes = {:.3}", max_rel);

        // Sanity: a freshly-built strategy with zero training would
        // return posterior_mean = 0 for E and F, so showing non-zero
        // means above already demonstrates "non-zero prior". The
        // tolerance assertion is the stronger claim.
        let fresh = SharedStateOptionStrategy::new(1, 2, 1.0);
        // (Can't call posterior_mean on fresh for E — not registered.)
        // But we can verify the principle: a fresh strategy's posterior
        // mean for any (context, option) is 0 because θ̂ = A_inv·b = 0.
        let mut fresh = fresh;
        fresh.register_option("E", vec![0.5, 0.5]).unwrap();
        let fresh_est = fresh.posterior_mean("E", &[0.5]).unwrap();
        assert!(fresh_est.abs() < 1e-9, "fresh estimate is {}", fresh_est);
    }
}
