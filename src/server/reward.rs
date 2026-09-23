//! `POST .../reward` (and its alias `.../feedback`): record the outcome of
//! a decision and update the model.
//!
//! Rewards for one capsule apply in sequence-number order under the
//! capsule's reward lock, so a restart that replays rewards after the last
//! snapshot rebuilds exactly the live model.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::eventstore::RewardRecord;
use crate::store::now_ms;

use super::http::{HandlerResult, Request, Response};
use super::runtime::decision_learning_inputs;
use super::state::State;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RewardBody {
    decision_id: String,
    #[serde(alias = "value")]
    reward: f64,
    #[serde(default)]
    idempotency_key: Option<String>,
    /// Free-form JSON stored with the reward (for example the quality,
    /// latency and cost the reward was computed from).
    #[serde(default)]
    detail: Option<Value>,
    /// Wait until the reward is committed to the event store before
    /// answering (otherwise it is acknowledged once queued).
    #[serde(default)]
    durable: bool,
}

pub fn handle(
    state: &State,
    tenant: &str,
    job: &str,
    capsule: &str,
    req: &Request,
) -> HandlerResult {
    let raw = req.json()?;
    let body: RewardBody = serde_json::from_value(raw)
        .map_err(|e| Response::error(400, &format!("invalid reward request: {e}")))?;
    if !body.reward.is_finite() {
        return Err(Response::error(400, "reward must be a finite number"));
    }
    let rt = state.runtime(tenant, job, capsule)?;
    let outcome = apply_reward(
        state,
        &rt,
        &body.decision_id,
        body.reward,
        body.idempotency_key,
        body.detail,
    )?;
    if body.durable && !state.writer.flush(std::time::Duration::from_secs(5)) {
        return Err(
            Response::error(503, "reward accepted but not yet durable; retrying is safe")
                .with_header("retry-after", "1"),
        );
    }
    Ok(Response::json(200, &outcome))
}

/// Record and learn from one reward. Shared by the single and batch routes.
///
/// The model learns immediately; the reward row joins the write-behind
/// queue behind its decision (see `writer.rs` for the ordering and
/// durability guarantees).
pub fn apply_reward(
    state: &State,
    rt: &super::runtime::CapsuleRuntime,
    decision_id: &str,
    value: f64,
    idempotency_key: Option<String>,
    detail: Option<Value>,
) -> Result<Value, Response> {
    let _order = rt.reward_lock.lock().unwrap();
    let spec = rt.spec();
    // A decision waiting for activation keeps its rewards until then.
    if rt.hold_reward(
        decision_id,
        super::runtime::HeldReward {
            value,
            idempotency_key: idempotency_key.clone(),
            detail: detail.clone(),
        },
    ) {
        let version = rt.engine.read().unwrap().model_version();
        return Ok(json!({ "ok": true, "applied": false, "held": true, "modelVersion": version }));
    }
    let decision = state
        .find_decision(&rt.key, decision_id)?
        .ok_or_else(|| Response::error(404, &format!("decision {decision_id:?} not found")))?;

    let frozen = matches!(spec.mode, crate::decision::Mode::Frozen);
    let idempotency_key = match (&spec.rewards, idempotency_key) {
        // `first`: one reward per decision, whatever key the caller sends.
        (crate::decision::spec::RewardAggregation::First, _) => decision_id.to_string(),
        (crate::decision::spec::RewardAggregation::Sum, Some(k)) => k,
        // `sum` counts every reward, so each gets a unique key unless the
        // caller supplies one for retries.
        (crate::decision::spec::RewardAggregation::Sum, None) => {
            format!("{decision_id}:{:016x}", crate::decision::random_seed())
        }
    };
    // The event store refuses NUL, and a reward the store refuses after the
    // model learned from it would make the model and the log diverge.
    if idempotency_key.is_empty() || idempotency_key.len() > 256 || idempotency_key.contains('\0') {
        return Err(Response::error(
            400,
            "idempotencyKey must be 1-256 bytes with no NUL",
        ));
    }
    let model_version = || rt.engine.read().unwrap().model_version();
    let duplicate = || json!({ "ok": true, "applied": false, "duplicate": true, "modelVersion": model_version() });
    // Queued, then committed: the writer drops a key from its queue only
    // after committing it, so checking in this order cannot miss a reward
    // that moves from one to the other in between.
    if state.writer.reward_queued(&rt.key, &idempotency_key) {
        return Ok(duplicate());
    }
    let committed = state
        .events
        .rewards_for_decision(&rt.key, decision_id)
        .map_err(|e| Response::error(500, &format!("reading rewards: {e}")))?;
    if committed
        .iter()
        .any(|r| r.idempotency_key == idempotency_key)
    {
        return Ok(duplicate());
    }

    let mut stored_detail = match detail {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(m)) => Value::Object(m),
        Some(other) => json!({ "value": other }),
    };
    // `learned` is reserved: replay skips rewards stored with `false`, so a
    // caller-supplied value must not survive.
    if frozen || stored_detail.get("learned").is_some() {
        stored_detail["learned"] = json!(!frozen);
    }
    let record = RewardRecord {
        seq: 0,
        decision_id: decision_id.to_string(),
        key: rt.key.clone(),
        ts_ms: now_ms(),
        value,
        value_norm: spec.reward.normalize(value),
        idempotency_key,
        detail: Some(stored_detail.to_string()),
    };
    match state.writer.enqueue_reward(record) {
        Ok(true) => {}
        Ok(false) => return Ok(duplicate()),
        Err(e) => return Err(Response::error(503, &e).with_header("retry-after", "1")),
    }

    let version = if frozen {
        model_version()
    } else {
        let (context, derived, action) =
            decision_learning_inputs(&decision).map_err(|e| Response::error(500, &e))?;
        let probability = decision.probability.unwrap_or(1.0);
        let mut engine = rt.engine.write().unwrap();
        engine
            .learn(&context, &derived, &action, value, probability)
            .map_err(|e| Response::error(500, &format!("learning from reward: {e}")))?;
        engine.model_version()
    };
    if !frozen {
        let n = rt
            .since_snapshot
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if n >= spec.snapshot_every {
            state.snapshot(rt);
        }
    }
    Ok(json!({
        "ok": true,
        "applied": true,
        "learned": !frozen,
        "modelVersion": version,
    }))
}
