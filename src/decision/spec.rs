//! The decision spec: actions, reward range, exploration, learner settings
//! and serving mode for one capsule.
//!
//! JSON uses camelCase and rejects unknown fields at every level, so a typo
//! such as `{"exploration": {"gama": 5}}` is an error rather than a silently
//! ignored setting. Missing fields take their defaults. [`DecisionSpec::validate`]
//! checks every value's range; [`DecisionSpec::merge_patch`] applies an
//! RFC 7396 JSON merge patch and re-validates the result.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::features::{Feature, action_features};
use super::learner::{MAX_BITS, MIN_BITS};

/// Maximum number of actions in a spec or a request.
pub const MAX_ACTIONS: usize = 1024;
/// Maximum length of an action id, in characters.
pub const MAX_ACTION_ID_CHARS: usize = 256;

/// One action: a unique id plus optional features (a JSON object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub features: Map<String, Value>,
}

impl ActionSpec {
    /// An action with no features.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            features: Map::new(),
        }
    }
}

/// Raw reward range; rewards are normalized to `[0, 1]` by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct RewardSpec {
    /// `[lo, hi]` with `lo < hi`.
    pub range: [f64; 2],
}

impl Default for RewardSpec {
    fn default() -> Self {
        Self { range: [0.0, 1.0] }
    }
}

impl RewardSpec {
    /// Map a raw reward into `[0, 1]`: `(r - lo) / (hi - lo)`, clamped.
    /// The caller must pass a finite reward.
    pub fn normalize(&self, raw: f64) -> f64 {
        let [lo, hi] = self.range;
        ((raw - lo) / (hi - lo)).clamp(0.0, 1.0)
    }
}

/// Exploration algorithm used in `learner` and `frozen` modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExplorationKind {
    /// Inverse gap weighting (SquareCB).
    #[default]
    #[serde(rename = "squarecb")]
    SquareCb,
    #[serde(rename = "epsilonGreedy")]
    EpsilonGreedy,
}

/// Exploration settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ExplorationSpec {
    pub kind: ExplorationKind,
    /// SquareCB: `gamma = gammaScale * n^gammaExponent`.
    pub gamma_scale: f64,
    pub gamma_exponent: f64,
    /// Epsilon-greedy exploration rate.
    pub epsilon: f64,
    /// Share of probability spread uniformly over the eligible actions
    /// after the exploration algorithm, so each has at least `floor / K`.
    pub floor: f64,
}

impl Default for ExplorationSpec {
    fn default() -> Self {
        Self {
            kind: ExplorationKind::SquareCb,
            gamma_scale: 10.0,
            gamma_exponent: 0.5,
            epsilon: 0.05,
            floor: 0.05,
        }
    }
}

/// How rewards weight learner updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Importance {
    /// Every update has weight 1, as in the SquareCB analysis.
    #[default]
    #[serde(rename = "none")]
    Unweighted,
    /// Importance-weighted regression: weight `min(1 / p, maxImportanceWeight)`.
    #[serde(rename = "mtr")]
    Mtr,
}

/// Learner settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct LearnerSpec {
    /// Hash bits: the model has `2^bits` weight slots.
    pub bits: u32,
    pub learning_rate: f64,
    pub importance: Importance,
    pub max_importance_weight: f64,
}

impl Default for LearnerSpec {
    fn default() -> Self {
        Self {
            bits: 18,
            learning_rate: 0.5,
            importance: Importance::Unweighted,
            max_importance_weight: 100.0,
        }
    }
}

/// Serving mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// Serve the learned policy and keep learning.
    #[default]
    #[serde(rename = "learner")]
    Learner,
    /// Serve the caller's baseline action with probability
    /// `1 - baselineEpsilon`, otherwise uniform; the learner still trains.
    #[serde(rename = "baselineExplore")]
    BaselineExplore,
    /// Serve the learned policy without applying updates.
    #[serde(rename = "frozen")]
    Frozen,
}

/// How several rewards for one decision are treated (enforced by storage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RewardAggregation {
    /// Only the first reward counts; later ones are rejected.
    #[default]
    #[serde(rename = "first")]
    First,
    /// Every reward is applied.
    #[serde(rename = "sum")]
    Sum,
}

/// Complete configuration of one decision point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct DecisionSpec {
    /// Declared actions. May be empty when every request supplies actions.
    pub actions: Vec<ActionSpec>,
    pub reward: RewardSpec,
    pub exploration: ExplorationSpec,
    pub learner: LearnerSpec,
    pub mode: Mode,
    pub baseline_epsilon: f64,
    /// When set, the capsule's decisions are fully deterministic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Persist a model snapshot every this many updates.
    pub snapshot_every: u64,
    pub rewards: RewardAggregation,
}

impl Default for DecisionSpec {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            reward: RewardSpec::default(),
            exploration: ExplorationSpec::default(),
            learner: LearnerSpec::default(),
            mode: Mode::Learner,
            baseline_epsilon: 0.1,
            seed: None,
            snapshot_every: 1000,
            rewards: RewardAggregation::First,
        }
    }
}

impl DecisionSpec {
    /// Parse a spec from JSON, rejecting unknown fields, then validate it.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let spec = DecisionSpec::deserialize(value).map_err(|e| format!("invalid spec: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    /// The spec as JSON (camelCase, defaults included).
    pub fn to_json(&self) -> Value {
        // Every map key is a string and non-finite floats serialize as null,
        // so serde_json has no error to report for this type.
        serde_json::to_value(self).expect("a DecisionSpec always serializes to JSON")
    }

    /// Check every field's range. Returns the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        validate_actions(&self.actions, "actions")?;
        let [lo, hi] = self.reward.range;
        if !(lo.is_finite() && hi.is_finite() && lo < hi && (hi - lo).is_finite()) {
            return Err(format!(
                "reward.range must be two finite numbers [lo, hi] with lo < hi (got [{lo}, {hi}])"
            ));
        }
        let e = &self.exploration;
        check(
            "exploration.gammaScale",
            e.gamma_scale,
            Bound::Closed(0.0),
            Bound::Closed(1e6),
        )?;
        check(
            "exploration.gammaExponent",
            e.gamma_exponent,
            Bound::Closed(0.0),
            Bound::Closed(1.0),
        )?;
        check(
            "exploration.epsilon",
            e.epsilon,
            Bound::Closed(0.0),
            Bound::Closed(1.0),
        )?;
        // A zero floor would let actions get probability 0, which breaks
        // off-policy evaluation of the logs.
        check(
            "exploration.floor",
            e.floor,
            Bound::Open(0.0),
            Bound::Closed(1.0),
        )?;
        let l = &self.learner;
        if !(MIN_BITS..=MAX_BITS).contains(&l.bits) {
            return Err(format!(
                "learner.bits must be an integer in [{MIN_BITS}, {MAX_BITS}] (got {})",
                l.bits
            ));
        }
        check(
            "learner.learningRate",
            l.learning_rate,
            Bound::Open(0.0),
            Bound::Closed(10.0),
        )?;
        check(
            "learner.maxImportanceWeight",
            l.max_importance_weight,
            Bound::Closed(1.0),
            Bound::Closed(1e6),
        )?;
        check(
            "baselineEpsilon",
            self.baseline_epsilon,
            Bound::Open(0.0),
            Bound::Closed(1.0),
        )?;
        if !(1..=1_000_000_000).contains(&self.snapshot_every) {
            return Err(format!(
                "snapshotEvery must be an integer in [1, 1000000000] (got {})",
                self.snapshot_every
            ));
        }
        Ok(())
    }

    /// Apply an RFC 7396 JSON merge patch to this spec's JSON form, then
    /// parse strictly (unknown fields are errors) and validate. A `null`
    /// in the patch removes the field, restoring its default; arrays such
    /// as `actions` are replaced wholesale.
    pub fn merge_patch(&self, patch: &Value) -> Result<Self, String> {
        if !patch.is_object() {
            return Err("spec patch must be a JSON object".into());
        }
        let mut doc = self.to_json();
        apply_merge_patch(&mut doc, patch);
        Self::from_json(&doc)
    }
}

/// RFC 7396 JSON merge patch, applied in place.
pub fn apply_merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                apply_merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

/// Validate an action list (from a spec or a request): at most
/// [`MAX_ACTIONS`] entries, unique non-empty ids of at most
/// [`MAX_ACTION_ID_CHARS`] characters, and features that flatten cleanly.
/// `field` names the list in error messages.
pub fn validate_actions(actions: &[ActionSpec], field: &str) -> Result<(), String> {
    featurize_actions(actions, field).map(|_| ())
}

/// [`validate_actions`], returning each action's features on success.
pub(crate) fn featurize_actions(
    actions: &[ActionSpec],
    field: &str,
) -> Result<Vec<Vec<Feature>>, String> {
    if actions.len() > MAX_ACTIONS {
        return Err(format!(
            "{field} has {} entries; the limit is {MAX_ACTIONS}",
            actions.len()
        ));
    }
    let mut seen: HashMap<&str, usize> = HashMap::with_capacity(actions.len());
    let mut features = Vec::with_capacity(actions.len());
    for (i, action) in actions.iter().enumerate() {
        if action.id.is_empty() {
            return Err(format!("{field}[{i}].id must not be empty"));
        }
        if action.id.chars().count() > MAX_ACTION_ID_CHARS {
            return Err(format!(
                "{field}[{i}].id is longer than {MAX_ACTION_ID_CHARS} characters"
            ));
        }
        if let Some(first) = seen.insert(action.id.as_str(), i) {
            return Err(format!(
                "{field}[{i}].id {:?} duplicates {field}[{first}].id",
                action.id
            ));
        }
        features.push(
            action_features(action).map_err(|e| format!("{field}[{i}] ({:?}): {e}", action.id))?,
        );
    }
    Ok(features)
}

enum Bound {
    Open(f64),
    Closed(f64),
}

/// Range check that also rejects NaN.
fn check(name: &str, value: f64, lo: Bound, hi: Bound) -> Result<(), String> {
    let above = match lo {
        Bound::Open(b) => value > b,
        Bound::Closed(b) => value >= b,
    };
    let below = match hi {
        Bound::Open(b) => value < b,
        Bound::Closed(b) => value <= b,
    };
    if above && below {
        return Ok(());
    }
    let (open, lo) = match lo {
        Bound::Open(b) => ("(", b),
        Bound::Closed(b) => ("[", b),
    };
    let (close, hi) = match hi {
        Bound::Open(b) => (")", b),
        Bound::Closed(b) => ("]", b),
    };
    Err(format!(
        "{name} must be in {open}{lo}, {hi}{close} (got {value})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Result<DecisionSpec, String> {
        DecisionSpec::from_json(&value)
    }

    fn err(value: Value) -> String {
        parse(value).unwrap_err()
    }

    #[test]
    fn defaults() {
        let spec = parse(json!({})).unwrap();
        assert_eq!(spec, DecisionSpec::default());
        assert!(spec.actions.is_empty());
        assert_eq!(spec.reward.range, [0.0, 1.0]);
        assert_eq!(spec.exploration.kind, ExplorationKind::SquareCb);
        assert_eq!(spec.exploration.gamma_scale, 10.0);
        assert_eq!(spec.exploration.gamma_exponent, 0.5);
        assert_eq!(spec.exploration.floor, 0.05);
        assert_eq!(spec.learner.bits, 18);
        assert_eq!(spec.learner.learning_rate, 0.5);
        assert_eq!(spec.learner.importance, Importance::Unweighted);
        assert_eq!(spec.learner.max_importance_weight, 100.0);
        assert_eq!(spec.mode, Mode::Learner);
        assert_eq!(spec.baseline_epsilon, 0.1);
        assert_eq!(spec.seed, None);
        assert_eq!(spec.snapshot_every, 1000);
        assert_eq!(spec.rewards, RewardAggregation::First);
    }

    #[test]
    fn full_spec_round_trips() {
        let value = json!({
            "actions": [{"id": "small", "features": {"cost": 0.2}}, {"id": "large"}],
            "reward": {"range": [-1, 1]},
            "exploration": {"kind": "epsilonGreedy", "gammaScale": 5, "gammaExponent": 0.25,
                            "epsilon": 0.1, "floor": 0.02},
            "learner": {"bits": 20, "learningRate": 0.25, "importance": "mtr",
                        "maxImportanceWeight": 50},
            "mode": "baselineExplore",
            "baselineEpsilon": 0.2,
            "seed": 18446744073709551615u64,
            "snapshotEvery": 10,
            "rewards": "sum",
        });
        let spec = parse(value.clone()).unwrap();
        assert_eq!(spec.exploration.kind, ExplorationKind::EpsilonGreedy);
        assert_eq!(spec.learner.importance, Importance::Mtr);
        assert_eq!(spec.mode, Mode::BaselineExplore);
        assert_eq!(spec.rewards, RewardAggregation::Sum);
        assert_eq!(spec.seed, Some(u64::MAX));
        assert_eq!(spec.actions[0].features["cost"], json!(0.2));
        // JSON form round-trips (numbers come back as floats where typed so).
        let again = parse(spec.to_json()).unwrap();
        assert_eq!(again, spec);
        assert_eq!(spec.to_json()["mode"], json!("baselineExplore"));
        assert_eq!(
            spec.to_json()["exploration"]["kind"],
            json!("epsilonGreedy")
        );
        assert!(spec.to_json()["actions"][1].get("features").is_none());
        assert!(DecisionSpec::default().to_json().get("seed").is_none());
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        for value in [
            json!({"explore": {}}),
            json!({"exploration": {"gama": 5}}),
            json!({"learner": {"bitz": 18}}),
            json!({"reward": {"range": [0, 1], "scale": 2}}),
            json!({"actions": [{"id": "a", "weight": 1}]}),
        ] {
            let e = err(value.clone());
            assert!(e.contains("unknown field"), "{value}: {e}");
        }
    }

    #[test]
    fn wrong_types_and_enum_values_are_rejected() {
        assert!(err(json!({"mode": "greedy"})).contains("unknown variant"));
        assert!(err(json!({"exploration": {"kind": "SquareCB"}})).contains("unknown variant"));
        assert!(err(json!({"learner": {"importance": "ips"}})).contains("unknown variant"));
        assert!(err(json!({"actions": [{"id": "a", "features": [1]}]})).contains("invalid type"));
        assert!(err(json!({"actions": [{"features": {}}]})).contains("missing field `id`"));
        assert!(err(json!({"learner": {"bits": 18.5}})).contains("invalid type"));
        assert!(err(json!({"reward": {"range": [0, 1, 2]}})).contains("invalid length"));
        assert!(err(json!({"seed": -1})).contains("invalid value"));
    }

    #[test]
    fn range_errors_are_precise() {
        let cases = [
            (
                json!({"reward": {"range": [1, 0]}}),
                "reward.range must be two finite numbers [lo, hi] with lo < hi (got [1, 0])",
            ),
            (json!({"reward": {"range": [2, 2]}}), "lo < hi (got [2, 2])"),
            (
                json!({"reward": {"range": [-1e308, 1e308]}}),
                "reward.range",
            ),
            (
                json!({"exploration": {"gammaScale": -1}}),
                "exploration.gammaScale must be in [0, 1000000] (got -1)",
            ),
            (
                json!({"exploration": {"gammaExponent": 1.5}}),
                "exploration.gammaExponent must be in [0, 1] (got 1.5)",
            ),
            (
                json!({"exploration": {"epsilon": 2}}),
                "exploration.epsilon must be in [0, 1] (got 2)",
            ),
            (
                json!({"exploration": {"floor": 0}}),
                "exploration.floor must be in (0, 1] (got 0)",
            ),
            (
                json!({"exploration": {"floor": 1.01}}),
                "exploration.floor must be in (0, 1] (got 1.01)",
            ),
            (
                json!({"learner": {"bits": 9}}),
                "learner.bits must be an integer in [10, 24] (got 9)",
            ),
            (
                json!({"learner": {"bits": 25}}),
                "learner.bits must be an integer in [10, 24] (got 25)",
            ),
            (
                json!({"learner": {"learningRate": 0}}),
                "learner.learningRate must be in (0, 10] (got 0)",
            ),
            (
                json!({"learner": {"maxImportanceWeight": 0.5}}),
                "learner.maxImportanceWeight must be in [1, 1000000] (got 0.5)",
            ),
            (
                json!({"baselineEpsilon": 0}),
                "baselineEpsilon must be in (0, 1] (got 0)",
            ),
            (
                json!({"snapshotEvery": 0}),
                "snapshotEvery must be an integer in [1, 1000000000] (got 0)",
            ),
        ];
        for (value, want) in cases {
            let e = err(value.clone());
            assert!(e.contains(want), "{value}: {e}");
        }
        // Boundaries that are allowed.
        for value in [
            json!({"exploration": {"gammaScale": 0, "gammaExponent": 0, "epsilon": 0, "floor": 1}}),
            json!({"exploration": {"gammaScale": 1e6, "gammaExponent": 1, "epsilon": 1}}),
            json!({"learner": {"bits": 10, "learningRate": 10, "maxImportanceWeight": 1}}),
            json!({"learner": {"bits": 24, "maxImportanceWeight": 1e6}}),
            json!({"baselineEpsilon": 1, "snapshotEvery": 1000000000}),
        ] {
            parse(value.clone()).unwrap_or_else(|e| panic!("{value}: {e}"));
        }
    }

    #[test]
    fn nan_is_rejected_when_built_in_code() {
        let mut spec = DecisionSpec::default();
        spec.exploration.floor = f64::NAN;
        assert!(spec.validate().unwrap_err().contains("exploration.floor"));
        let mut spec = DecisionSpec::default();
        spec.reward.range = [0.0, f64::INFINITY];
        assert!(spec.validate().unwrap_err().contains("reward.range"));
    }

    #[test]
    fn action_list_rules() {
        assert_eq!(
            err(json!({"actions": [{"id": "a"}, {"id": ""}]})),
            "actions[1].id must not be empty"
        );
        assert_eq!(
            err(json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "a"}]})),
            "actions[2].id \"a\" duplicates actions[0].id"
        );
        let long = "é".repeat(257);
        assert_eq!(
            err(json!({"actions": [{"id": long}]})),
            "actions[0].id is longer than 256 characters"
        );
        // 256 two-byte characters are fine: the limit counts characters.
        parse(json!({"actions": [{"id": "é".repeat(256)}]})).unwrap();
        let many: Vec<Value> = (0..=MAX_ACTIONS)
            .map(|i| json!({"id": format!("a{i}")}))
            .collect();
        assert_eq!(
            err(json!({ "actions": many })),
            "actions has 1025 entries; the limit is 1024"
        );
        let e = err(json!({"actions": [{"id": "x", "features": {"v": 1e39}}]}));
        assert!(
            e.starts_with("actions[0] (\"x\"): action feature \"v\""),
            "{e}"
        );
        let ok: Vec<ActionSpec> = (0..MAX_ACTIONS)
            .map(|i| ActionSpec::new(format!("a{i}")))
            .collect();
        validate_actions(&ok, "actions").unwrap();
        assert!(
            validate_actions(&[ActionSpec::new("")], "request.actions")
                .unwrap_err()
                .starts_with("request.actions[0]")
        );
    }

    #[test]
    fn reward_normalization() {
        let r = RewardSpec { range: [-1.0, 3.0] };
        assert_eq!(r.normalize(-1.0), 0.0);
        assert_eq!(r.normalize(1.0), 0.5);
        assert_eq!(r.normalize(3.0), 1.0);
        assert_eq!(r.normalize(10.0), 1.0);
        assert_eq!(r.normalize(-5.0), 0.0);
    }

    #[test]
    fn merge_patch_follows_rfc_7396() {
        let base = parse(json!({
            "actions": [{"id": "a"}, {"id": "b"}],
            "exploration": {"gammaScale": 20, "floor": 0.1},
            "seed": 7,
        }))
        .unwrap();
        // Nested fields merge; untouched siblings keep their values.
        let patched = base
            .merge_patch(&json!({"exploration": {"floor": 0.2}}))
            .unwrap();
        assert_eq!(patched.exploration.floor, 0.2);
        assert_eq!(patched.exploration.gamma_scale, 20.0);
        assert_eq!(patched.actions, base.actions);
        // null removes a field, which restores its default.
        let patched = base
            .merge_patch(&json!({"seed": null, "exploration": {"gammaScale": null}}))
            .unwrap();
        assert_eq!(patched.seed, None);
        assert_eq!(patched.exploration.gamma_scale, 10.0);
        assert_eq!(patched.exploration.floor, 0.1);
        // Arrays are replaced, not merged.
        let patched = base
            .merge_patch(&json!({"actions": [{"id": "z"}]}))
            .unwrap();
        assert_eq!(patched.actions, vec![ActionSpec::new("z")]);
        // Mode switch.
        let patched = base.merge_patch(&json!({"mode": "frozen"})).unwrap();
        assert_eq!(patched.mode, Mode::Frozen);
        // Empty patch is the identity.
        assert_eq!(base.merge_patch(&json!({})).unwrap(), base);
    }

    #[test]
    fn merge_patch_rejects_bad_results() {
        let base = DecisionSpec::default();
        assert!(
            base.merge_patch(&json!({"exploration": {"gama": 1}}))
                .unwrap_err()
                .contains("unknown field `gama`")
        );
        assert!(
            base.merge_patch(&json!({"typo": 1}))
                .unwrap_err()
                .contains("unknown field `typo`")
        );
        assert!(
            base.merge_patch(&json!({"learner": {"bits": 40}}))
                .unwrap_err()
                .contains("learner.bits")
        );
        assert!(
            base.merge_patch(&json!({"exploration": 3}))
                .unwrap_err()
                .contains("invalid type")
        );
        assert_eq!(
            base.merge_patch(&json!([1])).unwrap_err(),
            "spec patch must be a JSON object"
        );
        // Replacing a sub-object with a scalar and back is still strict.
        assert!(
            base.merge_patch(&json!({"reward": {"range": [5, 1]}}))
                .unwrap_err()
                .contains("reward.range")
        );
    }

    #[test]
    fn raw_merge_patch_examples_from_the_rfc() {
        // Selected test cases from RFC 7396 appendix A.
        let cases = [
            (json!({"a": "b"}), json!({"a": "c"}), json!({"a": "c"})),
            (
                json!({"a": "b"}),
                json!({"b": "c"}),
                json!({"a": "b", "b": "c"}),
            ),
            (json!({"a": "b"}), json!({"a": null}), json!({})),
            (
                json!({"a": "b", "b": "c"}),
                json!({"a": null}),
                json!({"b": "c"}),
            ),
            (json!({"a": ["b"]}), json!({"a": "c"}), json!({"a": "c"})),
            (json!({"a": "c"}), json!({"a": ["b"]}), json!({"a": ["b"]})),
            (
                json!({"a": {"b": "c"}}),
                json!({"a": {"b": "d", "c": null}}),
                json!({"a": {"b": "d"}}),
            ),
            (
                json!({"a": [{"b": "c"}]}),
                json!({"a": [1]}),
                json!({"a": [1]}),
            ),
            (json!(["a", "b"]), json!(["c", "d"]), json!(["c", "d"])),
            (json!({"a": "b"}), json!(["c"]), json!(["c"])),
            (json!({"a": "foo"}), json!(null), json!(null)),
            (json!({"a": "foo"}), json!("bar"), json!("bar")),
            (
                json!({"e": null}),
                json!({"a": 1}),
                json!({"e": null, "a": 1}),
            ),
            (
                json!([1, 2]),
                json!({"a": "b", "c": null}),
                json!({"a": "b"}),
            ),
            (
                json!({}),
                json!({"a": {"bb": {"ccc": null}}}),
                json!({"a": {"bb": {}}}),
            ),
        ];
        for (target, patch, want) in cases {
            let mut doc = target.clone();
            apply_merge_patch(&mut doc, &patch);
            assert_eq!(doc, want, "target {target} patch {patch}");
        }
    }
}
