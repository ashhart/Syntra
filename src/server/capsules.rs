//! Capsule management: spec, mode, feature program, policy, deletion.
//! Every change is audited and replaces the cached runtime.

use serde_json::{Value, json};

use crate::auth_tokens::Scope;
use crate::decision::DecisionSpec;
use crate::eventstore::CapsuleKey;
use crate::store::sha256_hex;

use super::http::{HandlerResult, Request, Response};
use super::runtime::FeatureProgram;
use super::state::State;

fn key(tenant: &str, job: &str, capsule: &str) -> Result<CapsuleKey, Response> {
    CapsuleKey::new(tenant, job, capsule).map_err(|e| Response::error(400, &e.to_string()))
}

fn internal(e: impl std::fmt::Display) -> Response {
    Response::error(500, &e.to_string())
}

/// `GET .../capsules/{c}`: spec, program, policy and model summary.
pub fn get(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    let rt = state.runtime(t, j, c)?;
    let spec = rt.spec();
    let stats = state
        .events
        .stats(&rt.key)
        .map(|s| {
            json!({
                "decisions": s.decisions,
                "rewards": s.rewards,
                "firstDecisionMs": s.first_decision_ms,
                "lastDecisionMs": s.last_decision_ms,
                "lastRewardSeq": s.last_reward_seq,
            })
        })
        .unwrap_or(Value::Null);
    let program = state.store.read_manifest(t, j, c);
    Ok(Response::json(
        200,
        &json!({
            "tenant": t, "job": j, "capsule": c,
            "spec": spec.to_json(),
            "modelVersion": rt.engine.read().unwrap().model_version(),
            "program": program,
            "policyError": rt.policy_error,
            "stats": stats,
        }),
    ))
}

/// `GET .../spec`.
pub fn get_spec(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    match state.store.load_spec(t, j, c).map_err(internal)? {
        Some(spec) => Ok(Response::json(200, &spec.to_json())),
        None => Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        )),
    }
}

/// `PUT .../spec`: an RFC 7396 merge patch over the current spec (or over
/// the defaults, which creates the capsule). Unknown fields are rejected.
pub fn put_spec(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let patch = req.json()?;
    apply_spec_patch(state, t, j, c, &patch, "spec_updated")
}

/// `POST .../mode`: `{"mode": "learner|baselineExplore|frozen",
/// "baselineEpsilon"?: number}`, a narrow form of the spec patch.
pub fn post_mode(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let body = req.json()?;
    let obj = body
        .as_object()
        .ok_or_else(|| Response::error(400, "mode request must be a JSON object"))?;
    for k in obj.keys() {
        if k != "mode" && k != "baselineEpsilon" {
            return Err(Response::error(
                400,
                &format!("unknown field {k:?} (allowed: mode, baselineEpsilon)"),
            ));
        }
    }
    if !obj.contains_key("mode") {
        return Err(Response::error(400, "mode is required"));
    }
    if !state.store.capsule_exists(t, j, c) {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    apply_spec_patch(state, t, j, c, &body, "mode_changed")
}

fn apply_spec_patch(
    state: &State,
    t: &str,
    j: &str,
    c: &str,
    patch: &Value,
    event: &str,
) -> HandlerResult {
    apply_spec_patch_checked(state, t, j, c, patch, event, None, json!({}))
}

/// Apply a spec merge patch. With `expect_base`, refuse (409) unless the
/// stored spec is still exactly that one, so a change decided on an older
/// spec cannot overwrite a newer one. `audit_extra` fields join the audit
/// record.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_spec_patch_checked(
    state: &State,
    t: &str,
    j: &str,
    c: &str,
    patch: &Value,
    event: &str,
    expect_base: Option<&crate::decision::DecisionSpec>,
    audit_extra: Value,
) -> HandlerResult {
    let k = key(t, j, c)?;
    let lock = state.locks.get(t, j, c);
    let _guard = lock.lock().unwrap();
    let current = state.store.load_spec(t, j, c).map_err(internal)?;
    if let Some(expected) = expect_base
        && current.as_ref() != Some(expected)
    {
        return Err(Response::error(
            409,
            "the spec changed while the candidate was being evaluated; evaluate again",
        ));
    }
    let created = current.is_none();
    let base = current.unwrap_or_default();
    let spec = base
        .merge_patch(patch)
        .map_err(|e| Response::error(400, &e))?;
    state.store.save_spec(t, j, c, &spec).map_err(internal)?;
    // Hot swap when the learned model still fits; otherwise reload, which
    // rebuilds the model from the reward log.
    let swapped = state
        .runtimes
        .get(&state.store, &*state.events, t, j, c)
        .ok()
        .map(|rt| rt.engine.write().unwrap().set_spec(spec.clone()).is_ok())
        .unwrap_or(false);
    if !swapped {
        state.runtimes.invalidate(t, j, c);
    }
    let spec_json = spec.to_json();
    let mut detail =
        json!({ "specSha256": sha256_hex(spec_json.to_string().as_bytes()), "patch": patch });
    if let (Some(d), Value::Object(extra)) = (detail.as_object_mut(), audit_extra) {
        d.extend(extra);
    }
    state.audit(&k, if created { "capsule_created" } else { event }, detail);
    Ok(Response::json(if created { 201 } else { 200 }, &spec_json))
}

/// `POST .../install`: upload a compiled feature program (`.lyc`). Creates
/// the capsule with a default spec when it does not exist.
pub fn install(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let k = key(t, j, c)?;
    let program =
        tokio_blocking(|| FeatureProgram::load(&req.body)).map_err(|e| Response::error(400, &e))?;
    let lock = state.locks.get(t, j, c);
    let _guard = lock.lock().unwrap();
    if !state.store.capsule_exists(t, j, c) {
        state
            .store
            .save_spec(t, j, c, &DecisionSpec::default())
            .map_err(internal)?;
    }
    state
        .store
        .save_program(t, j, c, &req.body)
        .map_err(internal)?;
    state.runtimes.invalidate(t, j, c);
    state.audit(
        &k,
        "program_installed",
        json!({ "programSha256": program.sha256, "bytes": req.body.len() }),
    );
    Ok(Response::json(
        200,
        &json!({ "ok": true, "programSha256": program.sha256, "bytes": req.body.len() }),
    ))
}

/// `DELETE .../program`.
pub fn delete_program(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    let k = key(t, j, c)?;
    let lock = state.locks.get(t, j, c);
    let _guard = lock.lock().unwrap();
    if !state.store.capsule_exists(t, j, c) {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    let removed = state.store.delete_program(t, j, c).map_err(internal)?;
    state.runtimes.invalidate(t, j, c);
    if removed {
        state.audit(&k, "program_removed", json!({}));
    }
    Ok(Response::json(
        200,
        &json!({ "ok": true, "removed": removed }),
    ))
}

/// `GET .../policy`.
pub fn get_policy(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    if !state.store.capsule_exists(t, j, c) {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    let text = state
        .store
        .load_policy_text(t, j, c)
        .map_err(internal)?
        .unwrap_or_else(|| crate::store::DEFAULT_POLICY.to_string());
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(Response::json(200, &value))
}

/// Strict policy validation. Opening a capsule to private networks reaches
/// beyond the tenant (cloud metadata, the store host), so only the
/// operator key may set `deny_private_networks: false`.
pub fn validate_policy_put(body: &str, scope: &Scope) -> Result<(), Response> {
    let policy = crate::context::ExecutionPolicy::from_policy_json(body)
        .map_err(|e| Response::error(400, &e))?;
    if !policy.deny_private_networks && !matches!(scope, Scope::Admin) {
        return Err(Response::error(
            403,
            "only the operator admin key may set deny_private_networks to false",
        ));
    }
    Ok(())
}

/// `PUT .../policy`.
pub fn put_policy(
    state: &State,
    t: &str,
    j: &str,
    c: &str,
    req: &Request,
    scope: &Scope,
    principal: Option<&str>,
) -> HandlerResult {
    let k = key(t, j, c)?;
    let text = req.text()?;
    validate_policy_put(text, scope)?;
    let lock = state.locks.get(t, j, c);
    let _guard = lock.lock().unwrap();
    if !state.store.capsule_exists(t, j, c) {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    state
        .store
        .save_policy_text(t, j, c, text)
        .map_err(internal)?;
    state.runtimes.invalidate(t, j, c);
    state.audit(
        &k,
        "policy_updated",
        json!({ "policySha256": sha256_hex(text.as_bytes()), "principalId": principal }),
    );
    Ok(Response::json(200, &json!({ "ok": true })))
}

/// `DELETE .../capsules/{c}`: the capsule's files, decisions, rewards,
/// models and audit rows (data erasure).
pub fn delete(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    let k = key(t, j, c)?;
    let lock = state.locks.get(t, j, c);
    let _guard = lock.lock().unwrap();
    // Queued decisions must land first, or they would reappear after the
    // delete.
    state.writer.flush(std::time::Duration::from_secs(5));
    let removed_files = state.store.delete_capsule(t, j, c).map_err(internal)?;
    let removed_rows = state.events.delete_capsule(&k).map_err(internal)?;
    state.runtimes.invalidate(t, j, c);
    tracing::info!(
        tenant = t,
        job = j,
        capsule = c,
        removed_rows,
        "capsule deleted"
    );
    if !removed_files && removed_rows == 0 {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    Ok(Response::json(
        200,
        &json!({ "ok": true, "removedRows": removed_rows }),
    ))
}

/// `DELETE .../logs`: erase decisions and rewards, keep the spec, program
/// and learned model. Off-policy evaluation can no longer use the erased
/// traffic.
pub fn purge_logs(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    let k = key(t, j, c)?;
    if !state.store.capsule_exists(t, j, c) {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    state.writer.flush(std::time::Duration::from_secs(5));
    let removed = state
        .events
        .prune_decisions_before(&k, i64::MAX)
        .map_err(internal)?;
    let counts =
        json!({ "removedDecisions": removed.decisions, "removedRewards": removed.rewards });
    state.audit(&k, "logs_purged", counts.clone());
    Ok(Response::json(
        200,
        &json!({ "ok": true, "removed": counts }),
    ))
}

/// Run blocking work off the async scheduler when inside a multi-threaded
/// tokio runtime; run it directly anywhere else (tests, CLI).
pub fn tokio_blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}
