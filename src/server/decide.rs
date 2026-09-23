//! `POST .../decide`: choose an action and log the decision with the
//! probability it was chosen with.
//!
//! Hot path: engine read-lock, feature scoring and sampling (microseconds),
//! then a push onto the write-behind queue. The request never waits on disk.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::decision::{ActionSpec, DecideInput};
use crate::eventstore::DecisionRecord;
use crate::store::{now_ms, sha256_hex};

use super::http::{HandlerResult, Request, Response};
use super::runtime::ProgramOutput;
use super::state::State;

/// Request body. Unknown fields are rejected so a typo never silently
/// changes a decision.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecideBody {
    #[serde(default)]
    context: Option<Value>,
    #[serde(default)]
    actions: Option<Vec<ActionSpec>>,
    #[serde(default)]
    excluded_actions: Vec<String>,
    #[serde(default)]
    baseline_action: Option<String>,
    #[serde(default)]
    event_id: Option<String>,
    /// v1 compatibility: a discrete context key becomes `context.contextKey`.
    #[serde(default)]
    context_key: Option<String>,
    /// v1 compatibility: `features` merge into the context.
    #[serde(default)]
    features: Option<Value>,
    /// Wait until the decision is committed to the event store before
    /// answering (otherwise it is acknowledged once queued).
    #[serde(default)]
    durable: bool,
}

/// Client-supplied decision ids: 1-128 characters from `[A-Za-z0-9_.:-]`.
pub fn validate_event_id_str(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
    if ok {
        Ok(())
    } else {
        Err("decision ids must be 1-128 characters from A-Z a-z 0-9 _ . : -".to_string())
    }
}

fn validate_event_id(id: &str) -> Result<(), Response> {
    validate_event_id_str(id).map_err(|e| Response::error(400, &e))
}

/// A new decision id: millisecond timestamp then 48 random bits, so ids
/// sort by time and never collide in practice.
pub fn new_decision_id(ts_ms: i64) -> String {
    format!(
        "dec_{:011x}{:012x}",
        ts_ms.max(0),
        crate::decision::random_seed() & 0xFFFF_FFFF_FFFF
    )
}

pub fn handle(
    state: &State,
    tenant: &str,
    job: &str,
    capsule: &str,
    req: &Request,
) -> HandlerResult {
    let started = std::time::Instant::now();
    let raw = req.json()?;
    let body: DecideBody = serde_json::from_value(raw.clone())
        .map_err(|e| Response::error(400, &format!("invalid decide request: {e}")))?;
    if let Some(id) = &body.event_id {
        validate_event_id(id)?;
    }

    let mut context = match body.context {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(Value::Object(m)) => Value::Object(m),
        Some(_) => return Err(Response::error(400, "context must be a JSON object")),
    };
    if let Some(k) = body.context_key {
        context["contextKey"] = Value::String(k);
    }
    if let Some(features) = body.features {
        let Value::Object(f) = features else {
            return Err(Response::error(400, "features must be a JSON object"));
        };
        for (k, v) in f {
            context[k] = v;
        }
    }

    let rt = state.runtime(tenant, job, capsule)?;
    let request_sha256 = sha256_hex(&req.body);

    // eventId: the same id with the same request returns the original
    // decision (safe client retries); a different request is a conflict.
    // Concurrent requests with one id are serialized until it is queued.
    let event_guard = body.event_id.as_deref().map(|id| rt.event_lock(id));
    if let Some(id) = &body.event_id
        && let Some(existing) = state.find_decision(&rt.key, id)?
    {
        return if existing.request_sha256 == request_sha256 {
            Ok(Response::json(200, &decision_response(&existing, true)))
        } else {
            Err(Response::error(
                409,
                &format!("eventId {id:?} was already used for a different request"),
            ))
        };
    }

    // Feature program: may compute derived features and restrict actions.
    let program_out = match &rt.program {
        Some(program) => {
            let policy = rt.policy.clone();
            let data_dir = rt.data_dir.clone();
            let result =
                super::capsules::tokio_blocking(|| program.run(&context, &policy, &data_dir));
            match result {
                Ok(out) => out,
                Err(e) => {
                    state.audit(
                        &rt.key,
                        "execution_denied",
                        json!({"error": e, "requestId": req.request_id}),
                    );
                    return Err(Response::error(
                        500,
                        &format!("feature program failed: {e}"),
                    ));
                }
            }
        }
        None => ProgramOutput::default(),
    };

    let mut excluded = body.excluded_actions;
    excluded.extend(program_out.excluded.iter().cloned());
    let input = DecideInput {
        context,
        derived: program_out.derived.clone(),
        actions: body.actions,
        excluded,
        eligible: program_out.only.clone(),
        baseline: body.baseline_action,
    };

    let engine = rt.engine.read().unwrap();
    let seed = rt.next_seed(engine.spec());
    let decision = engine
        .decide(&input, seed)
        .map_err(|e| Response::error(400, &e.to_string()))?;
    drop(engine);

    let ts_ms = now_ms();
    let id = body.event_id.unwrap_or_else(|| new_decision_id(ts_ms));
    let chosen = decision.chosen_action();
    let record = DecisionRecord {
        id,
        key: rt.key.clone(),
        ts_ms,
        model_version: decision.model_version,
        mode: mode_name(&decision.mode).to_string(),
        context: input.context.to_string(),
        actions: serde_json::to_string(&decision.actions).unwrap_or_else(|_| "[]".into()),
        eligible: serde_json::to_string(&decision.eligible).unwrap_or_else(|_| "[]".into()),
        pmf: Some(serde_json::to_string(&decision.pmf).unwrap_or_else(|_| "[]".into())),
        chosen_index: decision.chosen as i64,
        chosen_id: chosen.id.clone(),
        probability: Some(decision.probability),
        seed: decision.seed,
        derived: input.derived.to_string(),
        reason: program_out.reason.clone(),
        request_sha256,
        program_sha256: rt.program.as_ref().map(|p| p.sha256.clone()),
    };
    let mut response = json!({
        "decisionId": record.id,
        "action": record.chosen_id,
        "actionIndex": record.chosen_index,
        "probability": decision.probability,
        "ranking": decision
            .ranking()
            .into_iter()
            .map(|(i, p)| json!({ "id": decision.actions[i].id, "probability": p }))
            .collect::<Vec<_>>(),
        "mode": record.mode,
        "modelVersion": decision.model_version,
    });
    if let Some(reason) = &record.reason {
        response["reason"] = json!(reason);
    }
    state
        .writer
        .enqueue(record)
        .map_err(|e| Response::error(503, &e).with_header("retry-after", "1"))?;
    drop(event_guard);
    if body.durable && !state.writer.flush(std::time::Duration::from_secs(5)) {
        return Err(Response::error(503, "decision logged but not yet durable")
            .with_header("retry-after", "1"));
    }
    state.metrics.observe_decide(started.elapsed());
    Ok(Response::json(200, &response))
}

pub fn mode_name(mode: &crate::decision::Mode) -> &'static str {
    match mode {
        crate::decision::Mode::Learner => "learner",
        crate::decision::Mode::BaselineExplore => "baselineExplore",
        crate::decision::Mode::Frozen => "frozen",
    }
}

/// The response for a decision record (freshly made or replayed).
pub fn decision_response(record: &DecisionRecord, replayed: bool) -> Value {
    let actions: Vec<ActionSpec> = serde_json::from_str(&record.actions).unwrap_or_default();
    let eligible: Vec<usize> = serde_json::from_str(&record.eligible).unwrap_or_default();
    let pmf: Vec<f64> = record
        .pmf
        .as_deref()
        .and_then(|p| serde_json::from_str(p).ok())
        .unwrap_or_default();
    let mut ranking: Vec<(usize, f64)> =
        eligible.iter().copied().zip(pmf.iter().copied()).collect();
    ranking.sort_by(|a, b| b.1.total_cmp(&a.1));
    let ranking: Vec<Value> = ranking
        .into_iter()
        .filter_map(|(i, p)| {
            actions
                .get(i)
                .map(|a| json!({"id": a.id, "probability": p}))
        })
        .collect();
    let mut v = json!({
        "decisionId": record.id,
        "action": record.chosen_id,
        "actionIndex": record.chosen_index,
        "probability": record.probability,
        "ranking": ranking,
        "mode": record.mode,
        "modelVersion": record.model_version,
    });
    if let Some(reason) = &record.reason {
        v["reason"] = json!(reason);
    }
    if replayed {
        v["replayed"] = json!(true);
    }
    v
}
