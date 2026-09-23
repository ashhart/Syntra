//! Input validation shared by all backends. Everything here runs before a
//! database lock is taken.

use super::{
    DecisionRecord, MAX_BATCH_ROWS, MAX_ID_CHARS, MAX_KEY_PART_CHARS, ModelSnapshot, Result,
    RewardRecord, StoreError,
};

/// Longest slice of a rejected value echoed back in an error message.
const PREVIEW_CHARS: usize = 48;

fn preview(value: &str) -> String {
    let mut out: String = value.chars().take(PREVIEW_CHARS).collect();
    if value.chars().nth(PREVIEW_CHARS).is_some() {
        out.push('…');
    }
    out
}

pub(crate) fn key_part(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(StoreError::InvalidInput(format!(
            "capsule key {field} must not be empty"
        )));
    }
    if value.chars().count() > MAX_KEY_PART_CHARS {
        return Err(StoreError::InvalidInput(format!(
            "capsule key {field} {:?} is longer than {MAX_KEY_PART_CHARS} characters",
            preview(value)
        )));
    }
    if value.contains(['/', '\\', '\0']) {
        return Err(StoreError::InvalidInput(format!(
            "capsule key {field} {:?} contains a path separator or NUL",
            preview(value)
        )));
    }
    if value == "." || value == ".." {
        return Err(StoreError::InvalidInput(format!(
            "capsule key {field} must not be {value:?}"
        )));
    }
    Ok(())
}

/// Decision ids, idempotency keys, audit event names.
pub(crate) fn id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(StoreError::InvalidInput(format!(
            "{field} must not be empty"
        )));
    }
    if value.chars().count() > MAX_ID_CHARS {
        return Err(StoreError::InvalidInput(format!(
            "{field} {:?} is longer than {MAX_ID_CHARS} characters",
            preview(value)
        )));
    }
    if value.contains('\0') {
        return Err(StoreError::InvalidInput(format!(
            "{field} {:?} contains NUL",
            preview(value)
        )));
    }
    Ok(())
}

fn non_empty(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(StoreError::InvalidInput(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

/// Accepts exactly one RFC 8259 JSON value (serde_json's nesting limit of
/// 128 applies).
pub(crate) fn json(field: &str, text: &str) -> Result<()> {
    serde_json::from_str::<serde::de::IgnoredAny>(text)
        .map(|_| ())
        .map_err(|e| StoreError::InvalidInput(format!("{field} is not valid JSON: {e}")))
}

fn finite(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() {
        return Err(StoreError::InvalidInput(format!(
            "{field} must be finite, got {value}"
        )));
    }
    Ok(())
}

/// SQLite integers are signed 64-bit. Values above `i64::MAX` are rejected
/// rather than wrapped, because versions are compared and ordered.
pub(crate) fn u64_to_i64(field: &str, value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidInput(format!(
            "{field} {value} exceeds the storable maximum {}",
            i64::MAX
        ))
    })
}

/// Seeds are opaque bits, so they are stored as the `i64` with the same bit
/// pattern and read back the same way. Every `u64` round-trips exactly.
pub(crate) fn seed_to_sql(seed: u64) -> i64 {
    i64::from_ne_bytes(seed.to_ne_bytes())
}

pub(crate) fn seed_from_sql(stored: i64) -> u64 {
    u64::from_ne_bytes(stored.to_ne_bytes())
}

pub(crate) fn decision(d: &DecisionRecord) -> Result<()> {
    id("decision id", &d.id)?;
    u64_to_i64("model_version", d.model_version)?;
    non_empty("mode", &d.mode)?;
    json("context", &d.context)?;
    json("actions", &d.actions)?;
    json("eligible", &d.eligible)?;
    if let Some(pmf) = &d.pmf {
        json("pmf", pmf)?;
    }
    non_empty("chosen_id", &d.chosen_id)?;
    // (0.0..=1.0).contains is false for NaN, so this also rejects NaN.
    if let Some(p) = d.probability.filter(|p| !(0.0..=1.0).contains(p)) {
        return Err(StoreError::InvalidInput(format!(
            "probability must be within [0, 1], got {p}"
        )));
    }
    json("derived", &d.derived)?;
    Ok(())
}

pub(crate) fn reward(r: &RewardRecord) -> Result<()> {
    id("decision_id", &r.decision_id)?;
    id("idempotency_key", &r.idempotency_key)?;
    finite("value", r.value)?;
    finite("value_norm", r.value_norm)?;
    if let Some(detail) = &r.detail {
        json("reward detail", detail)?;
    }
    Ok(())
}

pub(crate) fn snapshot(s: &ModelSnapshot) -> Result<()> {
    u64_to_i64("model version", s.version)?;
    if s.reward_seq < 0 {
        return Err(StoreError::InvalidInput(format!(
            "reward_seq must not be negative, got {}",
            s.reward_seq
        )));
    }
    Ok(())
}

pub(crate) fn audit(event: &str, detail_json: &str) -> Result<()> {
    id("audit event", event)?;
    json("audit detail", detail_json)
}

pub(crate) fn batch_len(rows: usize) -> Result<()> {
    if rows > MAX_BATCH_ROWS {
        return Err(StoreError::InvalidInput(format!(
            "batch of {rows} rows exceeds the maximum of {MAX_BATCH_ROWS}; split it"
        )));
    }
    Ok(())
}

/// The message a batch reports for a row that failed validation.
pub(crate) fn rejection(err: StoreError) -> String {
    match err {
        StoreError::InvalidInput(message) => message,
        other => other.to_string(),
    }
}

/// Converts a half-open window `[since, until)` to inclusive SQL bounds.
/// `None` when the window is empty.
pub(crate) fn ts_bounds(since_ms: Option<i64>, until_ms: Option<i64>) -> Option<(i64, i64)> {
    let lo = since_ms.unwrap_or(i64::MIN);
    let hi = match until_ms {
        None => i64::MAX,
        Some(until) => until.checked_sub(1)?,
    };
    (lo <= hi).then_some((lo, hi))
}

/// SQLite LIMIT takes an i64.
pub(crate) fn limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_bit_cast_round_trips_all_ranges() {
        for seed in [
            0,
            1,
            i64::MAX as u64,
            i64::MAX as u64 + 1,
            0xDEAD_BEEF_CAFE_F00D,
            u64::MAX - 1,
            u64::MAX,
        ] {
            assert_eq!(seed_from_sql(seed_to_sql(seed)), seed);
        }
        assert_eq!(seed_to_sql(u64::MAX), -1);
    }

    #[test]
    fn json_validation_accepts_values_and_rejects_garbage() {
        for ok in [
            "{}",
            "[]",
            "null",
            "1.5",
            r#"{"user":{"名前":"Zoë 🚀"}}"#,
            " [1, 2] ",
        ] {
            json("f", ok).unwrap_or_else(|e| panic!("{ok:?}: {e}"));
        }
        for bad in ["", "{", "{} {}", "{'a':1}", "NaN", "[1,]"] {
            let err = json("context", bad).unwrap_err();
            assert!(
                err.to_string().contains("context is not valid JSON"),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn id_validation() {
        id("decision id", "dec_01J").unwrap();
        id("decision id", &"x".repeat(MAX_ID_CHARS)).unwrap();
        assert!(id("decision id", "").is_err());
        assert!(id("decision id", "a\0b").is_err());
        let err = id("decision id", &"x".repeat(MAX_ID_CHARS + 1)).unwrap_err();
        // The message previews the value instead of echoing all of it.
        assert!(err.to_string().len() < 200, "{err}");
    }

    #[test]
    fn version_range_is_enforced() {
        assert_eq!(u64_to_i64("v", i64::MAX as u64).unwrap(), i64::MAX);
        assert!(u64_to_i64("v", i64::MAX as u64 + 1).is_err());
    }

    #[test]
    fn batch_size_is_capped_and_rejections_keep_their_message() {
        batch_len(0).unwrap();
        batch_len(MAX_BATCH_ROWS).unwrap();
        assert!(batch_len(MAX_BATCH_ROWS + 1).is_err());
        assert_eq!(
            rejection(StoreError::InvalidInput("value must be finite".into())),
            "value must be finite"
        );
    }

    #[test]
    fn half_open_windows_become_inclusive_bounds() {
        assert_eq!(ts_bounds(None, None), Some((i64::MIN, i64::MAX)));
        assert_eq!(ts_bounds(Some(10), Some(20)), Some((10, 19)));
        assert_eq!(ts_bounds(Some(10), Some(11)), Some((10, 10)));
        assert_eq!(ts_bounds(Some(10), Some(10)), None);
        assert_eq!(ts_bounds(Some(10), Some(5)), None);
        assert_eq!(ts_bounds(None, Some(i64::MIN)), None);
        assert_eq!(limit(usize::MAX), i64::MAX);
    }
}
