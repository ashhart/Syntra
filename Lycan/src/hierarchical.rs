//! Hierarchical bandits — non-leaf decisions are themselves bandits, leaves
//! return concrete actions. No temporal rollouts; only nested discrete
//! choices. Same reward is propagated to every level along the path.

use serde::{Deserialize, Serialize};

/// Maximum allowed nesting depth (root = depth 1). A spec deeper than this
/// is rejected by [`HierarchicalSpec::validate`].
pub const MAX_DEPTH: usize = 4;

/// Maximum allowed total number of distinct leaf paths in the tree.
pub const MAX_LEAVES: usize = 256;

/// Minimum branching factor at every level.
pub const MIN_OPTIONS_PER_LEVEL: usize = 2;

/// Reward declaration mirroring Syntra's `RewardSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewardSpec {
    #[serde(rename = "type")]
    pub kind: RewardKind,
    /// Inclusive bounds when `kind` is `continuous` or `sparse_continuous`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
}

/// Mirror of the capsule-spec `RewardType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardKind {
    /// Binary {0, 1} outcomes.
    Bernoulli,
    /// Real-valued reward in a fixed range.
    Continuous,
    /// Mostly-zero real-valued reward in a fixed range.
    SparseContinuous,
}

/// One entry in a hierarchical option list — either a leaf (name only) or
/// a branch (name + nested `sub_capsule`). Untagged serde with `Branch`
/// first so `{name, subCapsule}` maps don't silently match `Leaf`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HierarchicalOption {
    /// Named intermediate decision whose sub-options form a nested capsule.
    Branch {
        /// Branch name.
        name: String,
        /// Nested capsule whose leaves are reachable through this branch.
        #[serde(rename = "sub_capsule", alias = "subCapsule")]
        sub_capsule: Box<HierarchicalSpec>,
    },
    /// A terminal action with an explicit `{ name }` mapping.
    Leaf {
        /// Action name.
        name: String,
    },
    /// A terminal action represented as a bare string in YAML/JSON
    /// (e.g. `- variant_x`).
    BareLeaf(String),
}

impl HierarchicalOption {
    /// Return the option name (whether leaf or branch).
    pub fn name(&self) -> &str {
        match self {
            HierarchicalOption::BareLeaf(n) => n,
            HierarchicalOption::Leaf { name } => name,
            HierarchicalOption::Branch { name, .. } => name,
        }
    }

    /// Return the nested spec if this is a branch.
    pub fn sub_capsule(&self) -> Option<&HierarchicalSpec> {
        match self {
            HierarchicalOption::Branch { sub_capsule, .. } => Some(sub_capsule.as_ref()),
            _ => None,
        }
    }

    /// True iff this option is a terminal leaf.
    pub fn is_leaf(&self) -> bool {
        matches!(
            self,
            HierarchicalOption::Leaf { .. } | HierarchicalOption::BareLeaf(_)
        )
    }
}

/// How a leaf reward propagates upward. Only the outermost spec's value
/// is consulted by [`propagate_reward`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum RewardPropagation {
    /// Same reward at every level. Default.
    Full,
    /// Reward at depth `d` along a length-`N` path is `reward * factor^(N-1-d)`.
    Discounted { factor: f64 },
}

impl Default for RewardPropagation {
    fn default() -> Self { RewardPropagation::Full }
}

/// Recursive description of a hierarchical capsule's option tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HierarchicalSpec {
    pub options: Vec<HierarchicalOption>,
    pub reward: RewardSpec,
    /// Credit-assignment mode; only the root's value matters. Defaults to `Full`.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "rewardPropagation")]
    pub reward_propagation: Option<RewardPropagation>,
}

/// Root-to-leaf path as the option indices traversed at each level.
pub type DecisionPath = Vec<usize>;

/// Identifies a single bandit-state bucket along a decision path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HierState {
    /// 0-indexed nesting depth (root = 0).
    pub depth: usize,
    /// Indices traversed before reaching this level.
    pub parent_option_path: Vec<usize>,
    /// Number of arms at this level.
    pub branching_factor: usize,
}

/// A single hierarchical decision, recorded for later feedback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HierarchicalDecision {
    pub path: DecisionPath,
    pub leaf_name: String,
    /// Per-level meta-bandit candidate id (e.g. `"Thompson"`, `"LinUcb"`).
    pub per_level_candidate_ids: Vec<String>,
}

impl HierarchicalSpec {
    pub fn from_json(j: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value::<HierarchicalSpec>(j.clone())
            .map_err(|e| format!("invalid hierarchical spec JSON: {e}"))
    }

    /// Serialize with camelCase keys (e.g. `subCapsule`).
    pub fn to_json(&self) -> serde_json::Value {
        // Re-serialize each option manually so `subCapsule` is camelCase.
        let opts: Vec<serde_json::Value> = self
            .options
            .iter()
            .map(|opt| match opt {
                HierarchicalOption::BareLeaf(n) => serde_json::json!({ "name": n }),
                HierarchicalOption::Leaf { name } => serde_json::json!({ "name": name }),
                HierarchicalOption::Branch { name, sub_capsule } => serde_json::json!({
                    "name": name,
                    "subCapsule": sub_capsule.to_json(),
                }),
            })
            .collect();
        let mut reward = serde_json::Map::new();
        reward.insert(
            "type".into(),
            serde_json::to_value(self.reward.kind).unwrap_or(serde_json::Value::Null),
        );
        if let Some(r) = self.reward.range {
            reward.insert("range".into(), serde_json::json!([r[0], r[1]]));
        }
        let mut root = serde_json::Map::new();
        root.insert("options".into(), serde_json::Value::Array(opts));
        root.insert("reward".into(), serde_json::Value::Object(reward));
        if let Some(ref rp) = self.reward_propagation {
            root.insert(
                "rewardPropagation".into(),
                serde_json::to_value(rp).unwrap_or(serde_json::Value::Null),
            );
        }
        serde_json::Value::Object(root)
    }

    /// Validate structural invariants (depth, branching, name uniqueness,
    /// leaf count, reward range).
    pub fn validate(&self) -> Result<(), String> {
        self.validate_at(1)?;
        let leaves = self.count_leaves();
        if leaves > MAX_LEAVES {
            return Err(format!(
                "total leaf count {leaves} exceeds maximum {MAX_LEAVES}"
            ));
        }
        Ok(())
    }

    /// Internal recursive validator. `depth` is 1-indexed (root = 1).
    fn validate_at(&self, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err(format!(
                "hierarchical spec depth {depth} exceeds maximum {MAX_DEPTH}"
            ));
        }
        if self.options.len() < MIN_OPTIONS_PER_LEVEL {
            return Err(format!(
                "level at depth {depth} has {} option(s); minimum is {MIN_OPTIONS_PER_LEVEL}",
                self.options.len()
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, opt) in self.options.iter().enumerate() {
            let name = opt.name();
            if name.trim().is_empty() {
                return Err(format!(
                    "option[{i}] at depth {depth} has empty name"
                ));
            }
            if !seen.insert(name.to_string()) {
                return Err(format!(
                    "duplicate option name '{name}' at depth {depth}"
                ));
            }
            if let Some(sub) = opt.sub_capsule() {
                sub.validate_at(depth + 1)?;
            }
        }
        validate_reward(&self.reward, depth)?;
        Ok(())
    }

    /// Total number of reachable leaves across the whole tree.
    pub fn count_leaves(&self) -> usize {
        self.options
            .iter()
            .map(|opt| match opt.sub_capsule() {
                Some(sub) => sub.count_leaves(),
                None => 1,
            })
            .sum()
    }

    /// Maximum depth of the tree (root = 1, a tree with one nested level
    /// reports 2, and so on).
    pub fn max_depth(&self) -> usize {
        1 + self
            .options
            .iter()
            .filter_map(|opt| opt.sub_capsule().map(|s| s.max_depth()))
            .max()
            .unwrap_or(0)
    }

    /// Enumerate every path from root to leaf in deterministic order.
    pub fn enumerate_paths(&self) -> Vec<DecisionPath> {
        let mut out = Vec::new();
        let mut prefix = Vec::new();
        self.enumerate_into(&mut prefix, &mut out);
        out
    }

    fn enumerate_into(&self, prefix: &mut Vec<usize>, out: &mut Vec<DecisionPath>) {
        for (i, opt) in self.options.iter().enumerate() {
            prefix.push(i);
            match opt.sub_capsule() {
                Some(sub) => sub.enumerate_into(prefix, out),
                None => out.push(prefix.clone()),
            }
            prefix.pop();
        }
    }

    /// Resolve a decision path to its leaf option name, or `None` if the
    /// path is empty, out of bounds, too short, or too long.
    pub fn resolve_path(&self, path: &DecisionPath) -> Option<&str> {
        if path.is_empty() {
            return None;
        }
        let mut cur = self;
        for (level, &idx) in path.iter().enumerate() {
            let opt = cur.options.get(idx)?;
            let is_last = level + 1 == path.len();
            match opt.sub_capsule() {
                Some(sub) => {
                    if is_last {
                        // Path ends on a branch — not a valid leaf path.
                        return None;
                    }
                    cur = sub;
                }
                None => {
                    if !is_last {
                        // Path continues past a leaf.
                        return None;
                    }
                    return Some(opt.name());
                }
            }
        }
        None
    }

    /// Return one [`HierState`] per level along `path` (empty if invalid).
    pub fn state_keys_for_path(&self, path: &DecisionPath) -> Vec<HierState> {
        if path.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(path.len());
        let mut cur = self;
        for (level, &idx) in path.iter().enumerate() {
            if idx >= cur.options.len() {
                return Vec::new();
            }
            let branching_factor = cur.options.len();
            let parent_option_path = path[..level].to_vec();
            out.push(HierState {
                depth: level,
                parent_option_path,
                branching_factor,
            });
            let is_last = level + 1 == path.len();
            match cur.options[idx].sub_capsule() {
                Some(sub) => {
                    if is_last {
                        // Path terminates on a branch — invalid.
                        return Vec::new();
                    }
                    cur = sub;
                }
                None => {
                    if !is_last {
                        return Vec::new();
                    }
                }
            }
        }
        out
    }
}

fn validate_reward(reward: &RewardSpec, depth: usize) -> Result<(), String> {
    match reward.kind {
        RewardKind::Continuous | RewardKind::SparseContinuous => {
            let r = reward.range.ok_or_else(|| {
                format!("reward at depth {depth} requires range when type is continuous")
            })?;
            if !(r[0].is_finite() && r[1].is_finite()) {
                return Err(format!("reward.range at depth {depth} is not finite"));
            }
            if r[0] >= r[1] {
                return Err(format!(
                    "reward.range at depth {depth} must satisfy lo < hi"
                ));
            }
        }
        RewardKind::Bernoulli => {}
    }
    Ok(())
}

/// Translate a leaf reward into one `(state, reward)` update per level
/// along the path, honoring the spec's [`RewardPropagation`] mode.
/// Empty vector on invalid path.
pub fn propagate_reward(
    spec: &HierarchicalSpec,
    path: &DecisionPath,
    reward: f64,
) -> Vec<(HierState, f64)> {
    let states = spec.state_keys_for_path(path);
    if states.is_empty() {
        return Vec::new();
    }
    let n = states.len();
    let mode = spec.reward_propagation.clone().unwrap_or_default();
    states
        .into_iter()
        .enumerate()
        .map(|(idx, state)| {
            let level_reward = match &mode {
                RewardPropagation::Full => reward,
                RewardPropagation::Discounted { factor } => {
                    // depth N-1 = leaf (full reward); root attenuated.
                    let exp = (n - 1 - idx) as i32;
                    reward * factor.powi(exp)
                }
            };
            (state, level_reward)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ────────────────────────────────────────────────────────

    fn leaf(name: &str) -> HierarchicalOption {
        HierarchicalOption::Leaf {
            name: name.to_string(),
        }
    }

    fn branch(name: &str, sub: HierarchicalSpec) -> HierarchicalOption {
        HierarchicalOption::Branch {
            name: name.to_string(),
            sub_capsule: Box::new(sub),
        }
    }

    fn cont_reward() -> RewardSpec {
        RewardSpec {
            kind: RewardKind::Continuous,
            range: Some([-1.0, 1.0]),
        }
    }

    /// Build a 2x2 spec: root has two branches, each branch has two leaves.
    fn spec_2x2() -> HierarchicalSpec {
        HierarchicalSpec {
            options: vec![
                branch(
                    "route_to_a",
                    HierarchicalSpec {
                        options: vec![leaf("variant_x"), leaf("variant_y")],
                        reward: cont_reward(),
                        reward_propagation: None,
                    },
                ),
                branch(
                    "route_to_b",
                    HierarchicalSpec {
                        options: vec![leaf("variant_p"), leaf("variant_q")],
                        reward: cont_reward(),
                        reward_propagation: None,
                    },
                ),
            ],
            reward: cont_reward(),
            reward_propagation: None,
        }
    }

    /// Build a 3-level spec (depth 3): each branch nests once more.
    fn spec_3_levels() -> HierarchicalSpec {
        HierarchicalSpec {
            options: vec![
                branch(
                    "a",
                    HierarchicalSpec {
                        options: vec![
                            branch(
                                "a1",
                                HierarchicalSpec {
                                    options: vec![leaf("a1x"), leaf("a1y")],
                                    reward: cont_reward(),
                                    reward_propagation: None,
                                },
                            ),
                            branch(
                                "a2",
                                HierarchicalSpec {
                                    options: vec![leaf("a2x"), leaf("a2y")],
                                    reward: cont_reward(),
                                    reward_propagation: None,
                                },
                            ),
                        ],
                        reward: cont_reward(),
                        reward_propagation: None,
                    },
                ),
                branch(
                    "b",
                    HierarchicalSpec {
                        options: vec![
                            branch(
                                "b1",
                                HierarchicalSpec {
                                    options: vec![leaf("b1x"), leaf("b1y")],
                                    reward: cont_reward(),
                                    reward_propagation: None,
                                },
                            ),
                            branch(
                                "b2",
                                HierarchicalSpec {
                                    options: vec![leaf("b2x"), leaf("b2y")],
                                    reward: cont_reward(),
                                    reward_propagation: None,
                                },
                            ),
                        ],
                        reward: cont_reward(),
                        reward_propagation: None,
                    },
                ),
            ],
            reward: cont_reward(),
            reward_propagation: None,
        }
    }

    /// Convenience to parse from a JSON literal in tests.
    fn parse_json(v: serde_json::Value) -> Result<HierarchicalSpec, String> {
        HierarchicalSpec::from_json(&v)
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[test]
    fn parse_two_level_hierarchy() {
        // Mirrors the YAML in the module doc but as JSON, since Lang
        // doesn't depend on serde_yml.
        let j = serde_json::json!({
            "options": [
                {
                    "name": "route_to_a",
                    "subCapsule": {
                        "options": [{"name": "variant_x"}, {"name": "variant_y"}],
                        "reward": {"type": "continuous", "range": [-1.0, 1.0]}
                    }
                },
                {
                    "name": "route_to_b",
                    "subCapsule": {
                        "options": [{"name": "variant_p"}, {"name": "variant_q"}],
                        "reward": {"type": "continuous", "range": [-1.0, 1.0]}
                    }
                }
            ],
            "reward": {"type": "continuous", "range": [-1.0, 1.0]}
        });
        let spec = parse_json(j).expect("parses");
        spec.validate().expect("valid");
        assert_eq!(spec.max_depth(), 2);
        assert_eq!(spec.count_leaves(), 4);
    }

    #[test]
    fn parse_three_level_hierarchy() {
        let spec = spec_3_levels();
        let j = spec.to_json();
        let round = HierarchicalSpec::from_json(&j).expect("parses");
        round.validate().expect("valid");
        assert_eq!(round.max_depth(), 3);
        assert_eq!(round.count_leaves(), 8);
    }

    #[test]
    fn rejects_five_level_hierarchy() {
        // Build a chain of nested branches 5 deep.
        fn nest(depth_remaining: usize) -> HierarchicalSpec {
            if depth_remaining == 1 {
                HierarchicalSpec {
                    options: vec![leaf("x"), leaf("y")],
                    reward: cont_reward(),
                    reward_propagation: None,
                }
            } else {
                HierarchicalSpec {
                    options: vec![
                        branch("l", nest(depth_remaining - 1)),
                        branch("r", nest(depth_remaining - 1)),
                    ],
                    reward: cont_reward(),
                    reward_propagation: None,
                }
            }
        }
        let spec = nest(5);
        assert_eq!(spec.max_depth(), 5);
        let err = spec.validate().unwrap_err();
        assert!(
            err.contains("depth") && err.contains("maximum"),
            "expected depth-limit error, got: {err}"
        );
    }

    #[test]
    fn rejects_branch_with_one_option() {
        let spec = HierarchicalSpec {
            options: vec![branch(
                "only",
                HierarchicalSpec {
                    options: vec![leaf("solo")],
                    reward: cont_reward(),
                    reward_propagation: None,
                },
            ), leaf("other")],
            reward: cont_reward(),
            reward_propagation: None,
        };
        let err = spec.validate().unwrap_err();
        assert!(err.contains("minimum"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_option_names() {
        let spec = HierarchicalSpec {
            options: vec![leaf("dup"), leaf("dup")],
            reward: cont_reward(),
            reward_propagation: None,
        };
        let err = spec.validate().unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn enumerate_paths_2x2_returns_four() {
        let spec = spec_2x2();
        let paths = spec.enumerate_paths();
        assert_eq!(paths.len(), 4);
        assert_eq!(paths[0], vec![0, 0]);
        assert_eq!(paths[1], vec![0, 1]);
        assert_eq!(paths[2], vec![1, 0]);
        assert_eq!(paths[3], vec![1, 1]);
    }

    #[test]
    fn resolve_path_returns_leaf_or_none() {
        let spec = spec_2x2();
        assert_eq!(spec.resolve_path(&vec![0, 0]), Some("variant_x"));
        assert_eq!(spec.resolve_path(&vec![1, 1]), Some("variant_q"));
        // Out-of-bounds at root.
        assert_eq!(spec.resolve_path(&vec![5, 0]), None);
        // Out-of-bounds at nested level.
        assert_eq!(spec.resolve_path(&vec![0, 7]), None);
        // Path too short — terminates on a branch.
        assert_eq!(spec.resolve_path(&vec![0]), None);
        // Path too long — continues past a leaf.
        assert_eq!(spec.resolve_path(&vec![0, 0, 0]), None);
        // Empty path.
        assert_eq!(spec.resolve_path(&vec![]), None);
    }

    #[test]
    fn propagate_reward_depth3_returns_three_entries() {
        let spec = spec_3_levels();
        let path = vec![0, 1, 0]; // a -> a2 -> a2x
        assert_eq!(spec.resolve_path(&path), Some("a2x"));
        let updates = propagate_reward(&spec, &path, 0.42);
        assert_eq!(updates.len(), 3);

        // Level 0 (root): no parent, 2 options.
        assert_eq!(updates[0].0.depth, 0);
        assert_eq!(updates[0].0.parent_option_path, Vec::<usize>::new());
        assert_eq!(updates[0].0.branching_factor, 2);
        assert!((updates[0].1 - 0.42).abs() < 1e-12);

        // Level 1 (inside 'a'): parent = [0], 2 options.
        assert_eq!(updates[1].0.depth, 1);
        assert_eq!(updates[1].0.parent_option_path, vec![0]);
        assert_eq!(updates[1].0.branching_factor, 2);
        assert!((updates[1].1 - 0.42).abs() < 1e-12);

        // Level 2 (inside 'a2'): parent = [0, 1], 2 options.
        assert_eq!(updates[2].0.depth, 2);
        assert_eq!(updates[2].0.parent_option_path, vec![0, 1]);
        assert_eq!(updates[2].0.branching_factor, 2);
        assert!((updates[2].1 - 0.42).abs() < 1e-12);
    }

    #[test]
    fn propagate_reward_discounted_attenuates_root_relative_to_leaf() {
        // 3-level spec with Discounted { factor: 0.5 }. A leaf reward of
        // 1.0 should credit:
        //   leaf (depth 2): 1.0 * 0.5^0 = 1.0
        //   mid  (depth 1): 1.0 * 0.5^1 = 0.5
        //   root (depth 0): 1.0 * 0.5^2 = 0.25
        let mut spec = spec_3_levels();
        spec.reward_propagation = Some(RewardPropagation::Discounted { factor: 0.5 });
        let path = vec![0, 1, 0];
        let updates = propagate_reward(&spec, &path, 1.0);
        assert_eq!(updates.len(), 3);
        assert!((updates[0].1 - 0.25).abs() < 1e-12,
                "root should attenuate to 0.25, got {}", updates[0].1);
        assert!((updates[1].1 - 0.5).abs() < 1e-12,
                "mid should attenuate to 0.5, got {}", updates[1].1);
        assert!((updates[2].1 - 1.0).abs() < 1e-12,
                "leaf should keep 1.0, got {}", updates[2].1);
    }

    #[test]
    fn propagate_reward_full_is_default_and_returns_raw_reward() {
        // Explicit Full and missing-field both produce the same result:
        // every level sees the input reward unchanged.
        let mut spec = spec_3_levels();
        let path = vec![0, 1, 0];

        spec.reward_propagation = Some(RewardPropagation::Full);
        let explicit = propagate_reward(&spec, &path, 0.42);
        for (_, r) in &explicit { assert!((r - 0.42).abs() < 1e-12); }

        spec.reward_propagation = None;
        let defaulted = propagate_reward(&spec, &path, 0.42);
        for (_, r) in &defaulted { assert!((r - 0.42).abs() < 1e-12); }

        // Factor 1.0 must be equivalent to Full as a smoke check on
        // the math edge case.
        spec.reward_propagation = Some(RewardPropagation::Discounted { factor: 1.0 });
        let factor_one = propagate_reward(&spec, &path, 0.42);
        for (_, r) in &factor_one { assert!((r - 0.42).abs() < 1e-12); }
    }

    #[test]
    fn reward_propagation_round_trips_through_json() {
        // The optional field must serialize when set (so sidecars carry
        // the operator's choice) and be parseable in both
        // `{"mode": "full"}` and `{"mode": "discounted", "factor": 0.7}`
        // forms.
        let mut spec = spec_2x2();
        spec.reward_propagation = Some(RewardPropagation::Discounted { factor: 0.7 });
        let j = spec.to_json();
        assert!(j.to_string().contains("rewardPropagation"),
                "discounted setting must appear in serialized form: {}", j);
        let round = HierarchicalSpec::from_json(&j).expect("round-trip");
        match round.reward_propagation {
            Some(RewardPropagation::Discounted { factor }) => {
                assert!((factor - 0.7).abs() < 1e-12);
            }
            other => panic!("expected Discounted{{0.7}}, got {other:?}"),
        }

        // Absent field round-trips back to None (not Some(Full)).
        let mut spec_clean = spec_2x2();
        spec_clean.reward_propagation = None;
        let j_clean = spec_clean.to_json();
        assert!(!j_clean.to_string().contains("rewardPropagation"),
                "absent field must not appear in serialized form: {}", j_clean);
        let round_clean = HierarchicalSpec::from_json(&j_clean).expect("round-trip");
        assert!(round_clean.reward_propagation.is_none());
    }

    #[test]
    fn json_roundtrip_preserves_tree() {
        let original = spec_3_levels();
        let j = original.to_json();
        // Ensure camelCase `subCapsule` appears in the serialized form.
        let s = j.to_string();
        assert!(s.contains("subCapsule"), "expected camelCase key: {s}");
        let round = HierarchicalSpec::from_json(&j).expect("round-trips");
        round.validate().expect("valid");
        assert_eq!(round, original);
        assert_eq!(round.enumerate_paths(), original.enumerate_paths());
    }

    #[test]
    fn rejects_continuous_reward_without_range() {
        let spec = HierarchicalSpec {
            options: vec![leaf("a"), leaf("b")],
            reward: RewardSpec {
                kind: RewardKind::Continuous,
                range: None,
            },
            reward_propagation: None,
        };
        let err = spec.validate().unwrap_err();
        assert!(err.contains("range"), "got: {err}");
    }

    #[test]
    fn state_keys_match_propagate_for_invalid_path() {
        let spec = spec_2x2();
        // Out-of-bounds index — both should report empty.
        let bad: DecisionPath = vec![9, 0];
        assert!(spec.state_keys_for_path(&bad).is_empty());
        assert!(propagate_reward(&spec, &bad, 0.5).is_empty());
    }

    #[test]
    fn bare_leaf_form_parses() {
        // YAML allows `- variant_x` instead of `- name: variant_x`; the
        // untagged enum supports both. JSON literal form here uses a bare
        // string in the options array.
        let j = serde_json::json!({
            "options": ["variant_x", "variant_y"],
            "reward": {"type": "continuous", "range": [-1.0, 1.0]}
        });
        let spec = parse_json(j).expect("parses");
        spec.validate().expect("valid");
        assert_eq!(spec.options.len(), 2);
        assert_eq!(spec.options[0].name(), "variant_x");
        assert!(spec.options[0].is_leaf());
    }
}
