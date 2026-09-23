//! Logged decision rows, the input to off-policy evaluation.
//!
//! A row is one decision the logging policy made: the context, the action
//! list, which actions were eligible, the PMF the chosen action was sampled
//! from, and the raw reward that came back, if any. On disk rows are JSONL,
//! one camelCase JSON object per line, in either of two shapes (a file may
//! mix them):
//!
//! ```json
//! {"decisionId": "dec_1", "tsMs": 1758585600000, "context": {"segment": "A"},
//!  "actions": [{"id": "small"}, {"id": "large"}], "eligible": [0, 1],
//!  "pmf": [0.9, 0.1], "chosen": 1, "probability": 0.1, "reward": 1.0}
//! ```
//!
//! or a decision exactly as the v2 server returns it from
//! `GET .../decisions/{id}`, with `chosenIndex` and `action` in place of
//! `chosen`, the decision metadata, and the reward events:
//!
//! ```json
//! {"decisionId": "dec_1", "tsMs": 1758585600000, "modelVersion": 42, "mode": "learner",
//!  "context": {"segment": "A"}, "derived": {}, "actions": [{"id": "small"}, {"id": "large"}],
//!  "eligible": [0, 1], "pmf": [0.9, 0.1], "chosenIndex": 1, "action": "large",
//!  "probability": 0.1, "seed": "1234", "reason": null, "requestSha256": "...",
//!  "programSha256": null,
//!  "rewards": [{"seq": 7, "tsMs": 1758585601000, "reward": 1.0, "rewardNormalized": 1.0,
//!               "idempotencyKey": null, "detail": null}]}
//! ```
//!
//! | Field | |
//! |---|---|
//! | `decisionId` | required, unique |
//! | `tsMs` | optional, informational |
//! | `context`, `derived` | optional JSON objects (or null) |
//! | `actions` | required, as in a decide request |
//! | `eligible` | required: indices into `actions`, or action ids, in PMF order |
//! | `pmf` | required; `null` marks a legacy decision imported from v1, which has no propensities and is counted and skipped |
//! | `chosen` or `chosenIndex` | required: index into `actions` of the action taken |
//! | `action` | optional: the chosen action's id, which must match `chosen` |
//! | `probability` | required unless `pmf` is null |
//! | `reward` | optional raw reward |
//! | `rewards` | optional reward events `{seq, tsMs, reward, rewardNormalized, idempotencyKey, detail}`, used when `reward` is absent or null |
//! | `targetPmf` | optional, for the `target-column` policy |
//! | `seed`, `modelVersion`, `mode`, `reason`, `requestSha256`, `programSha256` | accepted and ignored |
//!
//! Any other field is an error, in a record and in a reward event, so a
//! misspelled optional field such as `rewrd` fails instead of silently
//! dropping the reward.
//!
//! A `rewards` array becomes one raw reward by the capsule's aggregation
//! rule ([`RewardAggregation`], `--reward-aggregation`): `first` takes the
//! reward with the lowest `seq`, `sum` adds them all in `seq` order. An
//! empty array means no reward. Rows without a reward are counted and
//! skipped when the evaluation data is built.
//!
//! # Validation
//!
//! [`LoggedRow::check`] enforces, for every row:
//!
//! - `decisionId` is not empty;
//! - `actions` is a valid action list (unique non-empty ids, features that
//!   flatten), as for a decide request;
//! - `eligible` is non-empty, in range and has no repeated action;
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
//! core's limits). [`read_jsonl`] and [`from_records`] run it on every
//! record, reject repeated decision ids, and stop at the first problem,
//! naming its line or record.
//!
//! # The event store
//!
//! The store adapter (`syntra evaluate --store`, not written yet) turns each
//! of a capsule's decision records into the `GET .../decisions/{id}` JSON,
//! rewards included, and passes them to [`from_records`]. The mapping from
//! [`Decision`](crate::decision::Decision) is field for field: `actions`,
//! `eligible` (indices), `pmf`, `chosen` (as `chosenIndex`) and
//! `probability`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::decision::ActionSpec;
use crate::decision::spec::{RewardAggregation, validate_actions};

use super::data::featurize;

/// Largest allowed distance between the sum of a PMF and 1.
pub const PMF_SUM_TOLERANCE: f64 = 1e-6;
/// Largest allowed distance between `probability` and the PMF entry of the
/// chosen action.
pub const PROBABILITY_TOLERANCE: f64 = 1e-9;

/// One logged decision, validated. Serializes to the row format of the
/// module documentation; [`LoggedRow::from_record`] parses either format.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggedRow {
    /// Unique id of the decision; it also picks the cross-fitting fold.
    pub decision_id: String,
    /// Decision time in Unix milliseconds. Informational only.
    pub ts_ms: i64,
    /// Request context: a JSON object, or null.
    pub context: Value,
    /// Derived features from the feature program: a JSON object, or null.
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
    pub reward: Option<f64>,
    /// A target policy's PMF over the eligible actions, aligned with
    /// `eligible`; used by the `target-column` policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_pmf: Option<Vec<f64>>,
}

impl LoggedRow {
    /// Parse and validate one record in either format of the module
    /// documentation, reducing a `rewards` array by `aggregation`.
    /// `Ok(None)` for a legacy decision whose `pmf` is null.
    pub fn from_record(
        record: Value,
        aggregation: RewardAggregation,
    ) -> Result<Option<Self>, String> {
        let record = Record::from_value(record)?;
        match record.into_row(aggregation)? {
            Some((row, _)) => {
                row.validate()?;
                Ok(Some(row))
            }
            None => Ok(None),
        }
    }

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

/// Rows read from a log, with what was skipped or aggregated on the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadedRows {
    /// Rows with a logged PMF, in input order, with or without a reward.
    pub rows: Vec<LoggedRow>,
    /// Legacy decisions whose `pmf` is null; without propensities OPE
    /// cannot weight them, so they are skipped.
    pub without_pmf: usize,
    /// How `rewards` arrays were reduced to one reward.
    pub aggregation: RewardAggregation,
    /// Rows whose reward came from a `rewards` array.
    pub aggregated: usize,
}

impl From<Vec<LoggedRow>> for LoadedRows {
    fn from(rows: Vec<LoggedRow>) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }
}

/// A record as it appears on the wire, in either format.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    decision_id: String,
    #[serde(default)]
    ts_ms: i64,
    #[serde(default)]
    context: Value,
    #[serde(default)]
    derived: Value,
    actions: Vec<ActionSpec>,
    /// Action indices or action ids.
    eligible: Vec<Value>,
    /// `None` when the field is absent, `Some(Null)` for a legacy decision.
    #[serde(default, deserialize_with = "present")]
    pmf: Option<Value>,
    #[serde(alias = "chosenIndex")]
    chosen: usize,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    probability: Option<f64>,
    #[serde(default)]
    reward: Option<f64>,
    #[serde(default)]
    rewards: Option<Vec<RewardEvent>>,
    #[serde(default)]
    target_pmf: Option<Vec<f64>>,
    // Decision metadata that OPE does not use.
    #[serde(default, rename = "seed")]
    _seed: Option<IgnoredAny>,
    #[serde(default, rename = "modelVersion")]
    _model_version: Option<IgnoredAny>,
    #[serde(default, rename = "mode")]
    _mode: Option<IgnoredAny>,
    #[serde(default, rename = "reason")]
    _reason: Option<IgnoredAny>,
    #[serde(default, rename = "requestSha256")]
    _request_sha256: Option<IgnoredAny>,
    #[serde(default, rename = "programSha256")]
    _program_sha256: Option<IgnoredAny>,
}

/// One entry of a decision's `rewards` array.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RewardEvent {
    seq: i64,
    reward: f64,
    #[serde(default, rename = "tsMs")]
    _ts_ms: Option<IgnoredAny>,
    #[serde(default, rename = "rewardNormalized")]
    _reward_normalized: Option<IgnoredAny>,
    #[serde(default, rename = "idempotencyKey")]
    _idempotency_key: Option<IgnoredAny>,
    #[serde(default, rename = "detail")]
    _detail: Option<IgnoredAny>,
}

/// Deserialize a field that is present, keeping an explicit null.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

impl Record {
    /// Parse a JSON value, which must be an object (serde would otherwise
    /// read an array positionally).
    fn from_value(value: Value) -> Result<Self, String> {
        if !value.is_object() {
            return Err("expected a JSON object".into());
        }
        Record::deserialize(value).map_err(|e| json_error(&e))
    }

    /// The row, and whether its reward came from a `rewards` array; `None`
    /// for a legacy decision whose `pmf` is null. Not yet validated.
    fn into_row(self, aggregation: RewardAggregation) -> Result<Option<(LoggedRow, bool)>, String> {
        let pmf = match self.pmf {
            None => return Err("missing field `pmf`".into()),
            Some(Value::Null) => return Ok(None),
            Some(value) => {
                Vec::<f64>::deserialize(value).map_err(|e| format!("pmf: {}", json_error(&e)))?
            }
        };
        let probability = self
            .probability
            .ok_or("missing field `probability` (null is allowed only when pmf is null)")?;
        let eligible = eligible_indices(&self.eligible, &self.actions)?;
        if let Some(id) = &self.action {
            match self.actions.get(self.chosen) {
                Some(action) if action.id == *id => {}
                Some(action) => {
                    return Err(format!(
                        "action {id:?} does not match actions[{}].id {:?}",
                        self.chosen, action.id
                    ));
                }
                None => {
                    return Err(format!(
                        "chosen = {} is out of range for {} actions",
                        self.chosen,
                        self.actions.len()
                    ));
                }
            }
        }
        let (reward, aggregated) = match (self.reward, &self.rewards) {
            (Some(reward), _) => (Some(reward), false),
            (None, Some(events)) => {
                let reward = aggregate(events, aggregation)?;
                (reward, reward.is_some())
            }
            (None, None) => (None, false),
        };
        let row = LoggedRow {
            decision_id: self.decision_id,
            ts_ms: self.ts_ms,
            context: self.context,
            derived: self.derived,
            actions: self.actions,
            eligible,
            pmf,
            chosen: self.chosen,
            probability,
            reward,
            target_pmf: self.target_pmf,
        };
        Ok(Some((row, aggregated)))
    }
}

/// `eligible` as indices into `actions`: either every entry is an index
/// or every entry is an action id.
fn eligible_indices(items: &[Value], actions: &[ActionSpec]) -> Result<Vec<usize>, String> {
    let by_id = items.iter().any(Value::is_string);
    items
        .iter()
        .enumerate()
        .map(|(k, item)| match item {
            Value::String(id) => actions
                .iter()
                .position(|a| a.id == *id)
                .ok_or_else(|| format!("eligible[{k}] = {id:?} names no action")),
            Value::Number(_) if by_id => Err(format!(
                "eligible mixes action ids and indices (eligible[{k}] = {item})"
            )),
            Value::Number(n) => n
                .as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .ok_or_else(|| format!("eligible[{k}] = {n} is not an action index")),
            other => Err(format!(
                "eligible[{k}] must be an action index or an action id, not {other}"
            )),
        })
        .collect()
}

/// One raw reward from a decision's reward events: the lowest `seq` under
/// `first`, the sum in `seq` order under `sum`; `None` for no events.
fn aggregate(events: &[RewardEvent], how: RewardAggregation) -> Result<Option<f64>, String> {
    let mut ordered: Vec<&RewardEvent> = events.iter().collect();
    ordered.sort_by_key(|e| e.seq);
    if let Some(pair) = ordered.windows(2).find(|pair| pair[0].seq == pair[1].seq) {
        return Err(format!("rewards lists seq {} twice", pair[0].seq));
    }
    Ok(match (how, ordered.first()) {
        (_, None) => None,
        (RewardAggregation::First, Some(first)) => Some(first.reward),
        (RewardAggregation::Sum, Some(_)) => {
            Some(ordered.iter().fold(0.0, |sum, e| sum + e.reward))
        }
    })
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

/// Read and validate every record of a JSONL file. Errors start with the
/// path and the 1-based line number.
pub fn load_jsonl(path: &Path, aggregation: RewardAggregation) -> Result<LoadedRows, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    read_jsonl(BufReader::new(file), aggregation).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read and validate JSONL records: one JSON object per line, blank lines
/// ignored, `rewards` arrays reduced by `aggregation`. Every row must pass
/// [`LoggedRow::validate`] and decision ids must be unique. Stops at the
/// first problem; the error names its 1-based line.
pub fn read_jsonl(
    reader: impl BufRead,
    aggregation: RewardAggregation,
) -> Result<LoadedRows, String> {
    let mut rows = Collector::new(aggregation, |k| format!("line {k}"));
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
        let record: Record = serde_json::from_str(&line)
            .map_err(|e| format!("line {number}: {}", json_error(&e)))?;
        rows.add(record, number)?;
    }
    Ok(rows.finish())
}

/// Rows read from the event store, in the JSON `GET .../decisions/{id}`
/// serves, with each decision's rewards already reduced (read them with
/// [`rewards_mode`] of the same `aggregation`).
pub fn from_logged_rows(
    rows: Vec<crate::eventstore::LoggedRow>,
    aggregation: RewardAggregation,
) -> Result<LoadedRows, String> {
    from_records(
        rows.into_iter().map(|r| {
            let mut v = crate::server::query::decision_json(&r.decision);
            if let Some(reward) = r.reward {
                v["reward"] = serde_json::json!(reward);
            }
            v
        }),
        aggregation,
    )
}

/// The event-store reduction matching `aggregation`.
pub fn rewards_mode(aggregation: RewardAggregation) -> crate::eventstore::RewardsMode {
    match aggregation {
        RewardAggregation::First => crate::eventstore::RewardsMode::First,
        RewardAggregation::Sum => crate::eventstore::RewardsMode::Sum,
    }
}

/// Validate records given as JSON values (the store adapter's path), with
/// the same rules as [`read_jsonl`]. Errors name the record's 0-based index.
pub fn from_records(
    records: impl IntoIterator<Item = Value>,
    aggregation: RewardAggregation,
) -> Result<LoadedRows, String> {
    let mut rows = Collector::new(aggregation, |k| format!("records[{k}]"));
    for (index, value) in records.into_iter().enumerate() {
        let record = Record::from_value(value).map_err(|e| format!("records[{index}]: {e}"))?;
        rows.add(record, index)?;
    }
    Ok(rows.finish())
}

/// Accumulates validated rows, counting legacy decisions and rejecting
/// repeated ids.
struct Collector {
    loaded: LoadedRows,
    /// Where each decision id was first seen.
    seen: HashMap<String, usize>,
    place: fn(usize) -> String,
}

impl Collector {
    fn new(aggregation: RewardAggregation, place: fn(usize) -> String) -> Self {
        Self {
            loaded: LoadedRows {
                aggregation,
                ..LoadedRows::default()
            },
            seen: HashMap::new(),
            place,
        }
    }

    fn add(&mut self, record: Record, at: usize) -> Result<(), String> {
        let place = (self.place)(at);
        let id = record.decision_id.clone();
        let context = |e: String| format!("{place} (decision {id:?}): {e}");
        let Some((row, aggregated)) = record.into_row(self.loaded.aggregation).map_err(context)?
        else {
            self.loaded.without_pmf += 1;
            return Ok(());
        };
        row.validate().map_err(context)?;
        if let Some(first) = self.seen.insert(id.clone(), at) {
            return Err(format!(
                "{place}: decisionId {id:?} repeats {}",
                (self.place)(first)
            ));
        }
        self.loaded.aggregated += usize::from(aggregated);
        self.loaded.rows.push(row);
        Ok(())
    }

    fn finish(self) -> LoadedRows {
        self.loaded
    }
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

    const FIRST: RewardAggregation = RewardAggregation::First;
    const SUM: RewardAggregation = RewardAggregation::Sum;

    fn good() -> Value {
        json!({
            "decisionId": "d1", "tsMs": 1_758_585_600_000i64,
            "context": {"segment": "A", "x": 0.5}, "derived": {"score": 2},
            "actions": [{"id": "a"}, {"id": "b", "features": {"cost": 0.2}}, {"id": "c"}],
            "eligible": [0, 2], "pmf": [0.75, 0.25], "chosen": 2, "probability": 0.25,
            "reward": 1.5
        })
    }

    /// The same decision as `GET .../decisions/{id}` returns it, with two
    /// reward events listed out of `seq` order.
    fn decision() -> Value {
        json!({
            "decisionId": "d1", "tsMs": 1_758_585_600_000i64, "modelVersion": 42,
            "mode": "learner", "context": {"segment": "A", "x": 0.5}, "derived": {"score": 2},
            "actions": [{"id": "a"}, {"id": "b", "features": {"cost": 0.2}}, {"id": "c"}],
            "eligible": [0, 2], "pmf": [0.75, 0.25], "chosenIndex": 2, "action": "c",
            "probability": 0.25, "seed": "18446744073709551615", "reason": null,
            "requestSha256": "ab12", "programSha256": null,
            "rewards": [
                {"seq": 9, "tsMs": 1_758_585_602_000i64, "reward": 0.5, "rewardNormalized": 0.5,
                 "idempotencyKey": "k2", "detail": {"source": "late"}},
                {"seq": 4, "tsMs": 1_758_585_601_000i64, "reward": 1.5, "rewardNormalized": 1.0,
                 "idempotencyKey": null, "detail": null}
            ]
        })
    }

    fn edit(mut value: Value, change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Value {
        change(value.as_object_mut().unwrap());
        value
    }

    fn with(change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
        edit(good(), change).to_string()
    }

    fn read(lines: &[String]) -> Result<LoadedRows, String> {
        read_jsonl(lines.join("\n").as_bytes(), FIRST)
    }

    fn err_for(change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
        read(&[good().to_string(), with(change)]).unwrap_err()
    }

    fn record(value: Value, how: RewardAggregation) -> Result<Option<LoggedRow>, String> {
        LoggedRow::from_record(value, how)
    }

    #[test]
    fn parses_a_valid_row_with_camel_case_fields() {
        let loaded = read(&[good().to_string()]).unwrap();
        assert_eq!((loaded.without_pmf, loaded.aggregated), (0, 0));
        let row = &loaded.rows[0];
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
        let again = record(serde_json::from_str(&text).unwrap(), FIRST).unwrap();
        assert_eq!(again.as_ref(), Some(row));
    }

    #[test]
    fn optional_fields_default() {
        let line = json!({"decisionId": "d", "actions": [{"id": "a"}], "eligible": [0],
                          "pmf": [1.0], "chosen": 0, "probability": 1.0})
        .to_string();
        let row = &read(&[line]).unwrap().rows[0];
        assert_eq!(row.ts_ms, 0);
        assert_eq!(row.context, Value::Null);
        assert_eq!(row.derived, Value::Null);
        assert_eq!(row.reward, None);
        let with_null = with(|m| {
            m.insert("reward".into(), Value::Null);
        });
        assert_eq!(read(&[with_null]).unwrap().rows[0].reward, None);
    }

    #[test]
    fn accepts_decisions_as_the_server_returns_them() {
        let row = record(decision(), FIRST).unwrap().unwrap();
        let plain = record(good(), FIRST).unwrap().unwrap();
        // The first reward by seq (4), not the first listed.
        assert_eq!(row, plain);
        let summed = record(decision(), SUM).unwrap().unwrap();
        assert_eq!(summed.reward, Some(2.0));
        // Loading counts the rows whose reward came from events.
        let lines = [
            decision().to_string(),
            with(|m| {
                m.insert("decisionId".into(), json!("d2"));
            }),
        ];
        let loaded = read_jsonl(lines.join("\n").as_bytes(), SUM).unwrap();
        assert_eq!((loaded.aggregated, loaded.aggregation), (1, SUM));
        assert_eq!(loaded.rows[0].reward, Some(2.0));
        assert_eq!(loaded.rows[1].reward, Some(1.5));
        // The list endpoint has no rewards array: no reward.
        let listed = edit(decision(), |m| {
            m.remove("rewards");
        });
        assert_eq!(record(listed, FIRST).unwrap().unwrap().reward, None);
    }

    #[test]
    fn reward_aggregation_rules() {
        // A top-level reward wins over the events.
        let both = edit(decision(), |m| {
            m.insert("reward".into(), json!(7.0));
        });
        assert_eq!(record(both, SUM).unwrap().unwrap().reward, Some(7.0));
        // A null top-level reward defers to them.
        let null = edit(decision(), |m| {
            m.insert("reward".into(), Value::Null);
        });
        assert_eq!(record(null, FIRST).unwrap().unwrap().reward, Some(1.5));
        // No events: no reward, and nothing counted as aggregated.
        let none = edit(decision(), |m| {
            m.insert("rewards".into(), json!([]));
        });
        assert_eq!(record(none.clone(), SUM).unwrap().unwrap().reward, None);
        let loaded = from_records([none], SUM).unwrap();
        assert_eq!(loaded.aggregated, 0);
        // Repeated sequence numbers are corrupt input.
        let twice = edit(decision(), |m| {
            m.insert(
                "rewards".into(),
                json!([{"seq": 3, "reward": 1}, {"seq": 3, "reward": 2}]),
            );
        });
        assert_eq!(
            record(twice, FIRST).unwrap_err(),
            "rewards lists seq 3 twice"
        );
        // Sums add in seq order.
        let tiny = edit(decision(), |m| {
            m.insert(
                "rewards".into(),
                json!([{"seq": 2, "reward": 1e-17}, {"seq": 1, "reward": 1.0}, {"seq": 3, "reward": -1.0}]),
            );
        });
        assert_eq!(
            record(tiny, SUM).unwrap().unwrap().reward,
            Some((1.0 + 1e-17) - 1.0)
        );
    }

    #[test]
    fn legacy_decisions_without_a_pmf_are_counted_and_skipped() {
        let legacy = edit(decision(), |m| {
            m.insert("pmf".into(), Value::Null);
            m.insert("probability".into(), Value::Null);
            m.insert("eligible".into(), json!([]));
        });
        assert_eq!(record(legacy.clone(), FIRST).unwrap(), None);
        let lines = [legacy.to_string(), good().to_string()];
        let loaded = read(&lines).unwrap();
        assert_eq!((loaded.rows.len(), loaded.without_pmf), (1, 1));
        // An absent pmf is an error, not a legacy row; so is a null
        // probability next to a pmf.
        let e = err_for(|m| {
            m.remove("pmf");
        });
        assert!(
            e.starts_with("line 2 (decision \"d1\"): missing field `pmf`"),
            "{e}"
        );
        let e = err_for(|m| {
            m.insert("probability".into(), Value::Null);
        });
        assert!(e.contains("missing field `probability`"), "{e}");
        let e = err_for(|m| {
            m.insert("pmf".into(), json!("0.5"));
        });
        assert!(e.contains("pmf: invalid type: string"), "{e}");
    }

    #[test]
    fn eligible_may_list_action_ids() {
        let by_id = edit(good(), |m| {
            m.insert("eligible".into(), json!(["c", "a"]));
            m.insert("pmf".into(), json!([0.25, 0.75]));
        });
        let row = record(by_id, FIRST).unwrap().unwrap();
        assert_eq!(row.eligible, vec![2, 0]);
        assert_eq!(row.chosen_position(), Some(0));
        for (eligible, want) in [
            (json!(["a", "zz"]), "eligible[1] = \"zz\" names no action"),
            (
                json!(["a", 2]),
                "eligible mixes action ids and indices (eligible[1] = 2)",
            ),
            (json!([0, -2]), "eligible[1] = -2 is not an action index"),
            (json!([0, 1.5]), "eligible[1] = 1.5 is not an action index"),
            (
                json!([0, null]),
                "eligible[1] must be an action index or an action id, not null",
            ),
            (json!(["a", "a"]), "eligible lists action index 0 twice"),
        ] {
            let bad = edit(good(), |m| {
                m.insert("eligible".into(), eligible);
            });
            let e = record(bad, FIRST).unwrap_err();
            assert!(e.contains(want), "{e}");
        }
    }

    #[test]
    fn records_stay_strict_about_unknown_fields() {
        let e = record(
            edit(decision(), |m| {
                m.insert("rewardz".into(), json!([]));
            }),
            FIRST,
        )
        .unwrap_err();
        assert!(
            e.starts_with("unknown field `rewardz`, expected one of"),
            "{e}"
        );
        let e = record(
            edit(decision(), |m| {
                m.insert(
                    "rewards".into(),
                    json!([{"seq": 1, "reward": 1, "value": 2}]),
                );
            }),
            FIRST,
        )
        .unwrap_err();
        assert!(e.starts_with("unknown field `value`"), "{e}");
        let e = record(
            edit(decision(), |m| {
                m.insert("rewards".into(), json!([{"reward": 1}]));
            }),
            FIRST,
        )
        .unwrap_err();
        assert!(e.starts_with("missing field `seq`"), "{e}");
        let e = record(
            edit(decision(), |m| {
                m.insert("action".into(), json!("a"));
            }),
            FIRST,
        )
        .unwrap_err();
        assert_eq!(e, "action \"a\" does not match actions[2].id \"c\"");
        let e = record(
            edit(decision(), |m| {
                m.insert("chosen".into(), json!(2));
            }),
            FIRST,
        )
        .unwrap_err();
        assert!(e.starts_with("duplicate field `chosen`"), "{e}");
    }

    #[test]
    fn from_records_names_the_record() {
        let second = edit(decision(), |m| {
            m.insert("decisionId".into(), json!("d2"));
        });
        let loaded = from_records([decision(), second.clone()], FIRST).unwrap();
        assert_eq!((loaded.rows.len(), loaded.aggregated), (2, 2));
        let e = from_records([decision(), decision()], FIRST).unwrap_err();
        assert_eq!(e, "records[1]: decisionId \"d1\" repeats records[0]");
        let bad = edit(second, |m| {
            m.insert("pmf".into(), json!([0.5, 0.25]));
        });
        let e = from_records([decision(), bad], FIRST).unwrap_err();
        assert!(
            e.starts_with("records[1] (decision \"d2\"): pmf sums to 0.75"),
            "{e}"
        );
        let e = from_records([json!([1])], FIRST).unwrap_err();
        assert_eq!(e, "records[0]: expected a JSON object");
        let e = from_records([json!({"decisionId": 3})], FIRST).unwrap_err();
        assert!(
            e.starts_with("records[0]: invalid type: integer `3`, expected a string"),
            "{e}"
        );
        assert_eq!(
            record(json!("text"), FIRST).unwrap_err(),
            "expected a JSON object"
        );
    }

    #[test]
    fn blank_lines_are_skipped_and_line_numbers_stay_true() {
        let loaded = read(&[
            good().to_string(),
            String::new(),
            "   ".into(),
            with(|m| {
                m.insert("decisionId".into(), json!("d2"));
            }),
        ])
        .unwrap();
        assert_eq!(loaded.rows.len(), 2);
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
            m.insert("chosen".into(), json!(-1));
        });
        assert!(e.starts_with("line 2: invalid value: integer `-1`"), "{e}");
    }

    #[test]
    fn a_leading_byte_order_mark_is_ignored() {
        let text = format!("\u{feff}{}\n", good());
        assert_eq!(read_jsonl(text.as_bytes(), FIRST).unwrap().rows.len(), 1);
    }

    #[test]
    fn repeated_decision_ids_are_rejected() {
        let e = read(&[good().to_string(), good().to_string()]).unwrap_err();
        assert_eq!(e, "line 2: decisionId \"d1\" repeats line 1");
    }

    #[test]
    fn a_missing_file_is_a_clear_error() {
        let e = load_jsonl(Path::new("definitely/not/here.jsonl"), FIRST).unwrap_err();
        assert!(
            e.starts_with("cannot open definitely/not/here.jsonl"),
            "{e}"
        );
    }

    #[test]
    fn check_accepts_boundaries() {
        let mut row = record(good(), FIRST).unwrap().unwrap();
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
