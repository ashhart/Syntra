//! End-to-end checks of the v2 decision core through `Engine::decide` and
//! `Engine::learn` on synthetic contextual bandits with fixed seeds.
//!
//! Run with `--nocapture` to see the measured numbers.

use serde_json::{Value, json};
use syntra::decision::{
    ActionSpec, DecideError, DecideInput, Decision, DecisionSpec, Engine, SplitMix64,
};

fn engine(spec: Value) -> Engine {
    Engine::new(DecisionSpec::from_json(&spec).unwrap()).unwrap()
}

fn context(value: Value) -> DecideInput {
    DecideInput {
        context: value,
        ..DecideInput::default()
    }
}

fn bernoulli(rng: &mut SplitMix64, p: f64) -> f64 {
    if rng.next_f64() < p { 1.0 } else { 0.0 }
}

/// Expected reward of the decision's PMF under the true means.
fn policy_value(d: &Decision, means: &[f64]) -> f64 {
    d.eligible
        .iter()
        .zip(&d.pmf)
        .map(|(&i, p)| p * means[i])
        .sum()
}

fn max(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// Averages over the evaluation window.
#[derive(Debug, Default)]
struct Window {
    rounds: usize,
    realized: f64,
    policy: f64,
    optimal: f64,
    uniform: f64,
}

impl Window {
    fn add(&mut self, realized: f64, d: &Decision, means: &[f64]) {
        self.rounds += 1;
        self.realized += realized;
        self.policy += policy_value(d, means);
        self.optimal += max(means);
        self.uniform += mean(means);
    }

    fn averages(&self) -> (f64, f64, f64, f64) {
        let n = self.rounds as f64;
        (
            self.realized / n,
            self.policy / n,
            self.optimal / n,
            self.uniform / n,
        )
    }
}

const ROUNDS: usize = 6000;
const WINDOW: usize = 1000;

/// (a) Categorical context: the best of three actions differs per segment.
#[test]
fn learns_the_best_action_per_categorical_segment() {
    const SEGMENTS: [&str; 3] = ["A", "B", "C"];
    // Bernoulli reward means: MEANS[segment][action].
    const MEANS: [[f64; 3]; 3] = [[0.8, 0.5, 0.3], [0.3, 0.75, 0.5], [0.4, 0.5, 0.7]];
    let mut e = engine(json!({"actions": [{"id": "a0"}, {"id": "a1"}, {"id": "a2"}]}));
    let mut env = SplitMix64::new(101);
    let mut seeds = SplitMix64::new(202);
    let mut window = Window::default();
    for t in 0..ROUNDS {
        let s = (env.next_u64() % 3) as usize;
        let input = context(json!({"segment": SEGMENTS[s]}));
        let d = e.decide(&input, seeds.next_u64()).unwrap();
        let reward = bernoulli(&mut env, MEANS[s][d.chosen]);
        e.learn(
            &input.context,
            &input.derived,
            d.chosen_action(),
            reward,
            d.probability,
        )
        .unwrap();
        if t >= ROUNDS - WINDOW {
            window.add(reward, &d, &MEANS[s]);
        }
    }
    let (realized, policy, optimal, uniform) = window.averages();
    println!(
        "(a) categorical: realized {realized:.4}, policy {policy:.4}, optimal {optimal:.4}, \
         uniform {uniform:.4}, realized/optimal {:.3}",
        realized / optimal
    );
    assert!(
        realized >= 0.9 * optimal,
        "realized {realized} vs optimal {optimal}"
    );
    for (s, segment) in SEGMENTS.iter().enumerate() {
        let d = e.decide(&context(json!({"segment": segment})), 0).unwrap();
        let (best, p) = d.ranking()[0];
        println!(
            "    segment {segment}: most probable a{best} (p = {p:.3}), predictions {:.3?}",
            d.predictions
        );
        assert_eq!(best, s, "segment {segment}: {:?}", d.pmf);
    }
}

/// True means of the continuous problem: two crossing lines and a flat
/// action that is best in the middle third.
fn linear_means(x: f64) -> [f64; 3] {
    [0.9 - 0.8 * x, 0.1 + 0.8 * x, 0.6]
}

/// Runs the continuous-context problem, presenting `x` multiplied by
/// `scale` to the learner.
fn run_continuous(scale: f64) -> (Engine, Window) {
    let mut e = engine(json!({"actions": [{"id": "a0"}, {"id": "a1"}, {"id": "a2"}]}));
    let mut env = SplitMix64::new(303);
    let mut seeds = SplitMix64::new(404);
    let mut window = Window::default();
    for t in 0..ROUNDS {
        let x = env.next_f64();
        let means = linear_means(x);
        let input = context(json!({"x": x * scale}));
        let d = e.decide(&input, seeds.next_u64()).unwrap();
        let reward = bernoulli(&mut env, means[d.chosen]);
        e.learn(
            &input.context,
            &input.derived,
            d.chosen_action(),
            reward,
            d.probability,
        )
        .unwrap();
        if t >= ROUNDS - WINDOW {
            window.add(reward, &d, &means);
        }
    }
    (e, window)
}

/// Probe points well inside each regime, with the true best action.
const PROBES: [(f64, usize); 3] = [(0.1, 0), (0.5, 2), (0.9, 1)];

/// (b) Continuous context: rewards linear in x, the best action switches
/// at x = 0.375 and x = 0.625.
#[test]
fn learns_a_policy_over_a_continuous_context() {
    let (e, window) = run_continuous(1.0);
    let (realized, policy, optimal, uniform) = window.averages();
    println!(
        "(b) continuous: realized {realized:.4}, policy {policy:.4}, optimal {optimal:.4}, \
         uniform {uniform:.4}, realized/optimal {:.3}",
        realized / optimal
    );
    assert!(
        realized >= 0.9 * optimal,
        "realized {realized} vs optimal {optimal}"
    );
    for (x, best) in PROBES {
        let d = e.decide(&context(json!({ "x": x })), 0).unwrap();
        let truth = linear_means(x);
        println!(
            "    x = {x}: predictions {:.3?}, truth {truth:.3?}",
            d.predictions
        );
        assert_eq!(d.ranking()[0].0, best, "x = {x}: {:?}", d.pmf);
    }
}

/// (c) Every request brings fresh actions described only by features, so
/// nothing can be learned about ids: any gain over uniform comes from
/// generalizing through `quality` and `cost`.
#[test]
fn generalizes_through_action_features() {
    const K: usize = 5;
    const ROUNDS_C: usize = 4000;
    let mut e = engine(json!({"reward": {"range": [-1.1, 1.1]}}));
    let mut env = SplitMix64::new(505);
    let mut seeds = SplitMix64::new(606);
    let mut window = Window::default();
    for t in 0..ROUNDS_C {
        let mut values = [0.0; K];
        let actions: Vec<ActionSpec> = (0..K)
            .map(|j| {
                let (quality, cost) = (env.next_f64(), env.next_f64());
                values[j] = quality - cost;
                serde_json::from_value(json!({
                    "id": format!("r{t}-{j}"),
                    "features": {"quality": quality, "cost": cost},
                }))
                .unwrap()
            })
            .collect();
        let device = if env.next_f64() < 0.5 {
            "mobile"
        } else {
            "desktop"
        };
        let input = DecideInput {
            context: json!({ "device": device }),
            actions: Some(actions),
            ..DecideInput::default()
        };
        let d = e.decide(&input, seeds.next_u64()).unwrap();
        let reward = values[d.chosen] + (env.next_f64() - 0.5) * 0.2;
        e.learn(
            &input.context,
            &input.derived,
            d.chosen_action(),
            reward,
            d.probability,
        )
        .unwrap();
        if t >= ROUNDS_C - WINDOW {
            window.add(reward, &d, &values);
        }
    }
    let (realized, policy, optimal, uniform) = window.averages();
    let share = (policy - uniform) / (optimal - uniform);
    println!(
        "(c) action features: realized {realized:.4}, policy {policy:.4}, optimal {optimal:.4}, \
         uniform {uniform:.4}, share of the uniform-to-optimal gap {share:.3}"
    );
    assert!(
        policy - uniform > 0.3,
        "policy {policy} vs uniform {uniform}"
    );
    assert!(share > 0.8, "closed only {share:.3} of the gap");
    assert!(
        realized - uniform > 0.3,
        "realized {realized} vs uniform {uniform}"
    );
}

/// Drives two engines through the same rounds, asserting identical
/// decisions. Returns the inputs used.
fn run_pair(
    a: &mut Engine,
    b: &mut Engine,
    env: &mut SplitMix64,
    seeds: &mut SplitMix64,
    rounds: usize,
) -> Vec<DecideInput> {
    let mut inputs = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let input = DecideInput {
            context: json!({"u": env.next_f64(), "seg": format!("s{}", env.next_u64() % 4)}),
            derived: json!({"score": env.next_f64()}),
            ..DecideInput::default()
        };
        let seed = seeds.next_u64();
        let da = a.decide(&input, seed).unwrap();
        let db = b.decide(&input, seed).unwrap();
        assert_eq!(da, db);
        let reward = bernoulli(env, 0.2 + 0.2 * da.chosen as f64);
        for e in [&mut *a, &mut *b] {
            e.learn(
                &input.context,
                &input.derived,
                da.chosen_action(),
                reward,
                da.probability,
            )
            .unwrap();
        }
        inputs.push(input);
    }
    inputs
}

/// (d) Same spec, inputs and seeds give identical decisions; a snapshot
/// restores to exactly the same predictions and future behaviour.
#[test]
fn decisions_are_reproducible_and_snapshots_exact() {
    let spec = json!({
        "actions": [{"id": "a", "features": {"size": 1}}, {"id": "b", "features": {"size": 2.5}},
                    {"id": "c", "features": {"tier": "gold"}}],
        "learner": {"bits": 16},
        "seed": 99,
    });
    let mut first = engine(spec.clone());
    let mut second = engine(spec);
    let mut env = SplitMix64::new(707);
    let mut seeds = SplitMix64::new(808);
    let inputs = run_pair(&mut first, &mut second, &mut env, &mut seeds, 1500);
    assert_eq!(first.snapshot(), second.snapshot());
    assert_eq!(first.model_version(), 1500);

    let snapshot = first.snapshot();
    let mut restored = Engine::restore(first.spec().clone(), &snapshot).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    for (i, input) in inputs.iter().enumerate().step_by(7) {
        let original = first.decide(input, i as u64).unwrap();
        let replayed = restored.decide(input, i as u64).unwrap();
        // Bit-for-bit equal predictions and PMFs, and the same draw.
        assert_eq!(original, replayed);
    }
    // Training continues identically after the restore.
    run_pair(&mut first, &mut restored, &mut env, &mut seeds, 500);
    assert_eq!(first.snapshot(), restored.snapshot());
    println!(
        "(d) determinism: 2000 identical decisions, snapshot {} bytes",
        snapshot.len()
    );
}

/// (e) Excluded or filtered-out actions never get probability mass or get
/// chosen, and every eligible action keeps at least floor / K.
#[test]
fn exclusions_get_no_mass_and_the_floor_always_holds() {
    let ids = ["a", "b", "c", "d", "e", "f"];
    for kind in ["squarecb", "epsilonGreedy"] {
        let actions: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
        let floor = 0.1;
        let mut e = engine(json!({
            "actions": actions,
            "exploration": {"kind": kind, "floor": floor, "epsilon": 0.0},
            "learner": {"bits": 14},
        }));
        let mut env = SplitMix64::new(909);
        let mut seeds = SplitMix64::new(1010);
        let mut min_ratio = f64::INFINITY;
        let mut refused = 0;
        for _ in 0..3000 {
            let excluded: Vec<String> = ids
                .iter()
                .filter(|_| env.next_f64() < 0.3)
                .map(|s| s.to_string())
                .collect();
            let filter: Option<Vec<String>> = (env.next_f64() < 0.3).then(|| {
                ids.iter()
                    .filter(|_| env.next_f64() < 0.6)
                    .map(|s| s.to_string())
                    .collect()
            });
            let allowed: Vec<bool> = ids
                .iter()
                .map(|id| {
                    !excluded.iter().any(|x| x == id)
                        && filter.as_ref().is_none_or(|f| f.iter().any(|x| x == id))
                })
                .collect();
            let input = DecideInput {
                context: json!({"seg": format!("s{}", env.next_u64() % 3)}),
                excluded,
                eligible: filter,
                ..DecideInput::default()
            };
            let seed = seeds.next_u64();
            if !allowed.contains(&true) {
                assert_eq!(
                    e.decide(&input, seed).unwrap_err(),
                    DecideError::NoEligibleActions
                );
                refused += 1;
                continue;
            }
            let d = e.decide(&input, seed).unwrap();
            let k = d.eligible.len();
            let sum: f64 = d.pmf.iter().sum();
            assert!((sum - 1.0).abs() <= 1e-12, "sum {sum}");
            assert_eq!(d.pmf.len(), k);
            for (idx, id) in ids.iter().enumerate() {
                let is_eligible = d.eligible.contains(&idx);
                assert_eq!(is_eligible, allowed[idx], "{id}: {:?}", d.eligible);
                if !is_eligible {
                    assert_eq!(d.probability_of(idx), 0.0);
                    assert_ne!(d.chosen, idx, "chose excluded {id}");
                }
            }
            for &p in &d.pmf {
                assert!(p >= floor / k as f64, "{p} below floor / {k}: {:?}", d.pmf);
                min_ratio = min_ratio.min(p * k as f64 / floor);
            }
            assert!(d.eligible.contains(&d.chosen));
            // Reward action "a" so the PMF becomes peaked and the floor binds.
            let reward = if d.chosen == 0 { 1.0 } else { 0.0 };
            e.learn(
                &input.context,
                &input.derived,
                d.chosen_action(),
                reward,
                d.probability,
            )
            .unwrap();
        }
        println!(
            "(e) {kind}: smallest probability / (floor / K) = {min_ratio:.6}, \
             {refused} fully excluded requests refused"
        );
        // Epsilon-greedy with epsilon 0 leaves non-greedy actions exactly on
        // the floor. SquareCB always keeps 1 / (K + gamma * gap) > 0 on top of
        // it, so there the floor is approached but never reached.
        let bound = if kind == "squarecb" { 1.1 } else { 1.0 + 1e-12 };
        assert!(
            min_ratio <= bound,
            "{kind}: floor never approached ({min_ratio})"
        );
        assert!(refused > 0);
    }
}

/// (f) Scale invariance: presenting x multiplied by 1000 (or divided by
/// 1000) still converges, and to the same predictions.
#[test]
fn numeric_feature_scale_does_not_matter() {
    let (plain, _) = run_continuous(1.0);
    for scale in [1000.0, 0.001] {
        let (scaled, window) = run_continuous(scale);
        let (realized, policy, optimal, _) = window.averages();
        println!(
            "(f) x * {scale}: realized {realized:.4}, policy {policy:.4}, optimal {optimal:.4}, \
             realized/optimal {:.3}",
            realized / optimal
        );
        assert!(
            realized >= 0.9 * optimal,
            "scale {scale}: {realized} vs {optimal}"
        );
        for (x, best) in PROBES {
            let a = plain.decide(&context(json!({ "x": x })), 0).unwrap();
            let b = scaled
                .decide(&context(json!({ "x": x * scale })), 0)
                .unwrap();
            assert_eq!(b.ranking()[0].0, best, "scale {scale}, x = {x}");
            for (p, q) in a.predictions.iter().zip(&b.predictions) {
                assert!(
                    (p - q).abs() < 0.01,
                    "scale {scale}, x = {x}: {:?} vs {:?}",
                    a.predictions,
                    b.predictions
                );
            }
        }
    }
}
