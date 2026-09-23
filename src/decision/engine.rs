//! The decision engine for one capsule: spec, featurizer and learner.
//!
//! [`Engine::decide`] builds the eligible action set, predicts a normalized
//! reward for each eligible action, turns the predictions into a PMF by the
//! spec's mode, and samples one action with a seeded [`SplitMix64`].
//! [`Engine::learn`] applies one observed reward to the learner.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

use super::explore::{baseline_explore, learner_pmf, sample};
use super::features::{Feature, FeatureError, Featurizer};
use super::learner::LinearModel;
use super::rng::SplitMix64;
use super::spec::{ActionSpec, DecisionSpec, Importance, Mode, featurize_actions};

/// Maximum raw feature-vector terms summed over all eligible actions of one
/// decision; bounds the CPU a single request can use.
pub const MAX_DECIDE_TERMS: usize = 1 << 22;

/// What a decide call needs besides the engine's spec.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecideInput {
    /// Request context: a JSON object, or null for none.
    pub context: Value,
    /// Derived features from the feature program: a JSON object or null.
    pub derived: Value,
    /// Per-request actions. `None` uses the spec's actions.
    pub actions: Option<Vec<ActionSpec>>,
    /// Action ids removed from the eligible set.
    pub excluded: Vec<String>,
    /// If set, only these action ids are eligible (before exclusions).
    pub eligible: Option<Vec<String>>,
    /// The caller's incumbent action; required in `baselineExplore` mode.
    pub baseline: Option<String>,
}

/// A sampled decision with everything needed to log it, replay it and
/// evaluate other policies against it.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// The full action list the decision was made over (the spec's or the
    /// request's), in order.
    pub actions: Vec<ActionSpec>,
    /// Indices into `actions` of the eligible actions, ascending.
    pub eligible: Vec<usize>,
    /// Probability of each eligible action, aligned with `eligible`.
    pub pmf: Vec<f64>,
    /// Predicted normalized reward of each eligible action, clamped to
    /// `[0, 1]`, aligned with `eligible`. These are the values the PMF was
    /// computed from.
    pub predictions: Vec<f64>,
    /// Index into `actions` of the chosen action.
    pub chosen: usize,
    /// Probability with which the chosen action was sampled.
    pub probability: f64,
    /// Seed of the sampling RNG; replaying it reproduces the choice.
    pub seed: u64,
    /// Number of learner updates applied when the decision was made.
    pub model_version: u64,
    pub mode: Mode,
}

impl Decision {
    /// The chosen action.
    pub fn chosen_action(&self) -> &ActionSpec {
        &self.actions[self.chosen]
    }

    /// Probability the decision gave to `actions[index]`; 0 if ineligible.
    pub fn probability_of(&self, index: usize) -> f64 {
        self.eligible
            .iter()
            .position(|&i| i == index)
            .map_or(0.0, |k| self.pmf[k])
    }

    /// Eligible actions as `(index, probability)`, most probable first; ties
    /// keep action order.
    pub fn ranking(&self) -> Vec<(usize, f64)> {
        let mut ranking: Vec<(usize, f64)> = self
            .eligible
            .iter()
            .copied()
            .zip(self.pmf.iter().copied())
            .collect();
        // Stable sort, and `eligible` is ascending.
        ranking.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranking
    }
}

/// Why a decide request was rejected. All variants are client errors.
#[derive(Debug, Clone, PartialEq)]
pub enum DecideError {
    /// The request's `actions` list is invalid.
    InvalidActions(String),
    /// Neither the spec nor the request supplies any action.
    NoActions,
    /// An excluded, eligible or baseline id names no action.
    UnknownAction { field: &'static str, id: String },
    /// Exclusions and the eligible filter left nothing to choose from.
    NoEligibleActions,
    /// `baselineExplore` mode needs a baseline action.
    BaselineRequired,
    /// The baseline action was excluded or filtered out.
    BaselineNotEligible(String),
    /// The context, derived or action features could not be featurized.
    Features(FeatureError),
}

impl fmt::Display for DecideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecideError::InvalidActions(message) => f.write_str(message),
            DecideError::NoActions => {
                f.write_str("no actions: the spec declares none and the request supplied none")
            }
            DecideError::UnknownAction { field, id } => {
                write!(f, "{field} names unknown action {id:?}")
            }
            DecideError::NoEligibleActions => {
                f.write_str("no eligible actions remain after the eligible filter and exclusions")
            }
            DecideError::BaselineRequired => {
                f.write_str("baselineAction is required in baselineExplore mode")
            }
            DecideError::BaselineNotEligible(id) => write!(
                f,
                "baselineAction {id:?} is not eligible (excluded or filtered out)"
            ),
            DecideError::Features(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for DecideError {}

impl From<FeatureError> for DecideError {
    fn from(e: FeatureError) -> Self {
        DecideError::Features(e)
    }
}

/// One capsule's decision engine.
#[derive(Debug, Clone)]
pub struct Engine {
    spec: DecisionSpec,
    featurizer: Featurizer,
    model: LinearModel,
    /// Features of `spec.actions`, computed once.
    spec_action_features: Vec<Vec<Feature>>,
}

impl Engine {
    /// A fresh engine with an untrained model. Validates the spec.
    pub fn new(spec: DecisionSpec) -> Result<Self, String> {
        spec.validate()?;
        let model = LinearModel::new(spec.learner.bits, spec.learner.learning_rate)?;
        Self::assemble(spec, model)
    }

    /// An engine whose model is loaded from [`Self::snapshot`] bytes. The
    /// snapshot must have been trained with the same `learner.bits`
    /// (otherwise the model has to be rebuilt from the event log); the
    /// spec's learning rate replaces the stored one.
    pub fn restore(spec: DecisionSpec, snapshot: &[u8]) -> Result<Self, String> {
        spec.validate()?;
        let mut model = LinearModel::from_bytes(snapshot)?;
        check_bits(&spec, &model)?;
        model.set_learning_rate(spec.learner.learning_rate)?;
        Self::assemble(spec, model)
    }

    /// Replace the spec (for example to switch mode) and keep the model.
    /// Fails, leaving the engine unchanged, if the spec is invalid or
    /// changes `learner.bits`.
    pub fn set_spec(&mut self, spec: DecisionSpec) -> Result<(), String> {
        spec.validate()?;
        check_bits(&spec, &self.model)?;
        let features = featurize_actions(&spec.actions, "actions")?;
        self.model.set_learning_rate(spec.learner.learning_rate)?;
        self.spec = spec;
        self.spec_action_features = features;
        Ok(())
    }

    fn assemble(spec: DecisionSpec, model: LinearModel) -> Result<Self, String> {
        let featurizer = Featurizer::new(spec.learner.bits)?;
        let spec_action_features = featurize_actions(&spec.actions, "actions")?;
        Ok(Self {
            spec,
            featurizer,
            model,
            spec_action_features,
        })
    }

    pub fn spec(&self) -> &DecisionSpec {
        &self.spec
    }

    pub fn model(&self) -> &LinearModel {
        &self.model
    }

    pub fn featurizer(&self) -> &Featurizer {
        &self.featurizer
    }

    /// Number of learner updates applied; recorded with every decision.
    pub fn model_version(&self) -> u64 {
        self.model.n_updates()
    }

    /// The model state as a versioned, checksummed blob.
    pub fn snapshot(&self) -> Vec<u8> {
        self.model.to_bytes()
    }

    /// Choose an action. The draw is fully determined by the engine state,
    /// the input and `seed`.
    pub fn decide(&self, input: &DecideInput, seed: u64) -> Result<Decision, DecideError> {
        let request_features;
        let (actions, action_features): (&[ActionSpec], &[Vec<Feature>]) = match &input.actions {
            Some(actions) => {
                request_features =
                    featurize_actions(actions, "actions").map_err(DecideError::InvalidActions)?;
                (actions, &request_features)
            }
            None => (&self.spec.actions, &self.spec_action_features),
        };
        if actions.is_empty() {
            return Err(DecideError::NoActions);
        }

        let eligible = eligible_set(actions, input)?;
        let baseline_position = match self.spec.mode {
            Mode::BaselineExplore => {
                let id = input
                    .baseline
                    .as_deref()
                    .ok_or(DecideError::BaselineRequired)?;
                let position = eligible.iter().position(|&i| actions[i].id == id);
                Some(position.ok_or_else(|| DecideError::BaselineNotEligible(id.to_string()))?)
            }
            Mode::Learner | Mode::Frozen => None,
        };

        let context = self.featurizer.context(&input.context, &input.derived)?;
        let terms = eligible
            .iter()
            .map(|&i| Featurizer::phi_terms(&context, action_features[i].len()))
            .fold(0usize, usize::saturating_add);
        if terms > MAX_DECIDE_TERMS {
            return Err(FeatureError::TooManyTerms {
                terms,
                max: MAX_DECIDE_TERMS,
            }
            .into());
        }
        let mut phi = Vec::new();
        let mut predictions = Vec::with_capacity(eligible.len());
        for &i in &eligible {
            self.featurizer
                .phi(&context, &action_features[i], &mut phi)?;
            // True expected rewards lie in [0, 1], so clamping can only move
            // an estimate closer to the truth.
            predictions.push(self.model.predict(&phi).clamp(0.0, 1.0));
        }

        let pmf = match baseline_position {
            Some(b) => baseline_explore(eligible.len(), b, self.spec.baseline_epsilon),
            None => learner_pmf(&self.spec.exploration, &predictions, self.model.n_updates()),
        };
        let pick = sample(&pmf, &mut SplitMix64::new(seed));
        Ok(Decision {
            actions: actions.to_vec(),
            chosen: eligible[pick],
            probability: pmf[pick],
            eligible,
            pmf,
            predictions,
            seed,
            model_version: self.model.n_updates(),
            mode: self.spec.mode,
        })
    }

    /// Learn from one reward for `action` taken in `context` (plus the
    /// decision's `derived` features) with logged probability
    /// `probability`.
    ///
    /// The raw reward is normalized into `[0, 1]` by `reward.range`
    /// (values outside the range are clamped). The update has importance
    /// weight 1, or `min(1 / probability, maxImportanceWeight)` when
    /// `learner.importance` is `mtr`.
    ///
    /// In `frozen` mode the reward and probability are still validated, but
    /// `Ok(())` is returned without featurizing or touching the model, so
    /// the model version does not move.
    pub fn learn(
        &mut self,
        context: &Value,
        derived: &Value,
        action: &ActionSpec,
        reward: f64,
        probability: f64,
    ) -> Result<(), String> {
        if !reward.is_finite() {
            return Err(format!("reward must be a finite number (got {reward})"));
        }
        if !(probability > 0.0 && probability <= 1.0) {
            return Err(format!("probability must be in (0, 1] (got {probability})"));
        }
        if self.spec.mode == Mode::Frozen {
            return Ok(());
        }
        let target = self.spec.reward.normalize(reward);
        let weight = match self.spec.learner.importance {
            Importance::Unweighted => 1.0,
            Importance::Mtr => (1.0 / probability).min(self.spec.learner.max_importance_weight),
        };
        let request = self
            .featurizer
            .context(context, derived)
            .map_err(|e| e.to_string())?;
        let features = self.featurizer.action(action).map_err(|e| e.to_string())?;
        let mut phi = Vec::new();
        self.featurizer
            .phi(&request, &features, &mut phi)
            .map_err(|e| e.to_string())?;
        self.model.update(&phi, target, weight)
    }
}

/// Eligible indices, ascending: the `eligible` filter (or every action),
/// minus exclusions. Unknown ids are errors.
fn eligible_set(actions: &[ActionSpec], input: &DecideInput) -> Result<Vec<usize>, DecideError> {
    let index: HashMap<&str, usize> = actions
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id.as_str(), i))
        .collect();
    let lookup = |field: &'static str, id: &str| {
        index
            .get(id)
            .copied()
            .ok_or_else(|| DecideError::UnknownAction {
                field,
                id: id.to_string(),
            })
    };
    let mut is_eligible = vec![input.eligible.is_none(); actions.len()];
    if let Some(ids) = &input.eligible {
        for id in ids {
            is_eligible[lookup("eligible", id)?] = true;
        }
    }
    for id in &input.excluded {
        is_eligible[lookup("excludedActions", id)?] = false;
    }
    if let Some(id) = &input.baseline {
        lookup("baselineAction", id)?;
    }
    let eligible: Vec<usize> = (0..actions.len()).filter(|&i| is_eligible[i]).collect();
    if eligible.is_empty() {
        return Err(DecideError::NoEligibleActions);
    }
    Ok(eligible)
}

fn check_bits(spec: &DecisionSpec, model: &LinearModel) -> Result<(), String> {
    if spec.learner.bits == model.bits() {
        Ok(())
    } else {
        Err(format!(
            "the model was trained with learner.bits = {} but the spec sets {}; \
             rebuild the model from the event log",
            model.bits(),
            spec.learner.bits
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(value: Value) -> DecisionSpec {
        DecisionSpec::from_json(&value).unwrap()
    }

    fn engine(value: Value) -> Engine {
        Engine::new(spec(value)).unwrap()
    }

    fn three_actions() -> Value {
        json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "learner": {"bits": 12}})
    }

    fn input(context: Value) -> DecideInput {
        DecideInput {
            context,
            ..DecideInput::default()
        }
    }

    #[test]
    fn untrained_engine_decides_uniformly() {
        let e = engine(three_actions());
        let d = e.decide(&input(json!({"x": 1})), 42).unwrap();
        assert_eq!(d.eligible, vec![0, 1, 2]);
        assert_eq!(d.predictions, vec![0.0; 3]);
        for p in &d.pmf {
            assert!((p - 1.0 / 3.0).abs() < 1e-15);
        }
        assert_eq!(
            d.probability,
            d.pmf[d.eligible.iter().position(|&i| i == d.chosen).unwrap()]
        );
        assert_eq!(d.model_version, 0);
        assert_eq!(d.seed, 42);
        assert_eq!(d.mode, Mode::Learner);
        assert_eq!(d.actions.len(), 3);
        // Same seed, same draw.
        assert_eq!(e.decide(&input(json!({"x": 1})), 42).unwrap(), d);
    }

    #[test]
    fn eligibility_errors() {
        let e = engine(three_actions());
        let mut req = input(Value::Null);
        req.excluded = vec!["zzz".into()];
        assert_eq!(
            e.decide(&req, 1).unwrap_err(),
            DecideError::UnknownAction {
                field: "excludedActions",
                id: "zzz".into()
            }
        );
        req.excluded = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(
            e.decide(&req, 1).unwrap_err(),
            DecideError::NoEligibleActions
        );
        req.excluded.clear();
        req.eligible = Some(vec!["b".into(), "nope".into()]);
        assert_eq!(
            e.decide(&req, 1).unwrap_err().to_string(),
            "eligible names unknown action \"nope\""
        );
        req.eligible = Some(vec!["c".into(), "b".into(), "c".into()]);
        req.excluded = vec!["b".into()];
        let d = e.decide(&req, 1).unwrap();
        assert_eq!(d.eligible, vec![2]);
        assert_eq!(d.pmf, vec![1.0]);
        assert_eq!(d.chosen, 2);
        req.baseline = Some("q".into());
        assert!(matches!(
            e.decide(&req, 1),
            Err(DecideError::UnknownAction {
                field: "baselineAction",
                ..
            })
        ));
        let empty = engine(json!({"learner": {"bits": 10}}));
        assert_eq!(
            empty.decide(&input(Value::Null), 1).unwrap_err(),
            DecideError::NoActions
        );
    }

    #[test]
    fn request_actions_are_validated_and_used() {
        let e = engine(three_actions());
        let mut req = input(Value::Null);
        req.actions = Some(vec![ActionSpec::new("x"), ActionSpec::new("x")]);
        let err = e.decide(&req, 1).unwrap_err();
        assert_eq!(
            err,
            DecideError::InvalidActions("actions[1].id \"x\" duplicates actions[0].id".into())
        );
        req.actions = Some(vec![]);
        assert_eq!(e.decide(&req, 1).unwrap_err(), DecideError::NoActions);
        req.actions = Some(vec![ActionSpec::new("x"), ActionSpec::new("y")]);
        req.excluded = vec!["a".into()];
        assert!(
            matches!(e.decide(&req, 1), Err(DecideError::UnknownAction { .. })),
            "spec ids do not apply"
        );
        req.excluded = vec!["x".into()];
        let d = e.decide(&req, 1).unwrap();
        assert_eq!(d.chosen_action().id, "y");
        assert_eq!(d.probability_of(0), 0.0);
        assert_eq!(d.probability_of(1), 1.0);
    }

    #[test]
    fn feature_errors_are_reported() {
        let e = engine(three_actions());
        let err = e.decide(&input(json!("text")), 1).unwrap_err();
        assert_eq!(
            err.to_string(),
            "context must be a JSON object or null, not a string"
        );
        let mut req = input(json!({}));
        req.derived = json!({"d": 1e40});
        assert!(matches!(
            e.decide(&req, 1),
            Err(DecideError::Features(FeatureError::NonFinite { .. }))
        ));
    }

    #[test]
    fn decide_work_is_bounded() {
        let mut map = serde_json::Map::new();
        for i in 0..4000 {
            map.insert(format!("f{i}"), json!(1));
        }
        let features = Value::Object(map);
        let actions: Vec<Value> = (0..3)
            .map(|i| json!({"id": format!("a{i}"), "features": features}))
            .collect();
        let e = engine(json!({"actions": actions, "learner": {"bits": 10}}));
        let err = e.decide(&input(features.clone()), 1).unwrap_err();
        assert!(
            matches!(
                err,
                DecideError::Features(FeatureError::TooManyTerms { .. })
            ),
            "{err}"
        );
    }

    #[test]
    fn baseline_explore_mode() {
        let e = engine(
            json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}, {"id": "d"}],
                              "mode": "baselineExplore", "baselineEpsilon": 0.2, "learner": {"bits": 10}}),
        );
        assert_eq!(
            e.decide(&input(Value::Null), 1).unwrap_err(),
            DecideError::BaselineRequired
        );
        let mut req = input(Value::Null);
        req.baseline = Some("c".into());
        let d = e.decide(&req, 3).unwrap();
        assert_eq!(d.mode, Mode::BaselineExplore);
        assert!((d.probability_of(2) - 0.85).abs() < 1e-12);
        for i in [0, 1, 3] {
            assert!((d.probability_of(i) - 0.05).abs() < 1e-12);
        }
        req.excluded = vec!["c".into()];
        assert_eq!(
            e.decide(&req, 3).unwrap_err(),
            DecideError::BaselineNotEligible("c".into())
        );
        // The baseline is chosen about 85% of the time.
        req.excluded.clear();
        let hits = (0..2000)
            .filter(|&s| e.decide(&req, s).unwrap().chosen == 2)
            .count();
        assert!((1600..1800).contains(&hits), "{hits}");
    }

    #[test]
    fn learning_updates_the_model_and_version() {
        let mut e = engine(three_actions());
        let a = e.spec().actions[1].clone();
        e.learn(&json!({"x": 1}), &Value::Null, &a, 1.0, 0.5)
            .unwrap();
        assert_eq!(e.model_version(), 1);
        let d = e.decide(&input(json!({"x": 1})), 7).unwrap();
        assert_eq!(d.model_version, 1);
        assert!(
            d.predictions[1] > 0.0 && d.predictions[0] == d.predictions[2],
            "{:?}",
            d.predictions
        );
    }

    #[test]
    fn learn_rejects_bad_inputs() {
        let mut e = engine(three_actions());
        let a = ActionSpec::new("a");
        assert!(
            e.learn(&Value::Null, &Value::Null, &a, f64::NAN, 0.5)
                .is_err()
        );
        assert!(e.learn(&Value::Null, &Value::Null, &a, 1.0, 0.0).is_err());
        assert!(e.learn(&Value::Null, &Value::Null, &a, 1.0, 1.5).is_err());
        assert!(
            e.learn(&Value::Null, &Value::Null, &a, 1.0, f64::NAN)
                .is_err()
        );
        assert!(
            e.learn(&json!([1]), &Value::Null, &a, 1.0, 0.5)
                .unwrap_err()
                .contains("context")
        );
        assert_eq!(e.model_version(), 0);
    }

    #[test]
    fn frozen_mode_does_not_learn() {
        let mut e = engine(
            json!({"actions": [{"id": "a"}, {"id": "b"}], "mode": "frozen", "learner": {"bits": 10}}),
        );
        let before = e.snapshot();
        e.learn(
            &json!({"x": 1}),
            &Value::Null,
            &ActionSpec::new("a"),
            1.0,
            0.5,
        )
        .unwrap();
        assert_eq!(e.model_version(), 0);
        assert_eq!(e.snapshot(), before);
        assert!(
            e.learn(
                &Value::Null,
                &Value::Null,
                &ActionSpec::new("a"),
                f64::INFINITY,
                0.5
            )
            .is_err()
        );
    }

    #[test]
    fn rewards_are_normalized_and_clamped() {
        // Range [-10, 10]: raw 10 -> 1.0, raw 30 -> clamped to 1.0.
        let spec_value = json!({"actions": [{"id": "a"}], "reward": {"range": [-10, 10]}, "learner": {"bits": 10}});
        let mut one = engine(spec_value.clone());
        let mut clamped = engine(spec_value);
        let a = ActionSpec::new("a");
        one.learn(&Value::Null, &Value::Null, &a, 10.0, 1.0)
            .unwrap();
        clamped
            .learn(&Value::Null, &Value::Null, &a, 30.0, 1.0)
            .unwrap();
        assert_eq!(one.snapshot(), clamped.snapshot());
    }

    #[test]
    fn mtr_weights_by_inverse_probability() {
        let base = json!({"actions": [{"id": "a"}], "learner": {"bits": 10, "importance": "mtr", "maxImportanceWeight": 4}});
        let mut e = engine(base);
        let a = ActionSpec::new("a");
        e.learn(&Value::Null, &Value::Null, &a, 1.0, 0.5).unwrap();
        assert_eq!(e.model().total_weight(), 2.0);
        e.learn(&Value::Null, &Value::Null, &a, 1.0, 0.01).unwrap();
        assert_eq!(
            e.model().total_weight(),
            6.0,
            "capped at maxImportanceWeight"
        );
        let mut plain = engine(three_actions());
        plain
            .learn(&Value::Null, &Value::Null, &a, 1.0, 0.01)
            .unwrap();
        assert_eq!(plain.model().total_weight(), 1.0);
    }

    #[test]
    fn snapshot_restore_and_spec_changes() {
        let mut e = engine(three_actions());
        for (i, id) in ["a", "b", "c", "b"].iter().enumerate() {
            e.learn(
                &json!({"i": i}),
                &Value::Null,
                &ActionSpec::new(*id),
                0.25 * i as f64,
                0.3,
            )
            .unwrap();
        }
        let restored = Engine::restore(e.spec().clone(), &e.snapshot()).unwrap();
        assert_eq!(restored.model(), e.model());
        let req = input(json!({"i": 2}));
        assert_eq!(
            restored.decide(&req, 9).unwrap(),
            e.decide(&req, 9).unwrap()
        );

        let mut wider = e.spec().clone();
        wider.learner.bits = 14;
        assert!(
            Engine::restore(wider.clone(), &e.snapshot())
                .unwrap_err()
                .contains("learner.bits = 12")
        );
        assert!(e.set_spec(wider).is_err());
        assert!(Engine::restore(e.spec().clone(), b"garbage").is_err());

        let frozen = e
            .spec()
            .merge_patch(&json!({"mode": "frozen", "learner": {"learningRate": 0.1}}))
            .unwrap();
        e.set_spec(frozen).unwrap();
        assert_eq!(e.spec().mode, Mode::Frozen);
        assert_eq!(e.model().learning_rate(), 0.1);
        assert_eq!(e.decide(&req, 9).unwrap().mode, Mode::Frozen);
    }

    #[test]
    fn ranking_orders_by_probability_then_action_order() {
        let d = Decision {
            actions: vec![],
            eligible: vec![0, 2, 3, 5],
            pmf: vec![0.2, 0.4, 0.2, 0.2],
            predictions: vec![0.0; 4],
            chosen: 2,
            probability: 0.4,
            seed: 0,
            model_version: 0,
            mode: Mode::Learner,
        };
        assert_eq!(d.ranking(), vec![(2, 0.4), (0, 0.2), (3, 0.2), (5, 0.2)]);
    }
}
