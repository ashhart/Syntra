//! Read-only capsule routes: decisions, the model, audit and stats.

use serde_json::{Value, json};

use crate::eventstore::{CapsuleKey, DecisionRecord};

use super::http::{HandlerResult, Request, Response};
use super::state::State;

fn key(t: &str, j: &str, c: &str) -> Result<CapsuleKey, Response> {
    CapsuleKey::new(t, j, c).map_err(|e| Response::error(400, &e.to_string()))
}

fn exists(state: &State, t: &str, j: &str, c: &str) -> Result<(), Response> {
    if state.store.capsule_exists(t, j, c) {
        Ok(())
    } else {
        Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ))
    }
}

fn parse_i64(req: &Request, name: &str) -> Result<Option<i64>, Response> {
    req.query_param(name)
        .map(|v| {
            v.parse::<i64>()
                .map_err(|_| Response::error(400, &format!("{name} must be an integer")))
        })
        .transpose()
}

/// Full decision as JSON: the stored JSON columns are parsed back.
pub fn decision_json(d: &DecisionRecord) -> Value {
    let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
    json!({
        "decisionId": d.id,
        "tsMs": d.ts_ms,
        "modelVersion": d.model_version,
        "mode": d.mode,
        "context": parse(&d.context),
        "derived": parse(&d.derived),
        "actions": parse(&d.actions),
        "eligible": parse(&d.eligible),
        "pmf": d.pmf.as_deref().map(parse),
        "chosenIndex": d.chosen_index,
        "action": d.chosen_id,
        "probability": d.probability,
        "seed": d.seed.to_string(),
        "reason": d.reason,
        "requestSha256": d.request_sha256,
        "programSha256": d.program_sha256,
    })
}

/// `GET .../decisions?since=&until=&limit=&after=`: oldest first, paged
/// with `after` (the last id of the previous page).
pub fn list_decisions(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    exists(state, t, j, c)?;
    let k = key(t, j, c)?;
    let since = parse_i64(req, "since")?;
    let until = parse_i64(req, "until")?;
    let limit = parse_i64(req, "limit")?.unwrap_or(100).clamp(1, 1000) as usize;
    let after = req.query_param("after");
    // Committed decisions only; a decision younger than a few milliseconds
    // may still be in the write-behind queue.
    let rows = state
        .events
        .list_decisions(&k, since, until, limit, after.as_deref())
        .map_err(|e| Response::error(500, &e.to_string()))?;
    let next = if rows.len() == limit {
        rows.last().map(|d| d.id.clone())
    } else {
        None
    };
    let items: Vec<Value> = rows.iter().map(decision_json).collect();
    Ok(Response::json(
        200,
        &json!({ "decisions": items, "next": next }),
    ))
}

/// `GET .../decisions/{id}`: the decision and every reward recorded for it.
pub fn get_decision(state: &State, t: &str, j: &str, c: &str, id: &str) -> HandlerResult {
    exists(state, t, j, c)?;
    let k = key(t, j, c)?;
    let d = state
        .find_decision(&k, id)?
        .ok_or_else(|| Response::error(404, &format!("decision {id:?} not found")))?;
    let rewards = state
        .events
        .rewards_for_decision(&k, id)
        .map_err(|e| Response::error(500, &e.to_string()))?;
    let mut v = decision_json(&d);
    v["rewards"] = json!(
        rewards
            .iter()
            .map(|r| json!({
                "seq": r.seq,
                "tsMs": r.ts_ms,
                "reward": r.value,
                "rewardNormalized": r.value_norm,
                "idempotencyKey": r.idempotency_key,
                "detail": r.detail.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            }))
            .collect::<Vec<_>>()
    );
    Ok(Response::json(200, &v))
}

/// `GET .../model[?snapshot=true]`: spec and version, plus the snapshot
/// bytes (base64) for SDKs that decide locally.
pub fn get_model(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let rt = state.runtime(t, j, c)?;
    let engine = rt.engine.read().unwrap();
    let mut v = json!({
        "spec": engine.spec().to_json(),
        "modelVersion": engine.model_version(),
        "rewardWatermark": rt.reward_watermark.load(std::sync::atomic::Ordering::SeqCst),
        "programSha256": rt.program.as_ref().map(|p| p.sha256.clone()),
    });
    if req.query_param("snapshot").as_deref() == Some("true") {
        let bytes = engine.snapshot();
        v["snapshotBytes"] = json!(bytes.len());
        v["snapshot"] = json!(base64_encode(&bytes));
    }
    Ok(Response::json(200, &v).with_header("etag", &format!("\"{}\"", engine.model_version())))
}

/// `GET .../audits?limit=`: newest last.
pub fn list_audit(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let k = key(t, j, c)?;
    let limit = parse_i64(req, "limit")?.unwrap_or(100).clamp(1, 1000) as usize;
    let rows = state
        .events
        .list_audit(&k, limit)
        .map_err(|e| Response::error(500, &e.to_string()))?;
    Ok(Response::json(
        200,
        &json!({ "audits": serde_json::to_value(rows).unwrap_or(Value::Null) }),
    ))
}

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), want);
        }
    }
}
