//! Promotion gates: checks on an evaluation's numbers, such as
//!
//! ```text
//! dr.lower >= logged.mean + 0.01
//! ess >= 200
//! coverage >= 0.5
//! n >= 1000
//! ```
//!
//! # Grammar
//!
//! ```text
//! check   := metric op operand [ ("+" | "-") number ]
//! operand := metric | number
//! op      := ">=" | ">" | "<=" | "<" | "=="
//! metric  := ("dm" | "ips" | "snips" | "dr") "." ("estimate" | "lower" | "upper" | "se")
//!          | "logged." ("mean" | "lower" | "upper")
//!          | "ess" | "coverage" | "n" | "clip_rate" | "max_weight"
//! ```
//!
//! Numbers are decimal (`0.01`, `-2`, `1e-3`). Whitespace is optional.
//! Unknown metrics are parse errors. `lower` and `upper` are the ends of the
//! 95% interval (bootstrap percentile unless the bootstrap is off), `se`
//! the standard error. A check whose values are undefined (NaN, such as
//! SNIPS with no positive weight) fails.
//!
//! # Files
//!
//! A gates file (YAML, or JSON when the name ends in `.json`) holds a list
//! of check strings, either at the top level or under a `gates` key:
//!
//! ```yaml
//! gates:
//!   - dr.lower >= logged.mean + 0.01
//!   - ess >= 200
//! ```

use std::fmt;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::estimators::{Estimate, Evaluation};

/// One of the four estimators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estimator {
    Dm,
    Ips,
    Snips,
    Dr,
}

/// A statistic of an estimator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    Estimate,
    Lower,
    Upper,
    Se,
}

/// A number a gate can refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Estimator(Estimator, Stat),
    LoggedMean,
    LoggedLower,
    LoggedUpper,
    Ess,
    Coverage,
    N,
    ClipRate,
    MaxWeight,
}

const METRIC_HELP: &str = "{dm,ips,snips,dr}.{estimate,lower,upper,se}, \
     logged.{mean,lower,upper}, ess, coverage, n, clip_rate, max_weight";

impl Metric {
    /// Parse a metric name; unknown names are errors.
    pub fn parse(name: &str) -> Result<Self, String> {
        let metric = match name {
            "logged.mean" => Self::LoggedMean,
            "logged.lower" => Self::LoggedLower,
            "logged.upper" => Self::LoggedUpper,
            "ess" => Self::Ess,
            "coverage" => Self::Coverage,
            "n" => Self::N,
            "clip_rate" => Self::ClipRate,
            "max_weight" => Self::MaxWeight,
            _ => {
                let parsed = name.split_once('.').and_then(|(estimator, stat)| {
                    let estimator = match estimator {
                        "dm" => Estimator::Dm,
                        "ips" => Estimator::Ips,
                        "snips" => Estimator::Snips,
                        "dr" => Estimator::Dr,
                        _ => return None,
                    };
                    let stat = match stat {
                        "estimate" => Stat::Estimate,
                        "lower" => Stat::Lower,
                        "upper" => Stat::Upper,
                        "se" => Stat::Se,
                        _ => return None,
                    };
                    Some(Self::Estimator(estimator, stat))
                });
                parsed.ok_or_else(|| {
                    format!("unknown metric {name:?}; expected one of {METRIC_HELP}")
                })?
            }
        };
        Ok(metric)
    }

    /// The metric's value in `evaluation`.
    pub fn value(&self, evaluation: &Evaluation) -> f64 {
        let e = evaluation;
        let d = &e.diagnostics;
        match *self {
            Self::Estimator(estimator, stat) => {
                let est: &Estimate = match estimator {
                    Estimator::Dm => &e.estimators.dm,
                    Estimator::Ips => &e.estimators.ips,
                    Estimator::Snips => &e.estimators.snips,
                    Estimator::Dr => &e.estimators.dr,
                };
                match stat {
                    Stat::Estimate => est.estimate,
                    Stat::Lower => est.lower,
                    Stat::Upper => est.upper,
                    Stat::Se => est.se,
                }
            }
            Self::LoggedMean => e.logged.mean,
            Self::LoggedLower => e.logged.lower,
            Self::LoggedUpper => e.logged.upper,
            Self::Ess => d.ess,
            Self::Coverage => d.coverage,
            Self::N => d.n as f64,
            Self::ClipRate => d.clip_rate,
            Self::MaxWeight => d.max_weight,
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Estimator(estimator, stat) => {
                let estimator = match estimator {
                    Estimator::Dm => "dm",
                    Estimator::Ips => "ips",
                    Estimator::Snips => "snips",
                    Estimator::Dr => "dr",
                };
                let stat = match stat {
                    Stat::Estimate => "estimate",
                    Stat::Lower => "lower",
                    Stat::Upper => "upper",
                    Stat::Se => "se",
                };
                return write!(f, "{estimator}.{stat}");
            }
            Self::LoggedMean => "logged.mean",
            Self::LoggedLower => "logged.lower",
            Self::LoggedUpper => "logged.upper",
            Self::Ess => "ess",
            Self::Coverage => "coverage",
            Self::N => "n",
            Self::ClipRate => "clip_rate",
            Self::MaxWeight => "max_weight",
        };
        f.write_str(name)
    }
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

impl Op {
    /// Compare; false whenever either side is NaN.
    pub fn holds(self, left: f64, right: f64) -> bool {
        match self {
            Op::Ge => left >= right,
            Op::Gt => left > right,
            Op::Le => left <= right,
            Op::Lt => left < right,
            Op::Eq => left == right,
        }
    }
}

/// Right-hand side of a check before the offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operand {
    Metric(Metric),
    Number(f64),
}

/// One parsed check.
#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    /// The check as written (trimmed).
    pub text: String,
    pub left: Metric,
    pub op: Op,
    pub right: Operand,
    /// Added to the right-hand side (`- 0.01` is an offset of -0.01).
    pub offset: f64,
}

/// A check's outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateResult {
    pub check: String,
    /// Left-hand value (JSON null when undefined).
    pub left: f64,
    /// Right-hand value, offset included (JSON null when undefined).
    pub right: f64,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Name(String),
    Number(f64),
    Op(Op),
    Plus,
    Minus,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            _ if c.is_whitespace() => i += 1,
            '>' | '<' | '=' => {
                let (op, width) = match (c, next) {
                    ('>', Some('=')) => (Op::Ge, 2),
                    ('<', Some('=')) => (Op::Le, 2),
                    ('=', Some('=')) => (Op::Eq, 2),
                    ('>', _) => (Op::Gt, 1),
                    ('<', _) => (Op::Lt, 1),
                    _ => return Err("'=' must be written '=='".into()),
                };
                tokens.push(Token::Op(op));
                i += width;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            _ if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() {
                    let d = chars[i];
                    let exponent_sign = (d == '+' || d == '-') && matches!(chars[i - 1], 'e' | 'E');
                    if d.is_ascii_digit() || matches!(d, '.' | 'e' | 'E') || exponent_sign {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let literal: String = chars[start..i].iter().collect();
                let value: f64 = literal
                    .parse()
                    .map_err(|_| format!("{literal:?} is not a number"))?;
                if !value.is_finite() {
                    return Err(format!("{literal:?} is not a finite number"));
                }
                tokens.push(Token::Number(value));
            }
            _ if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                {
                    i += 1;
                }
                tokens.push(Token::Name(chars[start..i].iter().collect()));
            }
            _ => return Err(format!("unexpected character {c:?}")),
        }
    }
    Ok(tokens)
}

/// Reads an optionally signed number at `tokens[*at]`.
fn signed_number(tokens: &[Token], at: &mut usize) -> Option<f64> {
    let sign = match tokens.get(*at) {
        Some(Token::Minus) => -1.0,
        Some(Token::Plus) => 1.0,
        _ => {
            if let Some(Token::Number(v)) = tokens.get(*at) {
                *at += 1;
                return Some(*v);
            }
            return None;
        }
    };
    if let Some(Token::Number(v)) = tokens.get(*at + 1) {
        *at += 2;
        Some(sign * v)
    } else {
        None
    }
}

impl Gate {
    /// Parse one check; see the module documentation for the grammar.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        Self::parse_tokens(text).map_err(|e| format!("gate {text:?}: {e}"))
    }

    fn parse_tokens(text: &str) -> Result<Self, String> {
        let tokens = tokenize(text)?;
        let expected = "expected <metric> <op> <metric-or-number> [(+|-) <number>]";
        let mut at = 0;
        let left = match tokens.first() {
            Some(Token::Name(name)) => Metric::parse(name)?,
            Some(_) => return Err(format!("the left side must be a metric; {expected}")),
            None => return Err(format!("empty check; {expected}")),
        };
        at += 1;
        let op = match tokens.get(at) {
            Some(Token::Op(op)) => *op,
            _ => return Err(format!("missing comparison (>=, >, <=, <, ==); {expected}")),
        };
        at += 1;
        let right = match tokens.get(at) {
            Some(Token::Name(name)) => {
                at += 1;
                Operand::Metric(Metric::parse(name)?)
            }
            _ => match signed_number(&tokens, &mut at) {
                Some(v) => Operand::Number(v),
                None => return Err(format!("missing right-hand side; {expected}")),
            },
        };
        let offset = match tokens.get(at) {
            None => 0.0,
            Some(Token::Plus | Token::Minus) => {
                let sign = if tokens[at] == Token::Minus {
                    -1.0
                } else {
                    1.0
                };
                match tokens.get(at + 1) {
                    Some(Token::Number(v)) => {
                        at += 2;
                        sign * v
                    }
                    _ => return Err(format!("an offset must be a number; {expected}")),
                }
            }
            Some(_) => {
                return Err(format!(
                    "unexpected text after the right-hand side; {expected}"
                ));
            }
        };
        if at != tokens.len() {
            return Err(format!("unexpected text after the offset; {expected}"));
        }
        Ok(Self {
            text: text.to_string(),
            left,
            op,
            right,
            offset,
        })
    }

    /// Evaluate against an evaluation's numbers.
    pub fn check(&self, evaluation: &Evaluation) -> GateResult {
        let left = self.left.value(evaluation);
        let right = match self.right {
            Operand::Metric(m) => m.value(evaluation),
            Operand::Number(v) => v,
        } + self.offset;
        GateResult {
            check: self.text.clone(),
            left,
            right,
            pass: self.op.holds(left, right),
        }
    }
}

/// Parse a gates document. `json` selects JSON; otherwise YAML. The
/// document is a list of check strings, or a mapping whose only key is
/// `gates` holding that list.
pub fn parse_gates(text: &str, json: bool) -> Result<Vec<Gate>, String> {
    if text.trim().is_empty() {
        return Err("gates file is empty".into());
    }
    let doc: Value = if json {
        serde_json::from_str(text).map_err(|e| format!("gates file is not valid JSON: {e}"))?
    } else {
        serde_norway::from_str(text).map_err(|e| format!("gates file is not valid YAML: {e}"))?
    };
    let items = match doc {
        Value::Array(items) => items,
        Value::Object(mut map) if map.len() == 1 && map.contains_key("gates") => {
            match map.remove("gates") {
                Some(Value::Array(items)) => items,
                _ => return Err("gates must be a list of check strings".into()),
            }
        }
        Value::Null => return Err("gates file is empty".into()),
        _ => {
            return Err(
                "gates file must be a list of check strings, or a mapping with a single `gates` list"
                    .into(),
            );
        }
    };
    items
        .iter()
        .enumerate()
        .map(|(k, item)| match item {
            Value::String(text) => Gate::parse(text).map_err(|e| format!("gates[{k}]: {e}")),
            other => Err(format!("gates[{k}] must be a string, not {other}")),
        })
        .collect()
}

/// Read a gates file: JSON when the name ends in `.json`, YAML otherwise.
pub fn load_gates(path: &Path) -> Result<Vec<Gate>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read gates {}: {e}", path.display()))?;
    let json = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    parse_gates(&text, json).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::decision::spec::LearnerSpec;
    use crate::ope::data::RangeSource;
    use crate::ope::estimators::{
        DataSummary, Diagnostics, Estimators, Interval, LoggedValue, Settings,
    };

    fn est(estimate: f64, se: f64) -> Estimate {
        Estimate {
            estimate,
            se,
            lower: estimate - 2.0 * se,
            upper: estimate + 2.0 * se,
            normal_lower: estimate - 1.96 * se,
            normal_upper: estimate + 1.96 * se,
        }
    }

    /// A hand-made evaluation: DR 0.42 +- 0.02 against logged 0.37.
    pub(crate) fn sample() -> Evaluation {
        Evaluation {
            policy: "greedy".into(),
            data: DataSummary {
                rows: 5000,
                rows_without_reward: 12,
                folds: 5,
                reward_range: [0.0, 1.0],
                reward_range_source: RangeSource::Observed,
                rewards_outside_range: 0,
            },
            logged: LoggedValue {
                mean: 0.37,
                se: 0.005,
                lower: 0.36,
                upper: 0.38,
                normal_lower: 0.3602,
                normal_upper: 0.3798,
            },
            estimators: Estimators {
                dm: est(0.45, 0.004),
                ips: est(0.41, 0.03),
                snips: est(f64::NAN, f64::NAN),
                dr: est(0.42, 0.01),
            },
            diagnostics: Diagnostics {
                n: 4988,
                ess: 850.5,
                coverage: 0.4,
                clip_rate: 0.001,
                clipped_rows: 5,
                max_weight: 250.0,
                mean_weight: 0.99,
                target_ineligible: 0,
                unsupported_mass: 0.0,
            },
            settings: Settings {
                bootstrap: 1000,
                seed: 7,
                w_max: 100.0,
                confidence: 0.95,
                interval: Interval::BootstrapPercentile,
                reward_model: LearnerSpec::default(),
            },
            warnings: vec![],
        }
    }

    fn check(text: &str) -> GateResult {
        Gate::parse(text).unwrap().check(&sample())
    }

    #[test]
    fn parses_the_documented_examples() {
        let g = Gate::parse("dr.lower >= logged.mean + 0.01").unwrap();
        assert_eq!(g.left, Metric::Estimator(Estimator::Dr, Stat::Lower));
        assert_eq!(g.op, Op::Ge);
        assert_eq!(g.right, Operand::Metric(Metric::LoggedMean));
        assert_eq!(g.offset, 0.01);
        assert_eq!(g.text, "dr.lower >= logged.mean + 0.01");
        let g = Gate::parse("  ess>=200 ").unwrap();
        assert_eq!(
            (g.left, g.op, g.right),
            (Metric::Ess, Op::Ge, Operand::Number(200.0))
        );
        assert_eq!(g.text, "ess>=200");
        assert_eq!(
            Gate::parse("coverage >= 0.5").unwrap().right,
            Operand::Number(0.5)
        );
        assert_eq!(Gate::parse("n >= 1000").unwrap().left, Metric::N);
        let g = Gate::parse("snips.upper<ips.estimate-1e-3").unwrap();
        assert_eq!(g.op, Op::Lt);
        assert_eq!(g.offset, -1e-3);
        let g = Gate::parse("dm.se <= -2.5E+1 - 3").unwrap();
        assert_eq!((g.right, g.offset), (Operand::Number(-25.0), -3.0));
        assert_eq!(Gate::parse("max_weight == 250").unwrap().op, Op::Eq);
        assert_eq!(
            Gate::parse("clip_rate > .5").unwrap().right,
            Operand::Number(0.5)
        );
        // Every metric name round-trips through Display.
        for name in [
            "dm.estimate",
            "ips.lower",
            "snips.upper",
            "dr.se",
            "logged.mean",
            "logged.lower",
            "logged.upper",
            "ess",
            "coverage",
            "n",
            "clip_rate",
            "max_weight",
        ] {
            assert_eq!(Metric::parse(name).unwrap().to_string(), name);
        }
    }

    #[test]
    fn rejects_malformed_checks() {
        let cases = [
            (
                "dr.lowr >= 1",
                "unknown metric \"dr.lowr\"; expected one of",
            ),
            ("ess >= logged.median", "unknown metric \"logged.median\""),
            ("DR.lower >= 1", "unknown metric \"DR.lower\""),
            ("ess", "missing comparison"),
            ("ess >=", "missing right-hand side"),
            ("ess = 3", "'=' must be written '=='"),
            ("ess => 3", "'=' must be written '=='"),
            ("3 <= ess", "the left side must be a metric"),
            ("", "empty check"),
            ("ess >= 3 +", "an offset must be a number"),
            ("ess >= 3 + ess", "an offset must be a number"),
            ("ess >= 3 4", "unexpected text after the right-hand side"),
            ("ess >= 3 + 1 + 1", "unexpected text after the offset"),
            ("ess >= 1..2", "\"1..2\" is not a number"),
            ("ess >= 1e999", "not a finite number"),
            ("ess >= $3", "unexpected character '$'"),
        ];
        for (text, want) in cases {
            let e = Gate::parse(text).unwrap_err();
            assert!(e.contains(want), "{text:?}: {e}");
            assert!(e.starts_with("gate \""), "{e}");
        }
    }

    #[test]
    fn evaluates_left_right_and_pass() {
        // DR lower 0.40 vs logged 0.37 + 0.01.
        let r = check("dr.lower >= logged.mean + 0.01");
        assert!(r.pass);
        assert!((r.left - 0.40).abs() < 1e-12 && (r.right - 0.38).abs() < 1e-12);
        let r = check("dr.lower >= logged.mean + 0.05");
        assert!(!r.pass);
        assert!(check("ess >= 200").pass);
        assert!(!check("coverage >= 0.5").pass);
        assert!(check("n >= 1000").pass);
        assert!(check("n == 4988").pass);
        assert!(check("clip_rate < 0.01").pass);
        assert!(!check("max_weight <= 100").pass);
        assert!(check("dr.estimate > ips.estimate").pass);
        assert!(check("dr.se < ips.se - 0.01").pass);
        assert!(check("logged.upper <= 0.38").pass);
        assert!(!check("logged.upper < 0.38").pass);
        // An undefined value fails every comparison.
        for text in [
            "snips.estimate >= 0",
            "snips.lower <= 1",
            "ess >= snips.upper",
            "snips.se == snips.se",
        ] {
            let r = check(text);
            assert!(!r.pass, "{text}");
        }
        let json = serde_json::to_value(check("snips.estimate >= 0")).unwrap();
        assert_eq!(json["left"], Value::Null);
        assert_eq!(json["right"], serde_json::json!(0.0));
        assert_eq!(json["pass"], Value::Bool(false));
    }

    #[test]
    fn gate_files_in_yaml_and_json() {
        let yaml = "gates:\n  - dr.lower >= logged.mean + 0.01\n  - ess >= 200\n";
        let gates = parse_gates(yaml, false).unwrap();
        assert_eq!(gates.len(), 2);
        assert_eq!(gates[1].left, Metric::Ess);
        let list = "- n >= 1000\n- coverage >= 0.5\n";
        assert_eq!(parse_gates(list, false).unwrap().len(), 2);
        let json = r#"{"gates": ["dr.lower >= logged.mean + 0.01", "ess >= 200"]}"#;
        assert_eq!(parse_gates(json, true).unwrap(), gates);
        assert_eq!(parse_gates(r#"["ess > 1"]"#, true).unwrap().len(), 1);
        assert!(parse_gates("gates: []", false).unwrap().is_empty());
        let e = parse_gates("gates:\n  - ess >= 200\n  - dr.lowr >= 1\n", false).unwrap_err();
        assert!(
            e.starts_with("gates[1]: gate \"dr.lowr >= 1\": unknown metric"),
            "{e}"
        );
        assert!(parse_gates("", false).unwrap_err().contains("empty"));
        assert!(
            parse_gates("gates: 3", false)
                .unwrap_err()
                .contains("list of check strings")
        );
        assert!(
            parse_gates("other: [a]", false)
                .unwrap_err()
                .contains("single `gates` list")
        );
        assert!(
            parse_gates("- 3", false)
                .unwrap_err()
                .contains("gates[0] must be a string")
        );
        assert!(
            parse_gates("{", true)
                .unwrap_err()
                .contains("not valid JSON")
        );
        assert!(
            parse_gates("gates: [", false)
                .unwrap_err()
                .contains("not valid YAML")
        );
        assert!(
            load_gates(Path::new("no/such/gates.yaml"))
                .unwrap_err()
                .starts_with("cannot read gates no/such/gates.yaml")
        );
    }
}
