//! Off-policy evaluation (OPE): how would a target policy have done on
//! traffic the logging policy actually served?
//!
//! Every Syntra decision records the probability mass function (PMF) its
//! action was sampled from, and every eligible action keeps a positive
//! probability, so the logs support unbiased importance-weighted estimates
//! of any other policy's value. This module turns logged rows into those
//! estimates, with confidence intervals and promotion gates:
//!
//! 1. [`row`]: the logged-row format and its strict JSONL loader.
//! 2. [`data`]: rows that have a reward, featurized once, split into
//!    cross-fitting folds by a hash of the decision id, plus the reward
//!    scale used to train models.
//! 3. [`policy`]: target policies: `logged`, `constant:<id>`, `greedy` and
//!    `spec:<file>` (cross-fitted argmax of a learned reward model), and a
//!    per-row target PMF column.
//! 4. [`estimators`]: DM, IPS, SNIPS and doubly robust (DR) estimates with a
//!    cross-fitted reward model, weight clipping, diagnostics, and bootstrap
//!    and normal-approximation intervals.
//! 5. [`gates`]: promotion checks such as `dr.lower >= logged.mean + 0.01`.
//! 6. [`report`]: the JSON and Markdown report with a verdict.
//! 7. [`cli`]: `syntra evaluate`.
//!
//! All estimates are in raw reward units.

pub mod cli;
pub mod data;
pub mod estimators;
pub mod gates;
pub mod policy;
pub mod report;
pub mod row;

pub use data::{EvalData, RangeSource, RewardScale, fold_of};
pub use estimators::{
    DataSummary, Diagnostics, Estimate, Estimators, EvalConfig, Evaluation, LoggedValue,
    RewardModel, Settings, evaluate,
};
pub use gates::{Gate, GateResult, Metric, load_gates, parse_gates};
pub use policy::{Constant, Greedy, Logged, PolicyChoice, TargetColumn, TargetPolicy};
pub use report::Report;
pub use row::{LoggedRow, load_jsonl, read_jsonl};

/// The whole pipeline on in-memory rows: build the evaluation data, fit
/// the cross-fitted reward model, build the target policy, estimate its
/// value and check `gates`.
pub fn run(
    rows: Vec<LoggedRow>,
    policy: &PolicyChoice,
    config: &EvalConfig,
    gates: &[Gate],
) -> Result<Report, String> {
    config.validate()?;
    let data = EvalData::new(rows, config.folds, config.reward_range)?;
    let rewards = RewardModel::fit(&data, &config.reward_model)?;
    let target = policy.build(&data)?;
    let evaluation = evaluate(&data, &rewards, target.as_ref(), config)?;
    Ok(Report::new(evaluation, gates))
}
