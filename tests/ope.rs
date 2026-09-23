//! Off-policy evaluation against known ground truth, and `syntra evaluate`
//! end to end.
//!
//! The simulated environment has a finite context space, so every fixed
//! policy's true value is an exact finite sum:
//!
//! - context: segment `s0..s3` with probabilities 0.4, 0.3, 0.2, 0.1 and a
//!   level in {0, 0.5, 1}, uniform;
//! - actions `a0`, `a1`, `a2`; `a2` is not eligible in segment `s3`;
//! - mean reward `m(s, l, a) = BASE[s][a] + SLOPE[a] l`, all in [0.05, 0.95];
//! - logging: epsilon-greedy (epsilon 0.3) around a fixed, poor rule, so
//!   the rule's action gets `0.7 + 0.3 / K` and every other `0.3 / K`;
//! - rewards: Bernoulli(m), or `10 m + N(0, 1)` for the Gaussian variant.
//!
//! Run with `--nocapture` to see the measured numbers. The throughput test
//! is ignored by default: `cargo test --release --test ope -- --ignored
//! --nocapture`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{Value, json};
use syntra::decision::{ActionSpec, DecisionSpec, SplitMix64};
use syntra::ope::{
    Constant, EvalConfig, EvalData, Evaluation, Gate, Greedy, Logged, LoggedRow, PolicyChoice,
    RewardModel, TargetColumn, TargetPolicy, evaluate, load_jsonl,
};

const P_SEG: [f64; 4] = [0.4, 0.3, 0.2, 0.1];
const LEVELS: [f64; 3] = [0.0, 0.5, 1.0];
const BASE: [[f64; 3]; 4] = [
    [0.6, 0.3, 0.2],
    [0.25, 0.5, 0.3],
    [0.3, 0.3, 0.6],
    [0.5, 0.2, 0.3],
];
const SLOPE: [f64; 3] = [-0.2, 0.3, 0.1];
/// The logging policy's favourite action per segment: the worst or second
/// worst almost everywhere, so a learned policy has room to improve.
const RULE: [usize; 4] = [1, 0, 0, 1];
const EPSILON: f64 = 0.3;

fn eligible(segment: usize) -> Vec<usize> {
    if segment == 3 {
        vec![0, 1]
    } else {
        vec![0, 1, 2]
    }
}

fn mean_reward(segment: usize, level: f64, action: usize) -> f64 {
    BASE[segment][action] + SLOPE[action] * level
}

/// Logging PMF aligned with `eligible(segment)`.
fn logging_pmf(segment: usize) -> Vec<f64> {
    let actions = eligible(segment);
    let k = actions.len() as f64;
    actions
        .iter()
        .map(|&a| (1.0 - EPSILON) * f64::from(u8::from(a == RULE[segment])) + EPSILON / k)
        .collect()
}

/// The best eligible action as a one-hot PMF aligned with `eligible`.
fn optimal_pmf(segment: usize, level: f64) -> Vec<f64> {
    let actions = eligible(segment);
    let best = (0..actions.len())
        .max_by(|&x, &y| {
            mean_reward(segment, level, actions[x])
                .total_cmp(&mean_reward(segment, level, actions[y]))
        })
        .unwrap();
    (0..actions.len())
        .map(|k| f64::from(u8::from(k == best)))
        .collect()
}

/// A fixed stochastic target: 0.2 / 0.5 / 0.3 over eligible actions,
/// renormalized where `a2` is missing.
fn stochastic_pmf(segment: usize, _level: f64) -> Vec<f64> {
    let raw = [0.2, 0.5, 0.3];
    let actions = eligible(segment);
    let total: f64 = actions.iter().map(|&a| raw[a]).sum();
    actions.iter().map(|&a| raw[a] / total).collect()
}

fn constant_pmf(action: usize) -> impl Fn(usize, f64) -> Vec<f64> {
    move |segment, _| {
        eligible(segment)
            .iter()
            .map(|&a| f64::from(u8::from(a == action)))
            .collect()
    }
}

/// Exact value of a policy given as a PMF over `eligible(segment)`.
fn true_value(pmf: impl Fn(usize, f64) -> Vec<f64>) -> f64 {
    let mut value = 0.0;
    for (segment, p) in P_SEG.iter().enumerate() {
        for &level in &LEVELS {
            let actions = eligible(segment);
            let target = pmf(segment, level);
            let inner: f64 = actions
                .iter()
                .zip(&target)
                .map(|(&a, q)| q * mean_reward(segment, level, a))
                .sum();
            value += p / 3.0 * inner;
        }
    }
    value
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rewards {
    Bernoulli,
    Gaussian,
}

/// `n` logged rows; returns the rows and each row's (segment, level). With
/// `target`, each row carries the optimal policy's PMF as `targetPmf`.
fn simulate(
    seed: u64,
    n: usize,
    rewards: Rewards,
    target: bool,
) -> (Vec<LoggedRow>, Vec<(usize, f64)>) {
    let mut rng = SplitMix64::new(seed);
    let actions: Vec<ActionSpec> = (0..3).map(|a| ActionSpec::new(format!("a{a}"))).collect();
    let mut rows = Vec::with_capacity(n);
    let mut contexts = Vec::with_capacity(n);
    for t in 0..n {
        let u = rng.next_f64();
        let mut segment = 0;
        let mut cumulative = P_SEG[0];
        while u >= cumulative && segment < 3 {
            segment += 1;
            cumulative += P_SEG[segment];
        }
        let level = LEVELS[(rng.next_u64() % 3) as usize];
        let pmf = logging_pmf(segment);
        let draw = rng.next_f64();
        let mut position = pmf.len() - 1;
        let mut cumulative = 0.0;
        for (k, p) in pmf.iter().enumerate() {
            cumulative += p;
            if draw < cumulative {
                position = k;
                break;
            }
        }
        let chosen = eligible(segment)[position];
        let m = mean_reward(segment, level, chosen);
        let reward = match rewards {
            Rewards::Bernoulli => f64::from(u8::from(rng.next_f64() < m)),
            Rewards::Gaussian => {
                let (u1, u2) = (1.0 - rng.next_f64(), rng.next_f64());
                let normal = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
                10.0 * m + normal
            }
        };
        rows.push(LoggedRow {
            decision_id: format!("t{seed}-{t}"),
            ts_ms: 1_758_585_600_000 + t as i64,
            context: json!({"segment": format!("s{segment}"), "level": level}),
            derived: Value::Null,
            actions: actions.clone(),
            eligible: eligible(segment),
            probability: pmf[position],
            pmf,
            chosen,
            reward: Some(reward),
            target_pmf: target.then(|| optimal_pmf(segment, level)),
        });
        contexts.push((segment, level));
    }
    (rows, contexts)
}

/// A target policy computed from each row's context by a rule.
struct Rule {
    name: &'static str,
    pmf: fn(usize, f64) -> Vec<f64>,
}

impl TargetPolicy for Rule {
    fn label(&self) -> String {
        self.name.to_string()
    }

    fn pmf(&self, data: &EvalData, i: usize) -> Vec<f64> {
        let context = &data.row(i).context;
        let segment = context["segment"].as_str().unwrap()[1..].parse().unwrap();
        (self.pmf)(segment, context["level"].as_f64().unwrap())
    }
}

fn mean_and_se(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, (var / n).sqrt())
}

const TRIALS: usize = 100;
const TRIAL_ROWS: usize = 1000;
const TRIAL_BOOTSTRAP: usize = 500;
const TARGETS: [&str; 4] = [
    "logged",
    "constant:a1",
    "optimal (target column)",
    "stochastic",
];

/// Evaluations of the four targets in each of `TRIALS` independent logs.
struct Trials {
    truth: [f64; 4],
    runs: Vec<[Evaluation; 4]>,
}

fn trials() -> &'static Trials {
    static TRIALS_CELL: OnceLock<Trials> = OnceLock::new();
    TRIALS_CELL.get_or_init(|| {
        let truth = [
            true_value(|s, _| logging_pmf(s)),
            true_value(constant_pmf(1)),
            true_value(optimal_pmf),
            true_value(stochastic_pmf),
        ];
        let config = EvalConfig {
            bootstrap: TRIAL_BOOTSTRAP,
            ..EvalConfig::default()
        };
        let runs = (0..TRIALS as u64)
            .map(|seed| {
                let (rows, _) = simulate(10_000 + seed, TRIAL_ROWS, Rewards::Bernoulli, true);
                let data = EvalData::new(rows, config.folds, None).unwrap();
                let rewards = RewardModel::fit(&data, &config.reward_model).unwrap();
                let run = |policy: &dyn TargetPolicy| {
                    let config = EvalConfig {
                        seed: 7 + seed,
                        ..config.clone()
                    };
                    evaluate(&data, &rewards, policy, &config).unwrap()
                };
                [
                    run(&Logged),
                    run(&Constant::new("a1")),
                    run(&TargetColumn::new(&data).unwrap()),
                    run(&Rule {
                        name: "stochastic",
                        pmf: stochastic_pmf,
                    }),
                ]
            })
            .collect();
        Trials { truth, runs }
    })
}

#[test]
fn ips_snips_and_dr_are_unbiased_over_100_logs() {
    let t = trials();
    println!(
        "Mean error over {TRIALS} logs of {TRIAL_ROWS} rows (Bernoulli rewards), with the Monte Carlo SE:"
    );
    for (k, name) in TARGETS.iter().enumerate() {
        println!("  {name} (true value {:.4}):", t.truth[k]);
        for (estimator, pick) in [
            (
                "IPS",
                (|e: &Evaluation| e.estimators.ips.estimate) as fn(&Evaluation) -> f64,
            ),
            ("SNIPS", |e| e.estimators.snips.estimate),
            ("DR", |e| e.estimators.dr.estimate),
            ("DM", |e| e.estimators.dm.estimate),
        ] {
            let errors: Vec<f64> = t.runs.iter().map(|r| pick(&r[k]) - t.truth[k]).collect();
            let (bias, se) = mean_and_se(&errors);
            println!(
                "    {estimator:5} mean error {bias:+.5} +- {se:.5} ({:+.2} SE)",
                bias / se
            );
            // DM has no unbiasedness guarantee; the other three do (SNIPS
            // up to O(1/n)).
            if estimator != "DM" {
                assert!(
                    bias.abs() < 3.0 * se,
                    "{name} {estimator}: mean error {bias} with MC SE {se}"
                );
            }
        }
    }
    // Sanity: the logging policy is much worse than the optimal rule.
    assert!(t.truth[2] - t.truth[0] > 0.25, "{:?}", t.truth);
}

#[test]
fn intervals_cover_the_truth_at_about_the_nominal_rate() {
    let t = trials();
    println!("Share of {TRIALS} logs whose 95% interval covers the true value:");
    for (k, name) in TARGETS.iter().enumerate() {
        let mut line = format!("  {name:24}");
        for (estimator, pick) in [
            (
                "IPS",
                (|e: &Evaluation| e.estimators.ips) as fn(&Evaluation) -> syntra::ope::Estimate,
            ),
            ("SNIPS", |e| e.estimators.snips),
            ("DR", |e| e.estimators.dr),
        ] {
            let covered = |lower: fn(&syntra::ope::Estimate) -> (f64, f64)| {
                t.runs
                    .iter()
                    .filter(|r| {
                        let (lo, hi) = lower(&pick(&r[k]));
                        lo <= t.truth[k] && t.truth[k] <= hi
                    })
                    .count()
            };
            let bootstrap = covered(|e| (e.lower, e.upper));
            let normal = covered(|e| (e.normal_lower, e.normal_upper));
            line += &format!(" {estimator} {bootstrap}% / {normal}%;");
            assert!(
                bootstrap >= 85,
                "{name} {estimator}: bootstrap covered {bootstrap}%"
            );
            assert!(normal >= 85, "{name} {estimator}: normal covered {normal}%");
        }
        println!("{line} (bootstrap / normal)");
    }
    // The logged mean's own interval.
    let logged = t
        .runs
        .iter()
        .filter(|r| r[0].logged.lower <= t.truth[0] && t.truth[0] <= r[0].logged.upper)
        .count();
    println!("  logged mean: {logged}%");
    assert!(logged >= 85);
}

#[test]
fn dr_has_a_smaller_standard_error_than_ips() {
    let t = trials();
    for k in 1..4 {
        let ips: Vec<f64> = t.runs.iter().map(|r| r[k].estimators.ips.se).collect();
        let dr: Vec<f64> = t.runs.iter().map(|r| r[k].estimators.dr.se).collect();
        let wins = ips.iter().zip(&dr).filter(|(i, d)| d < i).count();
        let (ips_mean, _) = mean_and_se(&ips);
        let (dr_mean, _) = mean_and_se(&dr);
        println!(
            "{}: mean SE IPS {ips_mean:.4}, DR {dr_mean:.4} (ratio {:.2}); DR smaller in {wins}/{TRIALS} logs",
            TARGETS[k],
            dr_mean / ips_mean
        );
        assert!(dr_mean < 0.8 * ips_mean, "{}", TARGETS[k]);
        assert!(wins >= 95, "{}", TARGETS[k]);
    }
    // With a large reward offset (Gaussian rewards around 10 m), IPS pays
    // for the offset in variance and DR removes it.
    let (rows, _) = simulate(77, 4000, Rewards::Gaussian, true);
    let config = EvalConfig {
        bootstrap: 0,
        ..EvalConfig::default()
    };
    let data = EvalData::new(rows, 5, None).unwrap();
    let rewards = RewardModel::fit(&data, &config.reward_model).unwrap();
    let e = evaluate(&data, &rewards, &TargetColumn::new(&data).unwrap(), &config).unwrap();
    let truth = 10.0 * true_value(optimal_pmf);
    let (ips, dr) = (e.estimators.ips, e.estimators.dr);
    println!(
        "Gaussian rewards, optimal target: truth {truth:.3}; IPS {:.3} (SE {:.3}); DR {:.3} (SE {:.3}); SNIPS {:.3}; DM {:.3}",
        ips.estimate,
        ips.se,
        dr.estimate,
        dr.se,
        e.estimators.snips.estimate,
        e.estimators.dm.estimate
    );
    assert!(dr.se < 0.5 * ips.se);
    assert!((dr.estimate - truth).abs() < 4.0 * dr.se);
    assert_eq!(
        e.data.reward_range_source,
        syntra::ope::RangeSource::Observed
    );
}

#[test]
fn greedy_beats_the_logging_policy_and_its_estimate_matches_its_true_value() {
    let config = EvalConfig {
        bootstrap: 300,
        ..EvalConfig::default()
    };
    let optimum = true_value(optimal_pmf);
    let logging = true_value(|s, _| logging_pmf(s));
    for seed in [1u64, 2] {
        let (rows, contexts) = simulate(seed, 6000, Rewards::Bernoulli, false);
        let data = EvalData::new(rows.clone(), config.folds, None).unwrap();
        let rewards = RewardModel::fit(&data, &config.reward_model).unwrap();
        let greedy = Greedy::fit(&data, &DecisionSpec::default(), "greedy").unwrap();
        let e = evaluate(&data, &rewards, &greedy, &config).unwrap();
        // The value of the fitted (cross-fitted) policy on these contexts.
        let oracle = (0..data.len())
            .map(|i| {
                let (segment, level) = contexts[i];
                mean_reward(segment, level, data.row(i).eligible[greedy.choice(i)])
            })
            .sum::<f64>()
            / data.len() as f64;
        let dr = e.estimators.dr;
        println!(
            "seed {seed}: greedy true value {oracle:.4} (optimum {optimum:.4}, logging {logging:.4}); \
             DR {:.4} [{:.4}, {:.4}], IPS {:.4}, SNIPS {:.4}, DM {:.4}; logged mean {:.4} [{:.4}, {:.4}]",
            dr.estimate,
            dr.lower,
            dr.upper,
            e.estimators.ips.estimate,
            e.estimators.snips.estimate,
            e.estimators.dm.estimate,
            e.logged.mean,
            e.logged.lower,
            e.logged.upper
        );
        assert!(oracle > logging + 0.2, "the learned rule is far better");
        assert!(oracle > optimum - 0.03, "and close to optimal");
        assert!((dr.estimate - oracle).abs() < 3.0 * dr.se);
        assert!((e.estimators.ips.estimate - oracle).abs() < 3.0 * e.estimators.ips.se);
        assert!(dr.lower > e.logged.upper);
        // The same run through the one-call pipeline, with a promotion gate.
        let gates = [Gate::parse("dr.lower >= logged.mean + 0.1").unwrap()];
        let report = syntra::ope::run(rows, &PolicyChoice::Greedy, &config, &gates).unwrap();
        assert_eq!(report.evaluation, e);
        assert!(report.gates_passed, "{}", report.verdict);
        assert!(report.verdict.starts_with("PASS"), "{}", report.verdict);
    }
}

#[test]
fn constant_target_counts_ineligible_rows_as_zero_weight() {
    // a2 is not eligible in segment s3 (10% of rows).
    let (rows, contexts) = simulate(5, 20_000, Rewards::Bernoulli, false);
    let s3 = contexts.iter().filter(|(s, _)| *s == 3).count();
    let config = EvalConfig {
        bootstrap: 0,
        ..EvalConfig::default()
    };
    let data = EvalData::new(rows, 5, None).unwrap();
    let rewards = RewardModel::fit(&data, &config.reward_model).unwrap();
    let e = evaluate(&data, &rewards, &Constant::new("a2"), &config).unwrap();
    assert_eq!(e.diagnostics.target_ineligible, s3);
    assert!(
        e.warnings
            .iter()
            .any(|w| w.contains(&format!("no eligible action in {s3} of 20000 rows")))
    );
    // IPS, DR and DM estimate the value with reward 0 where a2 is
    // ineligible; SNIPS the value where it is eligible.
    let with_zero = true_value(constant_pmf(2));
    let eligible_share = 1.0 - P_SEG[3];
    let conditional = with_zero / eligible_share;
    let est = e.estimators;
    println!(
        "constant:a2: IPS {:.4} DR {:.4} vs {with_zero:.4}; SNIPS {:.4} vs {conditional:.4}",
        est.ips.estimate, est.dr.estimate, est.snips.estimate
    );
    assert!((est.ips.estimate - with_zero).abs() < 4.0 * est.ips.se);
    assert!((est.dr.estimate - with_zero).abs() < 4.0 * est.dr.se);
    assert!((est.snips.estimate - conditional).abs() < 4.0 * est.snips.se);
    // The rule never picks a2, so it is logged only as an exploration pick
    // (probability 0.1) in the 90% of rows where it is eligible.
    assert!(
        (e.diagnostics.coverage - 0.9 * (EPSILON / 3.0)).abs() < 0.01,
        "{}",
        e.diagnostics.coverage
    );
}

/// A directory under the worktree, removed when dropped (also when a test
/// fails part way).
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".ope-test-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn syntra(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .arg("evaluate")
        .args(args)
        .output()
        .expect("run syntra evaluate");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn cli_end_to_end_with_exit_codes() {
    let dir = TempDir::new("cli");
    let (mut rows, _) = simulate(3, 3000, Rewards::Bernoulli, true);
    rows[10].reward = None;
    let jsonl: String = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap() + "\n")
        .collect();
    let input = dir.file("rows.jsonl", &jsonl);
    assert_eq!(load_jsonl(&input).unwrap().len(), 3000);
    let input = input.to_str().unwrap();
    let pass = dir.file(
        "pass.yaml",
        "gates:\n  - dr.lower >= logged.mean + 0.05\n  - ess >= 200\n  - n >= 1000\n",
    );
    let fail = dir.file(
        "fail.json",
        r#"["dr.lower >= logged.mean + 0.05", "coverage >= 0.9"]"#,
    );
    let spec = dir.file(
        "spec.json",
        r#"{"learner": {"bits": 16, "learningRate": 0.3}}"#,
    );
    let (pass, fail, spec) = (
        pass.to_str().unwrap(),
        fail.to_str().unwrap(),
        spec.to_str().unwrap(),
    );

    // Passing gates, JSON on stdout.
    let (code, stdout, stderr) = syntra(&[
        "--input",
        input,
        "--policy",
        "greedy",
        "--gates",
        pass,
        "--bootstrap",
        "200",
        "--fail-on-gate",
    ]);
    assert_eq!(code, 0, "{stderr}");
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["policy"], "greedy");
    assert_eq!(report["gatesPassed"], true);
    assert_eq!(report["data"]["rows"], 3000);
    assert_eq!(report["data"]["rowsWithoutReward"], 1);
    assert_eq!(report["diagnostics"]["n"], 2999);
    assert_eq!(report["settings"]["bootstrap"], 200);
    assert_eq!(report["gates"].as_array().unwrap().len(), 3);
    assert!(
        report["verdict"]
            .as_str()
            .unwrap()
            .starts_with("PASS: all 3 gates pass.")
    );

    // A failing gate: exit 1 with --fail-on-gate, 0 without.
    let base = [
        "--input",
        input,
        "--policy",
        "target-column",
        "--gates",
        fail,
        "--bootstrap",
        "0",
    ];
    let (code, stdout, _) = syntra(&[&base[..], &["--fail-on-gate"]].concat());
    assert_eq!(code, 1);
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["gatesPassed"], false);
    assert_eq!(report["gates"][0]["pass"], true);
    assert_eq!(report["gates"][1]["pass"], false);
    assert!(
        report["verdict"]
            .as_str()
            .unwrap()
            .starts_with("FAIL: 1 of 2 gates fail (coverage >= 0.9")
    );
    let (code, _, _) = syntra(&base);
    assert_eq!(code, 0);

    // Markdown to a file, with a candidate spec and a constant target.
    let out = dir.0.join("report.md");
    let (code, stdout, stderr) = syntra(&[
        "--input",
        input,
        "--policy",
        &format!("spec:{spec}"),
        "--format",
        "markdown",
        "--out",
        out.to_str().unwrap(),
        "--bootstrap",
        "100",
        "--seed",
        "3",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("wrote") && stderr.contains("No gates set."),
        "{stderr}"
    );
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(
        md.starts_with(&format!("# Off-policy evaluation: spec:{spec}\n")),
        "{md}"
    );
    assert!(md.contains("| DR (doubly robust) |") && md.contains("No gates were set."));
    let (code, stdout, _) = syntra(&[
        "--input",
        input,
        "--policy",
        "constant:a2",
        "--bootstrap",
        "0",
        "--w-max",
        "5",
    ]);
    assert_eq!(code, 0);
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert!(report["diagnostics"]["targetIneligible"].as_u64().unwrap() > 0);
    assert!(
        report["diagnostics"]["clipRate"].as_f64().unwrap() > 0.0,
        "weights of 10 clip at 5"
    );

    // Data errors: exit 2 with the line number.
    let mut broken = jsonl.lines().map(String::from).collect::<Vec<_>>();
    broken[2] = broken[2].replace("\"probability\":", "\"probability\":0.123,\"x\":");
    let bad = dir.file("bad.jsonl", &broken.join("\n"));
    let (code, _, stderr) = syntra(&["--input", bad.to_str().unwrap(), "--policy", "logged"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("line 3: "), "{stderr}");
    let (code, _, stderr) = syntra(&["--input", input, "--policy", "constant:zz"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("action \"zz\" is not eligible in any row"),
        "{stderr}"
    );
    let (code, _, stderr) = syntra(&["--input", "no/such/file.jsonl", "--policy", "logged"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("cannot open no/such/file.jsonl"),
        "{stderr}"
    );

    // Usage errors: exit 2; help: exit 0.
    let (code, _, stderr) = syntra(&["--input", input]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--policy is required"), "{stderr}");
    let (code, _, stderr) = syntra(&[
        "--input", input, "--policy", "greedy", "--gates", pass, "--folds", "1",
    ]);
    assert_eq!(code, 2);
    assert!(stderr.contains("folds must be"), "{stderr}");
    let bad_gate = dir.file("bad-gate.yaml", "- dr.lowr >= 1\n");
    let (code, _, stderr) = syntra(&[
        "--input",
        input,
        "--policy",
        "greedy",
        "--gates",
        bad_gate.to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown metric \"dr.lowr\""), "{stderr}");
    let (code, _, stderr) = syntra(&["--store", "s", "--capsule", "t/j/c", "--policy", "logged"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("not available yet"), "{stderr}");
    let (code, _, stderr) = syntra(&["--help"]);
    assert_eq!(code, 0);
    assert!(stderr.contains("Usage:"), "{stderr}");

    let root = dir.0.clone();
    drop(dir);
    assert!(!root.exists(), "the temporary directory is removed");
}

/// Throughput on 100k rows, 5 folds, 1000 bootstrap resamples. Slow in a
/// debug build; run it in release.
#[test]
#[ignore]
fn throughput_100k_rows() {
    let n = 100_000;
    let mut rng = SplitMix64::new(9);
    let actions: Vec<ActionSpec> = (0..4)
        .map(|a| {
            serde_json::from_value(json!({"id": format!("model-{a}"), "features": {"cost": 0.25 * a as f64, "family": if a < 2 { "small" } else { "large" }}}))
                .unwrap()
        })
        .collect();
    let rows: Vec<LoggedRow> = (0..n)
        .map(|t| {
            let segment = rng.next_u64() % 8;
            let chosen = (rng.next_u64() % 4) as usize;
            let reward = f64::from(u8::from(
                rng.next_f64() < 0.2 + 0.1 * ((segment as usize + chosen) % 4) as f64,
            ));
            LoggedRow {
                decision_id: format!("dec_{t}"),
                ts_ms: t as i64,
                context: json!({
                    "segment": format!("s{segment}"),
                    "tokens": rng.next_u64() % 4000,
                    "user": {"tier": if rng.next_f64() < 0.3 { "pro" } else { "free" }},
                    "device": if rng.next_f64() < 0.5 { "mobile" } else { "desktop" },
                    "hour": rng.next_u64() % 24,
                }),
                derived: Value::Null,
                actions: actions.clone(),
                eligible: vec![0, 1, 2, 3],
                pmf: vec![0.25; 4],
                chosen,
                probability: 0.25,
                reward: Some(reward),
                target_pmf: None,
            }
        })
        .collect();
    let dir = TempDir::new("perf");
    let jsonl: String = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap() + "\n")
        .collect();
    let path = dir.file("rows.jsonl", &jsonl);
    let config = EvalConfig::default();

    let start = Instant::now();
    let loaded = load_jsonl(&path).unwrap();
    let load = start.elapsed();
    let data = EvalData::new(loaded, config.folds, None).unwrap();
    let prepare = start.elapsed() - load;
    let rewards = RewardModel::fit(&data, &config.reward_model).unwrap();
    let reward_model = start.elapsed() - load - prepare;
    let greedy = Greedy::fit(&data, &DecisionSpec::default(), "greedy").unwrap();
    let policy = start.elapsed() - load - prepare - reward_model;
    let e = evaluate(&data, &rewards, &greedy, &config).unwrap();
    let estimate = start.elapsed() - load - prepare - reward_model - policy;
    println!(
        "100k rows ({:.1} MB JSONL), 4 actions, 5 folds, 1000 resamples: total {:.2?} = load {load:.2?} + \
         featurize {prepare:.2?} + reward model {reward_model:.2?} + greedy {policy:.2?} + estimates and bootstrap {estimate:.2?}",
        jsonl.len() as f64 / 1e6,
        start.elapsed()
    );
    println!(
        "DR {:.4} [{:.4}, {:.4}]",
        e.estimators.dr.estimate, e.estimators.dr.lower, e.estimators.dr.upper
    );
    let start = Instant::now();
    let (code, _, stderr) = syntra(&["--input", path.to_str().unwrap(), "--policy", "greedy"]);
    assert_eq!(code, 0, "{stderr}");
    println!(
        "syntra evaluate --policy greedy (binary, same work plus JSON output): {:.2?}",
        start.elapsed()
    );
}
