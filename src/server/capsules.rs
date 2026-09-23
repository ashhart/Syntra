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
/// `PUT .../spec[?replace=true]`: a JSON merge patch over the stored spec,
/// or with `replace=true` over the default spec, which also repairs a
/// `spec.json` that no longer parses.
pub fn put_spec(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let patch = req.json()?;
    let replace = match req.query_param("replace").as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => {
            return Err(Response::error(
                400,
                &format!("replace must be true or false (got {other:?})"),
            ));
        }
    };
    change_spec(
        state,
        t,
        j,
        c,
        SpecChange {
            patch: &patch,
            event: if replace {
                "spec_replaced"
            } else {
                "spec_updated"
            },
            expect_base: None,
            audit_extra: json!({}),
            replace,
        },
    )
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
    change_spec(
        state,
        t,
        j,
        c,
        SpecChange {
            patch,
            event,
            expect_base: None,
            audit_extra: json!({}),
            replace: false,
        },
    )
}

/// A spec change.
pub(crate) struct SpecChange<'a> {
    /// A JSON merge patch.
    pub patch: &'a Value,
    /// Audit event name (`capsule_created` when the capsule is new).
    pub event: &'a str,
    /// Refuse (409) unless the stored spec is still exactly this one, so a
    /// change decided on an older spec cannot overwrite a newer one.
    pub expect_base: Option<&'a crate::decision::DecisionSpec>,
    /// Fields added to the audit record.
    pub audit_extra: Value,
    /// Apply the patch to the default spec instead of the stored one; the
    /// stored one need not parse.
    pub replace: bool,
}

/// Apply a spec change under the capsule lock.
pub(crate) fn change_spec(
    state: &State,
    t: &str,
    j: &str,
    c: &str,
    change: SpecChange<'_>,
) -> HandlerResult {
    let SpecChange {
        patch,
        event,
        expect_base,
        audit_extra,
        replace,
    } = change;
    let k = key(t, j, c)?;
    let lock = state.locks.get(t, j, c);
    let _guard = lock.lock().unwrap();
    let current = if replace {
        // The stored spec may be the thing being repaired.
        match state.store.load_spec(t, j, c) {
            Ok(spec) => spec,
            Err(_) if state.store.capsule_exists(t, j, c) => None,
            Err(e) => return Err(internal(e)),
        }
    } else {
        state.store.load_spec(t, j, c).map_err(internal)?
    };
    let exists = replace && state.store.capsule_exists(t, j, c);
    if let Some(expected) = expect_base
        && current.as_ref() != Some(expected)
    {
        return Err(Response::error(
            409,
            "the spec changed while the candidate was being evaluated; evaluate again",
        ));
    }
    let created = current.is_none() && !exists;
    let base = if replace {
        Default::default()
    } else {
        current.unwrap_or_default()
    };
    let spec = base
        .merge_patch(patch)
        .map_err(|e| Response::error(400, &e))?;
    // The rewards so far trained the model under the current spec (reward
    // range, learning rate, importance, mode). Snapshot them before the new
    // spec applies, and apply no reward in between, so a restart replays
    // each reward under the spec that trained it.
    let rt = if created {
        None
    } else {
        state.runtime(t, j, c).ok()
    };
    let _order = rt.as_ref().map(|rt| rt.reward_lock.lock().unwrap());
    if let Some(rt) = &rt {
        snapshot_now(state, rt)?;
    }
    state.store.save_spec(t, j, c, &spec).map_err(internal)?;
    // Hot swap when the learned model still fits; otherwise reload, which
    // rebuilds the model from the reward log.
    let swapped = rt
        .as_ref()
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
/// beyond the tenant (cloud metadata, the store host), so only a global
/// admin credential (the operator key or an `admin` token) may set
/// `deny_private_networks: false`.
pub fn validate_policy_put(body: &str, scope: &Scope) -> Result<(), Response> {
    let policy = crate::context::ExecutionPolicy::from_policy_json(body)
        .map_err(|e| Response::error(400, &e))?;
    if !policy.deny_private_networks && !matches!(scope, Scope::Admin) {
        return Err(Response::error(
            403,
            "only an admin credential (the operator key or an admin token) may set \
             deny_private_networks to false",
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
    if removed_files || removed_rows > 0 {
        // Audit events outlive the capsule; this one records its end.
        state.audit(
            &k,
            "capsule_deleted",
            json!({ "removedRows": removed_rows }),
        );
    }
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
    // The erased rewards can no longer be replayed, so the model is
    // snapshotted first, and no reward is applied until the log is gone.
    let rt = state.runtime(t, j, c)?;
    let _order = rt.reward_lock.lock().unwrap();
    snapshot_now(state, &rt)?;
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

/// Persist the model with every reward it has applied, or fail with 503.
/// Callers hold the runtime's reward lock.
fn snapshot_now(state: &State, rt: &super::runtime::CapsuleRuntime) -> Result<(), Response> {
    let version = rt.engine.read().unwrap().model_version();
    if version == 0 {
        return Ok(());
    }
    state.snapshot(rt);
    let saved = state.events.load_latest_model(&rt.key).map_err(internal)?;
    if saved.is_none_or(|s| s.version != version) {
        return Err(
            Response::error(503, "could not snapshot the model; nothing was changed")
                .with_header("retry-after", "1"),
        );
    }
    Ok(())
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
