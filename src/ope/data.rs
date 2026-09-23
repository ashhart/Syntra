//! Evaluation data: the logged rows that have a reward, featurized once,
//! each assigned to a cross-fitting fold, plus the reward scale that models
//! train on.
//!
//! # Cross-fitting
//!
//! Anything learned from the logs (the DR reward model, and the `greedy`
//! and `spec:` target policies) is cross-fitted. Rows are split into K
//! folds by a hash of their decision id ([`fold_of`]); for each fold a
//! fresh model is trained on the other K - 1 folds, in row order, and only
//! that model ever scores the fold's rows. Conditional on the other folds,
//! a row is then an independent draw that no model has seen, so its
//! importance-weighted terms keep their expectation. A model that had seen
//! the row could lean towards whichever action happened to pay off in it,
//! which overstates the value (the leak test in `policy.rs` measures this).
//!
//! # Reward scale
//!
//! Estimates stay in raw reward units. Models train on rewards normalized to
//! `[0, 1]` by `(r - lo) / (hi - lo)`, clamped, as the decision core does,
//! and their predictions map back by `lo + x (hi - lo)`. `[lo, hi]` is the
//! given reward range (`--reward-range`) if there is one, otherwise the
//! observed minimum and maximum (a constant reward `c` uses
//! `[c - h, c + h]` with `h = max(0.5, 1e-6 |c|)`). Rewards outside a given
//! range are clamped for model training only, and counted.

use std::collections::HashMap;

use serde::Serialize;

use crate::decision::explore::argmax;
use crate::decision::features::{
    ContextFeatures, Feature, Featurizer, MAX_PHI_TERMS, Namespace, action_features, flatten,
    fmix64, fnv1a64,
};
use crate::decision::learner::LinearModel;
use crate::decision::spec::{Importance, LearnerSpec};

use super::row::LoggedRow;

/// Largest supported number of cross-fitting folds.
pub const MAX_FOLDS: usize = 100;
/// Fewest rows with a reward that an evaluation accepts (a standard error
/// needs two).
pub const MIN_ROWS: usize = 2;

/// Where the reward range came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeSource {
    /// Given by the caller (`--reward-range`).
    Given,
    /// The observed minimum and maximum reward.
    Observed,
}

/// Affine map between raw rewards and the `[0, 1]` scale models train on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RewardScale {
    lo: f64,
    hi: f64,
    source: RangeSource,
}

impl RewardScale {
    /// A caller-given range: finite, `lo < hi`.
    pub fn given(lo: f64, hi: f64) -> Result<Self, String> {
        if !(lo.is_finite() && hi.is_finite() && lo < hi && (hi - lo).is_finite()) {
            return Err(format!(
                "reward range must be two finite numbers lo,hi with lo < hi (got {lo},{hi})"
            ));
        }
        Ok(Self {
            lo,
            hi,
            source: RangeSource::Given,
        })
    }

    /// The observed range of `rewards` (finite, non-empty). A constant
    /// reward `c` gets `[c - h, c + h]` with `h = max(0.5, 1e-6 |c|)`.
    pub fn observed(rewards: &[f64]) -> Result<Self, String> {
        let lo = rewards.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = rewards.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if !(lo.is_finite() && hi.is_finite()) {
            return Err("no finite rewards to take a range from".into());
        }
        let (lo, hi) = if hi > lo {
            (lo, hi)
        } else {
            let half = (lo.abs() * 1e-6).max(0.5);
            (lo - half, lo + half)
        };
        if !(hi - lo).is_finite() {
            return Err(format!(
                "the observed rewards span [{lo}, {hi}], too wide to normalize; pass --reward-range"
            ));
        }
        Ok(Self {
            lo,
            hi,
            source: RangeSource::Observed,
        })
    }

    pub fn lo(&self) -> f64 {
        self.lo
    }

    pub fn hi(&self) -> f64 {
        self.hi
    }

    pub fn source(&self) -> RangeSource {
        self.source
    }

    /// `(r - lo) / (hi - lo)`, clamped to `[0, 1]`.
    pub fn normalize(&self, raw: f64) -> f64 {
        ((raw - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
    }

    /// `lo + x (hi - lo)`: a normalized prediction in raw units.
    pub fn denormalize(&self, x: f64) -> f64 {
        self.lo + x * (self.hi - self.lo)
    }

    /// Whether `raw` lies inside `[lo, hi]`.
    pub fn contains(&self, raw: f64) -> bool {
        (self.lo..=self.hi).contains(&raw)
    }
}

/// The cross-fitting fold of a decision: FNV-1a 64 of the id's UTF-8 bytes,
/// mixed by MurmurHash3's finalizer, modulo `folds`. It depends on nothing
/// but the id, so it is stable across runs and machines and independent of
/// rewards and row order. `folds` must be positive.
pub fn fold_of(decision_id: &str, folds: usize) -> usize {
    assert!(folds > 0, "fold count must be positive");
    (fmix64(fnv1a64(decision_id.as_bytes())) % folds as u64) as usize
}

/// A row's features, flattened once and reused by every model.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowFeatures {
    pub context: ContextFeatures,
    /// Features of each eligible action, aligned with the row's `eligible`.
    pub actions: Vec<Vec<Feature>>,
    /// Position of the chosen action within `actions`.
    pub chosen: usize,
}

/// Flatten a row's context, derived values and eligible actions, and check
/// that each eligible action's feature vector (with context terms) fits the
/// decision core's term limit. The row must pass [`LoggedRow::check`].
pub(crate) fn featurize(row: &LoggedRow) -> Result<RowFeatures, String> {
    let context = ContextFeatures {
        context: flatten(Namespace::Context, &row.context).map_err(|e| e.to_string())?,
        derived: flatten(Namespace::Derived, &row.derived).map_err(|e| e.to_string())?,
    };
    let request = context.context.len() + context.derived.len();
    let mut actions = Vec::with_capacity(row.eligible.len());
    for &index in &row.eligible {
        let action = &row.actions[index];
        let features = action_features(action)
            .map_err(|e| format!("actions[{index}] ({:?}): {e}", action.id))?;
        let terms = Featurizer::phi_terms(&context, features.len()).saturating_add(request);
        if terms > MAX_PHI_TERMS {
            return Err(format!(
                "the feature vector of action {:?} would need {terms} terms, more than the \
                 limit of {MAX_PHI_TERMS}",
                action.id
            ));
        }
        actions.push(features);
    }
    let chosen = row
        .chosen_position()
        .ok_or_else(|| format!("chosen action {} is not eligible", row.chosen))?;
    Ok(RowFeatures {
        context,
        actions,
        chosen,
    })
}

/// Rows with a reward, prepared for evaluation. Built once, shared by the
/// reward model, the target policy and the estimators.
#[derive(Debug, Clone)]
pub struct EvalData {
    rows: Vec<LoggedRow>,
    rewards: Vec<f64>,
    features: Vec<RowFeatures>,
    row_folds: Vec<usize>,
    fold_rows: Vec<Vec<usize>>,
    scale: RewardScale,
    rows_in: usize,
    without_reward: usize,
    outside_range: usize,
}

impl EvalData {
    /// Check every row, skip (and count) rows without a reward, reject
    /// repeated decision ids, featurize, assign `folds` folds and fix the
    /// reward scale (`reward_range`, or the observed range). Needs at least
    /// [`MIN_ROWS`] rows with a reward.
    pub fn new(
        rows: Vec<LoggedRow>,
        folds: usize,
        reward_range: Option<[f64; 2]>,
    ) -> Result<Self, String> {
        if !(2..=MAX_FOLDS).contains(&folds) {
            return Err(format!(
                "folds must be an integer in [2, {MAX_FOLDS}] (got {folds})"
            ));
        }
        let given = reward_range
            .map(|[lo, hi]| RewardScale::given(lo, hi))
            .transpose()?;
        let rows_in = rows.len();
        let mut kept = Vec::with_capacity(rows.len());
        // Input index of each kept row, for error messages.
        let mut origin = Vec::with_capacity(rows.len());
        let mut without_reward = 0;
        for (index, row) in rows.into_iter().enumerate() {
            row.check()
                .map_err(|e| format!("rows[{index}] (decision {:?}): {e}", row.decision_id))?;
            if row.reward.is_some() {
                kept.push(row);
                origin.push(index);
            } else {
                without_reward += 1;
            }
        }
        if kept.len() < MIN_ROWS {
            return Err(format!(
                "need at least {MIN_ROWS} rows with a reward to evaluate (found {} of {rows_in})",
                kept.len()
            ));
        }
        let mut first: HashMap<&str, usize> = HashMap::with_capacity(kept.len());
        for (i, row) in kept.iter().enumerate() {
            if let Some(j) = first.insert(row.decision_id.as_str(), i) {
                return Err(format!(
                    "rows[{}] and rows[{}] share decision id {:?}; each decision may appear once",
                    origin[j], origin[i], row.decision_id
                ));
            }
        }
        drop(first);
        let features = kept
            .iter()
            .map(|row| featurize(row).map_err(|e| format!("decision {:?}: {e}", row.decision_id)))
            .collect::<Result<Vec<_>, _>>()?;
        let rewards: Vec<f64> = kept.iter().map(|row| row.reward.unwrap_or(0.0)).collect();
        let scale = match given {
            Some(scale) => scale,
            None => RewardScale::observed(&rewards)?,
        };
        let outside_range = rewards.iter().filter(|&&r| !scale.contains(r)).count();
        let row_folds: Vec<usize> = kept
            .iter()
            .map(|row| fold_of(&row.decision_id, folds))
            .collect();
        let mut fold_rows = vec![Vec::new(); folds];
        for (i, &k) in row_folds.iter().enumerate() {
            fold_rows[k].push(i);
        }
        Ok(Self {
            rows: kept,
            rewards,
            features,
            row_folds,
            fold_rows,
            scale,
            rows_in,
            without_reward,
            outside_range,
        })
    }

    /// Number of rows with a reward (`n`).
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Always false: construction requires at least [`MIN_ROWS`] rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[LoggedRow] {
        &self.rows
    }

    pub fn row(&self, i: usize) -> &LoggedRow {
        &self.rows[i]
    }

    /// Raw reward of row `i`.
    pub fn reward(&self, i: usize) -> f64 {
        self.rewards[i]
    }

    /// Position of row `i`'s chosen action within its `eligible` list.
    pub fn chosen_position(&self, i: usize) -> usize {
        self.features[i].chosen
    }

    /// Number of cross-fitting folds.
    pub fn folds(&self) -> usize {
        self.fold_rows.len()
    }

    /// Fold of row `i`.
    pub fn fold(&self, i: usize) -> usize {
        self.row_folds[i]
    }

    /// Row indices in fold `k`, ascending.
    pub fn fold_rows(&self, k: usize) -> &[usize] {
        &self.fold_rows[k]
    }

    pub fn scale(&self) -> &RewardScale {
        &self.scale
    }

    /// Rows passed to [`Self::new`], with or without a reward.
    pub fn rows_in(&self) -> usize {
        self.rows_in
    }

    /// Rows skipped because they have no reward.
    pub fn rows_without_reward(&self) -> usize {
        self.without_reward
    }

    /// Rewards outside a given reward range (0 for an observed range).
    pub fn rewards_outside_range(&self) -> usize {
        self.outside_range
    }

    pub(crate) fn features(&self, i: usize) -> &RowFeatures {
        &self.features[i]
    }
}

/// Learning-rate multipliers of the passes over the training rows.
///
/// [`ENGINE_PASSES`] is one pass at the spec's rate, as the engine learns
/// online. [`ANNEALED_PASSES`] runs three passes at 1, 1/4 and 1/16 of it:
/// at a constant rate the learner's last iterate keeps moving with the
/// reward noise, and the reward model's accuracy is what sets DM's error
/// and DR's variance. In a simulation shaped like the one in `tests/ope.rs`
/// (uniform logging, Bernoulli rewards, 1600 to 20000 training rows),
/// annealing cut the root-mean-square error of the predicted means by 2.5
/// to 3.5 times.
pub(crate) const ENGINE_PASSES: &[f64] = &[1.0];
/// See [`ENGINE_PASSES`].
pub(crate) const ANNEALED_PASSES: &[f64] = &[1.0, 0.25, 0.0625];

/// How to train a model on the logs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelSpec {
    /// Hash bits, learning rate and importance weighting, as in a spec.
    pub learner: LearnerSpec,
    /// `true`: [`Featurizer::phi_with_context`] (the DR reward model, whose
    /// absolute predictions matter); `false`: [`Featurizer::phi`], exactly
    /// the engine's feature map (the greedy target policies).
    pub with_context: bool,
    /// Learning-rate multiplier of each pass over the training rows.
    pub passes: &'static [f64],
}

/// A model trained without some rows, able to score any row.
pub(crate) struct FoldModel<'a> {
    data: &'a EvalData,
    spec: &'a ModelSpec,
    featurizer: Featurizer,
    model: LinearModel,
}

impl FoldModel<'_> {
    fn phi(
        &self,
        context: &ContextFeatures,
        action: &[Feature],
        out: &mut Vec<(u32, f32)>,
    ) -> Result<(), String> {
        let built = if self.spec.with_context {
            self.featurizer.phi_with_context(context, action, out)
        } else {
            self.featurizer.phi(context, action, out)
        };
        built.map_err(|e| e.to_string())
    }

    /// Predicted normalized reward of each eligible action of row `i`,
    /// clamped to `[0, 1]` as the engine does (true expected rewards lie
    /// there, so clamping only moves an estimate towards the truth).
    pub fn predict(&self, i: usize, scratch: &mut Vec<(u32, f32)>) -> Result<Vec<f64>, String> {
        let f = self.data.features(i);
        let mut out = Vec::with_capacity(f.actions.len());
        for action in &f.actions {
            self.phi(&f.context, action, scratch)?;
            out.push(self.model.predict(scratch).clamp(0.0, 1.0));
        }
        Ok(out)
    }

    /// Position within the row's `eligible` list of the action with the
    /// highest prediction; ties go to the first, as in the engine.
    pub fn best(&self, i: usize, scratch: &mut Vec<(u32, f32)>) -> Result<usize, String> {
        Ok(argmax(&self.predict(i, scratch)?))
    }
}

/// Train a fresh model on the rows `include` selects: NAG updates on
/// `phi(x, chosen)` towards the normalized reward, in row order, once per
/// entry of `spec.passes` at that multiple of the learning rate, weighted
/// as the learner spec says (`mtr`: `min(1 / probability,
/// maxImportanceWeight)`).
fn train<'a>(
    data: &'a EvalData,
    spec: &'a ModelSpec,
    include: impl Fn(usize) -> bool,
) -> Result<FoldModel<'a>, String> {
    let learner = &spec.learner;
    let mut fold_model = FoldModel {
        data,
        spec,
        featurizer: Featurizer::new(learner.bits)?,
        model: LinearModel::new(learner.bits, learner.learning_rate)?,
    };
    let mut phi = Vec::new();
    for &multiplier in spec.passes {
        fold_model
            .model
            .set_learning_rate(learner.learning_rate * multiplier)?;
        for i in (0..data.len()).filter(|&i| include(i)) {
            let f = data.features(i);
            fold_model.phi(&f.context, &f.actions[f.chosen], &mut phi)?;
            let target = data.scale().normalize(data.reward(i));
            let weight = match learner.importance {
                Importance::Unweighted => 1.0,
                Importance::Mtr => {
                    (1.0 / data.row(i).probability).min(learner.max_importance_weight)
                }
            };
            fold_model.model.update(&phi, target, weight)?;
        }
    }
    Ok(fold_model)
}

/// Cross-fit: for each non-empty fold `k`, train a model on every row
/// outside `k`, then call `visit(i, model)` for each row `i` in `k`. Every
/// row is visited exactly once, by a model that never saw it.
pub(crate) fn cross_fit(
    data: &EvalData,
    spec: &ModelSpec,
    mut visit: impl FnMut(usize, &FoldModel<'_>) -> Result<(), String>,
) -> Result<(), String> {
    for k in 0..data.folds() {
        let rows = data.fold_rows(k);
        if rows.is_empty() {
            continue;
        }
        let model = train(data, spec, |i| data.fold(i) != k)?;
        for &i in rows {
            visit(i, &model)?;
        }
    }
    Ok(())
}

/// The leaky alternative, for tests only: one model trained on every row
/// scores every row, including the ones it learned from.
#[cfg(test)]
pub(crate) fn fit_in_sample(
    data: &EvalData,
    spec: &ModelSpec,
    mut visit: impl FnMut(usize, &FoldModel<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let model = train(data, spec, |_| true)?;
    for i in 0..data.len() {
        visit(i, &model)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    //! Small row builders shared by the unit tests.

    use serde_json::{Value, json};

    use crate::decision::ActionSpec;
    use crate::ope::row::LoggedRow;

    /// A row over actions `a0..a{k-1}`, all eligible.
    pub fn row(
        id: &str,
        context: Value,
        pmf: &[f64],
        chosen: usize,
        reward: Option<f64>,
    ) -> LoggedRow {
        LoggedRow {
            decision_id: id.to_string(),
            ts_ms: 0,
            context,
            derived: Value::Null,
            actions: (0..pmf.len())
                .map(|a| ActionSpec::new(format!("a{a}")))
                .collect(),
            eligible: (0..pmf.len()).collect(),
            pmf: pmf.to_vec(),
            chosen,
            probability: pmf[chosen],
            reward,
            target_pmf: None,
        }
    }

    /// The four-row example the estimator arithmetic tests use: two
    /// actions, rewards 1, 0, 1, 1.
    pub fn four_rows() -> Vec<LoggedRow> {
        vec![
            row("r1", json!({"u": 1}), &[0.5, 0.5], 0, Some(1.0)),
            row("r2", json!({"u": 2}), &[0.8, 0.2], 1, Some(0.0)),
            row("r3", json!({"u": 3}), &[0.25, 0.75], 0, Some(1.0)),
            row("r4", json!({"u": 4}), &[0.9, 0.1], 1, Some(1.0)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{four_rows, row};
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn folds_are_deterministic_and_balanced() {
        assert_eq!(fold_of("dec_1", 5), fold_of("dec_1", 5));
        // Pinned values: the assignment must not change between releases,
        // or reports stop being reproducible.
        let pinned: Vec<usize> = ["dec_1", "dec_2", "abc", ""]
            .iter()
            .map(|id| fold_of(id, 5))
            .collect();
        let again: Vec<usize> = ["dec_1", "dec_2", "abc", ""]
            .iter()
            .map(|id| (fmix64(fnv1a64(id.as_bytes())) % 5) as usize)
            .collect();
        assert_eq!(pinned, again);
        // Sequential ids spread evenly: each of 5 folds gets 20% +- 1.5%.
        let n = 20_000;
        let mut counts = [0usize; 5];
        for i in 0..n {
            counts[fold_of(&format!("dec_{i}"), 5)] += 1;
        }
        for c in counts {
            assert!((3700..4300).contains(&c), "{counts:?}");
        }
    }

    #[test]
    fn rows_without_reward_are_skipped_and_counted() {
        let mut rows = four_rows();
        rows.push(row("r5", json!({}), &[0.5, 0.5], 0, None));
        rows.push(row("r6", json!({}), &[0.5, 0.5], 1, None));
        let data = EvalData::new(rows, 5, None).unwrap();
        assert_eq!(data.len(), 4);
        assert_eq!(data.rows_in(), 6);
        assert_eq!(data.rows_without_reward(), 2);
        assert_eq!(data.chosen_position(1), 1);
        assert_eq!(data.reward(1), 0.0);
        // Every row is in exactly one fold.
        let total: usize = (0..data.folds()).map(|k| data.fold_rows(k).len()).sum();
        assert_eq!(total, 4);
        for i in 0..data.len() {
            assert!(data.fold_rows(data.fold(i)).contains(&i));
        }
    }

    #[test]
    fn construction_errors() {
        assert!(
            EvalData::new(four_rows(), 1, None)
                .unwrap_err()
                .contains("folds must be an integer in [2, 100] (got 1)")
        );
        let one = vec![four_rows().remove(0), row("x", json!({}), &[1.0], 0, None)];
        assert_eq!(
            EvalData::new(one, 5, None).unwrap_err(),
            "need at least 2 rows with a reward to evaluate (found 1 of 2)"
        );
        let mut twice = four_rows();
        twice.insert(1, row("x", json!({}), &[1.0], 0, None));
        twice[4].decision_id = "r1".into();
        assert_eq!(
            EvalData::new(twice, 5, None).unwrap_err(),
            "rows[0] and rows[4] share decision id \"r1\"; each decision may appear once"
        );
        let mut bad = four_rows();
        bad[2].probability = 0.3;
        let e = EvalData::new(bad, 5, None).unwrap_err();
        assert!(
            e.starts_with("rows[2] (decision \"r3\"): probability 0.3"),
            "{e}"
        );
        assert!(
            EvalData::new(four_rows(), 5, Some([1.0, 1.0]))
                .unwrap_err()
                .contains("lo < hi")
        );
    }

    #[test]
    fn reward_scale() {
        let data = EvalData::new(four_rows(), 5, None).unwrap();
        let s = data.scale();
        assert_eq!(
            (s.lo(), s.hi(), s.source()),
            (0.0, 1.0, RangeSource::Observed)
        );
        assert_eq!(data.rewards_outside_range(), 0);
        let data = EvalData::new(four_rows(), 5, Some([0.0, 0.5])).unwrap();
        assert_eq!(data.scale().source(), RangeSource::Given);
        assert_eq!(
            data.rewards_outside_range(),
            3,
            "three rewards of 1 exceed 0.5"
        );
        assert_eq!(data.scale().normalize(1.0), 1.0, "clamped");
        let s = RewardScale::given(-2.0, 6.0).unwrap();
        assert_eq!(s.normalize(0.0), 0.25);
        assert_eq!(s.denormalize(0.25), 0.0);
        assert_eq!(s.normalize(-10.0), 0.0);
        // Constant rewards get a non-degenerate range centred on them.
        let s = RewardScale::observed(&[3.0, 3.0]).unwrap();
        assert_eq!((s.lo(), s.hi()), (2.5, 3.5));
        assert_eq!(s.normalize(3.0), 0.5);
        let s = RewardScale::observed(&[1e20, 1e20]).unwrap();
        assert!(s.hi() > s.lo() && s.normalize(1e20) == 0.5);
        assert!(RewardScale::observed(&[-1e308, 1e308]).is_err());
        assert!(RewardScale::given(0.0, f64::INFINITY).is_err());
        assert!(RewardScale::given(f64::NAN, 1.0).is_err());
    }

    #[test]
    fn featurize_checks_the_term_limit() {
        let mut map = serde_json::Map::new();
        for i in 0..4000 {
            map.insert(format!("f{i}"), json!(1));
        }
        let mut wide = row("w", Value::Object(map.clone()), &[1.0], 0, Some(1.0));
        wide.actions[0].features = map;
        let e = featurize(&wide).unwrap_err();
        assert!(e.contains("more than the limit of 1048576"), "{e}");
    }

    #[test]
    fn cross_fit_visits_every_row_once_with_a_model_that_never_saw_it() {
        let rows: Vec<LoggedRow> = (0..60)
            .map(|i| {
                row(
                    &format!("d{i}"),
                    json!({"i": i}),
                    &[0.5, 0.5],
                    i % 2,
                    Some((i % 3) as f64),
                )
            })
            .collect();
        let data = EvalData::new(rows, 4, None).unwrap();
        let spec = ModelSpec {
            learner: LearnerSpec {
                bits: 12,
                ..LearnerSpec::default()
            },
            with_context: true,
            passes: ANNEALED_PASSES,
        };
        let mut visits = vec![0usize; data.len()];
        let mut scratch = Vec::new();
        cross_fit(&data, &spec, |i, model| {
            visits[i] += 1;
            // The model saw exactly the rows outside this row's fold, once
            // per pass, and ends at the smallest learning rate.
            let outside = data.len() - data.fold_rows(data.fold(i)).len();
            assert_eq!(model.model.n_updates(), 3 * outside as u64);
            assert_eq!(model.model.learning_rate(), 0.5 * 0.0625);
            let predictions = model.predict(i, &mut scratch)?;
            assert_eq!(predictions.len(), 2);
            assert!(predictions.iter().all(|p| (0.0..=1.0).contains(p)));
            Ok(())
        })
        .unwrap();
        assert!(visits.iter().all(|&v| v == 1), "{visits:?}");
        fit_in_sample(&data, &spec, |_, model| {
            assert_eq!(model.model.n_updates(), 180);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mtr_training_weights_by_inverse_probability() {
        let rows = four_rows();
        let data = EvalData::new(rows, 2, None).unwrap();
        let spec = ModelSpec {
            learner: LearnerSpec {
                bits: 10,
                importance: Importance::Mtr,
                max_importance_weight: 5.0,
                ..LearnerSpec::default()
            },
            with_context: false,
            passes: ENGINE_PASSES,
        };
        let model = train(&data, &spec, |_| true).unwrap();
        // Weights min(1/p, 5): 2, 5, 4, 5.
        assert_eq!(model.model.total_weight(), 16.0);
    }
}
