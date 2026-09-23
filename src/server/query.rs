//! Read-only capsule routes: decisions, the model, audit and stats.

use serde_json::{Value, json};

use crate::eventstore::{CapsuleKey, DecisionRecord};

use super::http::{HandlerResult, Request, Response};
use super::state::State;

fn key(t: &str, j: &str, c: &str) -> Result<CapsuleKey, Response> {
    CapsuleKey::new(t, j, c).map_err(|e| Response::error(400, &e.to_string()))
}

/// Wait (briefly) for the write-behind queue, so log reads include every
/// decision and reward acknowledged before the read. Queued records commit
/// within milliseconds; if the log is backlogged the read goes ahead with
/// what is committed.
pub(crate) fn settle(state: &State) {
    state.writer.flush(std::time::Duration::from_millis(250));
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

/// `GET .../decisions?since=&until=&limit=&after=&order=`: oldest first
/// (or newest first with `order=newest`), paged with `after` (the last id
/// of the previous page), which continues in the same order.
pub fn list_decisions(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    exists(state, t, j, c)?;
    let k = key(t, j, c)?;
    let since = parse_i64(req, "since")?;
    let until = parse_i64(req, "until")?;
    let limit = parse_i64(req, "limit")?.unwrap_or(100).clamp(1, 1000) as usize;
    let after = req.query_param("after");
    let newest = match req.query_param("order").as_deref() {
        None | Some("oldest") => false,
        Some("newest") => true,
        Some(other) => {
            return Err(Response::error(
                400,
                &format!("order must be oldest or newest (got {other:?})"),
            ));
        }
    };
    settle(state);
    let listed = if newest {
        state
            .events
            .list_decisions_newest(&k, since, until, limit, after.as_deref())
    } else {
        state
            .events
            .list_decisions(&k, since, until, limit, after.as_deref())
    };
    let rows = listed.map_err(|e| match e {
        crate::eventstore::StoreError::UnknownCursor { .. } => Response::error(400, &e.to_string()),
        e => Response::error(500, &e.to_string()),
    })?;
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
    settle(state);
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

/// `GET .../model[?snapshot=true]`: spec and version. With
/// `snapshot=true` it returns the model published for local evaluation:
/// its `decide` section (what SDKs rebuild the spec from), snapshot bytes
/// (base64) and `modelTag`, which uploaded decisions name so the server can
/// replay them against exactly this model; `spec` is the live spec, for
/// people. The ETag is the quoted tag; `If-None-Match` with it answers 304,
/// so SDKs can poll cheaply.
pub fn get_model(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let rt = state.runtime(t, j, c)?;
    let want_snapshot = req.query_param("snapshot").as_deref() == Some("true");
    let watermark = rt
        .reward_watermark
        .load(std::sync::atomic::Ordering::SeqCst);
    let program_sha256 = rt.program.as_ref().map(|p| p.sha256.clone());
    if !want_snapshot {
        let engine = rt.engine.read().unwrap();
        return Ok(Response::json(
            200,
            &json!({
                "spec": engine.spec().to_json(),
                "modelVersion": engine.model_version(),
                "rewardWatermark": watermark,
                "programSha256": program_sha256,
            }),
        ));
    }
    let p = rt.publish().map_err(|e| Response::error(500, &e))?;
    let etag = format!("\"{}\"", p.tag);
    if req.header("if-none-match") == Some(etag.as_str()) {
        return Ok(Response::new(304, "application/json", "").with_header("etag", &etag));
    }
    let v = json!({
        "decide": p.decide,
        "spec": rt.spec().to_json(),
        "modelVersion": p.version,
        "modelTag": p.tag,
        "rewardWatermark": watermark,
        "programSha256": program_sha256,
        "snapshotBytes": p.snapshot.len(),
        "snapshot": base64_encode(&p.snapshot),
    });
    Ok(Response::json(200, &v).with_header("etag", &etag))
}

/// `GET .../audits?limit=`: newest last.
pub fn list_audit(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let k = key(t, j, c)?;
    let limit = parse_i64(req, "limit")?.unwrap_or(100).clamp(1, 1000) as usize;
    let rows = state
        .events
        .list_audit(&k, limit)
        .map_err(|e| Response::error(500, &e.to_string()))?;
    // A deleted capsule keeps its audit trail; 404 only when there is none.
    if rows.is_empty() {
        exists(state, t, j, c)?;
    }
    let audits: Vec<Value> = rows
        .iter()
        .map(|a| {
            json!({
                "seq": a.seq,
                "tsMs": a.ts_ms,
                "event": a.event,
                "detail": serde_json::from_str::<Value>(&a.detail).unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(Response::json(200, &json!({ "audits": audits })))
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
