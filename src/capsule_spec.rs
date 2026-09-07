use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub use lycan::hierarchical::HierarchicalSpec;

/// Maximum number of decisions a single capsule may declare.
pub const MAX_DECISIONS_PER_CAPSULE: usize = 8;

/// Top-level capsule specification parsed from a `*.capsule.yaml` file.
#[derive(Debug, Deserialize, Serialize)]
pub struct CapsuleSpec {
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// Action set for the capsule's primary (root) decision. When `decisions`
    /// is set, this must match `decisions[0].options` exactly.
    pub options: Vec<String>,
    #[serde(default)]
    pub contexts: Vec<String>,
    pub reward: RewardSpec,
    #[serde(default)]
    pub algorithm: AlgorithmSpec,
    #[serde(default)]
    pub learning: LearningSpec,
    /// Optional sequential decision graph; absent means a single implicit
    /// decision over the top-level `options` list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<DecisionSpec>>,

    /// Optional hierarchical bandit specification. Mutually exclusive with
    /// `decisions`. `options` must equal the enumerated leaf-name sequence.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "hierarchicalOptions",
        alias = "hierarchical_options"
    )]
    pub hierarchical_options: Option<HierarchicalSpec>,
}

/// One node in a capsule's sequential decision graph; maps to one
/// `AdaptiveChoice` node in the compiled `.lyc`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DecisionSpec {
    pub name: String,
    pub options: Vec<String>,
    /// Parent decision name; `None`/null means root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
}

/// Reward signal description.
#[derive(Debug, Deserialize, Serialize)]
pub struct RewardSpec {
    #[serde(rename = "type")]
    pub kind: RewardType,
    /// Required when `kind == Continuous`.
    pub range: Option<[f64; 2]>,
    #[serde(default)]
    pub components: Vec<RewardComponent>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum RewardType {
    Bernoulli,
    Continuous,
    SparseContinuous,
}

/// One weighted reward component used in multi-objective composition.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RewardComponent {
    pub name: String,
    pub weight: f64,
    pub normalize: NormalizeKind,
    /// Required for `NormalizeKind::Minmax`.
    pub range: Option<[f64; 2]>,
    /// Required for `NormalizeKind::Budget`.
    pub budget: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeKind {
    Minmax,
    Budget,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct AlgorithmSpec {
    #[serde(default = "default_auto", rename = "type")]
    pub kind: AlgorithmKind,
}

fn default_auto() -> AlgorithmKind { AlgorithmKind::Auto }

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlgorithmKind {
    #[default]
    Auto,
    Thompson,
    Ucb,
    EpsilonGreedy,
    Weighted,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct LearningSpec {
    #[serde(default = "default_min_exploration")]
    pub min_exploration: f64,
}

fn default_min_exploration() -> f64 { 0.02 }

impl CapsuleSpec {
    /// Parse a capsule from YAML and immediately run [`Self::validate`].
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        let spec: CapsuleSpec =
            serde_yml::from_str(yaml).map_err(|e| format!("invalid capsule YAML: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Run structural validation over the spec.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name is required".into());
        }
        if self.options.len() < 2 {
            return Err("options must contain at least two entries".into());
        }
        for (i, opt) in self.options.iter().enumerate() {
            if opt.trim().is_empty() {
                return Err(format!("options[{i}] must not be empty"));
            }
        }
        if matches!(self.reward.kind, RewardType::Continuous) && self.reward.range.is_none() {
            return Err("reward.range is required when reward.type is continuous".into());
        }
        let mut seen = HashSet::new();
        for (i, c) in self.reward.components.iter().enumerate() {
            if c.name.trim().is_empty() {
                return Err(format!("reward.components[{i}].name must not be empty"));
            }
            if !seen.insert(c.name.clone()) {
                return Err(format!("duplicate reward component name: {}", c.name));
            }
            if !c.weight.is_finite() {
                return Err(format!("reward.components[{}].weight is not finite", c.name));
            }
            match c.normalize {
                NormalizeKind::Minmax => {
                    if c.range.is_none() {
                        return Err(format!(
                            "reward.components[{}] has normalize: minmax but no range",
                            c.name
                        ));
                    }
                }
                NormalizeKind::Budget => {
                    if c.budget.is_none() {
                        return Err(format!(
                            "reward.components[{}] has normalize: budget but no budget",
                            c.name
                        ));
                    }
                }
            }
        }

        if let Some(decisions) = &self.decisions {
            self.validate_decisions(decisions)?;
        }

        if let Some(hier) = &self.hierarchical_options {
            self.validate_hierarchical(hier)?;
        }

        Ok(())
    }

    fn validate_hierarchical(&self, hier: &HierarchicalSpec) -> Result<(), String> {
        if self.decisions.is_some() {
            return Err(
                "hierarchical_options is mutually exclusive with decisions; \
                 use one shape, not both".into()
            );
        }

        hier.validate()?;

        // Flat options must equal the enumerated leaf-name sequence.
        let leaf_paths = hier.enumerate_paths();
        let mut leaf_names: Vec<String> = Vec::with_capacity(leaf_paths.len());
        for path in &leaf_paths {
            match hier.resolve_path(path) {
                Some(name) => leaf_names.push(name.to_string()),
                None => return Err(format!(
                    "hierarchical_options: enumerate_paths produced path {path:?} \
                     that resolve_path could not resolve — spec is malformed"
                )),
            }
        }
        if leaf_names != self.options {
            return Err(format!(
                "hierarchical_options: flat options must equal the enumerated leaf names \
                 (got top-level options={:?}, leaf names from tree={:?})",
                self.options, leaf_names
            ));
        }

        Ok(())
    }

    fn validate_decisions(&self, decisions: &[DecisionSpec]) -> Result<(), String> {
        if decisions.is_empty() {
            return Err("decisions, when present, must contain at least one entry".into());
        }
        if decisions.len() > MAX_DECISIONS_PER_CAPSULE {
            return Err(format!(
                "decisions has {} entries (max {})",
                decisions.len(),
                MAX_DECISIONS_PER_CAPSULE
            ));
        }

        let mut names: HashSet<&str> = HashSet::new();
        for (i, d) in decisions.iter().enumerate() {
            if d.name.trim().is_empty() {
                return Err(format!("decisions[{i}].name must not be empty"));
            }
            if !names.insert(d.name.as_str()) {
                return Err(format!("duplicate decision name: {}", d.name));
            }
            if d.options.len() < 2 {
                return Err(format!(
                    "decisions[{}] ({}) must declare at least two options",
                    i, d.name
                ));
            }
            for (j, opt) in d.options.iter().enumerate() {
                if opt.trim().is_empty() {
                    return Err(format!(
                        "decisions[{}].options[{j}] must not be empty",
                        d.name
                    ));
                }
            }
        }

        // Every depends_on must reference a known decision (and not itself).
        for d in decisions {
            if let Some(parent) = d.depends_on.as_deref() {
                if parent.is_empty() {
                    // Treat empty string the same as omitted — root.
                    continue;
                }
                if parent == d.name {
                    return Err(format!("decision {} cannot depend on itself", d.name));
                }
                if !names.contains(parent) {
                    return Err(format!(
                        "decision {} depends_on unknown decision: {parent}",
                        d.name
                    ));
                }
            }
        }

        // Cycle detection via Kahn's algorithm.
        let mut indegree: HashMap<&str, usize> = HashMap::new();
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for d in decisions {
            indegree.entry(d.name.as_str()).or_insert(0);
            children.entry(d.name.as_str()).or_default();
        }
        for d in decisions {
            if let Some(parent) = d.depends_on.as_deref() {
                if parent.is_empty() {
                    continue;
                }
                *indegree.entry(d.name.as_str()).or_insert(0) += 1;
                children.entry(parent).or_default().push(d.name.as_str());
            }
        }
        let mut frontier: Vec<&str> = indegree
            .iter()
            .filter_map(|(n, deg)| if *deg == 0 { Some(*n) } else { None })
            .collect();
        let mut visited = 0usize;
        while let Some(node) = frontier.pop() {
            visited += 1;
            if let Some(kids) = children.get(node) {
                for k in kids.clone() {
                    if let Some(deg) = indegree.get_mut(k) {
                        *deg -= 1;
                        if *deg == 0 {
                            frontier.push(k);
                        }
                    }
                }
            }
        }
        if visited != decisions.len() {
            return Err(format!(
                "decisions form a cycle (visited {visited} of {})",
                decisions.len()
            ));
        }

        // First entry must mirror the top-level options field.
        let first = &decisions[0];
        if first.options != self.options {
            return Err(format!(
                "decisions[0].options must match top-level options (decisions[0]={} has {:?}, top-level has {:?})",
                first.name, first.options, self.options
            ));
        }

        Ok(())
    }

    /// Topologically-sorted decision names. Empty when `decisions` is absent.
    /// Ties are broken by declaration order for deterministic output.
    pub fn decision_order(&self) -> Vec<&str> {
        let Some(decisions) = &self.decisions else {
            return Vec::new();
        };

        let mut indegree: HashMap<&str, usize> = HashMap::new();
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut declared_order: Vec<&str> = Vec::with_capacity(decisions.len());
        for d in decisions {
            indegree.entry(d.name.as_str()).or_insert(0);
            children.entry(d.name.as_str()).or_default();
            declared_order.push(d.name.as_str());
        }
        for d in decisions {
            if let Some(parent) = d.depends_on.as_deref() {
                if parent.is_empty() {
                    continue;
                }
                *indegree.entry(d.name.as_str()).or_insert(0) += 1;
                children.entry(parent).or_default().push(d.name.as_str());
            }
        }

        let mut out: Vec<&str> = Vec::with_capacity(decisions.len());
        // Stable Kahn's: pick earliest-declared zero-indegree node each pass.
        let mut remaining: HashSet<&str> = declared_order.iter().copied().collect();
        while !remaining.is_empty() {
            let mut picked: Option<&str> = None;
            for name in &declared_order {
                if remaining.contains(name) && indegree.get(name).copied().unwrap_or(0) == 0 {
                    picked = Some(name);
                    break;
                }
            }
            let Some(node) = picked else {
                // Validation should have ruled this out; preserve partial output.
                break;
            };
            out.push(node);
            remaining.remove(node);
            if let Some(kids) = children.get(node) {
                for k in kids.clone() {
                    if let Some(deg) = indegree.get_mut(k) {
                        *deg = deg.saturating_sub(1);
                    }
                }
            }
        }
        out
    }

    /// Map from each decision name to its `depends_on` parent (root => None).
    /// Empty when `decisions` is absent.
    pub fn dependency_map(&self) -> HashMap<String, Option<String>> {
        let mut map = HashMap::new();
        let Some(decisions) = &self.decisions else {
            return map;
        };
        for d in decisions {
            let parent = d
                .depends_on
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned();
            map.insert(d.name.clone(), parent);
        }
        map
    }

    /// Resolve `AlgorithmKind::Auto` to the concrete algorithm the runtime
    /// will use, based on the capsule's reward kind.
    pub fn resolved_algorithm(&self) -> AlgorithmKind {
        if matches!(self.algorithm.kind, AlgorithmKind::Auto) {
            match self.reward.kind {
                RewardType::Bernoulli => AlgorithmKind::Thompson,
                RewardType::Continuous => AlgorithmKind::Weighted,
                RewardType::SparseContinuous => AlgorithmKind::Ucb,
            }
        } else {
            self.algorithm.kind
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LLM_ROUTER_YAML: &str = r#"
name: llm-router
version: 0.1.0
options:
  - cheap_fast
  - balanced
  - expensive_accurate
contexts:
  - task_type
  - customer_tier
  - urgency
reward:
  type: continuous
  range: [-1.0, 1.0]
  components:
    - name: quality
      weight: 0.6
      normalize: minmax
      range: [0.0, 1.0]
    - name: latency_ms
      weight: -0.2
      normalize: budget
      budget: 2000
    - name: cost_usd
      weight: -0.2
      normalize: budget
      budget: 0.05
algorithm:
  type: auto
learning:
  min_exploration: 0.02
"#;

    #[test]
    fn parses_llm_router_example() {
        let spec = CapsuleSpec::from_yaml(LLM_ROUTER_YAML).expect("must parse");
        assert_eq!(spec.name, "llm-router");
        assert_eq!(spec.options.len(), 3);
        assert_eq!(spec.contexts.len(), 3);
        assert!(matches!(spec.reward.kind, RewardType::Continuous));
        assert_eq!(spec.reward.components.len(), 3);
        assert_eq!(spec.reward.components[0].weight, 0.6);
        assert_eq!(spec.reward.components[1].weight, -0.2);
        assert_eq!(spec.reward.components[2].budget, Some(0.05));
        assert_eq!(spec.resolved_algorithm(), AlgorithmKind::Weighted);
        assert!(spec.decisions.is_none());
        assert!(spec.decision_order().is_empty());
        assert!(spec.dependency_map().is_empty());
    }

    #[test]
    fn auto_picks_thompson_for_bernoulli() {
        let y = r#"
name: ab-test
options: [a, b]
reward: { type: bernoulli }
"#;
        let spec = CapsuleSpec::from_yaml(y).unwrap();
        assert_eq!(spec.resolved_algorithm(), AlgorithmKind::Thompson);
    }

    #[test]
    fn auto_picks_ucb_for_sparse_continuous() {
        let y = r#"
name: x
options: [a, b]
reward: { type: sparse_continuous, range: [0, 1] }
"#;
        let spec = CapsuleSpec::from_yaml(y).unwrap();
        assert_eq!(spec.resolved_algorithm(), AlgorithmKind::Ucb);
    }

    #[test]
    fn explicit_algorithm_overrides_auto() {
        let y = r#"
name: x
options: [a, b]
reward: { type: bernoulli }
algorithm: { type: epsilon_greedy }
"#;
        let spec = CapsuleSpec::from_yaml(y).unwrap();
        assert_eq!(spec.resolved_algorithm(), AlgorithmKind::EpsilonGreedy);
    }

    #[test]
    fn rejects_continuous_without_range() {
        let y = r#"
name: x
options: [a, b]
reward: { type: continuous }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("range is required"), "got: {err}");
    }

    #[test]
    fn rejects_minmax_component_without_range() {
        let y = r#"
name: x
options: [a, b]
reward:
  type: continuous
  range: [0, 1]
  components:
    - name: x
      weight: 1.0
      normalize: minmax
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("normalize: minmax but no range"), "got: {err}");
    }

    #[test]
    fn rejects_budget_component_without_budget() {
        let y = r#"
name: x
options: [a, b]
reward:
  type: continuous
  range: [0, 1]
  components:
    - name: latency
      weight: -0.5
      normalize: budget
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("normalize: budget but no budget"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_component_names() {
        let y = r#"
name: x
options: [a, b]
reward:
  type: continuous
  range: [0, 1]
  components:
    - { name: latency, weight: -0.5, normalize: budget, budget: 1000 }
    - { name: latency, weight: -0.5, normalize: budget, budget: 1000 }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("duplicate reward component"), "got: {err}");
    }

    #[test]
    fn rejects_single_option() {
        let y = r#"
name: x
options: [only]
reward: { type: bernoulli }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("at least two"), "got: {err}");
    }

    // ---------------------------------------------------------------------
    // Multi-decision schema tests.
    // ---------------------------------------------------------------------

    const RANKED_LIST_ROUTER_YAML: &str = r#"
name: ranked-list-router
options: [option_a, option_b, option_c]
reward: { type: continuous, range: [-1, 1] }
decisions:
  - name: primary
    options: [option_a, option_b, option_c]
  - name: secondary
    options: [variant_x, variant_y]
    depends_on: primary
  - name: tertiary
    options: [tier_1, tier_2, tier_3]
    depends_on: secondary
"#;

    #[test]
    fn parses_three_decision_hierarchy() {
        let spec = CapsuleSpec::from_yaml(RANKED_LIST_ROUTER_YAML).expect("must parse");
        let decisions = spec.decisions.as_ref().expect("decisions present");
        assert_eq!(decisions.len(), 3);
        assert_eq!(decisions[0].name, "primary");
        assert!(decisions[0].depends_on.is_none());
        assert_eq!(decisions[1].depends_on.as_deref(), Some("primary"));
        assert_eq!(decisions[2].depends_on.as_deref(), Some("secondary"));

        let order = spec.decision_order();
        assert_eq!(order, vec!["primary", "secondary", "tertiary"]);

        let dep = spec.dependency_map();
        assert_eq!(dep.get("primary"), Some(&None));
        assert_eq!(dep.get("secondary"), Some(&Some("primary".to_string())));
        assert_eq!(dep.get("tertiary"), Some(&Some("secondary".to_string())));
    }

    #[test]
    fn rejects_depends_on_unknown_name() {
        let y = r#"
name: bad
options: [a, b]
reward: { type: bernoulli }
decisions:
  - { name: root, options: [a, b] }
  - { name: child, options: [x, y], depends_on: ghost }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("unknown decision: ghost"), "got: {err}");
    }

    #[test]
    fn rejects_cyclic_dependencies() {
        let y = r#"
name: cyc
options: [a, b]
reward: { type: bernoulli }
decisions:
  - { name: A, options: [a, b], depends_on: B }
  - { name: B, options: [a, b], depends_on: A }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("cycle"), "got: {err}");
    }

    #[test]
    fn rejects_self_dependency() {
        let y = r#"
name: selfref
options: [a, b]
reward: { type: bernoulli }
decisions:
  - { name: A, options: [a, b], depends_on: A }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("cannot depend on itself"), "got: {err}");
    }

    #[test]
    fn rejects_more_than_eight_decisions() {
        let mut y = String::from(
            "name: too-many\noptions: [a, b]\nreward: { type: bernoulli }\ndecisions:\n",
        );
        for i in 0..9 {
            if i == 0 {
                y.push_str("  - { name: d0, options: [a, b] }\n");
            } else {
                y.push_str(&format!(
                    "  - {{ name: d{i}, options: [a, b], depends_on: d{} }}\n",
                    i - 1
                ));
            }
        }
        let err = CapsuleSpec::from_yaml(&y).unwrap_err();
        assert!(err.contains("max 8"), "got: {err}");
    }

    #[test]
    fn rejects_first_decision_options_mismatch() {
        let y = r#"
name: mismatch
options: [a, b, c]
reward: { type: bernoulli }
decisions:
  - { name: primary, options: [a, b] }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(
            err.contains("must match top-level options"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_decision_with_one_option() {
        let y = r#"
name: thin
options: [a, b]
reward: { type: bernoulli }
decisions:
  - { name: primary, options: [a, b] }
  - { name: secondary, options: [solo], depends_on: primary }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("at least two options"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_decision_names() {
        let y = r#"
name: dup
options: [a, b]
reward: { type: bernoulli }
decisions:
  - { name: primary, options: [a, b] }
  - { name: primary, options: [x, y], depends_on: primary }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("duplicate decision name"), "got: {err}");
    }

    #[test]
    fn yaml_roundtrip_preserves_decisions() {
        let spec = CapsuleSpec::from_yaml(RANKED_LIST_ROUTER_YAML).expect("parse");
        let dumped = serde_yml::to_string(&spec).expect("serialize yaml");
        let reparsed = CapsuleSpec::from_yaml(&dumped).expect("reparse yaml");
        let reparsed_decisions = reparsed.decisions.as_ref().expect("decisions preserved");
        assert_eq!(reparsed_decisions.len(), 3);
        assert_eq!(reparsed_decisions[0].name, "primary");
        assert_eq!(reparsed_decisions[1].depends_on.as_deref(), Some("primary"));
        assert_eq!(reparsed_decisions[2].depends_on.as_deref(), Some("secondary"));
        assert_eq!(reparsed.decision_order(), vec!["primary", "secondary", "tertiary"]);
    }

    #[test]
    fn json_roundtrip_preserves_decisions() {
        let spec = CapsuleSpec::from_yaml(RANKED_LIST_ROUTER_YAML).expect("parse");
        let json = serde_json::to_string(&spec).expect("serialize json");
        let reparsed: CapsuleSpec = serde_json::from_str(&json).expect("reparse json");
        reparsed.validate().expect("validate json roundtrip");
        let decisions = reparsed.decisions.as_ref().expect("decisions preserved");
        assert_eq!(decisions.len(), 3);
        assert_eq!(decisions[1].depends_on.as_deref(), Some("primary"));
        assert_eq!(reparsed.decision_order(), vec!["primary", "secondary", "tertiary"]);
    }

    #[test]
    fn null_depends_on_is_treated_as_root() {
        let y = r#"
name: explicit-null
options: [a, b]
reward: { type: bernoulli }
decisions:
  - { name: primary, options: [a, b], depends_on: null }
  - { name: secondary, options: [x, y], depends_on: primary }
"#;
        let spec = CapsuleSpec::from_yaml(y).expect("must parse");
        let order = spec.decision_order();
        assert_eq!(order, vec!["primary", "secondary"]);
        assert_eq!(spec.dependency_map().get("primary"), Some(&None));
    }

    // ---------------------------------------------------------------------
    // Hierarchical-options schema tests.
    // ---------------------------------------------------------------------

    const HIERARCHICAL_2X3_YAML: &str = r#"
name: hierarchical-region-routing
version: 0.1.0
options: [us_small, us_medium, us_large, eu_small, eu_medium, eu_large]
reward: { type: continuous, range: [-1, 1] }
hierarchical_options:
  options:
    - name: us
      sub_capsule:
        options: [us_small, us_medium, us_large]
        reward: { type: continuous, range: [-1, 1] }
    - name: eu
      sub_capsule:
        options: [eu_small, eu_medium, eu_large]
        reward: { type: continuous, range: [-1, 1] }
  reward: { type: continuous, range: [-1, 1] }
"#;

    #[test]
    fn parses_hierarchical_2x3_tree() {
        let spec = CapsuleSpec::from_yaml(HIERARCHICAL_2X3_YAML).expect("must parse");
        let hier = spec.hierarchical_options.as_ref().expect("hierarchical_options present");
        assert_eq!(hier.max_depth(), 2);
        assert_eq!(hier.count_leaves(), 6);
        assert_eq!(spec.options.len(), 6);
        let leaves: Vec<String> = hier.enumerate_paths().iter()
            .map(|p| hier.resolve_path(p).unwrap().to_string())
            .collect();
        assert_eq!(spec.options, leaves);
    }

    #[test]
    fn rejects_hierarchical_with_mismatched_flat_options() {
        let y = r#"
name: bad
options: [eu_small, us_small, us_medium, us_large, eu_medium, eu_large]
reward: { type: continuous, range: [-1, 1] }
hierarchical_options:
  options:
    - name: us
      sub_capsule:
        options: [us_small, us_medium, us_large]
        reward: { type: continuous, range: [-1, 1] }
    - name: eu
      sub_capsule:
        options: [eu_small, eu_medium, eu_large]
        reward: { type: continuous, range: [-1, 1] }
  reward: { type: continuous, range: [-1, 1] }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(
            err.contains("flat options must equal the enumerated leaf names"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_hierarchical_with_decisions_together() {
        let y = r#"
name: clash
options: [us_small, us_medium, eu_small, eu_medium]
reward: { type: continuous, range: [-1, 1] }
decisions:
  - { name: primary, options: [us_small, us_medium, eu_small, eu_medium] }
hierarchical_options:
  options:
    - name: us
      sub_capsule:
        options: [us_small, us_medium]
        reward: { type: continuous, range: [-1, 1] }
    - name: eu
      sub_capsule:
        options: [eu_small, eu_medium]
        reward: { type: continuous, range: [-1, 1] }
  reward: { type: continuous, range: [-1, 1] }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn rejects_hierarchical_with_invalid_internal_shape() {
        // Sub-capsule with one option violates MIN_OPTIONS_PER_LEVEL.
        let y = r#"
name: thin
options: [a, b]
reward: { type: continuous, range: [-1, 1] }
hierarchical_options:
  options:
    - name: only
      sub_capsule:
        options: [a]
        reward: { type: continuous, range: [-1, 1] }
    - name: other
      sub_capsule:
        options: [b]
        reward: { type: continuous, range: [-1, 1] }
  reward: { type: continuous, range: [-1, 1] }
"#;
        let err = CapsuleSpec::from_yaml(y).unwrap_err();
        assert!(err.contains("minimum"), "got: {err}");
    }

    #[test]
    fn flat_capsule_without_hierarchical_unchanged() {
        let spec = CapsuleSpec::from_yaml(LLM_ROUTER_YAML).expect("must parse");
        assert!(spec.hierarchical_options.is_none());
        spec.validate().expect("legacy capsule still validates");
    }
}
