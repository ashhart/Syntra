//! Per-seed results of `examples/learning_bench.rs`.
//!
//! learning_bench prints each environment and policy averaged over seeds
//! `1000 .. 1000 + seeds`. This binary compiles the example's own
//! environments, policies and run loop (see `build.rs`) and prints one JSON
//! line per (environment, policy, seed) with the same two metrics, so
//! `benchmarks/learning_vs_vw.py` can report the spread across seeds and
//! compare with Vowpal Wabbit seed by seed. Averaging its lines reproduces
//! learning_bench's table.
//!
//! ```text
//! cargo run --release --manifest-path benchmarks/learning_seeds/Cargo.toml -- \
//!     [--rounds 20000] [--seeds 5] [--first-seed 1000] [--learning-rate 0.5]
//! ```

#[allow(dead_code)]
mod learning_bench {
    include!(concat!(env!("OUT_DIR"), "/learning_bench.rs"));

    /// Run every environment and policy of learning_bench on seeds
    /// `first_seed .. first_seed + seeds` and print one JSON line each.
    pub fn per_seed(rounds: usize, seeds: u64, first_seed: u64, learning_rate: Option<f64>) {
        let a = Args {
            rounds,
            seeds,
            learning_rate,
        };
        let envs: Vec<Box<dyn Env>> = vec![
            Box::new(Segments { drift: false }),
            Box::new(Segments { drift: true }),
            Box::new(Catalog),
        ];
        for env in &envs {
            for policy in [Policy::SquareCb, Policy::EpsilonGreedy, Policy::Uniform] {
                for s in 0..seeds {
                    let seed = first_seed + s;
                    let o = run(env.as_ref(), policy, &a, seed);
                    println!(
                        "{}",
                        json!({
                            "env": env.name(),
                            "policy": policy.name(),
                            "seed": seed,
                            "rounds": rounds,
                            "learningRate": learning_rate,
                            "tailShare": o.tail_share,
                            "regretPerRound": o.regret_per_round,
                        })
                    );
                }
            }
        }
    }
}

fn main() {
    let mut rounds = 20_000usize;
    let mut seeds = 5u64;
    let mut first_seed = 1000u64;
    let mut learning_rate = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--rounds" => rounds = v.parse().expect("--rounds"),
            "--seeds" => seeds = v.parse().expect("--seeds"),
            "--first-seed" => first_seed = v.parse().expect("--first-seed"),
            "--learning-rate" => learning_rate = Some(v.parse().expect("--learning-rate")),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    learning_bench::per_seed(rounds, seeds, first_seed, learning_rate);
}
