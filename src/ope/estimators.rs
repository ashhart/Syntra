//! Estimators of a target policy's value from logged rows, with
//! diagnostics and confidence intervals.
//!
//! # Estimators
//!
//! For row `i` with logged action `a_i`, logging probability `mu_i`, raw
//! reward `r_i`, target PMF `pi_i` and cross-fitted reward predictions
//! `rhat_i(a)` (raw units), let `w_i = min(pi_i(a_i) / mu_i, w_max)`. Then
//!
//! ```text
//! DM    = mean_i  sum_a pi_i(a) rhat_i(a)
//! IPS   = mean_i  w_i r_i
//! SNIPS = sum_i w_i r_i / sum_i w_i
//! DR    = mean_i [ sum_a pi_i(a) rhat_i(a) + w_i (r_i - rhat_i(a_i)) ]
//! ```
//!
//! and the logged policy's on-policy value is `mean_i r_i`, reported with
//! every evaluation as the comparison point.
//!
//! # Lift
//!
//! The lift of an estimator is the target's value minus the logged
//! policy's, estimated row by row on the same rows: `mean_i (term_i - r_i)`
//! for DM, IPS and DR, and `SNIPS - mean_i r_i` for SNIPS. Its intervals
//! come from the same bootstrap resamples as the estimates (and its
//! standard error from the per-row differences), so they account for the
//! logged mean's own uncertainty and for the correlation between the two.
//! `lift.dr.lower >= x` is the recommended promotion gate.
//!
//! Without clipping, IPS and DR are unbiased whenever the logged
//! propensities are the ones actions were sampled with and the target only
//! puts mass where the logging policy does; DR stays unbiased for any
//! reward model that did not see the row (cross-fitting guarantees that)
//! and has lower variance the better the model is. SNIPS trades an
//! O(1/n) bias for robustness to large weights. DM is only as good as the
//! reward model and is biased when the model is. Clipping weights at
//! `w_max` (default 100) bounds the variance at the price of bias on the
//! clipped rows: their IPS terms shrink, their DR terms fall back towards
//! the reward model, and SNIPS gives them less say. The clip rate says how
//! often it happened.
//!
//! The reward model is the decision core's [`LinearModel`] on
//! `phi_with_context` features (so context-only terms help its absolute
//! calibration), trained unweighted with the default learner settings on
//! the normalized rewards of the other folds, in three passes at 1, 1/4 and
//! 1/16 of the learning rate (see [`super::data`]).
//!
//! # Intervals
//!
//! Every estimate gets a standard error and two 95% intervals:
//!
//! - `normalLower`/`normalUpper`: `estimate -+ 1.96 se`. For DM, IPS, DR and
//!   the logged mean `se` is the sample standard deviation of the per-row
//!   terms over `sqrt(n)`; for SNIPS it is the delta-method (linearized)
//!   standard error `sqrt(sum_i (w_i (r_i - SNIPS))^2 / ((n - 1) n)) / mean(w)`.
//! - `lower`/`upper`: a percentile bootstrap over rows (default 1000
//!   resamples, seeded, reproducible): resample `n` rows with replacement,
//!   recompute every estimator on the resample, take the 2.5% and 97.5%
//!   quantiles (linear interpolation). With `bootstrap = 0` they equal the
//!   normal interval.
//!
//! Lifts get the same pair of intervals; SNIPS's lift uses the delta-method
//! standard error of `SNIPS - mean(r)`, from the per-row terms
//! `w_i (r_i - SNIPS) / mean(w) - (r_i - mean(r))`.
//!
//! Both hold the reward model and any fitted target policy fixed, so they
//! describe sampling noise in the rows, not uncertainty in the fitted
//! models; DM's interval in particular ignores the model's bias. SNIPS
//! resamples whose weights are all zero are skipped.
//!
//! # Diagnostics
//!
//! - `ess = (sum w)^2 / sum w^2` over the clipped weights: how many rows the
//!   weighted estimates effectively rest on.
//! - `coverage`: share of rows where the target puts mass on the logged
//!   action (only those rows carry IPS weight).
//! - `clipRate` and `clippedRows`: rows whose weight exceeded `w_max`.
//! - `maxWeight`, `meanWeight`: of the unclipped weights. The mean should be
//!   close to the target's supported mass (1 for a target that stays inside
//!   the logging support); a significant gap suggests wrong propensities.
//! - `fallbackRows`, `fallbackRate`: rows where the target falls back to the
//!   logged PMF (a constant action that is not eligible there); above 5%
//!   the report warns.
//! - `unsupportedMass`: mean target probability on eligible actions the
//!   logging policy gave probability 0.
//!
//! [`LinearModel`]: crate::decision::LinearModel

use serde::Serialize;

use std::collections::HashMap;

use crate::decision::SplitMix64;
use crate::decision::spec::{DecisionSpec, LearnerSpec, RewardAggregation};

use super::data::{
    ANNEALED_PASSES, EvalData, MAX_FOLDS, ModelSpec, RangeSource, RewardScale, cross_fit,
};
use super::policy::TargetPolicy;
use super::row::PMF_SUM_TOLERANCE;

/// Confidence level of every interval.
pub const CONFIDENCE: f64 = 0.95;
/// The standard normal quantile at `(1 + CONFIDENCE) / 2`.
pub const Z_95: f64 = 1.959_963_984_540_054;
/// Fewest bootstrap resamples accepted (other than 0, which disables it).
pub const MIN_BOOTSTRAP: usize = 100;
/// Most bootstrap resamples accepted.
pub const MAX_BOOTSTRAP: usize = 100_000;
/// Below this effective sample size the report warns.
pub const LOW_ESS: f64 = 100.0;
/// Above this share of fallback rows the report warns.
pub const FALLBACK_WARNING: f64 = 0.05;

/// Evaluation settings. The defaults match `syntra evaluate`.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalConfig {
    /// Cross-fitting folds, in `[2, 100]`.
    pub folds: usize,
    /// Bootstrap resamples: 0 (normal intervals only) or in
    /// `[100, 100000]`.
    pub bootstrap: usize,
    /// Seed of the bootstrap resampling.
    pub seed: u64,
    /// Importance weights are clipped to at most this; at least 1, and
    /// `f64::INFINITY` disables clipping.
    pub w_max: f64,
    /// Raw reward range `[lo, hi]` used to normalize rewards for model
    /// training; `None` uses the observed range.
    pub reward_range: Option<[f64; 2]>,
    /// Learner settings of the DR reward model.
    pub reward_model: LearnerSpec,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            folds: 5,
            bootstrap: 1000,
            seed: 7,
            w_max: 100.0,
            reward_range: None,
            reward_model: LearnerSpec::default(),
        }
    }
}

impl EvalConfig {
    /// Check every setting's range.
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=MAX_FOLDS).contains(&self.folds) {
            return Err(format!(
                "folds must be an integer in [2, {MAX_FOLDS}] (got {})",
                self.folds
            ));
        }
        if self.bootstrap != 0 && !(MIN_BOOTSTRAP..=MAX_BOOTSTRAP).contains(&self.bootstrap) {
            return Err(format!(
                "bootstrap must be 0 (normal intervals only) or in \
                 [{MIN_BOOTSTRAP}, {MAX_BOOTSTRAP}] (got {})",
                self.bootstrap
            ));
        }
        if self.w_max.is_nan() || self.w_max < 1.0 {
            return Err(format!(
                "w-max must be at least 1, or inf for no clipping (got {})",
                self.w_max
            ));
        }
        if let Some([lo, hi]) = self.reward_range {
            RewardScale::given(lo, hi)?;
        }
        check_learner(&self.reward_model)
    }
}

/// The DR reward model's settings must be valid spec learner settings.
fn check_learner(learner: &LearnerSpec) -> Result<(), String> {
    DecisionSpec {
        learner: learner.clone(),
        ..DecisionSpec::default()
    }
    .validate()
    .map_err(|e| format!("reward model: {e}"))
}

/// Cross-fitted reward predictions `rhat(x_i, a)`, in raw units, for every
/// eligible action of every row.
#[derive(Debug, Clone, PartialEq)]
pub struct RewardModel {
    predictions: Vec<Vec<f64>>,
}

impl RewardModel {
    /// Fit the reward model: per fold, a [`crate::decision::LinearModel`] on
    /// `phi_with_context` trained on the other folds in three annealed
    /// passes (see the module documentation), predicting the fold's rows.
    pub fn fit(data: &EvalData, learner: &LearnerSpec) -> Result<Self, String> {
        check_learner(learner)?;
        let spec = ModelSpec {
            learner: learner.clone(),
            with_context: true,
            passes: ANNEALED_PASSES,
            action_features: HashMap::new(),
        };
        let scale = *data.scale();
        let mut predictions = vec![Vec::new(); data.len()];
        let mut scratch = Vec::new();
        cross_fit(data, &spec, |i, model| {
            predictions[i] = model
                .predict(i, &mut scratch)?
                .into_iter()
                .map(|x| scale.denormalize(x))
                .collect();
            Ok(())
        })?;
        Ok(Self { predictions })
    }

    /// Use externally computed predictions (raw units, one per eligible
    /// action of each row). DR stays unbiased for any predictions that do
    /// not depend on the row's own action and reward.
    pub fn from_predictions(data: &EvalData, predictions: Vec<Vec<f64>>) -> Result<Self, String> {
        if predictions.len() != data.len() {
            return Err(format!(
                "reward predictions cover {} rows, the data has {}",
                predictions.len(),
                data.len()
            ));
        }
        for (i, values) in predictions.iter().enumerate() {
            let row = data.row(i);
            if values.len() != row.eligible.len() || !values.iter().all(|v| v.is_finite()) {
                return Err(format!(
                    "reward predictions for decision {:?} must be {} finite numbers",
                    row.decision_id,
                    row.eligible.len()
                ));
            }
        }
        Ok(Self { predictions })
    }

    /// Predictions for row `i`, aligned with its `eligible` list.
    pub fn predictions(&self, i: usize) -> &[f64] {
        &self.predictions[i]
    }

    /// Number of rows covered.
    pub fn len(&self) -> usize {
        self.predictions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.predictions.is_empty()
    }
}

/// One estimator's value, in raw reward units. NaN (JSON null) where
/// undefined: SNIPS when every weight is zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    pub estimate: f64,
    pub se: f64,
    /// Lower end of the 95% interval: bootstrap percentile, or normal when
    /// the bootstrap is off.
    pub lower: f64,
    pub upper: f64,
    pub normal_lower: f64,
    pub normal_upper: f64,
}

/// The logging policy's on-policy mean reward with its intervals.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggedValue {
    pub mean: f64,
    pub se: f64,
    pub lower: f64,
    pub upper: f64,
    pub normal_lower: f64,
    pub normal_upper: f64,
}

/// The four estimates of the target policy's value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Estimators {
    pub dm: Estimate,
    pub ips: Estimate,
    pub snips: Estimate,
    pub dr: Estimate,
}

/// An estimator's lift over the logged policy, paired on the same rows (see
/// the module documentation). NaN (JSON null) where undefined.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lift {
    pub mean: f64,
    pub se: f64,
    /// Lower end of the paired 95% interval: bootstrap percentile, or
    /// normal when the bootstrap is off.
    pub lower: f64,
    pub upper: f64,
    pub normal_lower: f64,
    pub normal_upper: f64,
}

/// The paired lift of each estimator.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Lifts {
    pub dm: Lift,
    pub ips: Lift,
    pub snips: Lift,
    pub dr: Lift,
}

/// Weight and support diagnostics; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    /// Rows evaluated (rows with a reward).
    pub n: usize,
    pub ess: f64,
    pub coverage: f64,
    pub clip_rate: f64,
    pub clipped_rows: usize,
    pub max_weight: f64,
    pub mean_weight: f64,
    pub fallback_rows: usize,
    pub fallback_rate: f64,
    pub unsupported_mass: f64,
}

/// What went into the evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSummary {
    /// Rows read, with or without a PMF or a reward.
    pub rows: usize,
    /// Legacy decisions without a logged PMF, skipped.
    pub rows_without_pmf: usize,
    /// Uploaded decisions whose propensities could not be verified,
    /// skipped.
    pub rows_unverified: usize,
    pub rows_without_reward: usize,
    /// How `rewards` arrays were reduced to one reward per row.
    pub reward_aggregation: RewardAggregation,
    /// Rows whose reward came from a `rewards` array.
    pub aggregated_rewards: usize,
    pub folds: usize,
    /// `[lo, hi]` used to normalize rewards for the models.
    pub reward_range: [f64; 2],
    pub reward_range_source: RangeSource,
    /// Rewards outside a given range (clamped for model training only).
    pub rewards_outside_range: usize,
}

/// How `lower` and `upper` were computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Interval {
    BootstrapPercentile,
    Normal,
}

/// The estimator settings used.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub bootstrap: usize,
    pub seed: u64,
    /// JSON null when clipping is off (infinite).
    pub w_max: f64,
    pub confidence: f64,
    pub interval: Interval,
    pub reward_model: LearnerSpec,
}

/// The numbers of one evaluation, serializable as JSON (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    /// The target policy's label.
    pub policy: String,
    pub data: DataSummary,
    pub logged: LoggedValue,
    pub estimators: Estimators,
    /// Each estimator minus the logged mean, paired on the same rows.
    pub lift: Lifts,
    pub diagnostics: Diagnostics,
    pub settings: Settings,
    /// Plain-language caveats (clipping, low ESS, unsupported mass, ...).
    pub warnings: Vec<String>,
}

/// Per-row terms; every estimator is a mean or a ratio of sums of these.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Terms {
    /// Clipped importance weight.
    w: f64,
    /// `w * r`.
    wr: f64,
    /// `sum_a pi(a) rhat(a)`.
    dm: f64,
    /// `dm + w (r - rhat(a_i))`.
    dr: f64,
    /// Raw reward.
    r: f64,
}

impl Terms {
    fn add(&mut self, other: &Terms) {
        self.w += other.w;
        self.wr += other.wr;
        self.dm += other.dm;
        self.dr += other.dr;
        self.r += other.r;
    }

    fn is_finite(&self) -> bool {
        [self.w, self.wr, self.dm, self.dr, self.r]
            .iter()
            .all(|v| v.is_finite())
    }
}

/// Estimate the value of `policy` on `data`. `rewards` must come from the
/// same data; `config` supplies the clipping, bootstrap and seed.
pub fn evaluate(
    data: &EvalData,
    rewards: &RewardModel,
    policy: &dyn TargetPolicy,
    config: &EvalConfig,
) -> Result<Evaluation, String> {
    config.validate()?;
    if rewards.len() != data.len() {
        return Err(format!(
            "the reward model covers {} rows, the data has {}",
            rewards.len(),
            data.len()
        ));
    }
    let label = policy.label();
    let n = data.len();
    let mut terms = Vec::with_capacity(n);
    let mut raw_weights = Vec::with_capacity(n);
    // Unclipped weight minus its expectation under correct propensities.
    let mut weight_gaps = Vec::with_capacity(n);
    let (mut covered, mut clipped, mut fallback) = (0usize, 0usize, 0usize);
    let mut unsupported = 0.0;
    for i in 0..n {
        let row = data.row(i);
        let target = policy.pmf(data, i);
        let mass = check_target(&target, row.eligible.len()).map_err(|e| {
            format!(
                "target policy {label} on decision {:?}: {e}",
                row.decision_id
            )
        })?;
        let position = data.chosen_position(i);
        let predicted = rewards.predictions(i);
        let r = data.reward(i);
        let raw = target[position] / row.probability;
        let w = raw.min(config.w_max);
        let dm: f64 = target.iter().zip(predicted).map(|(p, q)| p * q).sum();
        let t = Terms {
            w,
            wr: w * r,
            dm,
            dr: dm + w * (r - predicted[position]),
            r,
        };
        if !t.is_finite() {
            return Err(format!(
                "decision {:?}: importance weight {raw} makes the estimates overflow; \
                 set a finite --w-max",
                row.decision_id
            ));
        }
        let off_support: f64 = target
            .iter()
            .zip(&row.pmf)
            .filter(|&(_, &mu)| mu == 0.0)
            .map(|(p, _)| p)
            .sum();
        covered += usize::from(target[position] > 0.0);
        clipped += usize::from(raw > config.w_max);
        fallback += usize::from(policy.falls_back(data, i));
        unsupported += off_support;
        terms.push(t);
        raw_weights.push(raw);
        weight_gaps.push(raw - (mass - off_support));
    }

    let nf = n as f64;
    let boot = (config.bootstrap > 0).then(|| bootstrap(&terms, config.bootstrap, config.seed));
    let pick = |column: usize| boot.as_ref().map(|b| b[column]);
    let (dm, _) = mean_se(&terms, |t| t.dm);
    let (ips, ips_se) = mean_se(&terms, |t| t.wr);
    let (dr, dr_se) = mean_se(&terms, |t| t.dr);
    let (logged, logged_se) = mean_se(&terms, |t| t.r);
    let (snips, snips_se) = snips(&terms);
    // DM gets a point estimate only. Its error is mostly the reward model's,
    // which resampling rows with the model held fixed does not see: such
    // intervals covered the true value in 14% of simulated datasets where
    // DR's covered 95% (benchmarks/ope_vs_obp.py). DR's correction term
    // accounts for the model's error, so DR carries the inference.
    let estimators = Estimators {
        dm: point_only(dm),
        ips: estimate(ips, ips_se, pick(IPS)),
        snips: estimate(snips, snips_se, pick(SNIPS)),
        dr: estimate(dr, dr_se, pick(DR)),
    };
    let lift_of = |(mean, se): (f64, f64), column| {
        let e = estimate(mean, se, pick(column));
        Lift {
            mean: e.estimate,
            se: e.se,
            lower: e.lower,
            upper: e.upper,
            normal_lower: e.normal_lower,
            normal_upper: e.normal_upper,
        }
    };
    let lift = Lifts {
        dm: {
            let p = point_only(mean_se(&terms, |t| t.dm - t.r).0);
            Lift {
                mean: p.estimate,
                se: p.se,
                lower: p.lower,
                upper: p.upper,
                normal_lower: p.normal_lower,
                normal_upper: p.normal_upper,
            }
        },
        ips: lift_of(mean_se(&terms, |t| t.wr - t.r), LIFT_IPS),
        snips: lift_of(snips_lift(&terms, snips, logged), LIFT_SNIPS),
        dr: lift_of(mean_se(&terms, |t| t.dr - t.r), LIFT_DR),
    };
    let logged = estimate(logged, logged_se, pick(LOGGED));

    let sum_w: f64 = terms.iter().map(|t| t.w).sum();
    let sum_w2: f64 = terms.iter().map(|t| t.w * t.w).sum();
    let diagnostics = Diagnostics {
        n,
        ess: if sum_w2 > 0.0 {
            sum_w * sum_w / sum_w2
        } else {
            0.0
        },
        coverage: covered as f64 / nf,
        clip_rate: clipped as f64 / nf,
        clipped_rows: clipped,
        max_weight: raw_weights.iter().copied().fold(0.0, f64::max),
        mean_weight: raw_weights.iter().sum::<f64>() / nf,
        fallback_rows: fallback,
        fallback_rate: fallback as f64 / nf,
        unsupported_mass: unsupported / nf,
    };
    let scale = data.scale();
    let summary = DataSummary {
        rows: data.rows_in(),
        rows_without_pmf: data.rows_without_pmf(),
        rows_unverified: data.rows_unverified(),
        rows_without_reward: data.rows_without_reward(),
        reward_aggregation: data.reward_aggregation(),
        aggregated_rewards: data.aggregated_rewards(),
        folds: data.folds(),
        reward_range: [scale.lo(), scale.hi()],
        reward_range_source: scale.source(),
        rewards_outside_range: data.rewards_outside_range(),
    };
    let (gap, gap_se) = mean_of(&weight_gaps);
    let warnings = warnings(&label, &diagnostics, &summary, config.w_max, gap / gap_se);
    Ok(Evaluation {
        policy: label,
        data: summary,
        logged: LoggedValue {
            mean: logged.estimate,
            se: logged.se,
            lower: logged.lower,
            upper: logged.upper,
            normal_lower: logged.normal_lower,
            normal_upper: logged.normal_upper,
        },
        estimators,
        lift,
        diagnostics,
        settings: Settings {
            bootstrap: config.bootstrap,
            seed: config.seed,
            w_max: config.w_max,
            confidence: CONFIDENCE,
            interval: if boot.is_some() {
                Interval::BootstrapPercentile
            } else {
                Interval::Normal
            },
            reward_model: config.reward_model.clone(),
        },
        warnings,
    })
}

/// A target PMF must have one entry per eligible action, each in `[0, 1]`,
/// summing to 1 within the row tolerance. Returns the sum.
fn check_target(pmf: &[f64], eligible: usize) -> Result<f64, String> {
    if pmf.len() != eligible {
        return Err(format!(
            "returned {} probabilities for {eligible} eligible actions",
            pmf.len()
        ));
    }
    if let Some((k, p)) = pmf
        .iter()
        .enumerate()
        .find(|(_, p)| !(0.0..=1.0).contains(*p))
    {
        return Err(format!("probability [{k}] = {p} is not in [0, 1]"));
    }
    let mass: f64 = pmf.iter().sum();
    if (mass - 1.0).abs() <= PMF_SUM_TOLERANCE {
        Ok(mass)
    } else {
        Err(format!(
            "probabilities sum to {mass}, not 1 (tolerance {PMF_SUM_TOLERANCE:e})"
        ))
    }
}

/// Mean of `f` over the rows and its standard error (sample standard
/// deviation over `sqrt(n)`).
fn mean_se(terms: &[Terms], f: impl Fn(&Terms) -> f64) -> (f64, f64) {
    let n = terms.len() as f64;
    let mean = terms.iter().map(&f).sum::<f64>() / n;
    let squares: f64 = terms.iter().map(|t| (f(t) - mean).powi(2)).sum();
    (mean, standard_error(squares, terms.len()))
}

/// Mean of `values` and its standard error.
fn mean_of(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let squares: f64 = values.iter().map(|v| (v - mean).powi(2)).sum();
    (mean, standard_error(squares, values.len()))
}

/// `sqrt(squares / (n - 1) / n)`; NaN below two rows.
fn standard_error(squares: f64, n: usize) -> f64 {
    if n < 2 {
        return f64::NAN;
    }
    let n = n as f64;
    (squares / (n - 1.0) / n).sqrt()
}

/// SNIPS and its delta-method standard error; NaN when every weight is 0.
fn snips(terms: &[Terms]) -> (f64, f64) {
    let sum_w: f64 = terms.iter().map(|t| t.w).sum();
    // Weights are finite and non-negative, so this means "all zero".
    if sum_w <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let value = terms.iter().map(|t| t.wr).sum::<f64>() / sum_w;
    let mean_w = sum_w / terms.len() as f64;
    let squares: f64 = terms.iter().map(|t| (t.wr - t.w * value).powi(2)).sum();
    (value, standard_error(squares, terms.len()) / mean_w)
}

/// `SNIPS - mean(r)` and its delta-method standard error, from the per-row
/// terms `w_i (r_i - SNIPS) / mean(w) - (r_i - mean(r))`, which sum to 0.
/// NaN when every weight is 0.
fn snips_lift(terms: &[Terms], snips: f64, logged: f64) -> (f64, f64) {
    let sum_w: f64 = terms.iter().map(|t| t.w).sum();
    if sum_w <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let mean_w = sum_w / terms.len() as f64;
    let squares: f64 = terms
        .iter()
        .map(|t| ((t.wr - t.w * snips) / mean_w - (t.r - logged)).powi(2))
        .sum();
    (snips - logged, standard_error(squares, terms.len()))
}

/// An estimate with its normal interval, and the bootstrap interval when
/// there is one (otherwise `lower`/`upper` are the normal interval).
/// An estimate without an interval (NaN, JSON null, where there would be one).
fn point_only(value: f64) -> Estimate {
    Estimate {
        estimate: value,
        se: f64::NAN,
        lower: f64::NAN,
        upper: f64::NAN,
        normal_lower: f64::NAN,
        normal_upper: f64::NAN,
    }
}

fn estimate(value: f64, se: f64, bootstrap: Option<(f64, f64)>) -> Estimate {
    let normal_lower = value - Z_95 * se;
    let normal_upper = value + Z_95 * se;
    let (lower, upper) = bootstrap.unwrap_or((normal_lower, normal_upper));
    Estimate {
        estimate: value,
        se,
        lower,
        upper,
        normal_lower,
        normal_upper,
    }
}

const DM: usize = 0;
const IPS: usize = 1;
const SNIPS: usize = 2;
const DR: usize = 3;
const LOGGED: usize = 4;
const LIFT_DM: usize = 5;
const LIFT_IPS: usize = 6;
const LIFT_SNIPS: usize = 7;
const LIFT_DR: usize = 8;
/// Number of bootstrapped statistics.
const SERIES: usize = 9;

/// Percentile bootstrap intervals, indexed by `DM`, `IPS`, `SNIPS`, `DR`,
/// `LOGGED` and the four `LIFT_*` columns, which are computed from the same
/// resample as the estimates (a paired bootstrap). Resample `b` draws its
/// `n` row indices from a SplitMix64 seeded by the `b`-th output of
/// `SplitMix64::new(seed)`, so results are reproducible from the seed
/// alone.
fn bootstrap(terms: &[Terms], resamples: usize, seed: u64) -> [(f64, f64); SERIES] {
    let n = terms.len();
    let nf = n as f64;
    let mut seeds = SplitMix64::new(seed);
    let mut draws: [Vec<f64>; SERIES] = std::array::from_fn(|_| Vec::with_capacity(resamples));
    for _ in 0..resamples {
        let mut rng = SplitMix64::new(seeds.next_u64());
        let mut sum = Terms::default();
        for _ in 0..n {
            sum.add(&terms[uniform_index(&mut rng, n)]);
        }
        let snips = if sum.w > 0.0 {
            sum.wr / sum.w
        } else {
            f64::NAN
        };
        let logged = sum.r / nf;
        draws[DM].push(sum.dm / nf);
        draws[IPS].push(sum.wr / nf);
        draws[SNIPS].push(snips);
        draws[DR].push(sum.dr / nf);
        draws[LOGGED].push(logged);
        draws[LIFT_DM].push((sum.dm - sum.r) / nf);
        draws[LIFT_IPS].push((sum.wr - sum.r) / nf);
        draws[LIFT_SNIPS].push(snips - logged);
        draws[LIFT_DR].push((sum.dr - sum.r) / nf);
    }
    let alpha = (1.0 - CONFIDENCE) / 2.0;
    draws.map(|mut values| {
        values.retain(|v| !v.is_nan());
        values.sort_unstable_by(f64::total_cmp);
        (quantile(&values, alpha), quantile(&values, 1.0 - alpha))
    })
}

/// A uniform index in `0..n` (Lemire's multiply-shift; the bias is below
/// `n / 2^64`).
fn uniform_index(rng: &mut SplitMix64, n: usize) -> usize {
    ((u128::from(rng.next_u64()) * n as u128) >> 64) as usize
}

/// The `p` quantile of sorted values with linear interpolation between
/// order statistics (Hyndman and Fan type 7, the R and NumPy default).
/// NaN for no values.
fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let h = (sorted.len() - 1) as f64 * p;
    let below = h.floor() as usize;
    let above = h.ceil() as usize;
    sorted[below] + (h - below as f64) * (sorted[above] - sorted[below])
}

/// Plain-language caveats. `weight_z` is the gap between the mean unclipped
/// weight and its expectation, in standard errors.
fn warnings(
    label: &str,
    d: &Diagnostics,
    data: &DataSummary,
    w_max: f64,
    weight_z: f64,
) -> Vec<String> {
    let mut out = Vec::new();
    if data.rows_unverified > 0 {
        out.push(format!(
            "{} uploaded decisions are left out: their model was retired before the server \
             could replay them, so their propensities are unverified",
            data.rows_unverified
        ));
    }
    if d.fallback_rate > FALLBACK_WARNING {
        out.push(format!(
            "{label} has no action of its own in {} of {} rows ({:.2}%) and falls back to the \
             logged policy there, so the estimates value it as an override that applies to \
             the other rows only",
            d.fallback_rows,
            d.n,
            100.0 * d.fallback_rate
        ));
    }
    if d.unsupported_mass > 0.0 {
        out.push(format!(
            "the target puts {:.2}% of its probability on eligible actions the logging policy \
             never took (probability 0); IPS and SNIPS cannot see that mass and DR relies on \
             the reward model for it",
            100.0 * d.unsupported_mass
        ));
    }
    if d.clipped_rows > 0 {
        out.push(format!(
            "{} rows ({:.2}%) had importance weights above w_max = {w_max} (largest {:.4}); \
             clipping bounds the variance but biases IPS, SNIPS and DR on those rows",
            d.clipped_rows,
            100.0 * d.clip_rate,
            d.max_weight
        ));
    }
    if d.ess < LOW_ESS {
        out.push(format!(
            "the effective sample size is only {:.1}; the weighted estimates rest on few rows \
             and their intervals may be too narrow",
            d.ess
        ));
    }
    if d.n >= 100 && weight_z.abs() > 4.0 {
        out.push(format!(
            "the mean importance weight {:.4} is {:.1} standard errors from what correct \
             propensities give; check that the logged pmf is the one actions were sampled from",
            d.mean_weight, weight_z
        ));
    }
    if data.rewards_outside_range > 0 {
        out.push(format!(
            "{} rewards fall outside the given reward range [{}, {}]; they are clamped for \
             model training, while the estimates use the raw rewards",
            data.rewards_outside_range, data.reward_range[0], data.reward_range[1]
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ope::data::testing::{four_rows, row};
    use crate::ope::policy::{Constant, Logged};
    use crate::ope::row::LoggedRow;
    use serde_json::json;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    fn config(w_max: f64) -> EvalConfig {
        EvalConfig {
            w_max,
            bootstrap: 0,
            ..EvalConfig::default()
        }
    }

    /// The four-row example with a fixed reward model predicting `value`
    /// for every action.
    fn run(policy: &dyn TargetPolicy, w_max: f64, value: f64) -> Evaluation {
        let data = EvalData::new(four_rows(), 2, None).unwrap();
        let rewards = RewardModel::from_predictions(&data, vec![vec![value; 2]; 4]).unwrap();
        evaluate(&data, &rewards, policy, &config(w_max)).unwrap()
    }

    /// Rows (pmf, chosen, reward): ([.5,.5], a0, 1), ([.8,.2], a1, 0),
    /// ([.25,.75], a0, 1), ([.9,.1], a1, 1). Constant a0 has weights
    /// 2, 0, 4, 0.
    #[test]
    fn hand_computed_weights_and_estimates() {
        let e = run(&Constant::new("a0"), 100.0, 0.5);
        let (est, d) = (&e.estimators, &e.diagnostics);
        assert!(close(est.ips.estimate, 1.5), "{}", est.ips.estimate);
        assert!(close(est.snips.estimate, 1.0));
        assert!(close(est.dm.estimate, 0.5));
        // DR = 0.5 + mean(w (r - 0.5)) = 0.5 + (1 + 0 + 2 + 0) / 4.
        assert!(close(est.dr.estimate, 1.25));
        assert!(close(e.logged.mean, 0.75));
        assert!(close(d.ess, 36.0 / 20.0), "{}", d.ess);
        assert!(close(d.coverage, 0.5));
        assert!(close(d.max_weight, 4.0));
        assert!(close(d.mean_weight, 1.5));
        assert_eq!((d.clipped_rows, d.clip_rate, d.fallback_rows), (0, 0.0, 0));
        assert_eq!(d.unsupported_mass, 0.0);
        assert_eq!(d.n, 4);
        // IPS terms 2, 0, 4, 0: sample variance 11 / 3, se sqrt(11 / 12).
        assert!(close(est.ips.se, (11.0f64 / 12.0).sqrt()));
        assert!(close(
            est.ips.normal_lower,
            1.5 - Z_95 * (11.0f64 / 12.0).sqrt()
        ));
        // Bootstrap off: lower/upper are the normal interval.
        assert_eq!(est.ips.lower, est.ips.normal_lower);
        assert_eq!(e.settings.interval, Interval::Normal);
        // Every SNIPS weight-residual is 0 here: w (r - 1) = 0 in each row.
        assert!(close(est.snips.se, 0.0));
    }

    #[test]
    fn clipping_changes_weights_but_not_max_weight() {
        let e = run(&Constant::new("a0"), 3.0, 0.5);
        let (est, d) = (&e.estimators, &e.diagnostics);
        // Weight 4 is clipped to 3: IPS = (2 + 3) / 4.
        assert!(close(est.ips.estimate, 1.25));
        assert!(close(est.snips.estimate, 1.0));
        assert!(close(d.ess, 25.0 / 13.0));
        assert_eq!(d.clipped_rows, 1);
        assert!(close(d.clip_rate, 0.25));
        assert!(close(d.max_weight, 4.0), "max weight is reported unclipped");
        assert!(e.warnings.iter().any(|w| w.contains("above w_max = 3")));
    }

    #[test]
    fn snips_delta_method_standard_error() {
        // Constant a1: weights 0, 5, 0, 10; SNIPS = 10 / 15. Residuals
        // w (r - SNIPS) = 0, -10/3, 0, 10/3: sum of squares 200 / 9, so
        // se = sqrt(200 / 9 / 3 / 4) / mean(w) with mean(w) = 3.75.
        let e = run(&Constant::new("a1"), 100.0, 0.0);
        let s = e.estimators.snips;
        assert!(close(s.estimate, 2.0 / 3.0));
        assert!(close(s.se, (200.0f64 / 108.0).sqrt() / 3.75), "{}", s.se);
        assert!(close(e.estimators.ips.estimate, 2.5));
        // With a zero reward model DR is IPS, term by term.
        assert!(close(e.estimators.dr.estimate, e.estimators.ips.estimate));
        assert!(close(e.estimators.dr.se, e.estimators.ips.se));
        assert!(close(e.estimators.dm.estimate, 0.0));
    }

    #[test]
    fn logged_target_reproduces_the_on_policy_mean() {
        let e = run(&Logged, 100.0, 0.3);
        let est = &e.estimators;
        for value in [est.ips.estimate, est.snips.estimate, e.logged.mean] {
            assert!(close(value, 0.75), "{value}");
        }
        assert!(close(est.ips.se, e.logged.se));
        assert!(close(e.diagnostics.ess, 4.0));
        assert!(close(e.diagnostics.coverage, 1.0));
        assert!(close(e.diagnostics.mean_weight, 1.0));
        // DR = DM + mean(r - rhat(a_i)) = 0.3 + (0.75 - 0.3).
        assert!(close(est.dr.estimate, 0.75));
    }

    #[test]
    fn fallback_rows_and_unsupported_mass_are_counted() {
        let mut rows = four_rows();
        // a0 is not eligible in row 1; a1 has logging probability 0 in row 0.
        rows[1].eligible = vec![1];
        rows[1].pmf = vec![1.0];
        rows[1].probability = 1.0;
        rows[0].pmf = vec![1.0, 0.0];
        rows[0].probability = 1.0;
        let data = EvalData::new(rows, 2, None).unwrap();
        let rewards = RewardModel::from_predictions(
            &data,
            (0..4)
                .map(|i| vec![0.0; data.row(i).eligible.len()])
                .collect(),
        )
        .unwrap();
        let e = evaluate(&data, &rewards, &Constant::new("a0"), &config(100.0)).unwrap();
        assert_eq!(e.diagnostics.fallback_rows, 1);
        assert!(close(e.diagnostics.fallback_rate, 0.25));
        assert_eq!(e.diagnostics.unsupported_mass, 0.0);
        assert!(
            e.warnings
                .iter()
                .any(|w| w
                    .starts_with("constant:a0 has no action of its own in 1 of 4 rows (25.00%)")),
            "{:?}",
            e.warnings
        );
        // The fallback row keeps weight 1 in every estimator: weights are
        // 1, 1 (fallback), 4, 0 and SNIPS = (1 + 0 + 4 + 0) / 6.
        assert!(close(e.estimators.ips.estimate, 5.0 / 4.0));
        assert!(close(e.estimators.snips.estimate, 5.0 / 6.0));
        let e = evaluate(&data, &rewards, &Constant::new("a1"), &config(100.0)).unwrap();
        // Row 0 puts all target mass on a1, which the logger never takes.
        assert!(close(e.diagnostics.unsupported_mass, 0.25));
        assert_eq!(e.diagnostics.fallback_rows, 0);
        assert!(
            e.warnings
                .iter()
                .any(|w| w.contains("25.00% of its probability"))
        );
    }

    /// Hand-computed paired lifts for constant a0 on the four-row example
    /// with a reward model predicting 0.5: rewards 1, 0, 1, 1, weights 2, 0,
    /// 4, 0, logged mean 0.75.
    #[test]
    fn paired_lift_arithmetic() {
        let e = run(&Constant::new("a0"), 100.0, 0.5);
        let l = &e.lift;
        // IPS lift terms w r - r: 1, 0, 3, -1.
        assert!(close(l.ips.mean, 0.75));
        assert!(close(l.ips.se, (8.75f64 / 12.0).sqrt()), "{}", l.ips.se);
        // DR lift terms dr - r: 0.5, 0.5, 1.5, -0.5.
        assert!(close(l.dr.mean, 0.5));
        assert!(close(l.dr.se, (2.0f64 / 12.0).sqrt()), "{}", l.dr.se);
        assert!(close(l.dr.normal_lower, 0.5 - Z_95 * l.dr.se));
        assert_eq!(
            (l.dr.lower, l.dr.upper),
            (l.dr.normal_lower, l.dr.normal_upper)
        );
        // DM lift terms 0.5 - r: -0.5, 0.5, -0.5, -0.5.
        assert!(close(l.dm.mean, -0.25));
        // SNIPS lift 1 - 0.75, delta-method terms -0.25, 0.75, -0.25, -0.25.
        assert!(close(l.snips.mean, 0.25));
        assert!(close(l.snips.se, 0.25), "{}", l.snips.se);
        // Lifts are the estimates minus the logged mean.
        for (lift, est) in [
            (l.dm.mean, e.estimators.dm.estimate),
            (l.ips.mean, e.estimators.ips.estimate),
            (l.snips.mean, e.estimators.snips.estimate),
            (l.dr.mean, e.estimators.dr.estimate),
        ] {
            assert!(close(lift, est - e.logged.mean));
        }
    }

    #[test]
    fn invalid_target_pmfs_are_rejected() {
        struct Bad(Vec<f64>);
        impl TargetPolicy for Bad {
            fn label(&self) -> String {
                "bad".into()
            }
            fn pmf(&self, _: &EvalData, _: usize) -> Vec<f64> {
                self.0.clone()
            }
        }
        let data = EvalData::new(four_rows(), 2, None).unwrap();
        let rewards = RewardModel::from_predictions(&data, vec![vec![0.0; 2]; 4]).unwrap();
        for (pmf, want) in [
            (vec![1.0], "returned 1 probabilities for 2 eligible actions"),
            (vec![0.5, 0.4], "probabilities sum to 0.9"),
            (vec![1.5, -0.5], "probability [0] = 1.5 is not in [0, 1]"),
            (vec![f64::NAN, 1.0], "is not in [0, 1]"),
            (vec![0.0, 0.0], "probabilities sum to 0, not 1"),
        ] {
            let e = evaluate(&data, &rewards, &Bad(pmf), &config(100.0)).unwrap_err();
            assert!(
                e.starts_with("target policy bad on decision \"r1\": "),
                "{e}"
            );
            assert!(e.contains(want), "{e}");
        }
        assert!(RewardModel::from_predictions(&data, vec![vec![0.0; 2]; 3]).is_err());
        assert!(RewardModel::from_predictions(&data, vec![vec![f64::NAN, 0.0]; 4]).is_err());
    }

    #[test]
    fn config_validation() {
        let ok = EvalConfig::default();
        ok.validate().unwrap();
        let cases = [
            (
                EvalConfig {
                    folds: 1,
                    ..ok.clone()
                },
                "folds must be an integer in [2, 100]",
            ),
            (
                EvalConfig {
                    bootstrap: 99,
                    ..ok.clone()
                },
                "bootstrap must be 0",
            ),
            (
                EvalConfig {
                    bootstrap: 100_001,
                    ..ok.clone()
                },
                "bootstrap must be 0",
            ),
            (
                EvalConfig {
                    w_max: 0.5,
                    ..ok.clone()
                },
                "w-max must be at least 1",
            ),
            (
                EvalConfig {
                    w_max: f64::NAN,
                    ..ok.clone()
                },
                "w-max must be at least 1",
            ),
            (
                EvalConfig {
                    reward_range: Some([1.0, 0.0]),
                    ..ok.clone()
                },
                "lo < hi",
            ),
            (
                EvalConfig {
                    reward_model: LearnerSpec {
                        bits: 30,
                        ..LearnerSpec::default()
                    },
                    ..ok.clone()
                },
                "reward model: learner.bits",
            ),
        ];
        for (config, want) in cases {
            let e = config.validate().unwrap_err();
            assert!(e.contains(want), "{e}");
        }
        EvalConfig {
            w_max: f64::INFINITY,
            bootstrap: 0,
            ..ok
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn quantiles_interpolate_between_order_statistics() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(quantile(&v, 0.0), 1.0);
        assert_eq!(quantile(&v, 1.0), 5.0);
        assert_eq!(quantile(&v, 0.5), 3.0);
        assert_eq!(quantile(&v, 0.1), 1.4);
        assert_eq!(quantile(&[7.0], 0.975), 7.0);
        assert!(quantile(&[], 0.5).is_nan());
    }

    #[test]
    fn uniform_index_stays_in_range_and_is_even() {
        let mut rng = SplitMix64::new(1);
        let mut counts = [0usize; 7];
        for _ in 0..70_000 {
            counts[uniform_index(&mut rng, 7)] += 1;
        }
        assert!(
            counts.iter().all(|&c| (9_500..10_500).contains(&c)),
            "{counts:?}"
        );
        assert_eq!(uniform_index(&mut rng, 1), 0);
    }

    fn noisy_rows(n: usize) -> Vec<LoggedRow> {
        let mut rng = SplitMix64::new(42);
        (0..n)
            .map(|t| {
                let chosen = usize::from(rng.next_f64() < 0.3);
                let reward = rng.next_f64() + chosen as f64;
                row(
                    &format!("b{t}"),
                    json!({}),
                    &[0.7, 0.3],
                    chosen,
                    Some(reward),
                )
            })
            .collect()
    }

    #[test]
    fn bootstrap_is_reproducible_from_the_seed() {
        let data = EvalData::new(noisy_rows(500), 5, None).unwrap();
        let rewards = RewardModel::from_predictions(&data, vec![vec![0.5, 1.5]; 500]).unwrap();
        let with_seed = |seed| {
            let config = EvalConfig {
                bootstrap: 200,
                seed,
                ..EvalConfig::default()
            };
            evaluate(&data, &rewards, &Constant::new("a1"), &config).unwrap()
        };
        let a = with_seed(7);
        let b = with_seed(7);
        let c = with_seed(8);
        // As JSON: DM's absent interval is NaN, which never equals itself.
        let json = |e: &Evaluation| serde_json::to_value(e).unwrap();
        assert_eq!(json(&a), json(&b), "same seed, identical report");
        // The lift intervals come from the same resamples: the logged
        // policy's IPS lift is 0 in every resample, so its interval is [0, 0].
        let logged = evaluate(
            &data,
            &rewards,
            &Logged,
            &EvalConfig {
                bootstrap: 200,
                ..EvalConfig::default()
            },
        )
        .unwrap();
        let zero = logged.lift.ips;
        assert_eq!(
            (zero.mean, zero.lower, zero.upper, zero.se),
            (0.0, 0.0, 0.0, 0.0)
        );
        assert!(logged.logged.upper - logged.logged.lower > 0.01);
        assert_eq!(a.settings.interval, Interval::BootstrapPercentile);
        assert_eq!(a.estimators.ips.estimate, c.estimators.ips.estimate);
        assert_ne!(a.estimators.ips.lower, c.estimators.ips.lower);
        assert_ne!(a.logged.upper, c.logged.upper);
        // The percentile interval of a mean of 500 rows is close to the
        // normal one (CLT); within 15% of its half-width here.
        for est in [a.estimators.ips, a.estimators.dr, a.estimators.snips] {
            let half = est.normal_upper - est.estimate;
            assert!(
                (est.lower - est.normal_lower).abs() < 0.15 * half,
                "{est:?}"
            );
            assert!(
                (est.upper - est.normal_upper).abs() < 0.15 * half,
                "{est:?}"
            );
            assert!(est.lower < est.estimate && est.estimate < est.upper);
        }
    }

    #[test]
    fn fitted_reward_model_learns_the_action_means() {
        let data = EvalData::new(noisy_rows(3000), 5, None).unwrap();
        let rewards = RewardModel::fit(&data, &LearnerSpec::default()).unwrap();
        // Rewards: uniform(0, 1) for a0 and 1 + uniform(0, 1) for a1.
        let (mut a0, mut a1) = (0.0, 0.0);
        for i in 0..data.len() {
            a0 += rewards.predictions(i)[0];
            a1 += rewards.predictions(i)[1];
        }
        let (a0, a1) = (a0 / 3000.0, a1 / 3000.0);
        assert!(
            (a0 - 0.5).abs() < 0.05 && (a1 - 1.5).abs() < 0.05,
            "{a0} {a1}"
        );
    }
}
