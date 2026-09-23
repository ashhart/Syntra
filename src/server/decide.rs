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
use super::runtime::{CapsuleRuntime, ProgramOutput};
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
    let body: DecideBody = serde_json::from_value(raw)
        .map_err(|e| Response::error(400, &format!("invalid decide request: {e}")))?;

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
    let request = DecideRequest {
        context,
        actions: body.actions,
        excluded: body.excluded_actions,
        baseline: body.baseline_action,
        event_id: body.event_id,
        request_sha256: sha256_hex(&req.body),
        defer: false,
        durable: body.durable,
    };
    let response = decide_core(state, &rt, request, &req.request_id, syntra_response)?;
    state.metrics.observe_decide(started.elapsed());
    Ok(Response::json(200, &response))
}

/// A decide request after parsing, from any API surface.
pub struct DecideRequest {
    /// A JSON object.
    pub context: Value,
    pub actions: Option<Vec<ActionSpec>>,
    pub excluded: Vec<String>,
    pub baseline: Option<String>,
    /// Caller-chosen decision id: repeating a request with it returns the
    /// original decision; a different request with it is a 409.
    pub event_id: Option<String>,
    /// Hash of the request as received, to tell a retry from a conflict.
    pub request_sha256: String,
    /// Hold the decision until it is activated instead of logging it now
    /// (Personalizer `deferActivation`).
    pub defer: bool,
    /// Wait for the record to be committed before answering.
    pub durable: bool,
}

/// What a response builder sees: the record, and the actions, eligible
/// indices and PMF it was drawn from.
pub struct DecisionView<'a> {
    pub record: &'a DecisionRecord,
    pub actions: &'a [ActionSpec],
    pub eligible: &'a [usize],
    pub pmf: &'a [f64],
    /// True when an `eventId` retry returned an earlier decision.
    pub replayed: bool,
}

/// Choose an action and log (or hold) the decision. `respond` builds the
/// answer before the record moves into the write-behind queue.
pub fn decide_core(
    state: &State,
    rt: &CapsuleRuntime,
    request: DecideRequest,
    request_id: &str,
    respond: impl FnOnce(&DecisionView<'_>) -> Value,
) -> Result<Value, Response> {
    if let Some(id) = &request.event_id {
        validate_event_id(id)?;
    }
    // eventId: the same id with the same request returns the original
    // decision (safe client retries); a different request is a conflict.
    // Concurrent requests with one id are serialized until it is queued.
    let event_guard = request.event_id.as_deref().map(|id| rt.event_lock(id));
    if let Some(id) = &request.event_id {
        let existing = match rt.deferred_record(id) {
            Some(r) => Some(r),
            None => state.find_decision(&rt.key, id)?,
        };
        if let Some(existing) = existing {
            // A retried durable request must not be answered before the
            // decision it replays is committed (its first attempt may have
            // answered 503 "not yet durable").
            if existing.request_sha256 == request.request_sha256
                && request.durable
                && !request.defer
                && !state.writer.flush(std::time::Duration::from_secs(5))
            {
                return Err(Response::error(503, "decision logged but not yet durable")
                    .with_header("retry-after", "1"));
            }
            return if existing.request_sha256 == request.request_sha256 {
                let actions: Vec<ActionSpec> =
                    serde_json::from_str(&existing.actions).unwrap_or_default();
                let eligible: Vec<usize> =
                    serde_json::from_str(&existing.eligible).unwrap_or_default();
                let pmf: Vec<f64> = existing
                    .pmf
                    .as_deref()
                    .and_then(|p| serde_json::from_str(p).ok())
                    .unwrap_or_default();
                annotate_decision(&existing, true);
                Ok(respond(&DecisionView {
                    record: &existing,
                    actions: &actions,
                    eligible: &eligible,
                    pmf: &pmf,
                    replayed: true,
                }))
            } else {
                Err(Response::error(
                    409,
                    &format!("eventId {id:?} was already used for a different request"),
                ))
            };
        }
    }

    // Feature program: may compute derived features and restrict actions.
    let program_out = match &rt.program {
        Some(program) => {
            let policy = rt.policy.clone();
            let data_dir = rt.data_dir.clone();
            let context = &request.context;
            let result =
                super::capsules::tokio_blocking(|| program.run(context, &policy, &data_dir));
            match result {
                Ok(out) => out,
                Err(e) => {
                    state.audit(
                        &rt.key,
                        "execution_denied",
                        json!({"error": e, "requestId": request_id}),
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

    let mut excluded = request.excluded;
    excluded.extend(program_out.excluded.iter().cloned());
    let input = DecideInput {
        context: request.context,
        derived: program_out.derived.clone(),
        actions: request.actions,
        excluded,
        eligible: program_out.only.clone(),
        baseline: request.baseline,
    };

    let engine = rt.engine.read().unwrap();
    let seed = rt.next_seed(engine.spec());
    let decision = engine
        .decide(&input, seed)
        .map_err(|e| Response::error(400, &e.to_string()))?;
    drop(engine);

    let ts_ms = now_ms();
    let id = request.event_id.unwrap_or_else(|| new_decision_id(ts_ms));
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
        request_sha256: request.request_sha256,
        program_sha256: rt.program.as_ref().map(|p| p.sha256.clone()),
    };
    annotate_decision(&record, false);
    let response = respond(&DecisionView {
        record: &record,
        actions: &decision.actions,
        eligible: &decision.eligible,
        pmf: &decision.pmf,
        replayed: false,
    });
    if request.defer {
        rt.defer(record)
            .map_err(|e| Response::error(503, &e).with_header("retry-after", "1"))?;
        return Ok(response);
    }
    state
        .writer
        .enqueue(record)
        .map_err(|e| Response::error(503, &e).with_header("retry-after", "1"))?;
    drop(event_guard);
    if request.durable && !state.writer.flush(std::time::Duration::from_secs(5)) {
        return Err(Response::error(503, "decision logged but not yet durable")
            .with_header("retry-after", "1"));
    }
    Ok(response)
}

/// Span attributes of a decision, when the request is traced.
fn annotate_decision(r: &DecisionRecord, replayed: bool) {
    super::otel::annotate(|a| {
        a.str("syntra.decision.id", &r.id)
            .str("syntra.decision.action", &r.chosen_id)
            .int("syntra.decision.model_version", r.model_version as i64)
            .str("syntra.decision.mode", &r.mode);
        if let Some(p) = r.probability {
            a.f64("syntra.decision.probability", p);
        }
        if replayed {
            a.bool("syntra.decision.replayed", true);
        }
    });
}

/// The `/decide` answer.
pub fn syntra_response(v: &DecisionView<'_>) -> Value {
    let mut ranking: Vec<(usize, f64)> = v
        .eligible
        .iter()
        .copied()
        .zip(v.pmf.iter().copied())
        .collect();
    ranking.sort_by(|a, b| b.1.total_cmp(&a.1));
    let ranking: Vec<Value> = ranking
        .into_iter()
        .filter_map(|(i, p)| {
            v.actions
                .get(i)
                .map(|a| json!({"id": a.id, "probability": p}))
        })
        .collect();
    let r = v.record;
    let mut out = json!({
        "decisionId": r.id,
        "action": r.chosen_id,
        "actionIndex": r.chosen_index,
        "probability": r.probability,
        "ranking": ranking,
        "mode": r.mode,
        "modelVersion": r.model_version,
    });
    if let Some(reason) = &r.reason {
        out["reason"] = json!(reason);
    }
    if v.replayed {
        out["replayed"] = json!(true);
    }
    out
}

pub fn mode_name(mode: &crate::decision::Mode) -> &'static str {
    match mode {
        crate::decision::Mode::Learner => "learner",
        crate::decision::Mode::BaselineExplore => "baselineExplore",
        crate::decision::Mode::Frozen => "frozen",
    }
}
