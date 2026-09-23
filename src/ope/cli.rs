//! `syntra evaluate`: off-policy evaluation from the command line.
//!
//! Exit codes: 0 when every gate passes (or none are set, or gates fail
//! without `--fail-on-gate`), 1 when a gate fails with `--fail-on-gate`,
//! 2 for a usage or data error (the message goes to stderr).

use std::io::Write as _;
use std::path::PathBuf;

use crate::decision::spec::RewardAggregation;

use super::estimators::EvalConfig;
use super::gates::load_gates;
use super::policy::PolicyChoice;
use super::report::Report;
use super::row::{LoadedRows, load_jsonl};

/// Help text for `syntra evaluate --help`.
pub const USAGE: &str = "\
Usage:
  syntra evaluate --input rows.jsonl --policy <policy> [options]

Estimates how a target policy would have done on logged decisions
(off-policy evaluation): DM, IPS, SNIPS and doubly robust estimates with
95% intervals, their lift over the logged policy paired on the same rows,
weight diagnostics and promotion gates. Values are in raw reward units.

Policies:
  logged             the logging policy itself (the incumbent's value)
  constant:<id>      play action <id> where it is eligible, else as logged
  greedy             argmax of a reward model learned from the logs,
                     cross-fitted so no row is scored by a model that saw it
  spec:<spec.json>   greedy as a candidate decision spec would run it: its
                     learner settings and declared action features
  target-column      each row's targetPmf

Options:
  --input <rows.jsonl>     logged rows, one JSON object per line (required)
  --policy <policy>        target policy (required)
  --folds <k>              cross-fitting folds, 2 to 100 (default 5)
  --bootstrap <b>          bootstrap resamples, 0 or 100 to 100000 (default 1000;
                           0 reports normal-approximation intervals only)
  --seed <s>               bootstrap seed (default 7)
  --w-max <w>              clip importance weights at w >= 1, or inf (default 100)
  --reward-range <lo,hi>   raw reward range for model training
                           (default: the observed minimum and maximum)
  --reward-aggregation first|sum
                           how a row's rewards array becomes one reward: the
                           first by seq, or the sum (default first)
  --gates <file>           promotion gates, YAML or JSON (*.json), e.g.
                           gates: [\"lift.dr.lower >= 0.01\", \"ess >= 200\"]
                           (lift.dr.lower >= x is the recommended gate)
  --format json|markdown   report format (default json)
  --out <path>             write the report to a file instead of stdout
  --fail-on-gate           exit 1 when a gate fails
  --store <root> --capsule <tenant/job/capsule>
                           read decisions from the event store (not available yet)

Rows: {decisionId, tsMs, context, derived, actions, eligible, pmf, chosen,
probability, reward, targetPmf}, or decisions exactly as GET .../decisions/{id}
returns them (chosenIndex, action, rewards and metadata). Legacy decisions
with a null pmf are counted and skipped.

Exit codes: 0 gates pass or none set, 1 a gate failed with --fail-on-gate,
2 usage or data error.";

/// Where the logged rows come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Jsonl(PathBuf),
    /// The event store (`<root>/syntra.db`), one capsule. Not available
    /// yet; see [`load_rows`].
    Store {
        root: PathBuf,
        capsule: String,
    },
}

/// Report format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Markdown,
}

/// Parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub input: Input,
    pub policy: String,
    pub config: EvalConfig,
    /// How a row's `rewards` array becomes one reward.
    pub aggregation: RewardAggregation,
    pub gates: Option<PathBuf>,
    pub format: Format,
    pub out: Option<PathBuf>,
    pub fail_on_gate: bool,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Evaluate(Box<Options>),
}

/// Run `syntra evaluate` with the arguments after the subcommand name and
/// return the process exit code.
pub fn run(args: &[String]) -> i32 {
    let options = match parse_args(args) {
        Ok(Command::Help) => {
            eprintln!("{USAGE}");
            return 0;
        }
        Ok(Command::Evaluate(options)) => options,
        Err(e) => {
            eprintln!("syntra evaluate: {e}");
            eprintln!("Run `syntra evaluate --help` for usage.");
            return 2;
        }
    };
    let report = match evaluate(&options) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("syntra evaluate: {e}");
            return 2;
        }
    };
    let rendered = match options.format {
        Format::Json => report.to_json(),
        Format::Markdown => report.to_markdown(),
    };
    let written = match &options.out {
        Some(path) => std::fs::write(path, rendered.as_bytes())
            .map(|()| {
                eprintln!(
                    "syntra evaluate: wrote {}. {}",
                    path.display(),
                    report.verdict
                )
            })
            .map_err(|e| format!("cannot write {}: {e}", path.display())),
        None => {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{}", rendered.trim_end())
                .and_then(|()| stdout.flush())
                .map_err(|e| format!("cannot write the report: {e}"))
        }
    };
    if let Err(e) = written {
        eprintln!("syntra evaluate: {e}");
        return 2;
    }
    if options.fail_on_gate && !report.gates_passed {
        1
    } else {
        0
    }
}

/// Load the policy, gates and rows, then evaluate. Arguments are checked
/// before the rows are read, so a typo fails fast.
fn evaluate(options: &Options) -> Result<Report, String> {
    options.config.validate()?;
    let policy = PolicyChoice::parse(&options.policy)?;
    let gates = match &options.gates {
        Some(path) => load_gates(path)?,
        None => Vec::new(),
    };
    let rows = load_rows(&options.input, options.aggregation)?;
    super::run(rows, &policy, &options.config, &gates)
}

/// Read the logged rows, reducing `rewards` arrays by `aggregation`.
///
/// EXTENSION POINT (event store): `Input::Store` should open
/// `<root>/syntra.db` read-only, turn each of the capsule's decision
/// records into the JSON of `GET .../decisions/{id}` (with its `rewards`),
/// and pass them to [`super::row::from_records`] with `aggregation`. Legacy
/// rows (null `pmf`) and rows without a reward are counted and skipped
/// there and downstream; nothing else in the pipeline changes.
pub fn load_rows(input: &Input, aggregation: RewardAggregation) -> Result<LoadedRows, String> {
    match input {
        Input::Jsonl(path) => load_jsonl(path, aggregation),
        Input::Store { root, capsule } => Err(format!(
            "--store {} --capsule {capsule}: reading decisions from the event store is not \
             available yet; export the capsule's decisions to JSONL and pass --input",
            root.display()
        )),
    }
}

/// Parse the arguments after `evaluate`.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Command::Help);
    }
    let mut input = None;
    let mut store = None;
    let mut capsule = None;
    let mut policy = None;
    let mut gates = None;
    let mut format = None;
    let mut out = None;
    let mut folds = None;
    let mut bootstrap = None;
    let mut seed = None;
    let mut w_max = None;
    let mut reward_range = None;
    let mut aggregation = None;
    let mut fail_on_gate = false;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--fail-on-gate" {
            fail_on_gate = true;
            i += 1;
            continue;
        }
        if !flag.starts_with("--") {
            return Err(format!("unexpected argument {flag:?}"));
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?
            .as_str();
        i += 2;
        let slot_error = || format!("{flag} is given more than once");
        match flag {
            "--input" => set(&mut input, PathBuf::from(value)).ok_or_else(slot_error)?,
            "--store" => set(&mut store, PathBuf::from(value)).ok_or_else(slot_error)?,
            "--capsule" => set(&mut capsule, parse_capsule(value)?).ok_or_else(slot_error)?,
            "--policy" => set(&mut policy, value.to_string()).ok_or_else(slot_error)?,
            "--gates" => set(&mut gates, PathBuf::from(value)).ok_or_else(slot_error)?,
            "--out" => set(&mut out, PathBuf::from(value)).ok_or_else(slot_error)?,
            "--format" => {
                let parsed = match value {
                    "json" => Format::Json,
                    "markdown" => Format::Markdown,
                    _ => return Err(format!("--format must be json or markdown (got {value:?})")),
                };
                set(&mut format, parsed).ok_or_else(slot_error)?
            }
            "--folds" => set(&mut folds, number(flag, value)?).ok_or_else(slot_error)?,
            "--bootstrap" => set(&mut bootstrap, number(flag, value)?).ok_or_else(slot_error)?,
            "--seed" => set(&mut seed, number(flag, value)?).ok_or_else(slot_error)?,
            "--w-max" => set(&mut w_max, number(flag, value)?).ok_or_else(slot_error)?,
            "--reward-range" => {
                set(&mut reward_range, parse_range(value)?).ok_or_else(slot_error)?
            }
            "--reward-aggregation" => {
                let parsed = match value {
                    "first" => RewardAggregation::First,
                    "sum" => RewardAggregation::Sum,
                    _ => {
                        return Err(format!(
                            "--reward-aggregation must be first or sum (got {value:?})"
                        ));
                    }
                };
                set(&mut aggregation, parsed).ok_or_else(slot_error)?
            }
            _ => return Err(format!("unknown option {flag}")),
        }
    }

    let input = match (input, store, capsule) {
        (Some(path), None, None) => Input::Jsonl(path),
        (None, Some(root), Some(capsule)) => Input::Store { root, capsule },
        (Some(_), _, _) => return Err("--input cannot be combined with --store/--capsule".into()),
        (None, Some(_), None) => return Err("--store needs --capsule <tenant/job/capsule>".into()),
        (None, None, Some(_)) => return Err("--capsule needs --store <root>".into()),
        (None, None, None) => return Err("--input <rows.jsonl> is required".into()),
    };
    let policy = policy.ok_or(
        "--policy is required (logged, constant:<id>, greedy, spec:<spec.json> or target-column)",
    )?;
    let defaults = EvalConfig::default();
    let config = EvalConfig {
        folds: folds.unwrap_or(defaults.folds),
        bootstrap: bootstrap.unwrap_or(defaults.bootstrap),
        seed: seed.unwrap_or(defaults.seed),
        w_max: w_max.unwrap_or(defaults.w_max),
        reward_range,
        reward_model: defaults.reward_model,
    };
    config.validate()?;
    Ok(Command::Evaluate(Box::new(Options {
        input,
        policy,
        config,
        aggregation: aggregation.unwrap_or_default(),
        gates,
        format: format.unwrap_or(Format::Json),
        out,
        fail_on_gate,
    })))
}

/// Fill an empty slot; `None` when it was already filled.
fn set<T>(slot: &mut Option<T>, value: T) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(value);
    Some(())
}

fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} expects a number (got {value:?})"))
}

/// `lo,hi` as two finite numbers with `lo < hi`.
fn parse_range(value: &str) -> Result<[f64; 2], String> {
    let parsed = value.split_once(',').and_then(|(lo, hi)| {
        Some([
            lo.trim().parse::<f64>().ok()?,
            hi.trim().parse::<f64>().ok()?,
        ])
    });
    match parsed {
        Some([lo, hi]) if lo.is_finite() && hi.is_finite() && lo < hi => Ok([lo, hi]),
        _ => Err(format!(
            "--reward-range expects lo,hi with finite lo < hi (got {value:?})"
        )),
    }
}

/// `tenant/job/capsule`, three non-empty segments.
fn parse_capsule(value: &str) -> Result<String, String> {
    let parts: Vec<&str> = value.split('/').collect();
    if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "--capsule expects tenant/job/capsule (got {value:?})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    fn options(text: &str) -> Options {
        match parse_args(&args(text)).unwrap() {
            Command::Evaluate(options) => *options,
            Command::Help => panic!("help"),
        }
    }

    fn error(text: &str) -> String {
        parse_args(&args(text)).unwrap_err()
    }

    #[test]
    fn defaults_and_every_option() {
        let o = options("--input rows.jsonl --policy greedy");
        assert_eq!(o.input, Input::Jsonl("rows.jsonl".into()));
        assert_eq!(o.policy, "greedy");
        assert_eq!(o.config, EvalConfig::default());
        assert_eq!(o.aggregation, RewardAggregation::First);
        assert_eq!(
            (o.format, o.out, o.gates, o.fail_on_gate),
            (Format::Json, None, None, false)
        );
        let o = options(
            "--policy constant:a --input r.jsonl --folds 10 --bootstrap 0 --seed 99 --w-max inf \
             --reward-range -1,2.5 --gates g.yaml --format markdown --out report.md --fail-on-gate \
             --reward-aggregation sum",
        );
        assert_eq!(o.config.folds, 10);
        assert_eq!(o.config.bootstrap, 0);
        assert_eq!(o.config.seed, 99);
        assert_eq!(o.config.w_max, f64::INFINITY);
        assert_eq!(o.config.reward_range, Some([-1.0, 2.5]));
        assert_eq!(o.gates, Some("g.yaml".into()));
        assert_eq!(o.format, Format::Markdown);
        assert_eq!(o.out, Some("report.md".into()));
        assert!(o.fail_on_gate);
        assert_eq!(o.aggregation, RewardAggregation::Sum);
        assert_eq!(parse_args(&args("--policy x -h")).unwrap(), Command::Help);
        assert_eq!(parse_args(&args("--help")).unwrap(), Command::Help);
    }

    #[test]
    fn store_input_is_parsed_for_the_extension_point() {
        let o = options("--store ./store --capsule acme/default/router --policy logged");
        assert_eq!(
            o.input,
            Input::Store {
                root: "./store".into(),
                capsule: "acme/default/router".into()
            }
        );
        let e = load_rows(&o.input, RewardAggregation::First).unwrap_err();
        assert!(e.contains("not available yet"), "{e}");
    }

    #[test]
    fn usage_errors() {
        let cases = [
            ("--policy greedy", "--input <rows.jsonl> is required"),
            ("--input r.jsonl", "--policy is required"),
            (
                "--input r.jsonl --policy greedy --folds 1",
                "folds must be an integer in [2, 100]",
            ),
            (
                "--input r.jsonl --policy greedy --folds x",
                "--folds expects a number (got \"x\")",
            ),
            (
                "--input r.jsonl --policy greedy --bootstrap 50",
                "bootstrap must be 0",
            ),
            (
                "--input r.jsonl --policy greedy --seed -1",
                "--seed expects a number",
            ),
            (
                "--input r.jsonl --policy greedy --w-max 0.5",
                "w-max must be at least 1",
            ),
            (
                "--input r.jsonl --policy greedy --reward-range 1",
                "--reward-range expects lo,hi",
            ),
            (
                "--input r.jsonl --policy greedy --reward-range 2,1",
                "--reward-range expects lo,hi",
            ),
            (
                "--input r.jsonl --policy greedy --format yaml",
                "--format must be json or markdown",
            ),
            (
                "--input r.jsonl --policy greedy --reward-aggregation last",
                "--reward-aggregation must be first or sum (got \"last\")",
            ),
            (
                "--input r.jsonl --policy greedy --policy logged",
                "--policy is given more than once",
            ),
            (
                "--input r.jsonl --policy greedy --out",
                "--out needs a value",
            ),
            (
                "--input r.jsonl --policy greedy --verbose 1",
                "unknown option --verbose",
            ),
            (
                "--input r.jsonl --policy greedy extra",
                "unexpected argument \"extra\"",
            ),
            ("--store s --policy greedy", "--store needs --capsule"),
            ("--capsule a/b/c --policy greedy", "--capsule needs --store"),
            (
                "--store s --capsule a/b --policy greedy",
                "--capsule expects tenant/job/capsule",
            ),
            (
                "--input r --store s --capsule a/b/c --policy greedy",
                "cannot be combined",
            ),
        ];
        for (text, want) in cases {
            let e = error(text);
            assert!(e.contains(want), "{text:?}: {e}");
        }
    }
}
