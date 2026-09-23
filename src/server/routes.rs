//! Request routing.
//!
//! `/v1/...` is the canonical API. The same paths without `/v1` are
//! deprecated aliases: identical behavior plus `Deprecation` and `Link`
//! headers. `/health`, `/ready`, `/metrics` and `/admin` are unversioned.

use serde_json::json;

use crate::auth_tokens::{Action, Scope};

use super::auth::{authenticate, authorize, rate_limit};
use super::http::{Request, Response};
use super::state::State;
use super::{capsules, decide, query, reward};

/// Route a request and record it in the metrics.
pub fn route(req: &Request, state: &State) -> Response {
    let (label, resp) = route_inner(req, state);
    state.metrics.record_request(label, resp.status);
    resp.with_header("x-request-id", &req.request_id)
}

fn route_inner(req: &Request, state: &State) -> (&'static str, Response) {
    match req.path.as_str() {
        "/health" => {
            return (
                "health",
                Response::json(200, &json!({ "ok": true, "service": state.service_name })),
            );
        }
        "/ready" => return ("ready", ready(state)),
        "/metrics" => {
            return (
                "metrics",
                Response::new(
                    200,
                    "text/plain; version=0.0.4",
                    super::metrics::render(state),
                ),
            );
        }
        "/admin" | "/v1/admin" => {
            return (
                "admin.console",
                Response::html(200, super::admin::console_html(&state.service_name)),
            );
        }
        _ => {}
    }

    let (path, versioned) = match req.path.strip_prefix("/v1") {
        Some("") => ("/".to_string(), true),
        Some(rest) if rest.starts_with('/') => (rest.to_string(), true),
        _ => (req.path.clone(), false),
    };

    let auth = match authenticate(req, state) {
        Ok(a) => a,
        Err(r) => return ("unauthorized", r),
    };
    let scope = auth.scope();
    let principal = auth.principal_id();
    if let Some(r) = rate_limit(state, principal.as_deref()) {
        return ("rate_limited", r);
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (label, result) = dispatch(req, state, &segments, &auth, &scope, principal.as_deref());
    let resp = result.unwrap_or_else(|e| e);
    let resp = if versioned {
        resp
    } else {
        resp.with_header("deprecation", "true").with_header(
            "link",
            &format!("</v1{}>; rel=\"successor-version\"", req.target()),
        )
    };
    (label, resp)
}

type Routed = (&'static str, Result<Response, Response>);

fn dispatch(
    req: &Request,
    state: &State,
    seg: &[&str],
    auth: &super::auth::AuthOutcome,
    scope: &Scope,
    principal: Option<&str>,
) -> Routed {
    let m = req.method.as_str();
    match (m, seg) {
        ("GET", ["auth", "whoami"]) => (
            "auth.whoami",
            Ok(Response::json(
                200,
                &json!({ "ok": true, "kind": auth.kind(), "principalId": principal, "scope": scope }),
            )),
        ),
        ("GET", ["capabilities"]) => (
            "capabilities",
            Ok(Response::new(
                200,
                "application/json",
                crate::capabilities::json_catalog(),
            )),
        ),

        // ── Tokens (operator only) ──
        ("POST", ["admin", "tokens"]) => (
            "admin.tokens.issue",
            admin_only(scope).and_then(|_| issue_token(req, state)),
        ),
        ("GET", ["admin", "tokens"]) => (
            "admin.tokens.list",
            admin_only(scope).and_then(|_| list_tokens(state)),
        ),
        ("DELETE", ["admin", "tokens", hash]) => (
            "admin.tokens.revoke",
            admin_only(scope).and_then(|_| revoke_token(state, hash)),
        ),
        ("GET", ["admin", "capsules"]) => (
            "admin.capsules",
            admin_only(scope).and_then(|_| list_all_capsules(state)),
        ),

        // ── Tenants and jobs ──
        ("GET", ["tenants"]) => ("tenants.list", Ok(list_tenants(state, scope))),
        ("DELETE", ["tenants", t]) => (
            "tenants.delete",
            authorize(scope, &Action::TenantOp { tenant: t }).and_then(|_| delete_tenant(state, t)),
        ),
        ("GET", ["tenants", t, "jobs"]) => (
            "jobs.list",
            authorize(scope, &Action::TenantOp { tenant: t }).and_then(|_| {
                let jobs = state
                    .store
                    .list_jobs(t)
                    .map_err(|e| Response::error(400, &e))?;
                Ok(Response::json(200, &json!({ "jobs": jobs })))
            }),
        ),
        ("POST", ["tenants", t, "jobs"]) => (
            "jobs.create",
            authorize(scope, &Action::TenantOp { tenant: t })
                .and_then(|_| create_job(state, t, req)),
        ),
        ("GET", ["tenants", t, "jobs", j]) => (
            "jobs.get",
            authorize(scope, &Action::TenantOp { tenant: t }).and_then(|_| {
                match state
                    .store
                    .get_job(t, j)
                    .map_err(|e| Response::error(400, &e))?
                {
                    Some(job) => Ok(Response::json(200, &job)),
                    None => Err(Response::error(404, &format!("job {t}/{j} not found"))),
                }
            }),
        ),
        ("DELETE", ["tenants", t, "jobs", j]) => (
            "jobs.delete",
            authorize(scope, &Action::TenantOp { tenant: t }).and_then(|_| delete_job(state, t, j)),
        ),
        ("GET", ["tenants", t, "jobs", j, "capsules"]) => (
            "capsules.list",
            authorize(scope, &Action::TenantOp { tenant: t }).and_then(|_| {
                let capsules = state
                    .store
                    .list_capsules(t, j)
                    .map_err(|e| Response::error(400, &e))?;
                Ok(Response::json(200, &json!({ "capsules": capsules })))
            }),
        ),

        // ── Capsules ──
        (_, ["tenants", t, "jobs", j, "capsules", c, rest @ ..]) => {
            capsule_route(req, state, m, t, j, c, rest, scope, principal)
        }

        _ => ("not_found", Err(Response::error(404, "no such route"))),
    }
}

#[allow(clippy::too_many_arguments)]
fn capsule_route(
    req: &Request,
    state: &State,
    m: &str,
    t: &str,
    j: &str,
    c: &str,
    rest: &[&str],
    scope: &Scope,
    principal: Option<&str>,
) -> Routed {
    let read = || {
        authorize(
            scope,
            &Action::CapsuleRead {
                tenant: t,
                job: j,
                capsule: c,
            },
        )
    };
    let data = || {
        authorize(
            scope,
            &Action::CapsuleDecide {
                tenant: t,
                job: j,
                capsule: c,
            },
        )
    };
    let mutate = || {
        authorize(
            scope,
            &Action::CapsuleMutate {
                tenant: t,
                job: j,
                capsule: c,
            },
        )
    };
    match (m, rest) {
        ("POST", ["decide"]) => (
            "capsule.decide",
            data().and_then(|_| decide::handle(state, t, j, c, req)),
        ),
        ("POST", ["reward"]) | ("POST", ["feedback"]) => (
            "capsule.reward",
            data().and_then(|_| reward::handle(state, t, j, c, req)),
        ),
        ("POST", ["decisions:batch"]) => (
            "capsule.decisions.upload",
            data().and_then(|_| super::upload::decisions_batch(state, t, j, c, req)),
        ),
        ("POST", ["rewards:batch"]) => (
            "capsule.rewards.upload",
            data().and_then(|_| super::upload::rewards_batch(state, t, j, c, req)),
        ),
        ("GET", []) => (
            "capsule.get",
            read().and_then(|_| capsules::get(state, t, j, c)),
        ),
        ("DELETE", []) => (
            "capsule.delete",
            mutate().and_then(|_| capsules::delete(state, t, j, c)),
        ),
        ("GET", ["spec"]) => (
            "capsule.spec.get",
            read().and_then(|_| capsules::get_spec(state, t, j, c)),
        ),
        ("PUT", ["spec"]) => (
            "capsule.spec.put",
            mutate().and_then(|_| capsules::put_spec(state, t, j, c, req)),
        ),
        ("POST", ["mode"]) => (
            "capsule.mode",
            mutate().and_then(|_| capsules::post_mode(state, t, j, c, req)),
        ),
        ("POST", ["install"]) => (
            "capsule.install",
            mutate().and_then(|_| capsules::install(state, t, j, c, req)),
        ),
        ("DELETE", ["program"]) => (
            "capsule.program.delete",
            mutate().and_then(|_| capsules::delete_program(state, t, j, c)),
        ),
        ("GET", ["policy"]) => (
            "capsule.policy.get",
            read().and_then(|_| capsules::get_policy(state, t, j, c)),
        ),
        ("PUT", ["policy"]) => (
            "capsule.policy.put",
            mutate().and_then(|_| capsules::put_policy(state, t, j, c, req, scope, principal)),
        ),
        ("DELETE", ["logs"]) => (
            "capsule.logs.purge",
            mutate().and_then(|_| capsules::purge_logs(state, t, j, c)),
        ),
        ("GET", ["decisions"]) => (
            "capsule.decisions.list",
            read().and_then(|_| query::list_decisions(state, t, j, c, req)),
        ),
        ("GET", ["decisions", id]) => (
            "capsule.decisions.get",
            read().and_then(|_| query::get_decision(state, t, j, c, id)),
        ),
        ("GET", ["model"]) => (
            "capsule.model",
            read().and_then(|_| query::get_model(state, t, j, c, req)),
        ),
        ("GET", ["audits"]) => (
            "capsule.audits",
            read().and_then(|_| query::list_audit(state, t, j, c, req)),
        ),
        _ => (
            "not_found",
            Err(Response::error(404, "no such capsule route")),
        ),
    }
}

fn admin_only(scope: &Scope) -> Result<(), Response> {
    authorize(scope, &Action::AdminGlobal)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn issue_token(req: &Request, state: &State) -> Result<Response, Response> {
    let body = req.json()?;
    let scope: Scope = serde_json::from_value(
        body.get("scope")
            .cloned()
            .ok_or_else(|| Response::error(400, "scope is required"))?,
    )
    .map_err(|e| Response::error(400, &format!("invalid scope: {e}")))?;
    let ttl = body.get("ttlSeconds").and_then(|v| v.as_u64());
    let label = body
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let now = now_secs();
    let (raw, hash) = state
        .tokens
        .lock()
        .unwrap()
        .issue(scope.clone(), ttl, label, now)
        .map_err(|e| Response::error(500, &e))?;
    Ok(Response::json(
        200,
        &json!({ "token": raw, "hash": hash, "scope": scope, "expiresAt": ttl.map(|t| now + t) }),
    ))
}

fn list_tokens(state: &State) -> Result<Response, Response> {
    let list: Vec<_> = state
        .tokens
        .lock()
        .unwrap()
        .list(now_secs())
        .into_iter()
        .map(|(hash, rec)| {
            json!({
                "hash": hash, "scope": rec.scope, "createdAt": rec.created_at,
                "expiresAt": rec.expires_at, "lastUsedAt": rec.last_used_at, "label": rec.label,
            })
        })
        .collect();
    Ok(Response::json(200, &json!({ "tokens": list })))
}

fn revoke_token(state: &State, hash: &str) -> Result<Response, Response> {
    match state.tokens.lock().unwrap().revoke(hash) {
        Ok(true) => Ok(Response::json(200, &json!({ "ok": true, "revoked": true }))),
        Ok(false) => Err(Response::error(404, "token hash not found")),
        Err(e) => Err(Response::error(500, &e)),
    }
}

fn list_tenants(state: &State, scope: &Scope) -> Response {
    let tenants: Vec<String> = state
        .store
        .list_tenants()
        .into_iter()
        .filter(|t| scope.allows(&Action::TenantOp { tenant: t }))
        .collect();
    Response::json(200, &json!({ "tenants": tenants }))
}

fn list_all_capsules(state: &State) -> Result<Response, Response> {
    let list: Vec<_> = state
        .store
        .list_all_capsules()
        .into_iter()
        .map(|(t, j, c)| json!({ "tenant": t, "job": j, "capsule": c }))
        .collect();
    Ok(Response::json(200, &json!({ "capsules": list })))
}

fn create_job(state: &State, t: &str, req: &Request) -> Result<Response, Response> {
    let body = req.json()?;
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Response::error(400, "id is required"))?;
    let name = body.get("name").and_then(|v| v.as_str());
    let created = state
        .store
        .create_job(t, id, name)
        .map_err(|e| Response::error(400, &e))?;
    let job = state
        .store
        .get_job(t, id)
        .map_err(|e| Response::error(400, &e))?;
    Ok(Response::json(
        if created { 201 } else { 200 },
        &json!({ "ok": true, "created": created, "job": job }),
    ))
}

/// Delete every capsule's events, then the files.
fn delete_job(state: &State, t: &str, j: &str) -> Result<Response, Response> {
    state.writer.flush(std::time::Duration::from_secs(5));
    let mut rows = 0;
    for c in state
        .store
        .list_capsules(t, j)
        .map_err(|e| Response::error(400, &e))?
    {
        if let Ok(k) = crate::eventstore::CapsuleKey::new(t, j, &c) {
            rows += state
                .events
                .delete_capsule(&k)
                .map_err(|e| Response::error(500, &e.to_string()))?;
        }
    }
    let removed = state
        .store
        .delete_job(t, j)
        .map_err(|e| Response::error(500, &e))?;
    state.runtimes.invalidate_prefix(t, Some(j));
    if !removed {
        return Err(Response::error(404, &format!("job {t}/{j} not found")));
    }
    Ok(Response::json(
        200,
        &json!({ "ok": true, "removedRows": rows }),
    ))
}

fn delete_tenant(state: &State, t: &str) -> Result<Response, Response> {
    state.writer.flush(std::time::Duration::from_secs(5));
    let mut rows = 0;
    for (tt, j, c) in state.store.list_all_capsules() {
        if tt != t {
            continue;
        }
        if let Ok(k) = crate::eventstore::CapsuleKey::new(&tt, &j, &c) {
            rows += state
                .events
                .delete_capsule(&k)
                .map_err(|e| Response::error(500, &e.to_string()))?;
        }
    }
    let removed = state
        .store
        .delete_tenant(t)
        .map_err(|e| Response::error(500, &e))?;
    state.runtimes.invalidate_prefix(t, None);
    if !removed {
        return Err(Response::error(404, &format!("tenant {t} not found")));
    }
    Ok(Response::json(
        200,
        &json!({ "ok": true, "removedRows": rows }),
    ))
}

/// `/ready`: 503 while the store is not writable.
fn ready(state: &State) -> Response {
    let root = state.store.root_path();
    let probe = root.join(".readiness_probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Response::json(200, &json!({ "ok": true, "service": state.service_name }))
        }
        Err(e) => Response::json(
            503,
            &json!({ "ok": false, "service": state.service_name, "reason": format!("store unwritable: {e}") }),
        ),
    }
}
