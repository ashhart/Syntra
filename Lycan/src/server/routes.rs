use crate::store::sha256_hex;
use crate::graph::NeuralGraph;
use crate::capabilities;
use crate::auth_tokens::{Action, Scope};

use super::admin::{admin_html, list_admin_capsules};
use super::auth::{authenticate, authorize_action, rate_limit_check};
use super::decide::do_decide;
use super::errors::{
    Resp, err_json, html_resp, json_resp, ok_json, read_body_bytes_limited,
    read_body_limited, text_resp,
};
use super::feedback::do_feedback;
use super::helpers::{audit_event_json, warn_if_strategy_nodes};
use super::inspect::{do_chaos, do_evaluate, do_evolve, do_report, inspect_graph_json};
use super::metrics::render_metrics;
use super::state::State;

pub(super) fn route(request: &mut tiny_http::Request, state: &State) -> Resp {
    let method = request.method().to_string();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).to_string();

    // Public routes — no auth. /admin serves only the static login shell;
    // every data endpoint it calls still requires the Bearer admin key.
    if path == "/health" {
        return json_resp(200, &serde_json::json!({
            "ok": true,
            "service": state.service_name,
        }).to_string());
    }
    if path == "/ready" {
        // Readiness probe: is the store actually writable right now? Writes
        // a 0-byte file to the store root and deletes it. Returns 503 with
        // a structured reason when the store is unreachable so a load
        // balancer or k8s probe can drain traffic correctly.
        let store_root = state.store.root_path().to_path_buf();
        let probe = store_root.join(".readiness_probe");
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                return json_resp(200, &serde_json::json!({
                    "ok": true,
                    "service": state.service_name,
                    "store": store_root.to_string_lossy(),
                }).to_string());
            }
            Err(e) => {
                return json_resp(503, &serde_json::json!({
                    "ok": false,
                    "service": state.service_name,
                    "store": store_root.to_string_lossy(),
                    "reason": format!("store unwritable: {e}"),
                }).to_string());
            }
        }
    }
    if path == "/metrics" {
        // Public scrape endpoint. Operators control access via the
        // network policy on the listener (or reverse proxy), same posture
        // as /health and /ready.
        let body = render_metrics(state);
        return tiny_http::Response::from_data(body.into_bytes())
            .with_status_code(200)
            .with_header(tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"text/plain; version=0.0.4"[..],
            ).unwrap());
    }
    if path == "/admin" {
        let body = admin_html(&state.service_name);
        return html_resp(200, &body);
    }

    // Auth check — granted_scope + principal_id carry forward for
    // scope-aware routes and the rate limiter.
    let (granted_scope, principal_id): (Scope, Option<String>) = match authenticate(request, state) {
        Ok(outcome) => (outcome.scope(), outcome.principal_id()),
        Err(r) => return r,
    };

    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    match (method.as_str(), segments.as_slice()) {
        // ── Admin: token management ──

        ("POST", ["admin", "tokens"]) => {
            if let Err(r) = authorize_action(&granted_scope, &Action::AdminGlobal) { return r; }
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let json: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
            };
            let scope_val = match json.get("scope") {
                Some(v) => v.clone(),
                None => return json_resp(400, &err_json("scope is required")),
            };
            let new_scope: Scope = match serde_json::from_value(scope_val) {
                Ok(s) => s,
                Err(e) => return json_resp(400, &err_json(&format!("invalid scope: {e}"))),
            };
            let ttl: Option<u64> = json.get("ttlSeconds").and_then(|v| v.as_u64());
            let label: String = json.get("label").and_then(|v| v.as_str())
                .unwrap_or("").to_string();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let mut store = state.tokens.lock().unwrap();
            match store.issue(new_scope.clone(), ttl, label, now) {
                Ok((raw, hash)) => json_resp(200, &serde_json::json!({
                    "token": raw,
                    "hash": hash,
                    "scope": new_scope,
                    "expiresAt": ttl.map(|t| now + t),
                }).to_string()),
                Err(e) => json_resp(500, &err_json(&e)),
            }
        }

        ("DELETE", ["admin", "tokens", token_hash]) => {
            if let Err(r) = authorize_action(&granted_scope, &Action::AdminGlobal) { return r; }
            let mut store = state.tokens.lock().unwrap();
            match store.revoke(token_hash) {
                Ok(true) => json_resp(200, r#"{"ok":true,"revoked":true}"#),
                Ok(false) => json_resp(404, &err_json("token hash not found")),
                Err(e) => json_resp(500, &err_json(&e)),
            }
        }

        ("GET", ["admin", "tokens"]) => {
            if let Err(r) = authorize_action(&granted_scope, &Action::AdminGlobal) { return r; }
            let store = state.tokens.lock().unwrap();
            let list: Vec<serde_json::Value> = store.list().into_iter()
                .map(|(hash, rec)| serde_json::json!({
                    "hash": hash,
                    "scope": rec.scope,
                    "createdAt": rec.created_at,
                    "expiresAt": rec.expires_at,
                    "label": rec.label,
                }))
                .collect();
            json_resp(200, &serde_json::json!({"tokens": list}).to_string())
        }

        // ── Admin: backup / restore ──

        ("POST", ["admin", "backup"]) => {
            if let Err(r) = authorize_action(&granted_scope, &Action::AdminGlobal) { return r; }
            match crate::backup::serialize_store(state.store.root_path()) {
                Ok(body) => tiny_http::Response::from_data(body)
                    .with_status_code(200)
                    .with_header(tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"application/json"[..],
                    ).unwrap())
                    .with_header(tiny_http::Header::from_bytes(
                        &b"Content-Disposition"[..],
                        &b"attachment; filename=\"syntra-backup.json\""[..],
                    ).unwrap()),
                Err(e) => json_resp(500, &err_json(&e)),
            }
        }

        ("POST", ["admin", "restore"]) => {
            if let Err(r) = authorize_action(&granted_scope, &Action::AdminGlobal) { return r; }
            let body = match read_body_bytes_limited(request) {
                Ok(b) => b, Err(r) => return r,
            };
            let root = state.store.root_path().to_path_buf();
            match crate::backup::restore_store(&root, &body) {
                Ok(n) => json_resp(200, &serde_json::json!({
                    "ok": true, "filesRestored": n,
                }).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // ── Admin: capsule listing (dashboard switcher) ──

        ("GET", ["admin", "capsules"]) => {
            if let Err(r) = authorize_action(&granted_scope, &Action::AdminGlobal) { return r; }
            list_admin_capsules(state)
        }

        // ── Read-only routes (no capsule lock) ──

        ("GET", ["capabilities"]) => json_resp(200, &capabilities::json_catalog()),

        ("GET", ["tenants"]) => {
            match state.store.list_tenants() {
                Ok(tenants) => json_resp(200, &serde_json::json!({"tenants": tenants}).to_string()),
                Err(e) => json_resp(500, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules"]) => {
            match state.store.list_capsules(tenant) {
                Ok(caps) => json_resp(200, &serde_json::json!({"tenant": tenant, "capsules": caps}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "report"]) => {
            do_report(state, tenant, "default", capsule)
        }

        // ── Job routes ──

        ("POST", ["tenants", tenant, "jobs"]) => {
            match read_body_limited(request) {
                Ok(body) => {
                    let json: serde_json::Value = match serde_json::from_str(&body) {
                        Ok(v) => v, Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
                    };
                    let id = match json.get("id").and_then(|v| v.as_str()) {
                        Some(s) => s, None => return json_resp(400, &err_json("id is required")),
                    };
                    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    let desc = json.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let meta = json.get("metadata").cloned().unwrap_or(serde_json::json!({}));
                    match state.store.create_job(tenant, id, name, desc, &meta) {
                        Ok(job) => json_resp(200, &serde_json::json!({"ok": true, "tenant": tenant, "job": job}).to_string()),
                        Err(e) if e.contains("already exists") => json_resp(409, &err_json(&e)),
                        Err(e) => json_resp(400, &err_json(&e)),
                    }
                }
                Err(r) => r,
            }
        }

        ("GET", ["tenants", tenant, "jobs"]) => {
            match state.store.list_jobs(tenant) {
                Ok(jobs) => json_resp(200, &serde_json::json!({"tenant": tenant, "jobs": jobs}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "jobs", job]) => {
            match state.store.get_job(tenant, job) {
                Ok(j) => json_resp(200, &serde_json::json!({"tenant": tenant, "job": j}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "jobs", job, "capsules"]) => {
            match state.store.list_capsules_in_job(tenant, job) {
                Ok(caps) => json_resp(200, &serde_json::json!({"tenant": tenant, "job": job, "capsules": caps}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // Job-aware capsule routes
        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "install"]) => {
            if let Err(r) = authorize_action(&granted_scope,
                &Action::CapsuleMutate { tenant, job, capsule }) { return r; }
            let body = match read_body_bytes_limited(request) { Ok(b) => b, Err(r) => return r };
            if body.len() < 4 || body[0] != 0x4C || body[1] != 0x59 || body[2] != 0x43 || body[3] != 0x4E {
                return json_resp(400, r#"{"error":"body must be a .lyc graph binary"}"#);
            }
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            match state.store.install_capsule_bytes_in_job(tenant, job, capsule, &body) {
                Ok(()) => {
                    let hash = sha256_hex(&body);
                    warn_if_strategy_nodes(tenant, job, capsule, &body);
                    state.store.append_audit_in_job(tenant, job, capsule,
                        &audit_event_json("install", tenant, job, capsule, serde_json::json!({"hash": hash}))).ok();
                    json_resp(200, &ok_json(serde_json::json!({"tenant": tenant, "job": job, "capsule": capsule, "hash": hash})))
                }
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "decide"]) => {
            if let Err(r) = authorize_action(&granted_scope,
                &Action::CapsuleDecide { tenant, job, capsule }) { return r; }
            if let Some(r) = rate_limit_check(state, principal_id.as_deref()) { return r; }
            let learn = url.contains("learn=true");
            match read_body_limited(request) {
                Ok(body) => {
                    let t0 = std::time::Instant::now();
                    let resp = if learn { let lock = state.locks.get(tenant, job, capsule); let _guard = lock.lock().unwrap(); do_decide(state, tenant, job, capsule, &body, true) }
                    else { do_decide(state, tenant, job, capsule, &body, false) };
                    state.metrics.observe_decide_latency(t0.elapsed().as_secs_f64());
                    let status = if resp.status_code().0 >= 400 { "err" } else { "ok" };
                    state.metrics.record_request("decide", tenant, job, capsule, status);
                    resp
                }
                Err(r) => r,
            }
        }

        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "feedback"]) => {
            if let Err(r) = authorize_action(&granted_scope,
                &Action::CapsuleMutate { tenant, job, capsule }) { return r; }
            if let Some(r) = rate_limit_check(state, principal_id.as_deref()) { return r; }
            match read_body_limited(request) {
                Ok(body) => {
                    let lock = state.locks.get(tenant, job, capsule);
                    let _guard = lock.lock().unwrap();
                    let resp = do_feedback(state, tenant, job, capsule, &body);
                    let status = if resp.status_code().0 >= 400 { "err" } else { "ok" };
                    state.metrics.record_request("feedback", tenant, job, capsule, status);
                    resp
                }
                Err(r) => r,
            }
        }

        // 2B: batched feedback. Single rate-limit hit per batch, single
        // per-capsule lock for the whole batch. Each event is processed
        // sequentially under the same lock so order is preserved. Per-event
        // failure does not abort the batch — results are returned in input
        // order with per-event ok/err shape.
        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "feedback", "batch"]) => {
            if let Err(r) = authorize_action(&granted_scope,
                &Action::CapsuleMutate { tenant, job, capsule }) { return r; }
            if let Some(r) = rate_limit_check(state, principal_id.as_deref()) { return r; }
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let json: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
            };
            let events: &Vec<serde_json::Value> = match json.get("events").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => return json_resp(400, &err_json("expected JSON object with 'events' array")),
            };
            if events.len() > 10_000 {
                return json_resp(400, &err_json("batch size limit: 10000 events"));
            }
            // Hold the per-capsule lock for the whole batch so we observe
            // a consistent capsule state across all events.
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            let mut results = Vec::with_capacity(events.len());
            let mut ok_count = 0usize;
            let mut err_count = 0usize;
            for ev in events {
                let ev_body = ev.to_string();
                let resp = do_feedback(state, tenant, job, capsule, &ev_body);
                let status_code = resp.status_code().0;
                let is_ok = status_code < 400;
                if is_ok { ok_count += 1; } else { err_count += 1; }
                // We can't easily reflect the per-event response body back to
                // the caller because tiny_http::Response doesn't expose its
                // buffered body. The status code alone is the contract — a
                // caller wanting the full per-event diagnostic should retry
                // the failed event individually via /feedback (single).
                let mut entry = serde_json::json!({
                    "ok": is_ok,
                    "status": status_code,
                });
                if let Some(did) = ev.get("decisionId").and_then(|v| v.as_str()) {
                    entry.as_object_mut().map(|m| m.insert(
                        "decisionId".into(), serde_json::json!(did)));
                }
                results.push(entry);
            }
            state.metrics.record_request("feedback_batch", tenant, job, capsule,
                if err_count == 0 { "ok" } else { "partial" });
            json_resp(200, &serde_json::json!({
                "ok": err_count == 0,
                "total": events.len(),
                "okCount": ok_count,
                "errCount": err_count,
                "results": results,
            }).to_string())
        }

        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "report"]) => do_report(state, tenant, job, capsule),
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "decisions"]) => {
            match state.store.read_decision_log_in_job(tenant, job, capsule) { Ok(d) => text_resp(200, &d), Err(e) => json_resp(400, &err_json(&e)) }
        }
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "audits"]) => {
            match state.store.read_audits_in_job(tenant, job, capsule) { Ok(d) => text_resp(200, &d), Err(e) => json_resp(400, &err_json(&e)) }
        }
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "evolution"]) => {
            match state.store.read_evolution_log_in_job(tenant, job, capsule) { Ok(d) => text_resp(200, &d), Err(e) => json_resp(400, &err_json(&e)) }
        }
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "snapshots"]) => {
            match state.store.list_snapshots_in_job(tenant, job, capsule) { Ok(s) => json_resp(200, &serde_json::json!({"snapshots": s}).to_string()), Err(e) => json_resp(400, &err_json(&e)) }
        }
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "policy"]) => {
            match state.store.load_policy_json_in_job(tenant, job, capsule) { Ok(j) => json_resp(200, &j), Err(e) => json_resp(400, &err_json(&e)) }
        }
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "inspect"]) => {
            match state.store.load_graph_in_job(tenant, job, capsule) {
                Ok(data) => match NeuralGraph::from_bytes(&data) {
                    Ok(ng) => json_resp(200, &inspect_graph_json(tenant, job, capsule, &data, &ng, state)),
                    Err(e) => json_resp(500, &err_json(&e)),
                }
                Err(e) => json_resp(404, &err_json(&e)),
            }
        }
        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "evolve"]) => {
            match read_body_limited(request) { Ok(body) => { let lock = state.locks.get(tenant, job, capsule); let _guard = lock.lock().unwrap(); do_evolve(state, tenant, job, capsule, &body) } Err(r) => r }
        }
        ("PUT", ["tenants", tenant, "jobs", job, "capsules", capsule, "policy"]) => {
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let parsed: serde_json::Value = match serde_json::from_str(&body) { Ok(v) => v, Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))) };
            if !parsed.is_object() { return json_resp(400, &err_json("policy must be a JSON object")); }
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            match state.store.save_policy_json_in_job(tenant, job, capsule, &body) { Ok(()) => json_resp(200, r#"{"ok":true}"#), Err(e) => json_resp(400, &err_json(&e)) }
        }

        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "reward_spec"]) => {
            match state.store.load_reward_spec_in_job(tenant, job, capsule) {
                Some(spec) => json_resp(200, &spec.to_string()),
                None => json_resp(404, &err_json("no reward_spec installed for this capsule")),
            }
        }
        ("PUT", ["tenants", tenant, "jobs", job, "capsules", capsule, "reward_spec"]) => {
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let parsed: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
            };
            if !parsed.is_object() {
                return json_resp(400, &err_json("reward_spec must be a JSON object"));
            }
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            match state.store.save_reward_spec_in_job(tenant, job, capsule, &parsed) {
                Ok(()) => json_resp(200, r#"{"ok":true}"#),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // Learning config
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "learning"]) => {
            let cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
            json_resp(200, &cfg.to_json().to_string())
        }
        ("PUT", ["tenants", tenant, "jobs", job, "capsules", capsule, "learning"]) => {
            if let Err(r) = authorize_action(&granted_scope,
                &Action::CapsuleMutate { tenant, job, capsule }) { return r; }
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let json: serde_json::Value = match serde_json::from_str(&body) { Ok(v) => v, Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))) };
            if !json.is_object() { return json_resp(400, &err_json("learning config must be a JSON object")); }
            let cfg = crate::learning::LearningConfig::from_json(&json);
            match state.store.save_learning_config_in_job(tenant, job, capsule, &cfg) {
                Ok(()) => json_resp(200, &serde_json::json!({"ok": true, "config": cfg.to_json()}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // Hierarchical spec sidecar (roadmap.md step 3 install hookup).
        // The capsule compiler writes hierarchical_spec.json next to the
        // compile-output `.lyc`; this endpoint is the upload path that
        // gets it into the runtime store so do_decide_hierarchical can
        // load it. Same auth scope as PUT /learning.
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "hierarchical_spec"]) => {
            match state.store.load_hierarchical_spec_in_job(tenant, job, capsule) {
                Some(s) => json_resp(200, &s.to_json().to_string()),
                None => json_resp(404, &err_json("no hierarchical_spec for this capsule")),
            }
        }
        ("PUT", ["tenants", tenant, "jobs", job, "capsules", capsule, "hierarchical_spec"]) => {
            if let Err(r) = authorize_action(&granted_scope,
                &Action::CapsuleMutate { tenant, job, capsule }) { return r; }
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let json: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
            };
            let spec = match crate::hierarchical::HierarchicalSpec::from_json(&json) {
                Ok(s) => s,
                Err(e) => return json_resp(400, &err_json(&e)),
            };
            match state.store.save_hierarchical_spec_in_job(tenant, job, capsule, &spec) {
                Ok(()) => json_resp(200, &serde_json::json!({
                    "ok": true,
                    "leaves": spec.count_leaves(),
                    "depth": spec.max_depth(),
                }).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // Contexts
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "contexts"]) => {
            let memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();
            let mut contexts = Vec::new();
            for (nid, sm) in &memory.strategies {
                for (ctx_key, bucket) in &sm.contexts {
                    let total_tries: u64 = bucket.stats.iter().map(|s| s.tries).sum();
                    contexts.push(serde_json::json!({
                        "nodeId": nid,
                        "contextKey": ctx_key,
                        "totalTries": total_tries,
                        "weights": bucket.weights,
                        "updatedAt": bucket.updated_at,
                    }));
                }
            }
            json_resp(200, &serde_json::json!({"tenant": tenant, "job": job, "capsule": capsule, "contexts": contexts}).to_string())
        }

        // Memory sidecar
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "memory"]) => {
            let memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();
            json_resp(200, &memory.to_json().to_string())
        }

        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "chaos"]) => {
            do_chaos(state, tenant, job, capsule)
        }

        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "evaluate"]) => {
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            do_evaluate(state, tenant, job, capsule, &body)
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "inspect"]) => {
            match state.store.load_graph(tenant, capsule) {
                Ok(data) => match NeuralGraph::from_bytes(&data) {
                    Ok(ng) => json_resp(200, &inspect_graph_json(tenant, "default", capsule, &data, &ng, state)),
                    Err(e) => json_resp(500, &err_json(&e)),
                }
                Err(e) => json_resp(404, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "decisions"]) => {
            match state.store.read_decision_log(tenant, capsule) {
                Ok(data) => text_resp(200, &data),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "audits"]) => {
            match state.store.read_audits(tenant, capsule) {
                Ok(data) => text_resp(200, &data),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "evolution"]) => {
            match state.store.read_evolution_log(tenant, capsule) {
                Ok(data) => text_resp(200, &data),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "snapshots"]) => {
            match state.store.list_snapshots(tenant, capsule) {
                Ok(snaps) => json_resp(200, &serde_json::json!({"snapshots": snaps}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("GET", ["tenants", tenant, "capsules", capsule, "policy"]) => {
            match state.store.load_policy_json(tenant, capsule) {
                Ok(json) => json_resp(200, &json),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // ── Mutation routes (per-capsule lock) ──

        ("POST", ["tenants", tenant, "capsules", capsule, "install"]) => {
            let body = match read_body_bytes_limited(request) {
                Ok(b) => b,
                Err(r) => return r,
            };
            if body.len() < 4 || body[0] != 0x4C || body[1] != 0x59 || body[2] != 0x43 || body[3] != 0x4E {
                return json_resp(400, r#"{"error":"body must be a .lyc graph binary"}"#);
            }
            let lock = state.locks.get(tenant, "default", capsule);
            let _guard = lock.lock().unwrap();
            match state.store.install_capsule_bytes(tenant, capsule, &body) {
                Ok(()) => {
                    let hash = sha256_hex(&body);
                    warn_if_strategy_nodes(tenant, "default", capsule, &body);
                    state.store.append_audit(tenant, capsule,
                        &audit_event_json("install", tenant, "default", capsule, serde_json::json!({"hash": hash}))).ok();
                    json_resp(200, &ok_json(serde_json::json!({"tenant": tenant, "capsule": capsule, "hash": hash})))
                }
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("POST", ["tenants", tenant, "capsules", capsule, "decide"]) => {
            let learn = url.contains("learn=true");
            match read_body_limited(request) {
                Ok(body) => {
                    if learn {
                        let lock = state.locks.get(tenant, "default", capsule);
                        let _guard = lock.lock().unwrap();
                        do_decide(state, tenant, "default", capsule, &body, true)
                    } else {
                        do_decide(state, tenant, "default", capsule, &body, false)
                    }
                }
                Err(r) => r,
            }
        }

        ("POST", ["tenants", tenant, "capsules", capsule, "feedback"]) => {
            match read_body_limited(request) {
                Ok(body) => {
                    let lock = state.locks.get(tenant, "default", capsule);
                    let _guard = lock.lock().unwrap();
                    do_feedback(state, tenant, "default", capsule, &body)
                }
                Err(r) => r,
            }
        }

        ("POST", ["tenants", tenant, "capsules", capsule, "evolve"]) => {
            match read_body_limited(request) {
                Ok(body) => {
                    let lock = state.locks.get(tenant, "default", capsule);
                    let _guard = lock.lock().unwrap();
                    do_evolve(state, tenant, "default", capsule, &body)
                }
                Err(r) => r,
            }
        }

        ("PUT", ["tenants", tenant, "capsules", capsule, "policy"]) => {
            let body = match read_body_limited(request) {
                Ok(b) => b,
                Err(r) => return r,
            };
            let parsed: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => return json_resp(400, &err_json(&format!("invalid policy JSON: {e}"))),
            };
            if !parsed.is_object() {
                return json_resp(400, &err_json("policy must be a JSON object"));
            }
            for field in ["allow_stdout", "allow_stdin", "allow_file_read", "allow_file_write", "allow_network"] {
                if let Some(v) = parsed.get(field) {
                    if !v.is_boolean() {
                        return json_resp(400, &err_json(&format!("policy.{field} must be boolean")));
                    }
                }
            }
            let lock = state.locks.get(tenant, "default", capsule);
            let _guard = lock.lock().unwrap();
            match state.store.save_policy_json(tenant, capsule, &body) {
                Ok(()) => json_resp(200, r#"{"ok":true}"#),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        // ── DELETE routes (data erasure / GDPR Art.17) ──

        ("DELETE", ["tenants", tenant, "jobs", job, "capsules", capsule]) => {
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            match state.store.delete_capsule_in_job(tenant, job, capsule) {
                Ok(()) => json_resp(200, &serde_json::json!({"ok": true, "deleted": "capsule"}).to_string()),
                Err(e) => json_resp(404, &err_json(&e)),
            }
        }
        ("DELETE", ["tenants", tenant, "jobs", job, "capsules", capsule, "logs"]) => {
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            match state.store.purge_logs_in_job(tenant, job, capsule) {
                Ok(n) => json_resp(200, &serde_json::json!({"ok": true, "purged": n}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }
        ("DELETE", ["tenants", tenant, "jobs", job]) => {
            match state.store.delete_job(tenant, job) {
                Ok(()) => json_resp(200, &serde_json::json!({"ok": true, "deleted": "job"}).to_string()),
                Err(e) => json_resp(404, &err_json(&e)),
            }
        }
        ("DELETE", ["tenants", tenant]) => {
            match state.store.delete_tenant(tenant) {
                Ok(()) => json_resp(200, &serde_json::json!({"ok": true, "deleted": "tenant"}).to_string()),
                Err(e) => json_resp(404, &err_json(&e)),
            }
        }
        ("DELETE", ["tenants", tenant, "capsules", capsule]) => {
            match state.store.delete_capsule(tenant, capsule) {
                Ok(()) => json_resp(200, &serde_json::json!({"ok": true, "deleted": "capsule"}).to_string()),
                Err(e) => json_resp(404, &err_json(&e)),
            }
        }
        ("DELETE", ["tenants", tenant, "capsules", capsule, "logs"]) => {
            match state.store.purge_logs_in_job(tenant, "default", capsule) {
                Ok(n) => json_resp(200, &serde_json::json!({"ok": true, "purged": n}).to_string()),
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        _ => json_resp(404, r#"{"error":"not_found"}"#),
    }
}
