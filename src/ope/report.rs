//! The evaluation report: the numbers, the gate results and a one-line
//! verdict, as JSON (camelCase; undefined numbers are null) or Markdown.

use std::fmt::Write as _;

use serde::Serialize;

use super::data::RangeSource;
use super::estimators::{Estimate, Evaluation, Interval};
use super::gates::{Gate, GateResult};

/// An evaluation with its gates checked.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    #[serde(flatten)]
    pub evaluation: Evaluation,
    pub gates: Vec<GateResult>,
    /// True when every gate passes, including when there are none.
    pub gates_passed: bool,
    /// One plain-language line: pass or fail, and DR against the logged
    /// policy.
    pub verdict: String,
}

impl Report {
    /// Check `gates` against `evaluation` and write the verdict.
    pub fn new(evaluation: Evaluation, gates: &[Gate]) -> Self {
        let results: Vec<GateResult> = gates.iter().map(|g| g.check(&evaluation)).collect();
        let gates_passed = results.iter().all(|r| r.pass);
        let verdict = verdict(&evaluation, &results);
        Self {
            evaluation,
            gates: results,
            gates_passed,
            verdict,
        }
    }

    /// Pretty-printed JSON.
    pub fn to_json(&self) -> String {
        // Keys are strings and non-finite floats become null, so there is
        // no error for serde_json to report.
        serde_json::to_string_pretty(self).expect("a report always serializes to JSON")
    }

    /// A Markdown page: verdict, one estimator table, diagnostics, gates
    /// and warnings.
    pub fn to_markdown(&self) -> String {
        let e = &self.evaluation;
        let d = &e.diagnostics;
        let data = &e.data;
        let mut out = String::new();
        // Writing to a String cannot fail.
        let _ = writeln!(out, "# Off-policy evaluation: {}\n", cell(&e.policy));
        let _ = writeln!(out, "**Verdict:** {}\n", self.verdict);
        let _ = writeln!(
            out,
            "Values are in raw reward units, over n = {} rows with a reward ({} of {} rows had none).\n",
            d.n, data.rows_without_reward, data.rows
        );
        let _ = writeln!(out, "| Estimator | Value | SE | 95% CI | Normal 95% CI |");
        let _ = writeln!(out, "|---|---:|---:|---:|---:|");
        let logged = &e.logged;
        let _ = writeln!(
            out,
            "| Logged policy (on-policy mean) | {} | {} | {} | {} |",
            num(logged.mean),
            num(logged.se),
            range(logged.lower, logged.upper),
            range(logged.normal_lower, logged.normal_upper)
        );
        let rows: [(&str, &Estimate); 4] = [
            ("DM (reward model)", &e.estimators.dm),
            ("IPS", &e.estimators.ips),
            ("SNIPS", &e.estimators.snips),
            ("DR (doubly robust)", &e.estimators.dr),
        ];
        for (name, est) in rows {
            let _ = writeln!(
                out,
                "| {name} | {} | {} | {} | {} |",
                num(est.estimate),
                num(est.se),
                range(est.lower, est.upper),
                range(est.normal_lower, est.normal_upper)
            );
        }
        let s = &e.settings;
        let method = match s.interval {
            Interval::BootstrapPercentile => format!(
                "percentile bootstrap over rows, {} resamples, seed {}",
                s.bootstrap, s.seed
            ),
            Interval::Normal => "normal approximation (bootstrap off)".to_string(),
        };
        let _ = writeln!(
            out,
            "\n95% CI: {method}. DM and DR use a {}-fold cross-fitted reward model; \
             the intervals hold the fitted models fixed.\n",
            data.folds
        );

        let _ = writeln!(out, "## Diagnostics\n");
        let _ = writeln!(out, "| Diagnostic | Value |");
        let _ = writeln!(out, "|---|---:|");
        let clip = if s.w_max.is_finite() {
            format!("clip at {}", s.w_max)
        } else {
            "no clipping".to_string()
        };
        let source = match data.reward_range_source {
            RangeSource::Given => "given",
            RangeSource::Observed => "observed",
        };
        let diagnostics = [
            ("Rows evaluated (n)", d.n.to_string()),
            ("Effective sample size", num(d.ess)),
            (
                "Coverage (target mass on the logged action)",
                percent(d.coverage),
            ),
            (
                "Clipped weights",
                format!("{} ({}, {clip})", d.clipped_rows, percent(d.clip_rate)),
            ),
            ("Largest importance weight", num(d.max_weight)),
            ("Mean importance weight", num(d.mean_weight)),
            (
                "Rows where the target has no eligible action",
                d.target_ineligible.to_string(),
            ),
            (
                "Target mass the logging policy never takes",
                percent(d.unsupported_mass),
            ),
            (
                "Reward range for model training",
                format!(
                    "[{}, {}] ({source})",
                    data.reward_range[0], data.reward_range[1]
                ),
            ),
        ];
        for (name, value) in diagnostics {
            let _ = writeln!(out, "| {name} | {value} |");
        }

        let _ = writeln!(out, "\n## Gates\n");
        if self.gates.is_empty() {
            let _ = writeln!(out, "No gates were set.");
        } else {
            let _ = writeln!(out, "| Check | Left | Right | Result |");
            let _ = writeln!(out, "|---|---:|---:|---|");
            for g in &self.gates {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} |",
                    cell(&g.check),
                    num(g.left),
                    num(g.right),
                    if g.pass { "pass" } else { "**fail**" }
                );
            }
        }
        if !e.warnings.is_empty() {
            let _ = writeln!(out, "\n## Warnings\n");
            for w in &e.warnings {
                let _ = writeln!(out, "- {w}");
            }
        }
        out
    }
}

/// The verdict line.
fn verdict(e: &Evaluation, gates: &[GateResult]) -> String {
    let dr = &e.estimators.dr;
    let logged = &e.logged;
    let headline = format!(
        "DR estimates {} at {} (95% CI {}) against {} for the logged policy (95% CI {})",
        e.policy,
        num(dr.estimate),
        range(dr.lower, dr.upper),
        num(logged.mean),
        range(logged.lower, logged.upper)
    );
    let failed: Vec<String> = gates
        .iter()
        .filter(|g| !g.pass)
        .map(|g| format!("{}: {} vs {}", g.check, num(g.left), num(g.right)))
        .collect();
    if gates.is_empty() {
        let relation = if dr.lower > logged.mean {
            "the whole DR interval lies above the logged mean"
        } else if dr.upper < logged.mean {
            "the whole DR interval lies below the logged mean"
        } else if dr.lower <= logged.mean && logged.mean <= dr.upper {
            "the DR interval contains the logged mean"
        } else {
            "the DR interval is undefined"
        };
        format!("No gates set. {headline}; {relation}.")
    } else if failed.is_empty() {
        format!("PASS: all {} gates pass. {headline}.", gates.len())
    } else {
        format!(
            "FAIL: {} of {} gates fail ({}). {headline}.",
            failed.len(),
            gates.len(),
            failed.join("; ")
        )
    }
}

/// Four significant digits; scientific notation for very large or small
/// magnitudes; `n/a` when undefined.
fn num(x: f64) -> String {
    if x.is_nan() {
        return "n/a".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    // The exponent after rounding to four digits, so 9.9996 counts as 10.
    let scientific = format!("{x:.3e}");
    let exponent: i32 = scientific
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);
    if !(-4..6).contains(&exponent) {
        return scientific;
    }
    let decimals = (3 - exponent).max(0) as usize;
    format!("{x:.decimals$}")
}

fn range(lower: f64, upper: f64) -> String {
    format!("{} to {}", num(lower), num(upper))
}

fn percent(x: f64) -> String {
    if x.is_nan() {
        "n/a".into()
    } else {
        format!("{:.2}%", 100.0 * x)
    }
}

/// Text safe inside a Markdown table cell or heading.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('`', "'")
        .replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ope::gates::tests::sample;
    use serde_json::Value;

    fn gates(texts: &[&str]) -> Vec<Gate> {
        texts.iter().map(|t| Gate::parse(t).unwrap()).collect()
    }

    #[test]
    fn verdicts() {
        let pass = Report::new(
            sample(),
            &gates(&["dr.lower >= logged.mean + 0.01", "n >= 1000"]),
        );
        assert!(pass.gates_passed);
        assert_eq!(
            pass.verdict,
            "PASS: all 2 gates pass. DR estimates greedy at 0.4200 (95% CI 0.4000 to 0.4400) \
             against 0.3700 for the logged policy (95% CI 0.3600 to 0.3800)."
        );
        let fail = Report::new(
            sample(),
            &gates(&["ess >= 200", "coverage >= 0.5", "snips.lower >= 0"]),
        );
        assert!(!fail.gates_passed);
        assert!(
            fail.verdict.starts_with(
                "FAIL: 2 of 3 gates fail (coverage >= 0.5: 0.4000 vs 0.5000; snips.lower >= 0: n/a vs 0)."
            ),
            "{}",
            fail.verdict
        );
        let none = Report::new(sample(), &[]);
        assert!(none.gates_passed);
        assert!(
            none.verdict
                .starts_with("No gates set. DR estimates greedy")
        );
        assert!(
            none.verdict
                .ends_with("the whole DR interval lies above the logged mean.")
        );
        let mut below = sample();
        below.estimators.dr.lower = 0.2;
        below.estimators.dr.upper = 0.3;
        assert!(
            Report::new(below, &[])
                .verdict
                .ends_with("lies below the logged mean.")
        );
        let mut overlap = sample();
        overlap.estimators.dr.lower = 0.36;
        assert!(
            Report::new(overlap, &[])
                .verdict
                .ends_with("contains the logged mean.")
        );
    }

    #[test]
    fn json_is_camel_case_and_null_for_undefined() {
        let report = Report::new(sample(), &gates(&["ess >= 200"]));
        let json: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["policy"], "greedy");
        assert_eq!(json["gatesPassed"], true);
        assert_eq!(json["estimators"]["dr"]["estimate"], 0.42);
        assert_eq!(json["estimators"]["dr"]["normalLower"], 0.42 - 1.96 * 0.01);
        assert_eq!(json["estimators"]["snips"]["estimate"], Value::Null);
        assert_eq!(json["logged"]["mean"], 0.37);
        assert_eq!(json["diagnostics"]["clipRate"], 0.001);
        assert_eq!(json["diagnostics"]["targetIneligible"], 0);
        assert_eq!(json["data"]["rowsWithoutReward"], 12);
        assert_eq!(json["data"]["rewardRangeSource"], "observed");
        assert_eq!(json["settings"]["interval"], "bootstrapPercentile");
        assert_eq!(json["settings"]["rewardModel"]["bits"], 18);
        assert_eq!(json["gates"][0]["check"], "ess >= 200");
        assert_eq!(json["gates"][0]["left"], 850.5);
        assert!(json["verdict"].as_str().unwrap().starts_with("PASS"));
        assert_eq!(json["warnings"], Value::Array(vec![]));
    }

    #[test]
    fn markdown_has_the_table_diagnostics_and_gates() {
        let mut evaluation = sample();
        evaluation.warnings = vec!["something to know".into()];
        let md = Report::new(evaluation, &gates(&["coverage >= 0.5"])).to_markdown();
        for want in [
            "# Off-policy evaluation: greedy\n",
            "**Verdict:** FAIL: 1 of 1 gates fail",
            "| Estimator | Value | SE | 95% CI | Normal 95% CI |",
            "| Logged policy (on-policy mean) | 0.3700 | 0.005000 | 0.3600 to 0.3800 | 0.3602 to 0.3798 |",
            "| DR (doubly robust) | 0.4200 | 0.01000 | 0.4000 to 0.4400 |",
            "| SNIPS | n/a | n/a | n/a to n/a | n/a to n/a |",
            "percentile bootstrap over rows, 1000 resamples, seed 7",
            "| Effective sample size | 850.5 |",
            "| Clipped weights | 5 (0.10%, clip at 100) |",
            "| Reward range for model training | [0, 1] (observed) |",
            "| `coverage >= 0.5` | 0.4000 | 0.5000 | **fail** |",
            "## Warnings\n\n- something to know\n",
        ] {
            assert!(md.contains(want), "missing {want:?} in\n{md}");
        }
        let md = Report::new(sample(), &[]).to_markdown();
        assert!(md.contains("No gates were set."));
        assert!(!md.contains("## Warnings"));
    }

    #[test]
    fn number_formatting() {
        assert_eq!(num(0.41234), "0.4123");
        assert_eq!(num(-0.41234), "-0.4123");
        assert_eq!(num(12.3456), "12.35");
        assert_eq!(num(1234.4), "1234");
        assert_eq!(num(9.99996), "10.00");
        assert_eq!(num(0.01), "0.01000");
        assert_eq!(num(123_456.0), "123456");
        assert_eq!(num(1_234_567.0), "1.235e6");
        assert_eq!(num(0.001234), "0.001234");
        assert_eq!(num(0.00001234), "1.234e-5");
        assert_eq!(num(0.0), "0");
        assert_eq!(num(f64::NAN), "n/a");
        assert_eq!(num(f64::INFINITY), "inf");
        assert_eq!(percent(0.1234), "12.34%");
        assert_eq!(cell("a|b`c"), "a\\|b'c");
    }
}
