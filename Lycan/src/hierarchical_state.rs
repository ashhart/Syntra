//! Persisted runtime state for hierarchical-bandit capsules (Syntra Prompt 2D).
//!
//! Wraps a [`crate::hierarchical::HierarchicalSpec`] with one bandit
//! "bucket" per reachable [`HierState`] in the decision tree. The bucket
//! mirrors the flat-bandit `ContextBucket` shape from
//! [`crate::learning`] — weights for selection, [`OptionStats`] for
//! audit, and a [`MetaBandit`] for portfolio-level meta-learning — but
//! one per level rather than one per `contextKey`.
//!
//! ## Lifecycle
//!
//! - Construct from a validated `HierarchicalSpec` with [`HierarchicalCapsuleState::new`].
//! - Buckets are populated **lazily**: the first time `select_path`
//!   walks past a level whose [`HierStateKey`] isn't in the map, a
//!   uniform-weighted bucket is allocated. This keeps the on-disk
//!   footprint to the paths the capsule actually explores.
//! - [`HierarchicalCapsuleState::to_json`] /
//!   [`HierarchicalCapsuleState::from_json`] persist the whole state
//!   alongside the rest of the capsule sidecar. Format mirrors
//!   `HierarchicalSpec::to_json` (camelCase, opaque to the server).
//!
//! ## v1 selection
//!
//! At every level the bucket's [`MetaBandit`] is consulted for
//! selection-history accounting, but the actual arm pick is a simple
//! weighted-random draw over the bucket's `weights`. Per-candidate
//! selection inside each level's bucket is roadmap work; see the
//! integration plan accompanying this prep.
//!
//! ## v1 credit assignment
//!
//! Matches [`crate::hierarchical::propagate_reward`]: the observed leaf
//! reward is applied unchanged at every level along the chosen path.
//! Each level's weights, [`OptionStats`], and [`MetaBandit`] all see the
//! same reward value. Eligibility-trace / doubly-robust variants live in
//! the same roadmap slot as the per-candidate selection.

use std::collections::HashMap;

use serde_json::Value;

use crate::hierarchical::{
    DecisionPath, HierState, HierarchicalDecision, HierarchicalSpec,
};
use crate::learning::OptionStats;
use crate::meta_bandit::{CandidateId, MetaBandit};

// ── Keys ────────────────────────────────────────────────────────────────

/// Serialisable string form of a [`HierState`].
///
/// HashMap-keyed JSON requires string keys, so we flatten the
/// `(depth, parent_option_path)` pair into a single string of the form
/// `"d{depth}|{i0,i1,...}"`. The root key is `"d0|"`; the bucket inside
/// option index 1 at depth 1 is `"d1|1"`; the bucket at depth 2 below
/// path `[0, 1]` is `"d2|0,1"`.
///
/// The `branching_factor` is not encoded in the key because it is fixed
/// by the spec at any given `(depth, parent_path)` and is therefore
/// already recovered when the spec is loaded alongside the state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HierStateKey(pub String);

impl HierStateKey {
    /// Build a key from a [`HierState`] view.
    pub fn from_state(state: &HierState) -> Self {
        Self::from_parts(state.depth, &state.parent_option_path)
    }

    /// Build a key from a `(depth, parent_option_path)` pair without
    /// constructing the intermediate [`HierState`].
    pub fn from_parts(depth: usize, parent_option_path: &[usize]) -> Self {
        let parents: Vec<String> = parent_option_path
            .iter()
            .map(|i| i.to_string())
            .collect();
        Self(format!("d{}|{}", depth, parents.join(",")))
    }

    /// String form (round-trippable through JSON).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&HierState> for HierStateKey {
    fn from(s: &HierState) -> Self {
        HierStateKey::from_state(s)
    }
}

// ── Bucket ──────────────────────────────────────────────────────────────

/// One bandit bucket inside the hierarchical state.
///
/// Holds the per-arm `weights` used by the v1 weighted-random selector,
/// the corresponding [`OptionStats`] for inspection / future selectors,
/// and the per-level [`MetaBandit`] for selection-history accounting.
#[derive(Debug, Clone)]
pub struct HierBucket {
    /// Per-arm selection weights, length `== branching_factor`.
    /// Initialised uniformly at `1.0 / n`.
    pub weights: Vec<f64>,
    /// Per-arm cumulative stats, length `== branching_factor`.
    pub stats: Vec<OptionStats>,
    /// Per-level meta-bandit (discrete portfolio; same default as the
    /// flat capsule path).
    pub meta_bandit: MetaBandit,
}

impl HierBucket {
    /// Allocate a fresh bucket of the given branching factor with
    /// uniform weights.
    pub fn new(branching_factor: usize) -> Self {
        let n = branching_factor.max(1);
        let init = 1.0 / n as f64;
        Self {
            weights: vec![init; n],
            stats: (0..n).map(|_| OptionStats::default()).collect(),
            meta_bandit: MetaBandit::new(),
        }
    }

    /// Total weight (used by the weighted-random selector).
    fn weight_sum(&self) -> f64 {
        self.weights.iter().copied().sum()
    }
}

// ── State ───────────────────────────────────────────────────────────────

/// Persisted runtime state for a hierarchical-bandit capsule.
#[derive(Debug, Clone)]
pub struct HierarchicalCapsuleState {
    /// Parsed and validated spec. Owned so the state is self-sufficient
    /// across persistence cycles.
    pub spec: HierarchicalSpec,
    /// One bucket per reachable [`HierStateKey`] that has been touched
    /// by selection so far. Populated lazily.
    pub buckets: HashMap<String, HierBucket>,
}

impl HierarchicalCapsuleState {
    /// Construct from a (validated) spec. Buckets start empty and are
    /// allocated on first selection / feedback at each level.
    ///
    /// Callers should run `spec.validate()` before passing the spec in;
    /// `new` itself does not re-validate (matching the pattern used in
    /// the rest of the Lang crate).
    pub fn new(spec: HierarchicalSpec) -> Self {
        Self { spec, buckets: HashMap::new() }
    }

    /// Walk the option tree and pick a leaf path.
    ///
    /// `rng_pair` is a closure that returns `(rng_value_for_explore_vs_exploit,
    /// rng_value_for_pick)` on each call. In production this should call
    /// [`crate::learning::rand_f64`] twice; tests pass a seeded variant.
    ///
    /// Returns `None` if the spec is empty (which validation already
    /// forbids, so in practice this is unreachable from a valid capsule).
    pub fn select_path<F>(&mut self, mut rng_pair: F) -> Option<HierarchicalDecision>
    where
        F: FnMut() -> (f64, f64),
    {
        // Read-only walks against the spec; the spec is owned so we can
        // re-borrow each level without aliasing concerns.
        let mut path: DecisionPath = Vec::new();
        let mut per_level_candidate_ids: Vec<String> = Vec::new();

        // Track the current sub-spec by re-descending through the spec
        // each level — avoids holding an immutable borrow while we
        // mutably touch `self.buckets`.
        loop {
            // Resolve the current sub-spec from `path` so far.
            let cur = match resolve_sub_spec(&self.spec, &path) {
                Some(s) => s,
                None => return None,
            };
            let bf = cur.options.len();
            if bf == 0 {
                return None;
            }
            let key = HierStateKey::from_parts(path.len(), &path).0;
            let bucket = self
                .buckets
                .entry(key)
                .or_insert_with(|| HierBucket::new(bf));

            let (r_meta_explore, r_meta_pick) = rng_pair();
            let (candidate, _exploratory) = bucket.meta_bandit.select(r_meta_explore, r_meta_pick);
            per_level_candidate_ids.push(candidate.as_str().to_string());

            // v1: weighted-random over bucket weights (candidate id
            // reserved for meta-bandit history; per-candidate selection
            // inside each level is roadmap work).
            let (_r_pick_unused, r_arm) = rng_pair();
            let chosen = select_weighted_index(&bucket.weights, r_arm);
            path.push(chosen);

            // Did we land on a leaf?
            match cur.options[chosen].sub_capsule() {
                Some(_sub) => continue,
                None => {
                    let leaf_name = cur.options[chosen].name().to_string();
                    return Some(HierarchicalDecision {
                        path,
                        leaf_name,
                        per_level_candidate_ids,
                    });
                }
            }
        }
    }

    /// Apply a single observed reward by propagating it to every level
    /// along `path`.
    ///
    /// `chosen_per_level` must be the same `path` that came back from
    /// the prior `select_path` call — it is the per-level option index
    /// the bandit picked. The two arguments are kept separate so the
    /// server can record both the resolved path and the original
    /// chosen-index audit trail without re-deriving one from the other.
    ///
    /// Returns the per-level `(HierState, reward)` updates that were
    /// applied — useful as the audit-event payload.
    pub fn apply_feedback(
        &mut self,
        path: &DecisionPath,
        chosen_per_level: &[usize],
        reward: f64,
    ) -> Vec<(HierState, f64)> {
        self.apply_feedback_inner(path, chosen_per_level, None, reward)
    }

    /// Same as [`apply_feedback`] but credits the exact per-level
    /// meta-bandit candidate that fired at decide time, recovered from
    /// the decision-log's `perLevelCandidateIds` field rather than
    /// proxied through `current_leader()`.
    ///
    /// `per_level_candidates` must have the same length as `path`. If
    /// it doesn't, this falls back to the greedy-proxy behaviour for
    /// the mismatched levels. This is the function the server's
    /// `do_feedback_hierarchical` calls; the bare `apply_feedback`
    /// remains for math-layer tests that don't track per-level
    /// candidate provenance.
    pub fn apply_feedback_with_candidates(
        &mut self,
        path: &DecisionPath,
        chosen_per_level: &[usize],
        per_level_candidates: &[CandidateId],
        reward: f64,
    ) -> Vec<(HierState, f64)> {
        self.apply_feedback_inner(
            path,
            chosen_per_level,
            Some(per_level_candidates),
            reward,
        )
    }

    fn apply_feedback_inner(
        &mut self,
        path: &DecisionPath,
        chosen_per_level: &[usize],
        per_level_candidates: Option<&[CandidateId]>,
        reward: f64,
    ) -> Vec<(HierState, f64)> {
        // Use `propagate_reward` for the canonical per-level reward.
        // With `RewardPropagation::Full` this is the input reward at
        // every level (backward-compatible); with
        // `RewardPropagation::Discounted { factor }` the root is
        // attenuated relative to the leaf by `factor^(N-1-depth)`.
        let propagated: Vec<(HierState, f64)> =
            crate::hierarchical::propagate_reward(&self.spec, path, reward);
        if propagated.is_empty() {
            return Vec::new();
        }
        if chosen_per_level.len() != propagated.len() {
            return Vec::new();
        }

        // If candidate ids were supplied but their length doesn't match
        // the path, fall back to the greedy proxy for the whole path
        // rather than silently using a partial mapping. A mismatch is
        // almost always a sign the decision log + spec drifted apart.
        let candidates_arr: Option<&[CandidateId]> = match per_level_candidates {
            Some(c) if c.len() == propagated.len() => Some(c),
            _ => None,
        };

        let mut out = Vec::with_capacity(propagated.len());

        for (level, (state, level_reward)) in propagated.into_iter().enumerate() {
            let chosen = chosen_per_level[level];
            let key = HierStateKey::from_state(&state).0;
            let bucket = self
                .buckets
                .entry(key)
                .or_insert_with(|| HierBucket::new(state.branching_factor));

            if chosen >= bucket.weights.len() {
                continue;
            }

            // Weighted-update step: nudge the chosen arm's weight
            // toward the (per-level) observed reward and renormalize.
            // Same shape as the flat-bandit `SimpleWeighted` update.
            let learning_rate = 0.1_f64;
            let w = bucket.weights[chosen];
            bucket.weights[chosen] = w + learning_rate * (level_reward - w);
            // Floor to keep weights non-negative for the weighted
            // sampler. Tiny epsilon preserves nonzero exploration.
            for w in bucket.weights.iter_mut() {
                if *w < 1e-6 { *w = 1e-6; }
            }
            let s = bucket.weight_sum();
            if s > 0.0 {
                for w in bucket.weights.iter_mut() {
                    *w /= s;
                }
            }

            // Update per-arm stats (mirror the flat-bandit single-arm
            // bookkeeping that downstream inspection tools rely on).
            if let Some(stat) = bucket.stats.get_mut(chosen) {
                stat.tries = stat.tries.saturating_add(1);
                stat.reward_sum += level_reward;
                stat.reward_sq_sum += level_reward * level_reward;
                stat.last_reward = level_reward;
                stat.effective_tries += 1.0;
            }

            // Meta-bandit credit assignment. With an explicit per-level
            // candidate id (threaded from the decision-log's
            // `perLevelCandidateIds`), credit goes to the candidate that
            // actually fired at decide time. Without one — backwards-
            // compat path for math-layer tests — fall back to the
            // greedy-leader proxy so total_rounds still advances and
            // the leader gets some bias.
            let chosen_candidate = match candidates_arr {
                Some(c) => c[level],
                None => bucket
                    .meta_bandit
                    .current_leader()
                    .unwrap_or(CandidateId::Thompson),
            };
            bucket.meta_bandit.record(chosen_candidate, level_reward);

            out.push((state, level_reward));
        }
        out
    }

    // ── JSON ─────────────────────────────────────────────────────────

    /// Serialize to a `serde_json::Value` for the sidecar store.
    pub fn to_json(&self) -> Value {
        let mut bucket_obj = serde_json::Map::new();
        for (k, b) in &self.buckets {
            let stats: Vec<Value> = b.stats.iter().map(|s| s.to_json()).collect();
            let meta = serde_json::to_value(&b.meta_bandit)
                .unwrap_or(Value::Null);
            bucket_obj.insert(
                k.clone(),
                serde_json::json!({
                    "weights": b.weights,
                    "stats": stats,
                    "metaBandit": meta,
                }),
            );
        }
        serde_json::json!({
            "spec": self.spec.to_json(),
            "buckets": Value::Object(bucket_obj),
        })
    }

    /// Parse from a `serde_json::Value`. Returns `Err` if `spec` is
    /// missing or invalid. Missing / malformed bucket entries are
    /// silently skipped (lazy re-allocation handles them at next
    /// selection).
    pub fn from_json(j: &Value) -> Result<Self, String> {
        let spec_v = j.get("spec").ok_or_else(|| "missing 'spec' field".to_string())?;
        let spec = HierarchicalSpec::from_json(spec_v)?;
        let mut buckets: HashMap<String, HierBucket> = HashMap::new();
        if let Some(obj) = j.get("buckets").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                let weights: Vec<f64> = v
                    .get("weights")
                    .and_then(|w| w.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
                    .unwrap_or_default();
                if weights.is_empty() {
                    continue;
                }
                let stats: Vec<OptionStats> = v
                    .get("stats")
                    .and_then(|s| s.as_array())
                    .map(|a| a.iter().map(OptionStats::from_json).collect())
                    .unwrap_or_else(|| (0..weights.len()).map(|_| OptionStats::default()).collect());
                let meta_bandit = v
                    .get("metaBandit")
                    .cloned()
                    .and_then(|m| serde_json::from_value::<MetaBandit>(m).ok())
                    .unwrap_or_else(MetaBandit::new);
                buckets.insert(
                    k.clone(),
                    HierBucket { weights, stats, meta_bandit },
                );
            }
        }
        Ok(Self { spec, buckets })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Descend the spec following `prefix` indices and return the sub-spec
/// at the resulting level (the level whose `options` are still pending
/// a choice). Returns `None` on an out-of-bounds or
/// past-a-leaf prefix.
fn resolve_sub_spec<'a>(
    spec: &'a HierarchicalSpec,
    prefix: &[usize],
) -> Option<&'a HierarchicalSpec> {
    let mut cur = spec;
    for &idx in prefix {
        let opt = cur.options.get(idx)?;
        cur = opt.sub_capsule()?;
    }
    Some(cur)
}

/// Pick an index proportional to `weights`. `r` is a uniform `[0, 1)`
/// draw. Falls through to the last index on rounding error.
fn select_weighted_index(weights: &[f64], r: f64) -> usize {
    if weights.is_empty() {
        return 0;
    }
    let sum: f64 = weights.iter().sum();
    if sum <= 0.0 {
        return 0;
    }
    let target = r * sum;
    let mut cum = 0.0;
    for (i, &w) in weights.iter().enumerate() {
        cum += w;
        if target < cum {
            return i;
        }
    }
    weights.len() - 1
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchical::{HierarchicalOption, RewardKind, RewardSpec};

    fn cont_reward() -> RewardSpec {
        RewardSpec { kind: RewardKind::Continuous, range: Some([-1.0, 1.0]) }
    }

    fn leaf(name: &str) -> HierarchicalOption {
        HierarchicalOption::Leaf { name: name.to_string() }
    }

    fn branch(name: &str, sub: HierarchicalSpec) -> HierarchicalOption {
        HierarchicalOption::Branch {
            name: name.to_string(),
            sub_capsule: Box::new(sub),
        }
    }

    /// 2 (region) × 3 (server type) = 6 leaves, matching the example
    /// capsule shipped alongside this prep.
    fn spec_2x3() -> HierarchicalSpec {
        HierarchicalSpec {
            options: vec![
                branch(
                    "us-east",
                    HierarchicalSpec {
                        options: vec![leaf("small"), leaf("medium"), leaf("large")],
                        reward: cont_reward(),
                        reward_propagation: None,
                    },
                ),
                branch(
                    "eu-west",
                    HierarchicalSpec {
                        options: vec![leaf("small"), leaf("medium"), leaf("large")],
                        reward: cont_reward(),
                        reward_propagation: None,
                    },
                ),
            ],
            reward: cont_reward(),
            reward_propagation: None,
        }
    }

    /// Tiny deterministic RNG so the unit test is reproducible. Returns
    /// pairs of independent `[0, 1)` draws.
    fn seeded_rng(seed: u64) -> impl FnMut() -> (f64, f64) {
        let mut state: u64 = seed.max(1);
        move || {
            let step = |s: &mut u64| -> f64 {
                *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((*s >> 11) as u32) as f64 / (u32::MAX as f64 + 1.0)
            };
            (step(&mut state), step(&mut state))
        }
    }

    #[test]
    fn key_round_trips_via_state() {
        let s = HierState {
            depth: 2,
            parent_option_path: vec![0, 1],
            branching_factor: 3,
        };
        let k = HierStateKey::from_state(&s);
        assert_eq!(k.as_str(), "d2|0,1");
        let k2 = HierStateKey::from_parts(0, &[]);
        assert_eq!(k2.as_str(), "d0|");
    }

    #[test]
    fn new_state_has_no_buckets_until_selection() {
        let s = HierarchicalCapsuleState::new(spec_2x3());
        assert!(s.buckets.is_empty(), "buckets must be allocated lazily");
    }

    #[test]
    fn first_selection_allocates_two_buckets() {
        let mut s = HierarchicalCapsuleState::new(spec_2x3());
        let mut rng = seeded_rng(1);
        let dec = s
            .select_path(|| rng())
            .expect("a 2x3 spec must produce a path");
        assert_eq!(dec.path.len(), 2, "depth-2 spec yields a length-2 path");
        assert_eq!(dec.per_level_candidate_ids.len(), 2);
        // Root bucket plus exactly one second-level bucket (the one
        // reachable along the chosen root option).
        assert_eq!(s.buckets.len(), 2);
        assert!(s.buckets.contains_key("d0|"));
        let second_key = format!("d1|{}", dec.path[0]);
        assert!(
            s.buckets.contains_key(&second_key),
            "expected second-level bucket {second_key}, have {:?}",
            s.buckets.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn weighted_index_picks_endpoints_correctly() {
        // Uniform [0.5, 0.5]: r=0.0 -> 0, r=0.99 -> 1.
        assert_eq!(select_weighted_index(&[0.5, 0.5], 0.0), 0);
        assert_eq!(select_weighted_index(&[0.5, 0.5], 0.99), 1);
        // Skewed [0.9, 0.1]: r=0.5 falls inside the first bucket.
        assert_eq!(select_weighted_index(&[0.9, 0.1], 0.5), 0);
    }

    #[test]
    fn json_round_trip_preserves_state() {
        let mut s = HierarchicalCapsuleState::new(spec_2x3());
        let mut rng = seeded_rng(7);
        // Touch a few buckets so the serialized form actually carries
        // state.
        for _ in 0..5 {
            let dec = s.select_path(|| rng()).expect("decision");
            s.apply_feedback(&dec.path, &dec.path, 0.42);
        }
        let j = s.to_json();
        // Ensure the camelCase 'metaBandit' / 'subCapsule' keys appear.
        let txt = j.to_string();
        assert!(txt.contains("metaBandit"), "{txt}");
        assert!(txt.contains("subCapsule"), "{txt}");
        let round = HierarchicalCapsuleState::from_json(&j)
            .expect("round-trips");
        assert_eq!(round.buckets.len(), s.buckets.len());
        for (k, b) in &s.buckets {
            let rb = round.buckets.get(k).expect("bucket survives");
            assert_eq!(rb.weights.len(), b.weights.len());
            for (a, b) in rb.weights.iter().zip(b.weights.iter()) {
                assert!((a - b).abs() < 1e-9, "weights diverged: {a} vs {b}");
            }
            assert_eq!(rb.meta_bandit.total_rounds, b.meta_bandit.total_rounds);
        }
    }

    #[test]
    fn invalid_feedback_path_returns_empty() {
        let mut s = HierarchicalCapsuleState::new(spec_2x3());
        // Out-of-bounds at the root.
        let updates = s.apply_feedback(&vec![9, 0], &[9, 0], 0.5);
        assert!(updates.is_empty());
        // Length mismatch between path and chosen_per_level.
        let updates = s.apply_feedback(&vec![0, 0], &[0], 0.5);
        assert!(updates.is_empty());
    }

    /// `apply_feedback_with_candidates` must credit the candidate
    /// supplied at each level, not the meta-bandit's current leader.
    /// This is the exact-credit-assignment fix from Phase I followup 14.
    #[test]
    fn apply_feedback_discounted_attenuates_root_bucket_stats() {
        // With Discounted { factor: 0.5 } in a 2-level (2x3) spec, a
        // leaf reward of 1.0 should land on the root bucket as 0.5 and
        // on the sub-bucket as 1.0. The stat accumulator picks this up
        // directly (reward_sum on the chosen arm).
        let mut spec = spec_2x3();
        spec.reward_propagation = Some(
            crate::hierarchical::RewardPropagation::Discounted { factor: 0.5 }
        );
        let mut s = HierarchicalCapsuleState::new(spec);

        // Apply feedback for path [0, 1] (us-east / medium) once.
        let path = vec![0, 1];
        let updates = s.apply_feedback(&path, &path, 1.0);
        assert_eq!(updates.len(), 2);
        // Returned per-level rewards reflect the discount.
        assert!((updates[0].1 - 0.5).abs() < 1e-12,
                "root level should see 0.5, got {}", updates[0].1);
        assert!((updates[1].1 - 1.0).abs() < 1e-12,
                "leaf level should see 1.0, got {}", updates[1].1);

        // The persisted bucket stats also reflect the discount: the
        // root bucket's chosen-arm reward_sum is 0.5, not 1.0.
        let root = s.buckets.get("d0|").expect("root bucket allocated");
        let root_sum = root.stats.get(0).map(|st| st.reward_sum).unwrap_or(0.0);
        assert!((root_sum - 0.5).abs() < 1e-12,
                "root chosen-arm reward_sum should be 0.5, got {root_sum}");

        let mid = s.buckets.get("d1|0").expect("us-east bucket allocated");
        let mid_sum = mid.stats.get(1).map(|st| st.reward_sum).unwrap_or(0.0);
        assert!((mid_sum - 1.0).abs() < 1e-12,
                "leaf chosen-arm reward_sum should be 1.0, got {mid_sum}");
    }

    #[test]
    fn apply_feedback_with_candidates_credits_supplied_candidate() {
        let mut s = HierarchicalCapsuleState::new(spec_2x3());
        // Walk path [0, 1] (us-east / medium) and credit two distinct,
        // non-leader candidates so the test can prove the supplied id
        // is what landed in the meta-bandit.
        let path = vec![0, 1];
        let candidates = vec![CandidateId::Weighted, CandidateId::EpsilonGreedy];
        let updates = s.apply_feedback_with_candidates(&path, &path, &candidates, 0.7);
        assert_eq!(updates.len(), 2, "feedback should hit both levels");

        // Root bucket must have credited Weighted (level 0).
        let root_bucket = s.buckets.get("d0|").expect("root bucket allocated");
        let weighted_trials = root_bucket
            .meta_bandit
            .candidates
            .iter()
            .find(|c| c.id == CandidateId::Weighted)
            .map(|c| c.trials)
            .unwrap_or(0.0);
        assert!(weighted_trials > 0.5,
                "root meta-bandit must have credited Weighted, got trials={weighted_trials}");

        // us-east bucket (depth 1, parent_path=[0]) must have credited
        // EpsilonGreedy (level 1).
        let us_bucket = s.buckets.get("d1|0").expect("us-east bucket allocated");
        let eg_trials = us_bucket
            .meta_bandit
            .candidates
            .iter()
            .find(|c| c.id == CandidateId::EpsilonGreedy)
            .map(|c| c.trials)
            .unwrap_or(0.0);
        assert!(eg_trials > 0.5,
                "us-east meta-bandit must have credited EpsilonGreedy, got trials={eg_trials}");
    }

    /// Length mismatch on the candidates array falls back to the
    /// greedy proxy rather than silently using a partial mapping —
    /// it's a data-integrity signal, not silent degradation.
    #[test]
    fn apply_feedback_with_candidates_falls_back_on_length_mismatch() {
        let mut s = HierarchicalCapsuleState::new(spec_2x3());
        let path = vec![0, 1];
        // Only one candidate supplied for a two-level path → fall back.
        let candidates = vec![CandidateId::Weighted];
        let updates = s.apply_feedback_with_candidates(&path, &path, &candidates, 0.7);
        assert_eq!(updates.len(), 2, "fallback path should still update both levels");
        // The proxy credits the current leader, which for a fresh
        // meta-bandit (no trials yet) falls back to Thompson via
        // unwrap_or. So both buckets credit Thompson, not Weighted.
        let root_bucket = s.buckets.get("d0|").expect("root bucket allocated");
        let weighted_trials = root_bucket
            .meta_bandit
            .candidates
            .iter()
            .find(|c| c.id == CandidateId::Weighted)
            .map(|c| c.trials)
            .unwrap_or(0.0);
        assert!(weighted_trials < 1e-9,
                "fallback must NOT credit the supplied Weighted candidate; got trials={weighted_trials}");
    }

    /// End-to-end 200-round validation per the prep brief: reward the
    /// `[0, 1]` (us-east / medium) path at 1.0, every other path at
    /// 0.1, and check that both the root and the us-east level have
    /// learned the right arm.
    #[test]
    fn learns_preferred_path_over_200_rounds() {
        let mut s = HierarchicalCapsuleState::new(spec_2x3());
        let mut rng = seeded_rng(42);
        for _ in 0..200 {
            let dec = s.select_path(|| rng()).expect("decision");
            let reward = if dec.path == vec![0, 1] { 1.0 } else { 0.1 };
            s.apply_feedback(&dec.path, &dec.path, reward);
        }
        // Root: us-east (0) should outweigh eu-west (1).
        let root = s.buckets.get("d0|").expect("root bucket exists");
        assert!(
            root.weights[0] > root.weights[1],
            "root weights[0]={:.4} weights[1]={:.4}",
            root.weights[0], root.weights[1]
        );
        // us-east level: medium (1) should outweigh small (0) and large (2).
        let east = s.buckets.get("d1|0").expect("us-east bucket exists");
        assert!(
            east.weights[1] > east.weights[0],
            "us-east weights[1]={:.4} weights[0]={:.4}",
            east.weights[1], east.weights[0]
        );
        assert!(
            east.weights[1] > east.weights[2],
            "us-east weights[1]={:.4} weights[2]={:.4}",
            east.weights[1], east.weights[2]
        );
        // Both levels should have received meta-bandit updates.
        assert!(root.meta_bandit.total_rounds > 0);
        assert!(east.meta_bandit.total_rounds > 0);
    }
}
