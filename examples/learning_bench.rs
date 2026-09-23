//! Learning-quality benchmark: how close the decision engine gets to the
//! best policy on simulated contextual bandits whose optimum is known.
//!
//! ```text
//! cargo run --release --example learning_bench [-- --rounds 20000 --seeds 5]
//! ```
//!
//! Environments (Bernoulli rewards; the expected reward of every
//! context-action pair is known, so the oracle is exact):
//!
//! - `segments`: four user segments x three actions; each segment has a
//!   different best action, with gaps of 0.1 to 0.3.
//! - `drift`: the same, but the best action of every segment changes
//!   halfway through.
//! - `catalog`: 20 of 200 items offered per request, described by two
//!   features; reward depends on how well the item's features match the
//!   user's, so the learner must generalize across items it rarely sees.
//!
//! Policies: the engine with SquareCB (the default), the engine with
//! epsilon-greedy (0.1), and uniform random. Each learns online from its
//! own draws, exactly as the server does from rewards. Reported per
//! environment: mean reward over the final 10% of rounds as a share of the
//! oracle's (1.0 is perfect), and mean regret per round over the whole
//! run. Results vary with the seed; several seeds are averaged.

use serde_json::{Value, json};
use syntra::decision::{ActionSpec, DecideInput, DecisionSpec, Engine, SplitMix64};

struct Args {
    rounds: usize,
    seeds: u64,
}

fn parse_args() -> Args {
    let mut a = Args {
        rounds: 20_000,
        seeds: 5,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--rounds" => a.rounds = v.parse().expect("--rounds"),
            "--seeds" => a.seeds = v.parse().expect("--seeds"),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    a
}

/// One round: a context, the offered actions and each one's expected
/// reward.
struct Round {
    context: Value,
    actions: Option<Vec<ActionSpec>>,
    means: Vec<f64>,
}

trait Env {
    fn name(&self) -> &'static str;
    fn spec_actions(&self) -> Vec<ActionSpec>;
    fn round(&self, t: usize, total: usize, rng: &mut SplitMix64) -> Round;
}

struct Segments {
    drift: bool,
}

impl Env for Segments {
    fn name(&self) -> &'static str {
        if self.drift { "drift" } else { "segments" }
    }
    fn spec_actions(&self) -> Vec<ActionSpec> {
        ["small", "medium", "large"]
            .into_iter()
            .map(ActionSpec::new)
            .collect()
    }
    fn round(&self, t: usize, total: usize, rng: &mut SplitMix64) -> Round {
        let segments = ["free", "pro", "team", "enterprise"];
        let s = (rng.next_u64() % 4) as usize;
        let mut means = match s {
            0 => vec![0.7, 0.5, 0.4],
            1 => vec![0.4, 0.7, 0.5],
            2 => vec![0.3, 0.5, 0.6],
            _ => vec![0.5, 0.6, 0.9],
        };
        if self.drift && t >= total / 2 {
            means.rotate_left(1);
        }
        Round {
            context: json!({"segment": segments[s], "hour": (t / 97) % 24}),
            actions: None,
            means,
        }
    }
}

struct Catalog;

impl Env for Catalog {
    fn name(&self) -> &'static str {
        "catalog"
    }
    fn spec_actions(&self) -> Vec<ActionSpec> {
        Vec::new()
    }
    fn round(&self, _t: usize, _total: usize, rng: &mut SplitMix64) -> Round {
        let user = [rng.next_f64(), rng.next_f64()];
        let mut actions = Vec::with_capacity(20);
        let mut means = Vec::with_capacity(20);
        // 20 distinct items of 200.
        let mut items: Vec<usize> = Vec::with_capacity(20);
        while items.len() < 20 {
            let item = (rng.next_u64() % 200) as usize;
            if !items.contains(&item) {
                items.push(item);
            }
        }
        for item in items {
            // Fixed item features from the item number.
            let mut ir = SplitMix64::new(item as u64 * 0x9E37_79B9 + 1);
            let feat = [ir.next_f64(), ir.next_f64()];
            let mut a = ActionSpec::new(format!("item{item}"));
            a.features.insert("x".into(), json!(feat[0]));
            a.features.insert("y".into(), json!(feat[1]));
            actions.push(a);
            let dist = ((user[0] - feat[0]).powi(2) + (user[1] - feat[1]).powi(2)).sqrt();
            means.push((0.9 - 0.8 * dist).clamp(0.05, 0.95));
        }
        Round {
            context: json!({"x": user[0], "y": user[1]}),
            actions: Some(actions),
            means,
        }
    }
}

#[derive(Clone, Copy)]
enum Policy {
    SquareCb,
    EpsilonGreedy,
    Uniform,
}

impl Policy {
    fn name(self) -> &'static str {
        match self {
            Policy::SquareCb => "squarecb",
            Policy::EpsilonGreedy => "epsilon-greedy 0.1",
            Policy::Uniform => "uniform",
        }
    }
}

struct Outcome {
    tail_share: f64,
    regret_per_round: f64,
}

fn run(env: &dyn Env, policy: Policy, rounds: usize, seed: u64) -> Outcome {
    let exploration = match policy {
        Policy::SquareCb => json!({"kind": "squarecb"}),
        Policy::EpsilonGreedy => json!({"kind": "epsilonGreedy", "epsilon": 0.1}),
        Policy::Uniform => json!({"kind": "epsilonGreedy", "epsilon": 1.0}),
    };
    let spec = DecisionSpec::from_json(&json!({
        "actions": serde_json::to_value(env.spec_actions()).unwrap(),
        "exploration": exploration,
    }))
    .unwrap();
    let mut engine = Engine::new(spec).unwrap();
    let mut rng = SplitMix64::new(seed);
    let mut draws = SplitMix64::new(seed ^ 0xD1CE);
    let tail_from = rounds - rounds / 10;
    let (mut tail_got, mut tail_best, mut regret) = (0.0, 0.0, 0.0);
    for t in 0..rounds {
        let r = env.round(t, rounds, &mut rng);
        let input = DecideInput {
            context: r.context.clone(),
            actions: r.actions.clone(),
            ..DecideInput::default()
        };
        let d = engine.decide(&input, draws.next_u64()).unwrap();
        let mean = r.means[d.chosen];
        let best = r.means.iter().cloned().fold(f64::MIN, f64::max);
        let reward = if rng.next_f64() < mean { 1.0 } else { 0.0 };
        let action = d.chosen_action().clone();
        if !matches!(policy, Policy::Uniform) {
            engine
                .learn(&r.context, &Value::Null, &action, reward, d.probability)
                .unwrap();
        }
        regret += best - mean;
        if t >= tail_from {
            tail_got += mean;
            tail_best += best;
        }
    }
    Outcome {
        tail_share: tail_got / tail_best,
        regret_per_round: regret / rounds as f64,
    }
}

fn main() {
    let a = parse_args();
    let envs: Vec<Box<dyn Env>> = vec![
        Box::new(Segments { drift: false }),
        Box::new(Segments { drift: true }),
        Box::new(Catalog),
    ];
    println!(
        "{} rounds, {} seeds; share of the oracle's expected reward over the last 10% of rounds, and mean regret per round\n",
        a.rounds, a.seeds
    );
    println!("| Environment | Policy | Final share of oracle | Regret per round |");
    println!("|---|---|---|---|");
    for env in &envs {
        for policy in [Policy::SquareCb, Policy::EpsilonGreedy, Policy::Uniform] {
            let mut share = 0.0;
            let mut regret = 0.0;
            for s in 0..a.seeds {
                let o = run(env.as_ref(), policy, a.rounds, 1000 + s);
                share += o.tail_share;
                regret += o.regret_per_round;
            }
            println!(
                "| {} | {} | {:.3} | {:.4} |",
                env.name(),
                policy.name(),
                share / a.seeds as f64,
                regret / a.seeds as f64
            );
        }
    }
}
