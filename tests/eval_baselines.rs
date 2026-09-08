//! Regression tests for the built-in `simulate --compare-baseline` reference
//! policies and for the fail-closed argument validation in `cli_simulate`.
//! `syntra::simulate` is a private module, so every assertion here goes
//! through the `syntra` binary: the numbers a reader reproduces from the CLI
//! are exactly the numbers under test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TWO_ARM_CAPSULE: &str = r#"
name: eval-two-arm
version: 0.1.0
options:
  - slow
  - fast
reward:
  type: continuous
  range: [0.0, 1.0]
algorithm:
  type: thompson
learning:
  min_exploration: 0.05
"#;

const FOUR_ARM_CAPSULE: &str = r#"
name: eval-four-arm
version: 0.1.0
options:
  - a
  - b
  - c
  - d
reward:
  type: continuous
  range: [0.0, 1.0]
algorithm:
  type: thompson
"#;

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "syntra-eval-baselines-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_capsule(dir: &Path, name: &str, yaml: &str) -> PathBuf {
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, yaml).expect("write capsule");
    path
}

fn simulate(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_syntra"))
        .arg("simulate")
        .args(args)
        .output()
        .expect("failed to run syntra simulate")
}

fn simulate_json(args: &[&str]) -> serde_json::Value {
    let out = simulate(&args.iter().map(|s| s.as_ref()).collect::<Vec<_>>());
    assert!(
        out.status.success(),
        "simulate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("simulate emitted valid JSON")
}

fn baseline_regret(json: &serde_json::Value) -> f64 {
    json["baselineComparison"]["meanCumulativeRegret"]
        .as_f64()
        .expect("baselineComparison missing — was --compare-baseline passed?")
}

/// `first-arm` regret is analytic: it never explores, so cumulative regret is
/// exactly `rounds * (best_mean - arms[0])` computed from the true means.
#[test]
fn first_arm_baseline_equals_brute_force() {
    let dir = workdir("first-arm");
    let capsule = write_capsule(&dir, "two", TWO_ARM_CAPSULE);
    let json = simulate_json(&[
        capsule.to_str().unwrap(),
        "--true-arm-rewards",
        "0.2,0.9",
        "--rounds",
        "10",
        "--seed",
        "7",
        "--compare-baseline",
        "first-arm",
        "--format",
        "json",
    ]);

    // Brute force: 10 rounds, each picking arm 0 (0.2) while the oracle
    // holds 0.9 → 10 * 0.7 = 7.0, independent of noise draws.
    let expected = 10.0 * (0.9 - 0.2);
    let got = baseline_regret(&json);
    assert!(
        (got - expected).abs() < 1e-6,
        "first-arm regret {got} != brute-force {expected}"
    );
    assert_eq!(json["baselineComparison"]["name"], "first-arm");
    assert_eq!(json["baselineComparison"]["stdCumulativeRegret"], 0.0);
}

/// With every arm's true mean equal, no policy can accumulate regret —
/// a zero that must hold exactly for all three built-in comparators.
#[test]
fn all_baselines_score_zero_regret_on_equal_arms() {
    let dir = workdir("equal");
    let capsule = write_capsule(&dir, "four", FOUR_ARM_CAPSULE);
    for baseline in ["random", "first-arm", "epsilon-greedy:0.1"] {
        let json = simulate_json(&[
            capsule.to_str().unwrap(),
            "--true-arm-rewards",
            "0.5,0.5,0.5,0.5",
            "--rounds",
            "200",
            "--seeds",
            "2",
            "--compare-baseline",
            baseline,
            "--format",
            "json",
        ]);
        assert_eq!(
            baseline_regret(&json),
            0.0,
            "baseline {baseline} accumulated regret on an all-equal stream"
        );
    }
}

/// The uniform-random comparator must be unbiased: on a 0/1 two-arm stream it
/// picks each arm half the time, so its regret tracks `rounds / 2`. Pinned to
/// a 2 % tolerance across 8 seeds — far tighter than seed-to-seed noise, far
/// looser than any plausible implementation error.
#[test]
fn random_baseline_tracks_expected_regret() {
    let dir = workdir("random");
    let capsule = write_capsule(&dir, "two", TWO_ARM_CAPSULE);
    let json = simulate_json(&[
        capsule.to_str().unwrap(),
        "--true-arm-rewards",
        "0.0,1.0",
        "--rounds",
        "2000",
        "--seeds",
        "8",
        "--compare-baseline",
        "random",
        "--format",
        "json",
    ]);
    let expected_per_seed = 2000.0 * 0.5 * (1.0 - 0.0);
    let got = baseline_regret(&json);
    assert!(
        (got - expected_per_seed).abs() / expected_per_seed < 0.02,
        "random baseline regret {got} strayed >2% from the unbiased value {expected_per_seed}"
    );
}

/// Behavioral ordering on a clear-gap stationary stream: an epsilon-greedy
/// reference must finish well below the random reference, which is the whole
/// point of the comparator ladder.
#[test]
fn epsilon_greedy_baseline_beats_random_baseline() {
    let dir = workdir("ordering");
    let capsule = write_capsule(&dir, "four", FOUR_ARM_CAPSULE);
    let mut by_baseline = Vec::new();
    for baseline in ["random", "epsilon-greedy:0.1"] {
        let json = simulate_json(&[
            capsule.to_str().unwrap(),
            "--true-arm-rewards",
            "0.1,0.3,0.5,0.9",
            "--rounds",
            "2000",
            "--seeds",
            "4",
            "--compare-baseline",
            baseline,
            "--format",
            "json",
        ]);
        by_baseline.push(baseline_regret(&json));
    }
    let (random, eps) = (by_baseline[0], by_baseline[1]);
    assert!(
        eps * 2.0 < random,
        "epsilon-greedy regret {eps} is not clearly below random regret {random}"
    );
}

/// Arm/reward-count mismatches are caller errors: `simulate` must fail the
/// process, not just print. (Pinned because a stale binary once reported the
/// mismatch on stderr while exiting 0.)
#[test]
fn arms_length_mismatch_exits_nonzero() {
    let dir = workdir("mismatch");
    let capsule = write_capsule(&dir, "four", FOUR_ARM_CAPSULE);

    let out = simulate(&[
        capsule.to_str().unwrap().as_ref(),
        "--true-arm-rewards".as_ref(),
        "0.9,0.5".as_ref(),
        "--rounds".as_ref(),
        "20".as_ref(),
    ]);
    assert!(
        !out.status.success(),
        "arms-length mismatch via --true-arm-rewards exited 0"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not match"), "stderr was: {err}");

    let traffic = dir.join("traffic.yaml");
    std::fs::write(&traffic, "arms: [0.9, 0.8]\n").expect("write traffic");
    let out = simulate(&[
        capsule.to_str().unwrap().as_ref(),
        "--traffic".as_ref(),
        traffic.as_os_str(),
        "--rounds".as_ref(),
        "20".as_ref(),
    ]);
    assert!(
        !out.status.success(),
        "arms-length mismatch via --traffic exited 0"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not match"), "stderr was: {err}");
}

/// A non-numeric reward token must abort rather than be silently dropped —
/// dropping it shrinks the arm list, which is exactly the mismatch the check
/// above is supposed to surface.
#[test]
fn non_numeric_reward_token_exits_nonzero() {
    let dir = workdir("badtoken");
    let capsule = write_capsule(&dir, "four", FOUR_ARM_CAPSULE);
    let out = simulate(&[
        capsule.to_str().unwrap().as_ref(),
        "--true-arm-rewards".as_ref(),
        "0.9,0.5,not-a-number,0.1".as_ref(),
        "--rounds".as_ref(),
        "20".as_ref(),
    ]);
    assert!(!out.status.success(), "bad reward token exited 0");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a finite number"), "stderr was: {err}");
}

/// Unparseable baselines must be rejected up front, before any seed runs.
#[test]
fn unknown_baseline_exits_nonzero() {
    let dir = workdir("badbaseline");
    let capsule = write_capsule(&dir, "four", FOUR_ARM_CAPSULE);
    for baseline in [
        "bogus",
        "epsilon-greedy:",
        "epsilon-greedy:0.0,5",
        "epsilon-greedy:101x",
    ] {
        let out = simulate(&[
            capsule.to_str().unwrap().as_ref(),
            "--true-arm-rewards".as_ref(),
            "0.9,0.5,0.3,0.1".as_ref(),
            "--rounds".as_ref(),
            "5".as_ref(),
            "--compare-baseline".as_ref(),
            baseline.as_ref(),
        ]);
        assert!(!out.status.success(), "baseline \"{baseline}\" accepted");
    }
}

/// `--seed` must pin the whole run, not just the traffic. The learner draws its
/// own randomness (Thompson samples, the weighted roulette, meta-bandit
/// tie-breaks) from `learning::rng`, which used to fall back to SystemTime
/// entropy: two identical invocations then disagreed on regret, picks, and the
/// final weight vector, so no number quoted from `simulate` could be
/// reproduced. Every field the report quotes must come back byte-identical;
/// `finalWeights` is compared with a tolerance because the learning layer's
/// float accumulation drifts at last-ulp scale across processes.
#[test]
fn repeated_invocation_is_reproducible() {
    let dir = workdir("repro");
    let capsule = write_capsule(&dir, "four", FOUR_ARM_CAPSULE);
    let traffic = dir.join("traffic.yaml");
    std::fs::write(&traffic, "arms: [0.9, 0.5, 0.3, 0.1]\nnoise_std: 0.05\n")
        .expect("write traffic");

    let run = || {
        let out = simulate(&[
            capsule.to_str().unwrap().as_ref(),
            "--traffic".as_ref(),
            traffic.as_os_str(),
            "--rounds".as_ref(),
            "400".as_ref(),
            "--seeds".as_ref(),
            "3".as_ref(),
            "--seed".as_ref(),
            "42".as_ref(),
            "--compare-baseline".as_ref(),
            "epsilon-greedy:0.1".as_ref(),
            "--format".as_ref(),
            "json".as_ref(),
        ]);
        assert!(
            out.status.success(),
            "simulate failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };

    let parse = |bytes: Vec<u8>| {
        let mut v: serde_json::Value =
            serde_json::from_slice(&bytes).expect("simulate emitted valid JSON");
        let mut weights = Vec::new();
        for seed in v["seeds"].as_array_mut().expect("seeds") {
            let taken = seed.as_object_mut().expect("seed").remove("finalWeights");
            weights.push(
                taken
                    .and_then(|w| serde_json::from_value::<Vec<f64>>(w).ok())
                    .unwrap_or_default(),
            );
        }
        (serde_json::to_string(&v).expect("serialize"), weights)
    };
    let (a_json, a_w) = parse(run());
    let (b_json, b_w) = parse(run());
    assert_eq!(
        a_json, b_json,
        "identical `simulate` invocations disagreed on a reported \
                                field — the learner's PRNG is not seeded from --seed"
    );
    for (ra, rb) in a_w.iter().zip(b_w.iter()) {
        for (x, y) in ra.iter().zip(rb.iter()) {
            assert!((x - y).abs() < 1e-9, "finalWeights drifted by {x} vs {y}");
        }
    }
}
