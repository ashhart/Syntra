//! Uploads from local-evaluation SDKs.
//!
//! An SDK decides in-process against a published model version (see
//! `GET .../model?snapshot=true`) and uploads its decisions in batches.
//! Each uploaded decision is verified by replaying it on the server with
//! the same model version, input and seed: the eligible set, the chosen
//! action and every probability must match. A client therefore cannot log a
//! propensity the model would not have produced, which keeps off-policy
//! evaluation of the logs sound. (A client that chooses its seeds to steer
//! the sampled action is not detectable this way; data-plane tokens are
//! trusted to sample honestly, as they are for server-side decides.)

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::decision::{ActionSpec, DecideInput, Engine};
use crate::eventstore::DecisionRecord;
use crate::store::{now_ms, sha256_hex};

use super::http::{HandlerResult, Request, Response};
use super::state::State;

/// Items per upload request.
pub const MAX_BATCH_ITEMS: usize = 4096;
/// Tolerance for probabilities recomputed on another machine (libm `pow`
/// may differ in the last bit across platforms).
const PMF_TOLERANCE: f64 = 1e-9;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadInput {
    #[serde(default)]
    context: Option<Value>,
    #[serde(default)]
    actions: Option<Vec<ActionSpec>>,
    #[serde(default)]
    excluded_actions: Vec<String>,
    #[serde(default)]
    baseline_action: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadedDecision {
    decision_id: String,
    ts_ms: i64,
    /// The `modelTag` of the published model the decision was made with.
    model_tag: String,
    /// Informational; must match the tag's version when present.
    #[serde(default)]
    model_version: Option<u64>,
    /// A u64; accepted as a string so JavaScript clients keep full precision.
    seed: Value,
    input: UploadInput,
    chosen_index: usize,
    probability: f64,
    pmf: Vec<f64>,
    eligible: Vec<usize>,
}

/// How far a client clock may run ahead of the server's.
const MAX_CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
/// Oldest decision an upload may carry.
const MAX_UPLOAD_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn parse_seed(v: &Value) -> Result<u64, String> {
    match v {
        Value::String(s) => s
            .parse::<u64>()
            .map_err(|_| "seed must be a u64".to_string()),
        Value::Number(n) => n.as_u64().ok_or_else(|| "seed must be a u64".to_string()),
        _ => Err("seed must be a u64 (number or string)".to_string()),
    }
}

fn items(req: &Request, field: &str) -> Result<Vec<Value>, Response> {
    let mut body = req.json()?;
    let list = match body.get_mut(field).map(Value::take) {
        Some(Value::Array(list)) => list,
        _ => {
            return Err(Response::error(
                400,
                &format!("body must be {{\"{field}\": [...]}}"),
            ));
        }
    };
    if list.len() > MAX_BATCH_ITEMS {
        return Err(Response::error(
            413,
            &format!("at most {MAX_BATCH_ITEMS} items per request"),
        ));
    }
    Ok(list)
}

/// Why an uploaded decision was not stored. `retryable` means the server
/// could not take it right now (the log is backlogged); the SDK keeps it and
/// uploads it again. Anything else is final.
struct Rejection {
    error: String,
    retryable: bool,
}

impl Rejection {
    fn retry(error: String) -> Self {
        Rejection {
            error,
            retryable: true,
        }
    }
}

impl From<String> for Rejection {
    fn from(error: String) -> Self {
        Rejection {
            error,
            retryable: false,
        }
    }
}

impl From<&str> for Rejection {
    fn from(error: &str) -> Self {
        error.to_string().into()
    }
}

enum Verified {
    New(Box<DecisionRecord>),
    /// Already stored with the same content: a retried upload.
    Duplicate,
}

/// `POST .../decisions:batch`: `{"decisions": [...]}`. Answers
/// `{accepted, duplicates, rejected: [{index, decisionId, error}]}`;
/// `accepted` counts retried uploads of already-stored decisions too.
pub fn decisions_batch(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let rt = state.runtime(t, j, c)?;
    if rt.program.is_some() {
        return Err(Response::error(
            400,
            "local evaluation does not support capsules with a feature program yet",
        ));
    }
    let list = items(req, "decisions")?;
    let mut engines: HashMap<String, Arc<Engine>> = HashMap::new();
    let mut accepted = 0usize;
    let mut duplicates = 0usize;
    let mut rejected = Vec::new();
    for (index, raw) in list.into_iter().enumerate() {
        let id = raw
            .get("decisionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Serialize with other requests logging this id (see decide.rs).
        let _event_guard = rt.event_lock(&id);
        let outcome = verify_one(state, &rt, raw, &mut engines).and_then(|v| match v {
            Verified::New(record) => state
                .writer
                .enqueue(*record)
                .map(|()| false)
                .map_err(Rejection::retry),
            Verified::Duplicate => Ok(true),
        });
        match outcome {
            Ok(duplicate) => {
                accepted += 1;
                duplicates += usize::from(duplicate);
            }
            Err(r) => {
                rejected.push(json!({
                    "index": index,
                    "decisionId": id,
                    "error": r.error,
                    "retryable": r.retryable,
                }));
            }
        }
    }
    if !rejected.is_empty() {
        state.audit(
            &rt.key,
            "upload_rejected",
            json!({ "rejected": rejected.len(), "first": rejected.first() }),
        );
    }
    state
        .metrics
        .record_uploads(accepted as u64, rejected.len() as u64);
    Ok(Response::json(
        200,
        &json!({ "accepted": accepted, "duplicates": duplicates, "rejected": rejected }),
    ))
}

fn verify_one(
    state: &State,
    rt: &super::runtime::CapsuleRuntime,
    raw: Value,
    engines: &mut HashMap<String, Arc<Engine>>,
) -> Result<Verified, Rejection> {
    let raw_input = raw.get("input").unwrap_or(&Value::Null).to_string();
    let request_sha256 = sha256_hex(raw_input.as_bytes());
    let d: UploadedDecision =
        serde_json::from_value(raw).map_err(|e| format!("invalid decision: {e}"))?;
    super::decide::validate_event_id_str(&d.decision_id)?;
    let seed = parse_seed(&d.seed)?;
    if let Some(existing) = state
        .find_decision(&rt.key, &d.decision_id)
        .map_err(|_| Rejection::retry("could not check for an existing decision".into()))?
    {
        return if existing.request_sha256 == request_sha256
            && existing.seed == seed
            && existing.chosen_index == d.chosen_index as i64
        {
            Ok(Verified::Duplicate)
        } else {
            Err("decisionId is already used by a different decision".into())
        };
    }
    let now = now_ms();
    if d.ts_ms > now + MAX_CLOCK_SKEW_MS || d.ts_ms < now - MAX_UPLOAD_AGE_MS {
        return Err("tsMs is outside the accepted window (7 days old to 5 minutes ahead)".into());
    }
    let engine = match engines.get(&d.model_tag) {
        Some(e) => e.clone(),
        None => {
            let published = rt.published(&d.model_tag).ok_or_else(|| {
                format!(
                    "model {:?} is not a published model (it may have been retired; resync)",
                    d.model_tag
                )
            })?;
            if d.model_version.is_some_and(|v| v != published.version) {
                return Err("modelVersion does not match modelTag".into());
            }
            let e = published.engine().map_err(Rejection::from)?;
            engines.insert(d.model_tag.clone(), e.clone());
            e
        }
    };
    if d.model_version.is_some_and(|v| v != engine.model_version()) {
        return Err("modelVersion does not match modelTag".into());
    }
    let context = match d.input.context {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err("input.context must be a JSON object".into()),
    };
    let input = DecideInput {
        context,
        derived: Value::Null,
        actions: d.input.actions,
        excluded: d.input.excluded_actions,
        eligible: None,
        baseline: d.input.baseline_action,
    };
    let replay = engine
        .decide(&input, seed)
        .map_err(|e| format!("input rejected: {e}"))?;
    let pmf_matches = replay.pmf.len() == d.pmf.len()
        && replay
            .pmf
            .iter()
            .zip(&d.pmf)
            .all(|(a, b)| (a - b).abs() <= PMF_TOLERANCE);
    if replay.eligible != d.eligible
        || replay.chosen != d.chosen_index
        || !pmf_matches
        || (replay.probability - d.probability).abs() > PMF_TOLERANCE
    {
        return Err(
            "decision does not replay: it was not produced by this model, input and seed".into(),
        );
    }
    Ok(Verified::New(Box::new(DecisionRecord {
        id: d.decision_id,
        key: rt.key.clone(),
        ts_ms: d.ts_ms,
        model_version: replay.model_version,
        mode: super::decide::mode_name(&replay.mode).to_string(),
        context: input.context.to_string(),
        actions: serde_json::to_string(&replay.actions).unwrap_or_else(|_| "[]".into()),
        eligible: serde_json::to_string(&replay.eligible).unwrap_or_else(|_| "[]".into()),
        pmf: Some(serde_json::to_string(&replay.pmf).unwrap_or_else(|_| "[]".into())),
        chosen_index: replay.chosen as i64,
        chosen_id: replay.chosen_action().id.clone(),
        probability: Some(replay.probability),
        seed,
        derived: Value::Null.to_string(),
        reason: None,
        request_sha256,
        program_sha256: None,
    })))
}

/// `POST .../rewards:batch`: `{"rewards": [{decisionId, reward, idempotencyKey?, detail?}]}`.
pub fn rewards_batch(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Item {
        decision_id: String,
        #[serde(alias = "value")]
        reward: f64,
        #[serde(default)]
        idempotency_key: Option<String>,
        #[serde(default)]
        detail: Option<Value>,
    }
    let rt = state.runtime(t, j, c)?;
    let list = items(req, "rewards")?;
    let mut results = Vec::with_capacity(list.len());
    for raw in list {
        let item: Item = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => {
                results.push(json!({ "ok": false, "error": format!("invalid reward: {e}") }));
                continue;
            }
        };
        if !item.reward.is_finite() {
            results.push(json!({ "ok": false, "decisionId": item.decision_id, "error": "reward must be finite" }));
            continue;
        }
        match super::reward::apply_reward(
            state,
            &rt,
            &item.decision_id,
            item.reward,
            item.idempotency_key,
            item.detail,
        ) {
            Ok(v) => results.push(v),
            Err(resp) => {
                let msg = serde_json::from_slice::<Value>(&resp.body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                    .unwrap_or_else(|| "rejected".into());
                results.push(json!({ "ok": false, "decisionId": item.decision_id, "status": resp.status, "error": msg }));
            }
        }
    }
    Ok(Response::json(200, &json!({ "results": results })))
}
