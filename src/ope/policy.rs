//! Target policies: the policies whose value OPE estimates.
//!
//! A [`TargetPolicy`] gives, for each evaluation row, a PMF over that row's
//! eligible actions (aligned with `eligible`). It may also be all zeros,
//! meaning the policy has no action in that row (a constant action that is
//! not eligible there); such a row contributes zero importance weight and
//! is counted in the report.
//!
//! | Policy          | CLI                | PMF in row i                                  |
//! |-----------------|--------------------|-----------------------------------------------|
//! | [`Logged`]      | `logged`           | the row's own `pmf` (the incumbent's value)   |
//! | [`Constant`]    | `constant:<id>`    | all mass on action `id`                        |
//! | [`Greedy`]      | `greedy`           | argmax of a cross-fitted model, default spec   |
//! | [`Greedy`]      | `spec:<spec.json>` | the same with a candidate spec's settings      |
//! | [`TargetColumn`]| `target-column`    | the row's `targetPmf`                          |
//!
//! [`Greedy`] answers "what would the learned policy have done": for each
//! fold it trains the decision core's [`LinearModel`] exactly as the engine
//! does (one online pass with the engine's `phi(x, a)` features, the spec's
//! hash bits, learning rate and importance weighting, rewards normalized by
//! the evaluation's reward scale) on the other folds' logged rewards, and
//! picks the argmax of
//! the clamped predictions, ties to the first eligible action. Its estimate
//! is therefore the value of the policies learned on (K - 1) / K of the
//! logs; the intervals treat those fitted policies as fixed. A candidate
//! spec contributes its `learner` settings; its `reward.range`,
//! `exploration` and declared `actions` are not used (rows keep their
//! logged action features).
//!
//! [`LinearModel`]: crate::decision::LinearModel

use std::path::Path;

use crate::decision::DecisionSpec;

use super::data::{ENGINE_PASSES, EvalData, ModelSpec, cross_fit};

/// A policy to evaluate. See the module documentation.
pub trait TargetPolicy {
    /// Short name for reports, such as `constant:small`.
    fn label(&self) -> String;

    /// Target probabilities over `data.row(i).eligible`, in the same order.
    /// Entries are in `[0, 1]` and sum to 1, or are all 0 when the policy has
    /// no action in this row. A policy learned from the logs must answer
    /// row `i` without having learned from row `i`'s fold.
    fn pmf(&self, data: &EvalData, i: usize) -> Vec<f64>;
}

/// The logging policy itself: each row's own PMF. Its IPS estimate equals
/// the on-policy mean reward exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Logged;

impl TargetPolicy for Logged {
    fn label(&self) -> String {
        "logged".into()
    }

    fn pmf(&self, data: &EvalData, i: usize) -> Vec<f64> {
        data.row(i).pmf.clone()
    }
}

/// Always play the action with this id. In rows where it is not eligible
/// the policy has no action: the PMF is all zeros, the row contributes zero
/// importance weight, DM, IPS and DR count it as reward 0, and SNIPS leaves
/// it out. The report counts such rows as `targetIneligible`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constant {
    id: String,
}

impl Constant {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Position of the action within row `i`'s eligible list, if eligible.
    fn position(&self, data: &EvalData, i: usize) -> Option<usize> {
        let row = data.row(i);
        row.eligible
            .iter()
            .position(|&index| row.actions[index].id == self.id)
    }
}

impl TargetPolicy for Constant {
    fn label(&self) -> String {
        format!("constant:{}", self.id)
    }

    fn pmf(&self, data: &EvalData, i: usize) -> Vec<f64> {
        let mut pmf = vec![0.0; data.row(i).eligible.len()];
        if let Some(position) = self.position(data, i) {
            pmf[position] = 1.0;
        }
        pmf
    }
}

/// Each row's `targetPmf` column, for example a policy scored outside
/// Syntra (Open Bandit Pipeline) whose value is to be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetColumn;

impl TargetColumn {
    /// Fails unless every row carries a `targetPmf`.
    pub fn new(data: &EvalData) -> Result<Self, String> {
        match data.rows().iter().find(|row| row.target_pmf.is_none()) {
            Some(row) => Err(format!(
                "the target-column policy needs targetPmf on every row; decision {:?} has none",
                row.decision_id
            )),
            None => Ok(Self),
        }
    }
}

impl TargetPolicy for TargetColumn {
    fn label(&self) -> String {
        "target-column".into()
    }

    fn pmf(&self, data: &EvalData, i: usize) -> Vec<f64> {
        let row = data.row(i);
        // `new` checked that the column is present; a missing entry would
        // read as "no action" rather than panic.
        row.target_pmf
            .clone()
            .unwrap_or_else(|| vec![0.0; row.eligible.len()])
    }
}

/// The greedy policy of a model learned from the logs, cross-fitted: row
/// `i` gets the argmax of the model trained on the folds other than its
/// own. Fitted for one [`EvalData`]; ask it only about that data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Greedy {
    label: String,
    /// Position within each row's `eligible` list of the chosen action.
    choices: Vec<usize>,
}

impl Greedy {
    /// Fit one model per fold with `spec`'s learner settings (see the
    /// module documentation) and record each row's out-of-fold argmax.
    pub fn fit(
        data: &EvalData,
        spec: &DecisionSpec,
        label: impl Into<String>,
    ) -> Result<Self, String> {
        spec.validate()?;
        let model = ModelSpec {
            learner: spec.learner.clone(),
            with_context: false,
            passes: ENGINE_PASSES,
        };
        let mut choices = vec![0; data.len()];
        let mut scratch = Vec::new();
        cross_fit(data, &model, |i, fitted| {
            choices[i] = fitted.best(i, &mut scratch)?;
            Ok(())
        })?;
        Ok(Self {
            label: label.into(),
            choices,
        })
    }

    /// The leaky variant for tests: one model trained on every row picks
    /// the action in every row, including the rows it learned from.
    #[cfg(test)]
    pub(crate) fn fit_in_sample(data: &EvalData, spec: &DecisionSpec) -> Result<Self, String> {
        let model = ModelSpec {
            learner: spec.learner.clone(),
            with_context: false,
            passes: ENGINE_PASSES,
        };
        let mut choices = vec![0; data.len()];
        let mut scratch = Vec::new();
        super::data::fit_in_sample(data, &model, |i, fitted| {
            choices[i] = fitted.best(i, &mut scratch)?;
            Ok(())
        })?;
        Ok(Self {
            label: "greedy-in-sample".into(),
            choices,
        })
    }

    /// Position within row `i`'s `eligible` list of the action this policy
    /// plays there.
    pub fn choice(&self, i: usize) -> usize {
        self.choices[i]
    }
}

impl TargetPolicy for Greedy {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn pmf(&self, data: &EvalData, i: usize) -> Vec<f64> {
        assert_eq!(
            self.choices.len(),
            data.len(),
            "a Greedy policy answers only for the data it was fitted on"
        );
        let mut pmf = vec![0.0; data.row(i).eligible.len()];
        pmf[self.choices[i]] = 1.0;
        pmf
    }
}

/// A target policy as named on the command line, before it is fitted.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyChoice {
    Logged,
    Constant(String),
    /// [`Greedy`] with the default spec's learner settings.
    Greedy,
    /// [`Greedy`] with a candidate spec's learner settings.
    SpecGreedy {
        label: String,
        spec: DecisionSpec,
    },
    TargetColumn,
}

impl PolicyChoice {
    /// Parse `logged`, `constant:<id>`, `greedy`, `spec:<spec.json>` (the
    /// file is read and validated now) or `target-column`.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "logged" => return Ok(Self::Logged),
            "greedy" => return Ok(Self::Greedy),
            "target-column" => return Ok(Self::TargetColumn),
            _ => {}
        }
        if let Some(id) = text.strip_prefix("constant:") {
            if id.is_empty() {
                return Err("constant:<id> needs an action id".into());
            }
            return Ok(Self::Constant(id.to_string()));
        }
        if let Some(path) = text.strip_prefix("spec:") {
            if path.is_empty() {
                return Err("spec:<spec.json> needs a file path".into());
            }
            let spec = read_spec(Path::new(path))?;
            return Ok(Self::SpecGreedy {
                label: text.to_string(),
                spec,
            });
        }
        Err(format!(
            "unknown policy {text:?}; expected logged, constant:<id>, greedy, \
             spec:<spec.json> or target-column"
        ))
    }

    /// Build (and for `greedy` and `spec:`, fit) the policy on `data`.
    pub fn build(&self, data: &EvalData) -> Result<Box<dyn TargetPolicy>, String> {
        Ok(match self {
            Self::Logged => Box::new(Logged),
            Self::Constant(id) => {
                let policy = Constant::new(id.clone());
                if (0..data.len()).all(|i| policy.position(data, i).is_none()) {
                    return Err(format!(
                        "constant:{id}: action {id:?} is not eligible in any row with a reward"
                    ));
                }
                Box::new(policy)
            }
            Self::Greedy => Box::new(Greedy::fit(data, &DecisionSpec::default(), "greedy")?),
            Self::SpecGreedy { label, spec } => Box::new(Greedy::fit(data, spec, label.clone())?),
            Self::TargetColumn => Box::new(TargetColumn::new(data)?),
        })
    }
}

/// Read a candidate decision spec (JSON, strict) from a file.
fn read_spec(path: &Path) -> Result<DecisionSpec, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read spec {}: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("spec {} is not valid JSON: {e}", path.display()))?;
    DecisionSpec::from_json(&value).map_err(|e| format!("spec {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::SplitMix64;
    use crate::decision::spec::LearnerSpec;
    use crate::ope::data::testing::{four_rows, row};
    use crate::ope::estimators::{EvalConfig, RewardModel, evaluate};
    use crate::ope::row::LoggedRow;
    use serde_json::json;

    #[test]
    fn logged_constant_and_target_column_pmfs() {
        let mut rows = four_rows();
        rows[1].eligible = vec![1];
        rows[1].pmf = vec![1.0];
        rows[1].probability = 1.0;
        for (row, target) in rows
            .iter_mut()
            .zip([[0.3, 0.7], [1.0, 0.0], [0.5, 0.5], [0.0, 1.0]])
        {
            let len = row.eligible.len();
            row.target_pmf = Some(target[..len].to_vec());
        }
        let data = EvalData::new(rows, 2, None).unwrap();
        assert_eq!(Logged.pmf(&data, 0), vec![0.5, 0.5]);
        assert_eq!(Logged.label(), "logged");
        let c = Constant::new("a0");
        assert_eq!(c.label(), "constant:a0");
        assert_eq!(c.pmf(&data, 0), vec![1.0, 0.0]);
        assert_eq!(c.pmf(&data, 1), vec![0.0], "a0 is not eligible in row 1");
        assert_eq!(Constant::new("a1").pmf(&data, 1), vec![1.0]);
        let t = TargetColumn::new(&data).unwrap();
        assert_eq!(t.pmf(&data, 0), vec![0.3, 0.7]);
        assert_eq!(t.pmf(&data, 1), vec![1.0]);
        assert_eq!(t.label(), "target-column");

        let data = EvalData::new(four_rows(), 2, None).unwrap();
        assert!(
            TargetColumn::new(&data)
                .unwrap_err()
                .contains("needs targetPmf on every row; decision \"r1\" has none")
        );
        assert!(
            PolicyChoice::Constant("zz".into())
                .build(&data)
                .err()
                .unwrap()
                .contains("action \"zz\" is not eligible in any row")
        );
    }

    #[test]
    fn parses_policy_names() {
        assert_eq!(PolicyChoice::parse("logged").unwrap(), PolicyChoice::Logged);
        assert_eq!(PolicyChoice::parse("greedy").unwrap(), PolicyChoice::Greedy);
        assert_eq!(
            PolicyChoice::parse("target-column").unwrap(),
            PolicyChoice::TargetColumn
        );
        assert_eq!(
            PolicyChoice::parse("constant:a:b").unwrap(),
            PolicyChoice::Constant("a:b".into())
        );
        assert!(PolicyChoice::parse("constant:").is_err());
        assert!(PolicyChoice::parse("spec:").is_err());
        assert!(
            PolicyChoice::parse("spec:no/such/spec.json")
                .unwrap_err()
                .starts_with("cannot read spec no/such/spec.json")
        );
        assert!(
            PolicyChoice::parse("Greedy")
                .unwrap_err()
                .contains("unknown policy \"Greedy\"")
        );
    }

    /// Two segments; in segment A action a0 pays 0.8 and a1 0.2, in B the
    /// reverse. Uniform logging, Bernoulli rewards.
    fn two_segment_rows(n: usize, seed: u64) -> Vec<LoggedRow> {
        let mut rng = SplitMix64::new(seed);
        (0..n)
            .map(|t| {
                let segment = if rng.next_f64() < 0.5 { "A" } else { "B" };
                let chosen = usize::from(rng.next_f64() < 0.5);
                let good = (segment == "A") == (chosen == 0);
                let reward = f64::from(u8::from(rng.next_f64() < if good { 0.8 } else { 0.2 }));
                row(
                    &format!("s{seed}-{t}"),
                    json!({ "segment": segment }),
                    &[0.5, 0.5],
                    chosen,
                    Some(reward),
                )
            })
            .collect()
    }

    #[test]
    fn greedy_learns_the_better_action_per_segment() {
        let data = EvalData::new(two_segment_rows(2000, 3), 5, None).unwrap();
        let greedy = Greedy::fit(&data, &DecisionSpec::default(), "greedy").unwrap();
        let mut right = 0;
        for i in 0..data.len() {
            let best = if data.row(i).context["segment"] == "A" {
                0
            } else {
                1
            };
            if greedy.choice(i) == best {
                right += 1;
            }
            let pmf = greedy.pmf(&data, i);
            assert_eq!(pmf.iter().sum::<f64>(), 1.0);
            assert_eq!(pmf[greedy.choice(i)], 1.0);
        }
        assert!(right > 1980, "{right} of 2000 rows got the better action");
        // A spec with other learner settings is a different model but
        // learns the same easy rule.
        let spec = DecisionSpec {
            learner: LearnerSpec {
                bits: 12,
                learning_rate: 0.1,
                ..LearnerSpec::default()
            },
            ..DecisionSpec::default()
        };
        let other = Greedy::fit(&data, &spec, "spec:x").unwrap();
        assert_eq!(other.label(), "spec:x");
        let agree = (0..data.len())
            .filter(|&i| other.choice(i) == greedy.choice(i))
            .count();
        assert!(agree > 1950, "{agree}");
    }

    /// The leak test. Rewards are pure noise: every action pays 1 with
    /// probability 0.5 for every user, so every policy's true value is
    /// exactly 0.5. Users are many and rows per user few, so a model can
    /// memorize individual rows. A greedy policy fitted in-sample picks, in
    /// each row, the action that row itself rewarded, and its IPS and DR
    /// estimates overstate the value; the cross-fitted greedy policy never
    /// scores a row with a model that saw it, and its estimates are right.
    #[test]
    fn cross_fitting_does_not_leak_but_in_sample_fitting_does() {
        let seeds = 12;
        let n = 1200;
        let config = EvalConfig {
            bootstrap: 0,
            ..EvalConfig::default()
        };
        let spec = DecisionSpec::default();
        let (mut cross, mut leaky) = (Vec::new(), Vec::new());
        for seed in 0..seeds {
            let mut rng = SplitMix64::new(1000 + seed);
            let rows: Vec<LoggedRow> = (0..n)
                .map(|t| {
                    let user = rng.next_u64() % 300;
                    let chosen = (rng.next_u64() % 3) as usize;
                    let reward = f64::from(u8::from(rng.next_f64() < 0.5));
                    row(
                        &format!("n{seed}-{t}"),
                        json!({ "user": format!("u{user}") }),
                        &[1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0],
                        chosen,
                        Some(reward),
                    )
                })
                .collect();
            let data = EvalData::new(rows, 5, Some([0.0, 1.0])).unwrap();
            let rewards = RewardModel::fit(&data, &LearnerSpec::default()).unwrap();
            let fitted = Greedy::fit(&data, &spec, "greedy").unwrap();
            let in_sample = Greedy::fit_in_sample(&data, &spec).unwrap();
            let a = evaluate(&data, &rewards, &fitted, &config).unwrap();
            let b = evaluate(&data, &rewards, &in_sample, &config).unwrap();
            cross.push((a.estimators.ips.estimate, a.estimators.dr.estimate));
            leaky.push((b.estimators.ips.estimate, b.estimators.dr.estimate));
        }
        let summary = |values: &[(f64, f64)], pick: fn(&(f64, f64)) -> f64| {
            let xs: Vec<f64> = values.iter().map(pick).collect();
            let mean = xs.iter().sum::<f64>() / xs.len() as f64;
            let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
            (mean, (var / xs.len() as f64).sqrt())
        };
        for (name, pick) in [
            ("IPS", (|p: &(f64, f64)| p.0) as fn(&(f64, f64)) -> f64),
            ("DR", |p| p.1),
        ] {
            let (c, c_se) = summary(&cross, pick);
            let (l, l_se) = summary(&leaky, pick);
            println!(
                "{name}: cross-fitted {c:.4} +- {c_se:.4}, in-sample {l:.4} +- {l_se:.4}, truth 0.5"
            );
            assert!(
                (c - 0.5).abs() < 3.0 * c_se.max(0.005),
                "{name} cross-fitted {c}"
            );
            assert!(
                l - 0.5 > 0.05 && l - 0.5 > 8.0 * l_se,
                "{name} in-sample {l}"
            );
        }
    }
}
