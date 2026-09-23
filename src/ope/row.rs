//! Logged decision rows, the input to off-policy evaluation.
//!
//! A row is one decision the logging policy made: the context, the action
//! list, which actions were eligible, the PMF the chosen action was sampled
//! from, and the raw reward that came back, if any. On disk it is JSONL:
//! one camelCase JSON object per line.
//!
//! ```json
//! {"decisionId": "dec_1", "tsMs": 1758585600000, "context": {"segment": "A"},
//!  "actions": [{"id": "small"}, {"id": "large"}], "eligible": [0, 1],
//!  "pmf": [0.9, 0.1], "chosen": 1, "probability": 0.1, "reward": 1.0}
//! ```
//!
//! Required: `decisionId`, `actions`, `eligible`, `pmf`, `chosen`,
//! `probability`. Optional: `tsMs` (informational, default 0), `context`
//! and `derived` (default null), `reward` (rows without one are counted and
//! skipped when the evaluation data is built), and `targetPmf` (only for the
//! `target-column` policy). Unknown fields are rejected, so a misspelled
//! optional field such as `rewrd` is an error rather than a silently
//! missing reward.
//!
//! # Validation
//!
//! [`LoggedRow::check`] enforces, for every row:
//!
//! - `decisionId` is not empty;
//! - `actions` is a valid action list (unique non-empty ids, features that
//!   flatten), as for a decide request;
//! - `eligible` is non-empty, in range and has no repeated index;
//! - `pmf` has one entry per eligible action, each in `[0, 1]`, summing to
//!   1 within [`PMF_SUM_TOLERANCE`];
//! - `chosen` is eligible, and `probability` is positive and equals the
//!   PMF entry of the chosen action within [`PROBABILITY_TOLERANCE`];
//! - `reward`, when present, is finite;
//! - `targetPmf`, when present, is a PMF over the eligible actions under the
//!   same rules as `pmf`.
//!
//! [`LoggedRow::validate`] also featurizes the row (the context and derived
//! values must be JSON objects or null that flatten within the decision
//! core's limits). [`read_jsonl`] runs it on every line, rejects repeated
//! decision ids, and stops at the first problem with its line number.
//!
//! # Mapping from the event store
//!
//! A row is the decision record plus its reward, so an adapter from the
//! SQLite event store (`syntra.db`) is a field-by-field copy:
//!
//! | Row field     | Event store source                                        |
//! |---------------|-----------------------------------------------------------|
//! | `decisionId`  | the decision's id (`dec_...` or the caller's `eventId`)    |
//! | `tsMs`        | the decision's timestamp in Unix milliseconds             |
//! | `context`     | the request context (JSON column)                          |
//! | `derived`     | the feature program's derived features (JSON, or null)    |
//! | `actions`     | [`Decision::actions`](crate::decision::Decision) (JSON)    |
//! | `eligible`    | `Decision::eligible` (JSON)                                |
//! | `pmf`         | `Decision::pmf` (JSON)                                     |
//! | `chosen`      | `Decision::chosen`                                         |
//! | `probability` | `Decision::probability`                                    |
//! | `reward`      | the aggregated raw reward (see below)                      |
//! | `targetPmf`   | `None`                                                     |
//!
//! The reward is the raw reward as the capsule aggregates it: the first
//! reward under `rewards: first`, their sum under `rewards: sum`, and `None`
//! when none arrived. The seed, model version, mode and predictions are not
//! needed for OPE.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::decision::ActionSpec;
use crate::decision::spec::validate_actions;

use super::data::featurize;

/// Largest allowed distance between the sum of a PMF and 1.
pub const PMF_SUM_TOLERANCE: f64 = 1e-6;
/// Largest allowed distance between `probability` and the PMF entry of the
/// chosen action.
pub const PROBABILITY_TOLERANCE: f64 = 1e-9;

/// One logged decision. See the module documentation for the rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoggedRow {
    /// Unique id of the decision; it also picks the cross-fitting fold.
    pub decision_id: String,
    /// Decision time in Unix milliseconds. Informational only.
    #[serde(default)]
    pub ts_ms: i64,
    /// Request context: a JSON object, or null.
    #[serde(default)]
    pub context: Value,
    /// Derived features from the feature program: a JSON object, or null.
    #[serde(default)]
    pub derived: Value,
    /// The full action list the decision was made over, in order.
    pub actions: Vec<ActionSpec>,
    /// Indices into `actions` of the actions the logging policy could pick.
    pub eligible: Vec<usize>,
    /// Logging probability of each eligible action, aligned with `eligible`.
    pub pmf: Vec<f64>,
    /// Index into `actions` of the action taken.
    pub chosen: usize,
    /// Probability with which `chosen` was sampled.
    pub probability: f64,
    /// Raw reward, in the units the report uses. `None`: no reward arrived.
    #[serde(default)]
    pub reward: Option<f64>,
    /// A target policy's PMF over the eligible actions, aligned with
    /// `eligible`; used by the `target-column` policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_pmf: Option<Vec<f64>>,
}

impl LoggedRow {
    /// Position of the chosen action within `eligible`, if it is eligible.
    pub fn chosen_position(&self) -> Option<usize> {
        self.eligible.iter().position(|&i| i == self.chosen)
    }

    /// Structural validation: every rule in the module documentation except
    /// featurization. Returns the first problem found.
    pub fn check(&self) -> Result<(), String> {
        if self.decision_id.is_empty() {
            return Err("decisionId must not be empty".into());
        }
        if self.actions.is_empty() {
            return Err("actions must not be empty".into());
        }
        validate_actions(&self.actions, "actions")?;
        if self.eligible.is_empty() {
            return Err("eligible must not be empty".into());
        }
        let mut listed = vec![false; self.actions.len()];
        for (k, &index) in self.eligible.iter().enumerate() {
            if index >= self.actions.len() {
                return Err(format!(
                    "eligible[{k}] = {index} is out of range for {} actions",
                    self.actions.len()
                ));
            }
            if listed[index] {
                return Err(format!("eligible lists action index {index} twice"));
            }
            listed[index] = true;
        }
        check_pmf(&self.pmf, self.eligible.len(), "pmf")?;
        if self.chosen >= self.actions.len() {
            return Err(format!(
                "chosen = {} is out of range for {} actions",
                self.chosen,
                self.actions.len()
            ));
        }
        let Some(position) = self.chosen_position() else {
            return Err(format!(
                "chosen action {} ({:?}) is not eligible",
                self.chosen, self.actions[self.chosen].id
            ));
        };
        if !(self.probability > 0.0 && self.probability <= 1.0) {
            return Err(format!(
                "probability must be in (0, 1] (got {})",
                self.probability
            ));
        }
        let logged = self.pmf[position];
        if (self.probability - logged).abs() > PROBABILITY_TOLERANCE {
            return Err(format!(
                "probability {} does not match pmf[{position}] = {logged} of the chosen action",
                self.probability
            ));
        }
        if let Some(reward) = self.reward
            && !reward.is_finite()
        {
            return Err(format!("reward must be a finite number (got {reward})"));
        }
        if let Some(target) = &self.target_pmf {
            check_pmf(target, self.eligible.len(), "targetPmf")?;
        }
        Ok(())
    }

    /// [`Self::check`], then featurize the row the way the evaluation does,
    /// so feature errors surface while the line number is still known.
    pub fn validate(&self) -> Result<(), String> {
        self.check()?;
        featurize(self).map(|_| ())
    }
}

/// `values` must hold `len` probabilities in `[0, 1]` summing to 1 within
/// [`PMF_SUM_TOLERANCE`].
fn check_pmf(values: &[f64], len: usize, field: &str) -> Result<(), String> {
    if values.len() != len {
        return Err(format!(
            "{field} has {} entries but eligible has {len}",
            values.len()
        ));
    }
    for (k, &p) in values.iter().enumerate() {
        if !(0.0..=1.0).contains(&p) {
            return Err(format!("{field}[{k}] = {p} is not a probability in [0, 1]"));
        }
    }
    let sum: f64 = values.iter().sum();
    if (sum - 1.0).abs() > PMF_SUM_TOLERANCE {
        return Err(format!(
            "{field} sums to {sum}, not 1 (tolerance {PMF_SUM_TOLERANCE:e})"
        ));
    }
    Ok(())
}

/// Read and validate every row of a JSONL file. Errors start with the path
/// and the 1-based line number.
pub fn load_jsonl(path: &Path) -> Result<Vec<LoggedRow>, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    read_jsonl(BufReader::new(file)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read and validate JSONL rows: one JSON object per line, blank lines
/// ignored. Every row must pass [`LoggedRow::validate`] and decision ids
/// must be unique. Stops at the first problem; the error names its 1-based
/// line.
pub fn read_jsonl(reader: impl BufRead) -> Result<Vec<LoggedRow>, String> {
    let mut rows = Vec::new();
    let mut first_line: HashMap<String, usize> = HashMap::new();
    for (index, line) in reader.lines().enumerate() {
        let number = index + 1;
        let mut line = line.map_err(|e| format!("line {number}: {e}"))?;
        if number == 1 && line.starts_with('\u{feff}') {
            // A byte-order mark some exporters write.
            line.drain(..'\u{feff}'.len_utf8());
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('{') {
            return Err(format!("line {number}: expected a JSON object"));
        }
        let row: LoggedRow = serde_json::from_str(&line)
            .map_err(|e| format!("line {number}: {}", json_error(&e)))?;
        row.validate()
            .map_err(|e| format!("line {number} (decision {:?}): {e}", row.decision_id))?;
        if let Some(first) = first_line.insert(row.decision_id.clone(), number) {
            return Err(format!(
                "line {number}: decisionId {:?} repeats line {first}",
                row.decision_id
            ));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// serde_json's message without its "at line 1 column N" suffix, which is
/// relative to the single line being parsed; the column is kept.
fn json_error(e: &serde_json::Error) -> String {
    let text = e.to_string();
    let message = text
        .rfind(" at line ")
        .map_or(text.as_str(), |cut| &text[..cut]);
    if e.column() > 0 {
        format!("{message} (column {})", e.column())
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({
            "decisionId": "d1", "tsMs": 1_758_585_600_000i64,
            "context": {"segment": "A", "x": 0.5}, "derived": {"score": 2},
            "actions": [{"id": "a"}, {"id": "b", "features": {"cost": 0.2}}, {"id": "c"}],
            "eligible": [0, 2], "pmf": [0.75, 0.25], "chosen": 2, "probability": 0.25,
            "reward": 1.5
        })
    }

    fn with(edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
        let mut value = good();
        edit(value.as_object_mut().unwrap());
        value.to_string()
    }

    fn read(lines: &[String]) -> Result<Vec<LoggedRow>, String> {
        read_jsonl(lines.join("\n").as_bytes())
    }

    fn err_for(edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
        read(&[good().to_string(), with(edit)]).unwrap_err()
    }

    #[test]
    fn parses_a_valid_row_with_camel_case_fields() {
        let rows = read(&[good().to_string()]).unwrap();
        let row = &rows[0];
        assert_eq!(row.decision_id, "d1");
        assert_eq!(row.ts_ms, 1_758_585_600_000);
        assert_eq!(row.eligible, vec![0, 2]);
        assert_eq!(row.chosen_position(), Some(1));
        assert_eq!(row.reward, Some(1.5));
        assert_eq!(row.target_pmf, None);
        // Serialization round-trips and keeps camelCase names.
        let text = serde_json::to_string(row).unwrap();
        assert!(text.contains("\"decisionId\"") && text.contains("\"tsMs\""));
        assert!(!text.contains("targetPmf"));
        assert_eq!(&serde_json::from_str::<LoggedRow>(&text).unwrap(), row);
    }

    #[test]
    fn optional_fields_default() {
        let line = json!({"decisionId": "d", "actions": [{"id": "a"}], "eligible": [0],
                          "pmf": [1.0], "chosen": 0, "probability": 1.0})
        .to_string();
        let row = &read(&[line]).unwrap()[0];
        assert_eq!(row.ts_ms, 0);
        assert_eq!(row.context, Value::Null);
        assert_eq!(row.derived, Value::Null);
        assert_eq!(row.reward, None);
        let with_null = with(|m| {
            m.insert("reward".into(), Value::Null);
        });
        assert_eq!(read(&[with_null]).unwrap()[0].reward, None);
    }

    #[test]
    fn blank_lines_are_skipped_and_line_numbers_stay_true() {
        let rows = read(&[
            good().to_string(),
            String::new(),
            "   ".into(),
            with(|m| {
                m.insert("decisionId".into(), json!("d2"));
            }),
        ])
        .unwrap();
        assert_eq!(rows.len(), 2);
        let e = read(&[
            good().to_string(),
            String::new(),
            with(|m| {
                m.insert("pmf".into(), json!([0.7, 0.25]));
            }),
        ])
        .unwrap_err();
        assert!(e.starts_with("line 3 "), "{e}");
    }

    #[test]
    fn validation_errors_name_the_line_and_the_rule() {
        let cases: Vec<(String, &str)> = vec![
            (
                err_for(|m| {
                    m.insert("pmf".into(), json!([0.7, 0.25]));
                }),
                "line 2 (decision \"d1\"): pmf sums to 0.95, not 1 (tolerance 1e-6)",
            ),
            (
                err_for(|m| {
                    m.insert("chosen".into(), json!(1));
                }),
                "line 2 (decision \"d1\"): chosen action 1 (\"b\") is not eligible",
            ),
            (
                err_for(|m| {
                    m.insert("probability".into(), json!(0.2500001));
                }),
                "probability 0.2500001 does not match pmf[1] = 0.25 of the chosen action",
            ),
            (
                err_for(|m| {
                    m.insert("pmf".into(), json!([1.0, 0.0]));
                    m.insert("probability".into(), json!(0.0));
                }),
                "probability must be in (0, 1] (got 0)",
            ),
            (
                err_for(|m| {
                    m.insert("eligible".into(), json!([0, 3]));
                }),
                "eligible[1] = 3 is out of range for 3 actions",
            ),
            (
                err_for(|m| {
                    m.insert("eligible".into(), json!([2, 2]));
                }),
                "eligible lists action index 2 twice",
            ),
            (
                err_for(|m| {
                    m.insert("eligible".into(), json!([]));
                }),
                "eligible must not be empty",
            ),
            (
                err_for(|m| {
                    m.insert("pmf".into(), json!([1.0]));
                }),
                "pmf has 1 entries but eligible has 2",
            ),
            (
                err_for(|m| {
                    m.insert("pmf".into(), json!([1.25, -0.25]));
                }),
                "pmf[0] = 1.25 is not a probability in [0, 1]",
            ),
            (
                err_for(|m| {
                    m.insert("chosen".into(), json!(7));
                }),
                "chosen = 7 is out of range for 3 actions",
            ),
            (
                err_for(|m| {
                    m.insert("decisionId".into(), json!(""));
                }),
                "decisionId must not be empty",
            ),
            (
                err_for(|m| {
                    m.insert(
                        "actions".into(),
                        json!([{"id": "a"}, {"id": "a"}, {"id": "c"}]),
                    );
                }),
                "actions[1].id \"a\" duplicates actions[0].id",
            ),
            (
                err_for(|m| {
                    m.insert("targetPmf".into(), json!([0.5]));
                }),
                "targetPmf has 1 entries but eligible has 2",
            ),
            (
                err_for(|m| {
                    m.insert("targetPmf".into(), json!([0.5, 0.6]));
                }),
                "targetPmf sums to 1.1, not 1",
            ),
            (
                err_for(|m| {
                    m.insert("context".into(), json!("text"));
                }),
                "context must be a JSON object or null, not a string",
            ),
            (
                err_for(|m| {
                    m.insert("derived".into(), json!({"big": 1e39}));
                }),
                "derived feature \"big\"",
            ),
        ];
        for (e, want) in cases {
            assert!(e.starts_with("line 2 "), "{e}");
            assert!(e.contains(want), "{e}\n  wanted: {want}");
        }
    }

    #[test]
    fn json_and_schema_errors_name_the_line() {
        let e = read(&[good().to_string(), "{not json".into()]).unwrap_err();
        assert!(
            e.starts_with("line 2: key must be a string (column 2)"),
            "{e}"
        );
        let e = read(&["[1, 2]".into()]).unwrap_err();
        assert_eq!(e, "line 1: expected a JSON object");
        let e = err_for(|m| {
            m.insert("rewrd".into(), json!(1));
        });
        assert!(
            e.starts_with("line 2: unknown field `rewrd`, expected one of"),
            "{e}"
        );
        let e = err_for(|m| {
            m.remove("pmf");
        });
        assert!(e.starts_with("line 2: missing field `pmf`"), "{e}");
        let e = err_for(|m| {
            m.insert("chosen".into(), json!(-1));
        });
        assert!(e.starts_with("line 2: invalid value: integer `-1`"), "{e}");
    }

    #[test]
    fn a_leading_byte_order_mark_is_ignored() {
        let text = format!("\u{feff}{}\n", good());
        assert_eq!(read_jsonl(text.as_bytes()).unwrap().len(), 1);
    }

    #[test]
    fn repeated_decision_ids_are_rejected() {
        let e = read(&[good().to_string(), good().to_string()]).unwrap_err();
        assert_eq!(e, "line 2: decisionId \"d1\" repeats line 1");
    }

    #[test]
    fn a_missing_file_is_a_clear_error() {
        let e = load_jsonl(Path::new("definitely/not/here.jsonl")).unwrap_err();
        assert!(
            e.starts_with("cannot open definitely/not/here.jsonl"),
            "{e}"
        );
    }

    #[test]
    fn check_accepts_boundaries() {
        let mut row: LoggedRow = serde_json::from_value(good()).unwrap();
        // A PMF off by less than the tolerance, and a probability that
        // differs from its entry by less than 1e-9.
        row.pmf = vec![0.75, 0.25 + 5e-7];
        row.probability = 0.25 + 5e-7 + 5e-10;
        row.check().unwrap();
        row.probability = 0.25 + 5e-7 + 2e-9;
        assert!(row.check().is_err());
        row.probability = 0.25 + 5e-7;
        row.reward = Some(f64::INFINITY);
        assert!(row.check().unwrap_err().contains("reward must be a finite"));
        row.reward = None;
        row.target_pmf = Some(vec![1.0, 0.0]);
        row.check().unwrap();
    }
}
