/// Lycan HTTP server — `lycan serve`
///
/// Concurrent request handling with per tenant/job/capsule locking.
/// Read-only routes never block. Mutation routes serialize per runtime scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::io::{Read as IoRead, Cursor};

use crate::store::{LycanStore, sha256_hex};
use crate::graph::{NeuralGraph, OpCode, Contract, Objective, Operand};
use crate::graph_executor::GraphExecutor;
use crate::context::ExecutionContext;
use crate::capabilities;
use crate::verifier;

pub struct ServerConfig {
    pub addr: String,
    pub store_path: String,
    pub admin_key: Option<String>,
    pub service_name: Option<String>,
}

/// Per-runtime lock manager. The global map lock is only held to retrieve
/// or create a scoped mutex — never during request execution.
struct CapsuleLockManager {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl CapsuleLockManager {
    fn new() -> Self {
        Self { locks: Mutex::new(HashMap::new()) }
    }

    fn get(&self, tenant: &str, job: &str, capsule: &str) -> Arc<Mutex<()>> {
        let key = format!("{tenant}/{job}/{capsule}");
        let mut map = self.locks.lock().unwrap();
        map.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
    }
}

/// Shared server state — no global mutex around the store.
struct SharedState {
    store: LycanStore,
    admin_key: Option<String>,
    service_name: String,
    locks: CapsuleLockManager,
}

type State = Arc<SharedState>;
type Resp = tiny_http::Response<Cursor<Vec<u8>>>;

fn json_resp(status: u16, body: &str) -> Resp {
    tiny_http::Response::from_data(body.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn text_resp(status: u16, body: &str) -> Resp {
    tiny_http::Response::from_data(body.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..]).unwrap())
}

fn html_resp(status: u16, body: &str) -> Resp {
    tiny_http::Response::from_data(body.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap())
}

fn err_json(msg: &str) -> String {
    serde_json::json!({"error": msg}).to_string()
}

fn ok_json(fields: serde_json::Value) -> String {
    let mut m = match fields {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    m.insert("ok".to_string(), serde_json::Value::Bool(true));
    serde_json::Value::Object(m).to_string()
}

fn audit_event_json(action: &str, tenant: &str, job: &str, capsule: &str, extra: serde_json::Value) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs();
    let mut m = serde_json::Map::new();
    m.insert("action".into(), serde_json::json!(action));
    m.insert("tenant".into(), serde_json::json!(tenant));
    m.insert("job".into(), serde_json::json!(job));
    m.insert("capsule".into(), serde_json::json!(capsule));
    m.insert("timestamp".into(), serde_json::json!(ts));
    if let serde_json::Value::Object(extra_map) = extra {
        for (k, v) in extra_map { m.insert(k, v); }
    }
    serde_json::Value::Object(m).to_string()
}

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const WORKER_THREADS: usize = 8;

pub fn run_server(config: ServerConfig) {
    let store = LycanStore::open_or_init(&config.store_path)
        .unwrap_or_else(|e| { eprintln!("cannot open store: {e}"); std::process::exit(1); });

    let state: State = Arc::new(SharedState {
        admin_key: config.admin_key,
        service_name: config.service_name.unwrap_or_else(|| "Lycan".to_string()),
        store,
        locks: CapsuleLockManager::new(),
    });

    if state.admin_key.is_none() {
        eprintln!("WARNING: no admin key set — all routes are unauthenticated");
        eprintln!("  set LYCAN_ADMIN_KEY or use --admin-key");
    }

    let server = Arc::new(tiny_http::Server::http(&config.addr)
        .unwrap_or_else(|e| { eprintln!("cannot bind {}: {e}", config.addr); std::process::exit(1); }));

    eprintln!("lycan serve listening on http://{}", config.addr);
    eprintln!("store: {}", config.store_path);
    eprintln!("workers: {WORKER_THREADS}");
    eprintln!("admin: http://{}/admin", config.addr);

    // Spawn worker threads
    let mut handles = Vec::new();
    for _ in 0..WORKER_THREADS {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        handles.push(std::thread::spawn(move || {
            loop {
                let mut request = match server.recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let resp = route(&mut request, &state);
                request.respond(resp).ok();
            }
        }));
    }

    for h in handles {
        h.join().ok();
    }
}

/// Constant-time byte comparison — prevents timing side-channel on key.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn check_auth(request: &tiny_http::Request, state: &SharedState) -> Option<Resp> {
    if let Some(ref key) = state.admin_key {
        let auth = request.headers().iter()
            .find(|h| h.field.as_str().to_ascii_lowercase() == "authorization")
            .map(|h| h.value.as_str().to_string());
        let expected = format!("Bearer {key}");
        match auth {
            Some(ref val) if constant_time_eq(val.as_bytes(), expected.as_bytes()) => None,
            _ => {
                let method = request.method().to_string();
                let url = request.url().to_string();
                let remote = request.remote_addr().map(|a| a.to_string()).unwrap_or_else(|| "unknown".into());
                eprintln!("AUTH_FAIL remote={remote} method={method} url={url}");
                Some(json_resp(401, r#"{"error":"unauthorized"}"#))
            }
        }
    } else {
        None
    }
}

fn route(request: &mut tiny_http::Request, state: &State) -> Resp {
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
    if path == "/admin" {
        return html_resp(200, ADMIN_HTML);
    }

    // Auth check
    if let Some(r) = check_auth(request, state) {
        return r;
    }

    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    match (method.as_str(), segments.as_slice()) {
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
            let body = match read_body_bytes_limited(request) { Ok(b) => b, Err(r) => return r };
            if body.len() < 4 || body[0] != 0x4C || body[1] != 0x59 || body[2] != 0x43 || body[3] != 0x4E {
                return json_resp(400, r#"{"error":"body must be a .lyc graph binary"}"#);
            }
            let lock = state.locks.get(tenant, job, capsule);
            let _guard = lock.lock().unwrap();
            match state.store.install_capsule_bytes_in_job(tenant, job, capsule, &body) {
                Ok(()) => {
                    let hash = sha256_hex(&body);
                    state.store.append_audit_in_job(tenant, job, capsule,
                        &audit_event_json("install", tenant, job, capsule, serde_json::json!({"hash": hash}))).ok();
                    json_resp(200, &ok_json(serde_json::json!({"tenant": tenant, "job": job, "capsule": capsule, "hash": hash})))
                }
                Err(e) => json_resp(400, &err_json(&e)),
            }
        }

        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "decide"]) => {
            let learn = url.contains("learn=true");
            match read_body_limited(request) {
                Ok(body) => {
                    if learn { let lock = state.locks.get(tenant, job, capsule); let _guard = lock.lock().unwrap(); do_decide(state, tenant, job, capsule, &body, true) }
                    else { do_decide(state, tenant, job, capsule, &body, false) }
                }
                Err(r) => r,
            }
        }

        ("POST", ["tenants", tenant, "jobs", job, "capsules", capsule, "feedback"]) => {
            match read_body_limited(request) {
                Ok(body) => { let lock = state.locks.get(tenant, job, capsule); let _guard = lock.lock().unwrap(); do_feedback(state, tenant, job, capsule, &body) }
                Err(r) => r,
            }
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
                    Ok(ng) => json_resp(200, &inspect_graph_json(tenant, job, capsule, &data, &ng)),
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

        // Learning config
        ("GET", ["tenants", tenant, "jobs", job, "capsules", capsule, "learning"]) => {
            let cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
            json_resp(200, &cfg.to_json().to_string())
        }
        ("PUT", ["tenants", tenant, "jobs", job, "capsules", capsule, "learning"]) => {
            let body = match read_body_limited(request) { Ok(b) => b, Err(r) => return r };
            let json: serde_json::Value = match serde_json::from_str(&body) { Ok(v) => v, Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))) };
            if !json.is_object() { return json_resp(400, &err_json("learning config must be a JSON object")); }
            let cfg = crate::learning::LearningConfig::from_json(&json);
            match state.store.save_learning_config_in_job(tenant, job, capsule, &cfg) {
                Ok(()) => json_resp(200, &serde_json::json!({"ok": true, "config": cfg.to_json()}).to_string()),
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

        ("GET", ["tenants", tenant, "capsules", capsule, "inspect"]) => {
            match state.store.load_graph(tenant, capsule) {
                Ok(data) => match NeuralGraph::from_bytes(&data) {
                    Ok(ng) => json_resp(200, &inspect_graph_json(tenant, "default", capsule, &data, &ng)),
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

// ── Core handlers (called with per-capsule lock held) ──

fn do_decide(state: &State, tenant: &str, job: &str, capsule: &str, body: &str, learn: bool) -> Resp {
    let data = match state.store.load_graph_in_job(tenant, job, capsule) {
        Ok(d) => d,
        Err(e) => return json_resp(404, &err_json(&e)),
    };
    let graph_hash = sha256_hex(&data);
    let mut ng = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => return json_resp(500, &err_json(&e)),
    };
    if let Err(e) = verifier::verify(&ng) {
        return json_resp(500, &err_json(&format!("{e}")));
    }

    // Load capsule policy — fail closed
    let policy = match state.store.load_execution_policy_in_job(tenant, job, capsule) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("policy load failed for {tenant}/{job}/{capsule}: {e} — denying all");
            Some(crate::context::ExecutionPolicy {
                allow_stdout: false, allow_stdin: false,
                allow_file_read: false, allow_file_write: false, allow_network: false,
                file_root: None, allowed_hosts: vec![], deny_private_networks: true,
            })
        }
    };

    // Parse input and extract contextKey
    let body_json: Option<serde_json::Value> = if !body.trim().is_empty() {
        serde_json::from_str(body).ok()
    } else {
        None
    };
    let context_key = body_json.as_ref()
        .and_then(|j| j.get("contextKey"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let input = body_json.as_ref().map(|v| {
        // If there's an "input" field, use that; otherwise use the whole body
        if let Some(inp) = v.get("input") {
            capabilities::CapValue::from_json(inp)
        } else {
            capabilities::CapValue::from_json(v)
        }
    });

    // Load learning config and memory before execution so context-specific
    // weights can drive the live decision, not just decorate the response.
    let learning_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    let memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();
    apply_context_memory_to_graph(&mut ng, &memory, context_key);

    let working_dir = state.store.capsule_dir_in_job(tenant, job, capsule).ok();
    let ctx = ExecutionContext { policy, input, working_dir };
    let mut executor = GraphExecutor::new_with_context(ng, ctx);
    let result = match executor.run() {
        Ok(val) => format!("{val}"),
        Err(e) => return json_resp(500, &err_json(&format!("{e}"))),
    };

    let stdout_lines = executor.stdout_buffer.clone();
    let graph = executor.into_graph();
    let decisions = extract_decisions(&graph);

    // Enrich decisions with context-specific weights from memory
    let enriched_decisions: Vec<serde_json::Value> = decisions.iter().map(|d| {
        let mut ed = d.clone();
        if let Some(nid) = d.get("node_id").and_then(|v| v.as_u64()) {
            if let Some(sm) = memory.strategies.get(&(nid as u32)) {
                if let Some(bucket) = sm.contexts.get(context_key) {
                    ed.as_object_mut().map(|m| {
                        m.insert("contextKey".into(), serde_json::json!(context_key));
                        m.insert("contextWeights".into(), serde_json::json!(bucket.weights));
                        let stats: Vec<serde_json::Value> = bucket.stats.iter().map(|s| s.to_json()).collect();
                        m.insert("contextStats".into(), serde_json::json!(stats));
                    });
                }
            }
        }
        ed
    }).collect();

    // Add algorithm info
    let alg_str = match &learning_cfg.algorithm {
        crate::learning::Algorithm::SimpleWeighted => "simpleWeighted",
        crate::learning::Algorithm::EpsilonGreedy { .. } => "epsilonGreedy",
        crate::learning::Algorithm::Ucb1 => "ucb1",
        crate::learning::Algorithm::ThompsonSampling => "thompsonSampling",
        crate::learning::Algorithm::Softmax { .. } => "softmax",
    };

    let decision_id = format!("dec_{}", sha256_hex(
        format!("{}{}{}{}", tenant, job, capsule, std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        ).as_bytes()
    ).get(..16).unwrap_or(""));

    let decision_event = serde_json::json!({
        "id": decision_id,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "contextKey": context_key,
        "algorithm": alg_str,
        "inputSha256": sha256_hex(body.as_bytes()).get(..16).unwrap_or(""),
        "graphHash": graph_hash.get(..16).unwrap_or(""),
        "learned": learn,
        "decisions": enriched_decisions,
    });
    state.store.append_decision_log_in_job(tenant, job, capsule, &decision_event.to_string()).ok();

    if learn {
        let updated_bytes = graph.to_bytes();
        let after_hash = sha256_hex(&updated_bytes);
        if graph_hash != after_hash {
            state.store.snapshot_in_job(tenant, job, capsule).ok();
        }
        state.store.save_graph_in_job(tenant, job, capsule, &updated_bytes).ok();

        state.store.append_audit_in_job(tenant, job, capsule,
            &audit_event_json("decide", tenant, job, capsule, serde_json::json!({
                "decisionId": decision_id, "learned": true,
                "beforeHash": graph_hash, "afterHash": after_hash,
            }))).ok();
    } else {
        state.store.append_audit_in_job(tenant, job, capsule,
            &audit_event_json("decide", tenant, job, capsule, serde_json::json!({
                "decisionId": decision_id, "learned": false, "graphHash": graph_hash,
            }))).ok();
    }

    json_resp(200, &serde_json::json!({
        "ok": true,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "decisionId": decision_id,
        "contextKey": context_key,
        "algorithm": alg_str,
        "learned": learn,
        "decisions": enriched_decisions,
        "result": result,
        "stdout": stdout_lines,
    }).to_string())
}

fn do_feedback(state: &State, tenant: &str, job: &str, capsule: &str, body: &str) -> Resp {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
    };

    let explicit_context_key = json.get("contextKey").and_then(|v| v.as_str()).map(|s| s.to_string());
    let mut context_key = explicit_context_key.clone().unwrap_or_else(|| "default".to_string());

    // Compute reward: explicit reward, or compute from outcome
    let learning_cfg = state.store.load_learning_config_in_job(tenant, job, capsule);
    let reward = if let Some(r) = json.get("reward").and_then(|v| v.as_f64()) {
        r
    } else if let Some(outcome) = json.get("outcome") {
        if let Some(ref rp) = learning_cfg.reward_policy {
            crate::learning::compute_reward(outcome, rp)
        } else {
            // No reward policy, try to extract "success" as simple reward
            outcome.get("success").and_then(|v| v.as_bool())
                .map(|b| if b { 1.0 } else { -1.0 })
                .unwrap_or(0.0)
        }
    } else {
        return json_resp(400, &err_json("reward or outcome is required"));
    };

    // Support two modes: explicit strategyId+option, or decisionId lookup
    let (node_id, option) = if let Some(dec_id) = json.get("decisionId").and_then(|v| v.as_str()) {
        // Look up the decision event to find strategyId and selected option
        match state.store.find_decision_in_job(tenant, job, capsule, dec_id) {
            Ok(Some(event_line)) => {
                // Parse the decision event to extract strategy info
                match serde_json::from_str::<serde_json::Value>(&event_line) {
                    Ok(ev) => {
                        if explicit_context_key.is_none() {
                            if let Some(ev_context) = ev.get("contextKey").and_then(|v| v.as_str()) {
                                context_key = ev_context.to_string();
                            }
                        }
                        let decisions = ev.get("decisions").and_then(|d| d.as_array());
                        match decisions.and_then(|d| d.first()) {
                            Some(dec) => {
                                let nid = dec.get("node_id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                                let opt = dec.get("chosen_option").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                (nid, opt)
                            }
                            None => return json_resp(400, &err_json("decision event has no strategy decisions")),
                        }
                    }
                    Err(_) => return json_resp(500, &err_json("cannot parse decision event")),
                }
            }
            Ok(None) => return json_resp(404, &err_json(&format!("decisionId not found: {dec_id}"))),
            Err(e) => return json_resp(500, &err_json(&e)),
        }
    } else {
        // Explicit mode
        let nid = match json.get("strategyId").or(json.get("nodeId")).and_then(|v| v.as_u64()) {
            Some(id) => id as u32,
            None => return json_resp(400, &err_json("strategyId/nodeId or decisionId is required")),
        };
        let opt = match json.get("option").and_then(|v| v.as_u64()) {
            Some(o) => o as usize,
            None => return json_resp(400, &err_json("option is required when using strategyId")),
        };
        (nid, opt)
    };

    let data = match state.store.load_graph_in_job(tenant, job, capsule) {
        Ok(d) => d,
        Err(e) => return json_resp(404, &err_json(&e)),
    };

    let mut ng = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => return json_resp(500, &err_json(&e)),
    };

    let node = match ng.nodes.get(node_id as usize) {
        Some(n) if matches!(n.op, OpCode::Strategy | OpCode::AdaptiveChoice) => n,
        _ => return json_resp(400, &err_json(&format!("node {node_id} is not a strategy node"))),
    };

    let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
        node.weights.len() - 1
    } else {
        node.weights.len()
    };

    if option >= n_options {
        return json_resp(400, &err_json(&format!("option {option} out of range ({n_options} options)")));
    }

    let before: Vec<f64> = ng.nodes[node_id as usize].weights[..n_options].to_vec();

    let learning_rate = 0.05;
    let delta = reward * learning_rate;
    for j in 0..n_options {
        if j == option {
            ng.nodes[node_id as usize].weights[j] =
                (ng.nodes[node_id as usize].weights[j] + delta).clamp(0.01, 0.99);
        } else if n_options > 1 {
            ng.nodes[node_id as usize].weights[j] =
                (ng.nodes[node_id as usize].weights[j] - delta / (n_options - 1) as f64).clamp(0.01, 0.99);
        }
    }
    let sum: f64 = ng.nodes[node_id as usize].weights[..n_options].iter().sum();
    if sum > 0.0 {
        for j in 0..n_options { ng.nodes[node_id as usize].weights[j] /= sum; }
    }

    if let Some(slot) = ng.nodes[node_id as usize].state_slot {
        let base = slot as usize + option * 3;
        if base + 2 < ng.state.len() {
            ng.state[base] += 1.0;
            if reward > 0.0 { ng.state[base + 2] += 1.0; }
        }
    }

    ng.journal.push(crate::graph::JournalEntry {
        run_number: ng.nodes.get(ng.entry as usize).map(|n| n.activation_count).unwrap_or(0),
        node_id,
        mutation: crate::graph::MutationKind::FeedbackReceived,
        reason: u32::MAX,
    });

    let after: Vec<f64> = ng.nodes[node_id as usize].weights[..n_options].to_vec();
    let updated = ng.to_bytes();
    state.store.snapshot_in_job(tenant, job, capsule).ok();
    state.store.save_graph_in_job(tenant, job, capsule, &updated).ok();

    state.store.append_audit_in_job(tenant, job, capsule,
        &audit_event_json("feedback", tenant, job, capsule, serde_json::json!({
            "nodeId": node_id, "option": option, "reward": reward,
        }))).ok();
    state.store.append_feedback_log_in_job(tenant, job, capsule,
        &serde_json::json!({
            "tenant": tenant,
            "job": job,
            "capsule": capsule,
            "nodeId": node_id,
            "option": option,
            "reward": reward,
        }).to_string()).ok();

    // Update memory sidecar with context-aware feedback
    let mut memory = state.store.load_memory_in_job(tenant, job, capsule).unwrap_or_default();
    let graph_weights = &after;
    let bucket = memory.get_or_init_context(node_id, &context_key, graph_weights, n_options);
    if let Err(e) = crate::learning::apply_feedback(bucket, option, reward, &learning_cfg) {
        eprintln!("learning feedback warning: {e}");
    }
    state.store.save_memory_in_job(tenant, job, capsule, &memory).ok();

    json_resp(200, &serde_json::json!({
        "ok": true,
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "nodeId": node_id,
        "option": option,
        "reward": reward,
        "before": before,
        "after": after,
        "contextKey": context_key,
    }).to_string())
}

fn do_evolve(state: &State, tenant: &str, job: &str, capsule: &str, body: &str) -> Resp {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_resp(400, &err_json(&format!("invalid JSON: {e}"))),
    };

    if json.get("agentCommand").is_some() || json.get("agent_command").is_some() {
        return json_resp(400, &err_json("agent-command is not allowed over HTTP — use CLI for agent mode"));
    }

    let proposal = match json.get("proposal") {
        Some(p) => p.to_string(),
        None => return json_resp(400, &err_json("proposal field required for server evolution")),
    };
    let dry_run = json.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(false);
    let min_improvement = json.get("minImprovement").and_then(|v| v.as_f64()).unwrap_or(0.05);

    let graph_path = match state.store.graph_path_in_job(tenant, job, capsule) {
        Ok(p) => p,
        Err(e) => return json_resp(404, &err_json(&e)),
    };

    if !graph_path.exists() {
        return json_resp(404, &err_json("capsule not found"));
    }

    let graph_path_str = graph_path.to_string_lossy().to_string();
    let tmp_proposal = format!("/tmp/lycan_evolve_server_{}_{:?}.json",
        std::process::id(), std::thread::current().id());
    if std::fs::write(&tmp_proposal, &proposal).is_err() {
        return json_resp(500, &err_json("cannot write temp proposal"));
    }

    // Load capsule policy — fail closed
    let policy = match state.store.load_execution_policy_in_job(tenant, job, capsule) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("policy load failed for {tenant}/{job}/{capsule}: {e} — denying all");
            Some(crate::context::ExecutionPolicy {
                allow_stdout: false, allow_stdin: false,
                allow_file_read: false, allow_file_write: false, allow_network: false,
                file_root: None, allowed_hosts: vec![], deny_private_networks: true,
            })
        }
    };

    let config = crate::evolution_loop::EvolutionConfig {
        iterations: 1,
        budget_ms: 30000,
        min_improvement,
        dry_run,
        agent_command: None,
        proposal_path: Some(tmp_proposal.clone()),
        json_output: false,
        policy,
    };

    let result = crate::evolution_loop::run_evolution(&graph_path_str, &config);
    let _ = std::fs::remove_file(&tmp_proposal);

    match result {
        Ok(r) => {
            let outcomes: Vec<serde_json::Value> = r.outcomes.iter().map(|o| serde_json::json!({
                "accepted": o.accepted,
                "reason": o.reason,
                "proposal": o.proposal_name,
                "target": o.target_strategy,
                "beforeHash": o.before_hash,
            })).collect();

            state.store.append_audit_in_job(tenant, job, capsule,
                &audit_event_json("evolve", tenant, job, capsule, serde_json::json!({
                    "accepted": r.proposals_accepted,
                    "rejected": r.proposals_rejected,
                    "dryRun": dry_run,
                }))).ok();

            json_resp(200, &serde_json::json!({
                "ok": true,
                "tenant": tenant,
                "job": job,
                "capsule": capsule,
                "accepted": r.proposals_accepted,
                "rejected": r.proposals_rejected,
                "outcomes": outcomes,
            }).to_string())
        }
        Err(e) => json_resp(500, &err_json(&e)),
    }
}

fn do_report(state: &State, tenant: &str, job: &str, capsule: &str) -> Resp {
    let data = match state.store.load_graph_in_job(tenant, job, capsule) {
        Ok(d) => d,
        Err(e) => return json_resp(404, &err_json(&e)),
    };
    let ng = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => return json_resp(500, &err_json(&e)),
    };

    let mut strategies = Vec::new();
    for node in &ng.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };

        let mut options = Vec::new();
        for i in 0..n_options {
            let (tries, total_ns, correct) = if let Some(slot) = node.state_slot {
                let base = slot as usize + i * 3;
                if base + 2 < ng.state.len() {
                    (ng.state[base] as u64, ng.state[base + 1], ng.state[base + 2] as u64)
                } else { (0, 0.0, 0) }
            } else { (0, 0.0, 0) };
            let avg_ms = if tries > 0 { (total_ns / tries as f64) / 1_000_000.0 } else { 0.0 };
            options.push(serde_json::json!({
                "option": i,
                "tries": tries,
                "correct": correct,
                "avg_ms": (avg_ms * 1000.0).round() / 1000.0,
                "weight": (node.weights.get(i).copied().unwrap_or(0.0) * 10000.0).round() / 10000.0,
            }));
        }
        strategies.push(serde_json::json!({
            "node_id": node.id,
            "activations": node.activation_count,
            "n_options": n_options,
            "options": options,
        }));
    }

    json_resp(200, &serde_json::json!({
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "hash": sha256_hex(&data),
        "strategies": strategies,
    }).to_string())
}

// ── Helpers ──

fn inspect_graph_json(
    tenant: &str,
    job: &str,
    capsule: &str,
    data: &[u8],
    graph: &NeuralGraph,
) -> String {
    let nodes: Vec<serde_json::Value> = graph.nodes.iter().map(|node| {
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        let operand_refs: Vec<u32> = node.operands.iter().filter_map(|operand| {
            match operand {
                Operand::NodeRef(id) => Some(*id),
                _ => None,
            }
        }).collect();
        serde_json::json!({
            "id": node.id,
            "op": format!("{:?}", node.op),
            "weightKind": format!("{:?}", node.weight_kind),
            "contract": format!("{:?}", node.contract),
            "objective": format!("{:?}", node.objective),
            "activationCount": node.activation_count,
            "operandCount": node.operands.len(),
            "operandRefs": operand_refs,
            "weights": node.weights.iter().take(n_options).map(|w| (w * 10000.0).round() / 10000.0).collect::<Vec<f64>>(),
            "stateSlot": node.state_slot,
        })
    }).collect();

    let edges: Vec<serde_json::Value> = graph.edges.iter().map(|edge| {
        serde_json::json!({
            "from": edge.from,
            "to": edge.to,
            "weight": (edge.weight * 10000.0).round() / 10000.0,
            "gated": edge.gate.is_some(),
        })
    }).collect();

    serde_json::json!({
        "tenant": tenant,
        "job": job,
        "capsule": capsule,
        "hash": sha256_hex(data),
        "nodes": graph.nodes.len(),
        "edges": graph.edges.len(),
        "entry": graph.entry,
        "journal": graph.journal.len(),
        "stateSize": graph.state.len(),
        "nodeList": nodes,
        "edgeList": edges,
    }).to_string()
}

fn apply_context_memory_to_graph(
    graph: &mut NeuralGraph,
    memory: &crate::learning::CapsuleMemory,
    context_key: &str,
) {
    for node in &mut graph.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        let Some(strategy_memory) = memory.strategies.get(&node.id) else { continue; };
        let Some(bucket) = strategy_memory.contexts.get(context_key) else { continue; };

        let limit = n_options.min(bucket.weights.len()).min(node.weights.len());
        for i in 0..limit {
            node.weights[i] = bucket.weights[i];
        }
    }
}

fn extract_decisions(graph: &NeuralGraph) -> Vec<serde_json::Value> {
    let mut decisions = Vec::new();
    for node in &graph.nodes {
        if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
        if node.activation_count == 0 { continue; }
        let n_options = if node.contract == Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };
        let chosen = node.bias as usize;
        let confidence = node.weights.get(chosen).copied().unwrap_or(0.0);
        let objective = match node.objective {
            Objective::Speed => "speed", Objective::Accuracy => "accuracy",
            Objective::Reliability => "reliability", Objective::Cost => "cost",
            Objective::Risk => "risk", Objective::Confidence => "confidence",
            Objective::Reward => "reward", Objective::MultiObjective => "multi",
            Objective::None => "general",
        };
        let weights: Vec<f64> = node.weights[..n_options].to_vec();
        decisions.push(serde_json::json!({
            "node_id": node.id,
            "chosen_option": chosen,
            "confidence": (confidence * 10000.0).round() / 10000.0,
            "objective": objective,
            "weights": weights.iter().map(|w| (w * 10000.0).round() / 10000.0).collect::<Vec<f64>>(),
            "activations": node.activation_count,
        }));
    }
    decisions
}

fn read_body_limited(request: &mut tiny_http::Request) -> Result<String, Resp> {
    let len = request.body_length().unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    let mut body = Vec::with_capacity(len.min(MAX_BODY_BYTES));
    request.as_reader().take(MAX_BODY_BYTES as u64 + 1).read_to_end(&mut body).ok();
    if body.len() > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    Ok(String::from_utf8_lossy(&body).to_string())
}

fn read_body_bytes_limited(request: &mut tiny_http::Request) -> Result<Vec<u8>, Resp> {
    let len = request.body_length().unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    let mut body = Vec::with_capacity(len.min(MAX_BODY_BYTES));
    request.as_reader().take(MAX_BODY_BYTES as u64 + 1).read_to_end(&mut body).ok();
    if body.len() > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    Ok(body)
}

// ── Admin Console HTML ──

const ADMIN_HTML: &str = r##"
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Lycan Console</title>
<style>
:root{
  --bg:#f5f6fa;--surface:#fff;--card:#fff;--card-hover:#f0f1f7;
  --border:#e0e2ee;--border-hover:#c8cade;
  --text:#1a1c2e;--muted:#6e7191;--dim:#a0a3bd;
  --peri:#6366f1;--peri-soft:#6366f114;--peri-glow:#6366f130;
  --green:#059669;--green-soft:#05966912;
  --red:#dc2626;--red-soft:#dc262612;
  --amber:#d97706;--amber-soft:#d9770612;
  --blue:#2563eb;
  --graph-bg:#0b0c10;
  --mono:'SF Mono','Cascadia Code','Fira Code','Consolas','Liberation Mono',monospace;
  --sans:-apple-system,BlinkMacSystemFont,'Segoe UI','Helvetica Neue',Arial,sans-serif;
  --radius:8px;--radius-lg:12px;
}
*{margin:0;padding:0;box-sizing:border-box}
html{font-size:13px}
body{font-family:var(--sans);background:var(--bg);color:var(--text);min-height:100vh;overflow:hidden}
button,input,select{font:inherit;color:inherit}
button{cursor:pointer;border:none;background:none}
::-webkit-scrollbar{width:5px}
::-webkit-scrollbar-track{background:transparent}
::-webkit-scrollbar-thumb{background:#c8cade;border-radius:3px}
::-webkit-scrollbar-thumb:hover{background:#a0a3bd}

/* Layout */
.app{display:grid;grid-template-columns:280px 1fr;height:100vh}

/* Sidebar */
.sidebar{background:#1a1c2e;border-right:1px solid #2a2d45;display:flex;flex-direction:column;overflow:hidden;color:#c8cade}
.sidebar-head{padding:20px 18px 16px;border-bottom:1px solid #2a2d45}
.logo-row{display:flex;align-items:center;gap:10px;margin-bottom:16px}
.logo-mark{width:32px;height:32px;background:var(--peri);border-radius:8px;display:flex;align-items:center;justify-content:center;font-weight:700;font-size:15px;color:#fff;font-family:var(--mono)}
.logo-text{font-size:17px;font-weight:700;letter-spacing:-.02em}
.logo-text span{color:var(--peri)}
.logo-sub{font-size:10px;color:var(--muted);letter-spacing:.04em;margin-top:1px}
.auth-box{display:flex;gap:6px}
.auth-box input{flex:1;background:#12131a;border:1px solid #2a2d45;border-radius:var(--radius);padding:8px 10px;font-family:var(--mono);font-size:11px;color:#e2e4f0;min-width:0}
.auth-box input:focus{outline:none;border-color:var(--peri);box-shadow:0 0 0 2px var(--peri-glow)}
.btn{padding:8px 14px;border-radius:var(--radius);font-size:12px;font-weight:600;border:1px solid var(--border);background:var(--card);transition:all .15s}
.btn:hover{background:var(--card-hover);border-color:var(--border-hover)}
.btn-peri{background:var(--peri);border-color:var(--peri);color:#fff}.btn-peri:hover{background:#6b72ee}
.btn-danger{background:var(--red);border-color:var(--red);color:#fff;font-size:11px}.btn-danger:hover{background:#e85555}
.btn-sm{padding:5px 10px;font-size:11px}
.status{display:flex;align-items:center;gap:6px;margin-top:10px;font-size:11px;color:var(--muted)}
.dot{width:7px;height:7px;border-radius:50%;flex-shrink:0}.dot.ok{background:var(--green)}.dot.err{background:var(--red)}

/* Tree */
.sidebar-body{flex:1;overflow-y:auto;padding:8px}
.search-box{padding:0 10px 8px}
.search-box input{width:100%;background:#12131a;border:1px solid #2a2d45;border-radius:var(--radius);padding:7px 10px;font-size:12px;color:#e2e4f0}
.search-box input:focus{outline:none;border-color:var(--peri)}
.tenant-block{margin-bottom:4px}
.tenant-name{padding:8px 12px 4px;font-size:10px;font-weight:700;letter-spacing:.08em;text-transform:uppercase;color:#8688a4;display:flex;justify-content:space-between;align-items:center}
.job-name{padding:4px 12px 2px;font-size:10px;color:#5a5d7a;font-weight:600;letter-spacing:.04em}
.cap-item{display:block;width:100%;text-align:left;padding:8px 12px;border-radius:var(--radius);margin:1px 0;transition:all .12s;border:1px solid transparent;color:#c8cade}
.cap-item:hover{background:#22243a;border-color:#2a2d45}
.cap-item.active{background:#6366f120;border-color:var(--peri);box-shadow:0 0 0 1px #6366f140}
.cap-label{font-size:12px;font-weight:600;color:#e2e4f0}
.cap-path{font-size:10px;color:#6e7191;font-family:var(--mono);margin-top:2px}

.sidebar-foot{border-top:1px solid #2a2d45;padding:12px 18px}
.sidebar-foot details{font-size:11px;color:#8688a4}
.sidebar-foot summary{cursor:pointer;font-weight:600;margin-bottom:6px}
.sidebar-foot input{width:100%;background:#12131a;border:1px solid #2a2d45;border-radius:var(--radius);padding:6px 8px;font-size:11px;margin-bottom:4px;color:#e2e4f0}

/* Main */
.main{display:flex;flex-direction:column;overflow:hidden}
.topbar{display:flex;align-items:center;justify-content:space-between;padding:14px 24px;border-bottom:1px solid var(--border);background:#fff;flex-shrink:0}
.topbar-left{display:flex;align-items:center;gap:12px}
.page-title{font-size:18px;font-weight:700;letter-spacing:-.01em}
.page-path{font-size:11px;color:var(--muted);font-family:var(--mono)}
.tabs{display:flex;gap:2px;background:#eef0f6;border-radius:var(--radius);padding:3px}
.tab{padding:6px 14px;border-radius:6px;font-size:12px;font-weight:600;color:var(--muted);transition:all .15s}
.tab:hover{color:var(--text)}
.tab.active{background:var(--peri);color:#fff;box-shadow:0 1px 3px #6366f140}

.content{flex:1;overflow-y:auto;padding:20px 24px 40px}
.error-box{margin-bottom:12px}
.error{background:var(--red-soft);border:1px solid var(--red);border-radius:var(--radius);padding:10px 14px;font-size:12px;color:var(--red)}

/* Stats */
.stats{display:grid;grid-template-columns:repeat(4,1fr);gap:10px;margin-bottom:20px}
.stat{background:#fff;border:1px solid var(--border);border-radius:var(--radius-lg);padding:16px;box-shadow:0 1px 3px rgba(0,0,0,.04)}
.stat-label{font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.06em;font-weight:600}
.stat-value{font-size:22px;font-weight:700;font-family:var(--mono);margin-top:4px}
.stat-value.peri{color:var(--peri)}
.stat-sub{font-size:10px;color:var(--dim);margin-top:2px}

/* Cards */
.card{background:#fff;border:1px solid var(--border);border-radius:var(--radius-lg);margin-bottom:14px;overflow:hidden;box-shadow:0 1px 3px rgba(0,0,0,.04)}
.card-head{padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center}
.card-title{font-size:12px;font-weight:700;text-transform:uppercase;letter-spacing:.06em;color:var(--muted)}
.card-body{padding:14px 16px}
.card-full{grid-column:1/-1}
.grid-2{display:grid;grid-template-columns:1fr 1fr;gap:14px}

/* Pill */
.pill{display:inline-block;padding:2px 8px;border-radius:20px;font-size:10px;font-weight:600;font-family:var(--mono);background:#eef0f6;color:var(--muted)}
.pill.ok{background:var(--green-soft);color:var(--green)}
.pill.warn{background:var(--amber-soft);color:var(--amber)}
.pill.bad{background:var(--red-soft);color:var(--red)}
.pill.peri{background:var(--peri-soft);color:var(--peri)}

/* Strategy */
.strategy{background:#f8f9fc;border:1px solid var(--border);border-radius:var(--radius);padding:14px;margin-bottom:10px}
.strategy-head{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.strategy-id{font-family:var(--mono);font-weight:700;color:var(--peri);font-size:13px}
.strategy-meta{font-size:11px;color:var(--muted)}
.option{display:grid;grid-template-columns:80px 1fr 70px 60px 70px;gap:8px;align-items:center;margin-bottom:5px;font-size:12px}
.option-name{font-family:var(--mono);font-weight:600;font-size:11px;color:var(--muted)}
.option-name.win{color:var(--green)}
.bar-bg{height:20px;background:#eef0f6;border-radius:4px;overflow:hidden}
.bar{height:100%;border-radius:4px;background:linear-gradient(90deg,var(--peri),#9BA1FF);transition:width .6s ease;min-width:2px}
.bar.win{background:linear-gradient(90deg,var(--green),#5eead4)}
.cell-label{font-size:9px;color:var(--dim);display:block;letter-spacing:.04em;text-transform:uppercase}
.cell-value{font-family:var(--mono);font-weight:600;font-size:12px}

/* Table */
.table{width:100%;border-collapse:collapse;font-size:12px}
.table th{text-align:left;font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.06em;font-weight:600;padding:6px 10px;border-bottom:1px solid var(--border)}
.table td{padding:6px 10px;border-bottom:1px solid var(--border);vertical-align:top}
.table tr:last-child td{border-bottom:none}
.mono{font-family:var(--mono)}

/* Timeline */
.event{padding:8px 0;border-bottom:1px solid var(--border)}
.event:last-child{border-bottom:none}
.event-main{display:flex;justify-content:space-between;align-items:center;gap:8px}
.event-main b{font-size:12px}
.event-meta{font-size:10px;color:var(--muted);margin-top:3px;font-family:var(--mono);word-break:break-all}

/* Graph */
.graph-shell{width:100%;height:340px;background:#0b0c10;border-radius:var(--radius-lg);overflow:hidden;position:relative;border:1px solid #252840}
.graph-shell canvas{width:100%;height:100%}
.graph-legend{display:flex;gap:14px;margin-top:8px;font-size:10px;color:var(--muted)}
.legend-dot{width:8px;height:8px;border-radius:50%;display:inline-block;margin-right:4px;vertical-align:middle}

.empty{color:var(--dim);font-size:12px;padding:16px 0;text-align:center}
.hide{display:none!important}

/* Caps panel */
.cap-group{margin-bottom:10px}
.cap-group b{font-size:11px;color:var(--peri)}
.cap-group div{font-size:11px;color:var(--muted);font-family:var(--mono);margin-top:4px;line-height:1.6}

@media(max-width:900px){.app{grid-template-columns:1fr}.sidebar{display:none}}
</style>
</head>
<body>
<div class="app">
  <!-- Sidebar -->
  <aside class="sidebar">
    <div class="sidebar-head">
      <div class="logo-row">
        <div class="logo-mark">L</div>
        <div><div class="logo-text"><span>Lycan</span> Console</div><div class="logo-sub">Adaptive Runtime</div></div>
      </div>
      <div class="auth-box">
        <input id="key" type="password" placeholder="Admin key">
        <button class="btn btn-peri" id="connect">Connect</button>
      </div>
      <div class="status"><span class="dot" id="health-dot"></span><span id="health-text">Disconnected</span></div>
    </div>
    <div class="search-box" style="padding-top:10px"><input id="tree-filter" placeholder="Search capsules..."></div>
    <div class="sidebar-body" id="capsule-list"><div class="empty">Connect to load capsules</div></div>
    <div class="sidebar-foot">
      <details>
        <summary>Create Job</summary>
        <input id="job-tenant" placeholder="Tenant">
        <input id="job-id" placeholder="Job ID">
        <input id="job-name" placeholder="Name (optional)">
        <input id="job-desc" placeholder="Description (optional)">
        <button class="btn btn-sm btn-peri" id="create-job" style="margin-top:4px;width:100%">Create</button>
      </details>
    </div>
  </aside>

  <!-- Main -->
  <div class="main">
    <div class="topbar">
      <div class="topbar-left">
        <div>
          <div class="page-title" id="page-title">Lycan Console</div>
          <div class="page-path" id="page-sub">Select a capsule to begin</div>
        </div>
      </div>
      <div style="display:flex;align-items:center;gap:10px">
        <div class="tabs">
          <button class="tab active" data-tab="overview">Overview</button>
          <button class="tab" data-tab="decisions">Decisions</button>
          <button class="tab" data-tab="logs">Logs</button>
          <button class="tab" data-tab="system">System</button>
        </div>
        <button class="btn btn-sm" id="refresh-main" title="Refresh">&#x21bb;</button>
      </div>
    </div>

    <div class="content">
      <div id="error-box" class="error-box"></div>

      <!-- Overview tab -->
      <div data-panel="overview">
        <div class="stats">
          <div class="stat"><div class="stat-label">Strategies</div><div class="stat-value peri" id="k-strategies">-</div><div class="stat-sub" id="hash-pill">-</div></div>
          <div class="stat"><div class="stat-label">Confidence</div><div class="stat-value" id="k-confidence">-</div><div class="stat-sub" id="winner-pill">-</div></div>
          <div class="stat"><div class="stat-label">Decisions</div><div class="stat-value" id="k-decisions">-</div><div class="stat-sub" id="decision-count">-</div></div>
          <div class="stat"><div class="stat-label">Audits</div><div class="stat-value" id="audit-summary">-</div><div class="stat-sub" id="audit-count">-</div></div>
        </div>

        <div class="card">
          <div class="card-head"><span class="card-title">Strategy Weights</span><span class="pill peri" id="activation-pill">-</span></div>
          <div class="card-body" id="strategies"><div class="empty">No capsule selected</div></div>
        </div>

        <div class="card">
          <div class="card-head"><span class="card-title">Capsule Graph</span><span class="pill peri" id="graph-mode">-</span></div>
          <div class="card-body">
            <div style="display:none"><span id="g-nodes">-</span><span id="g-edges">-</span><span id="g-strategies">-</span><span id="g-contexts">-</span></div>
            <div class="graph-shell" id="graph-shell"><canvas id="capsule-graph"></canvas></div>
            <div class="graph-legend">
              <span><span class="legend-dot" style="background:#94a3b8"></span>input</span>
              <span><span class="legend-dot" style="background:#7dd3fc"></span>compute</span>
              <span><span class="legend-dot" style="background:#facc15"></span>strategy</span>
              <span><span class="legend-dot" style="background:#c084fc"></span>capability</span>
              <span><span class="legend-dot" style="background:#fb7185"></span>output</span>
              <span><span class="legend-dot" style="background:#34d399"></span>context</span>
            </div>
            <div style="margin-top:6px;font-size:11px;color:var(--muted)" id="graph-note"></div>
          </div>
        </div>

        <div class="grid-2">
          <div class="card">
            <div class="card-head"><span class="card-title">Policy</span><span class="pill" id="policy-mode">-</span></div>
            <div class="card-body" id="policy"><div class="empty">No policy loaded</div></div>
          </div>
          <div class="card">
            <div class="card-head"><span class="card-title">Capsule Detail</span></div>
            <div class="card-body" id="selected-detail"><div class="empty">No capsule selected</div></div>
          </div>
        </div>
      </div>

      <!-- Decisions tab -->
      <div data-panel="decisions" class="hide">
        <div class="card">
          <div class="card-head"><span class="card-title">Recent Decisions</span><span class="pill peri" id="k-last">-</span></div>
          <div class="card-body" id="decisions"><div class="empty">No decisions logged</div></div>
        </div>
      </div>

      <!-- Logs tab -->
      <div data-panel="logs" class="hide">
        <div class="grid-2">
          <div class="card">
            <div class="card-head"><span class="card-title">Audit Log</span><span class="pill" id="audit-count-side">0</span></div>
            <div class="card-body" style="max-height:400px;overflow-y:auto" id="audits"><div class="empty">No audits</div></div>
          </div>
          <div class="card">
            <div class="card-head"><span class="card-title">Evolution Log</span><span class="pill" id="evolution-count">0</span></div>
            <div class="card-body" style="max-height:400px;overflow-y:auto" id="evolution"><div class="empty">No evolution events</div></div>
          </div>
        </div>
      </div>

      <!-- System tab -->
      <div data-panel="system" class="hide">
        <div class="stats">
          <div class="stat"><div class="stat-label">Tenants</div><div class="stat-value" id="k-tenants">-</div></div>
          <div class="stat"><div class="stat-label">Jobs</div><div class="stat-value" id="k-jobs">-</div></div>
          <div class="stat"><div class="stat-label">Capsules</div><div class="stat-value peri" id="k-capsules">-</div></div>
          <div class="stat"><div class="stat-label">Capabilities</div><div class="stat-value" id="cap-count">-</div></div>
        </div>
        <div class="card">
          <div class="card-head"><span class="card-title">Capability Registry</span></div>
          <div class="card-body" id="capabilities"><div class="empty">Not loaded</div></div>
        </div>
      </div>
    </div>
  </div>
</div>

<script>
/* ── Hidden compat IDs for JS that references them ── */
void(document.getElementById("context-name")||document.body.insertAdjacentHTML("beforeend",'<span id="context-name" class="hide"></span><span id="context-path" class="hide"></span><span id="selected-pill" class="hide"></span><span id="clock" class="hide"></span><span id="rail-refresh" class="hide"></span><span id="refresh-side" class="hide"></span>'));

const state={key:"",inventory:[],capabilities:[],selected:null,report:null,inspect:null,policy:null,contexts:[],memory:null,decisions:[],audits:[],evolution:[],activeTab:"overview",graphFrame:null,graphSeed:1};
const $=id=>document.getElementById(id);
const esc=v=>String(v??"").replace(/[&<>"']/g,m=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[m]));
const pct=v=>Number.isFinite(+v)?((+v)*100).toFixed(1)+"%":"-";
const short=v=>v?String(v).slice(0,16):"-";
const auth=()=>({"Authorization":"Bearer "+state.key,"Content-Type":"application/json"});
const enc=encodeURIComponent;
function setError(msg){$("error-box").innerHTML=msg?'<div class="error">'+esc(msg)+'</div>':""}
function setHealth(ok,label){$("health-dot").className="dot "+(ok?"ok":"err");$("health-text").textContent=label}
async function json(path){const r=await fetch(path,{headers:auth()});if(!r.ok)throw new Error(path+" -> HTTP "+r.status);return await r.json()}
async function maybeJson(path){try{return await json(path)}catch(_){return null}}
async function text(path){const r=await fetch(path,{headers:auth()});if(!r.ok)throw new Error(path+" -> HTTP "+r.status);return await r.text()}
function parseLines(raw){return String(raw||"").split(/\n+/).map(s=>s.trim()).filter(Boolean).map(s=>{try{return JSON.parse(s)}catch(_){return{raw:s}}})}
function capsuleBase(sel){return "/tenants/"+enc(sel.tenant)+"/jobs/"+enc(sel.job||"default")+"/capsules/"+enc(sel.capsule)}

async function connect(){
  state.key=$("key").value.trim();
  sessionStorage.setItem("lycanKey",state.key);
  setError("");
  try{
    const h=await fetch("/health").then(r=>r.json());
    if(!h.ok)throw new Error("health check failed");
    setHealth(true,"Online");
    await Promise.all([loadInventory(),loadCapabilities()]);
    renderCapabilities();renderInventory();
    const first=state.inventory.flatMap(t=>t.jobs.flatMap(j=>j.capsules.map(c=>({tenant:t.tenant,job:j.id,capsule:c})))).shift();
    if(!state.selected&&first)state.selected=first;
    if(state.selected)await loadCapsule(state.selected);
  }catch(e){setHealth(false,"Auth failed");setError(e.message)}
}

async function loadInventory(){
  const tenants=(await json("/tenants")).tenants||[];
  const inventory=[];
  for(const tenant of tenants){
    const jobsResp=await maybeJson("/tenants/"+enc(tenant)+"/jobs");
    if(jobsResp&&Array.isArray(jobsResp.jobs)){
      const jobs=[];
      for(const j of jobsResp.jobs){
        const id=typeof j==="string"?j:j.id;if(!id)continue;
        let capsules=Array.isArray(j.capsules)?j.capsules:[];
        if(!capsules.length){
          const detail=await maybeJson("/tenants/"+enc(tenant)+"/jobs/"+enc(id));
          if(detail&&Array.isArray(detail.capsules))capsules=detail.capsules;
          else if(detail&&Array.isArray(detail.capsuleList))capsules=detail.capsuleList;
          else if(detail&&detail.job&&Array.isArray(detail.job.capsules))capsules=detail.job.capsules;
          else if(detail&&detail.job&&Array.isArray(detail.job.capsuleList))capsules=detail.job.capsuleList;
        }
        jobs.push({id,name:j.name||id,capsules});
      }
      inventory.push({tenant,jobs});
    }else{
      const caps=(await json("/tenants/"+enc(tenant)+"/capsules")).capsules||[];
      inventory.push({tenant,jobs:[{id:"default",name:"default",capsules:caps}]});
    }
  }
  state.inventory=inventory;
  $("k-tenants").textContent=inventory.length;
  $("k-jobs").textContent=inventory.reduce((n,t)=>n+t.jobs.length,0);
  $("k-capsules").textContent=inventory.reduce((n,t)=>n+t.jobs.reduce((m,j)=>m+j.capsules.length,0),0);
}

async function loadCapabilities(){
  const caps=await json("/capabilities");
  state.capabilities=Array.isArray(caps)?caps:[];
  $("cap-count").textContent=state.capabilities.length+" kernels";
}

async function loadCapsule(sel){
  state.selected=sel;renderInventory();
  $("page-title").textContent=sel.capsule;
  $("page-sub").textContent=sel.tenant+" / "+sel.job+" / "+sel.capsule;
  setError("");
  try{
    const base=capsuleBase(sel);
    const [report,inspect,policy,contexts,memory,decisions,audits,evolution]=await Promise.all([
      json(base+"/report"),json(base+"/inspect"),
      json(base+"/policy").catch(e=>({error:e.message})),
      json(base+"/contexts").catch(()=>({contexts:[]})),
      json(base+"/memory").catch(()=>null),
      text(base+"/decisions").catch(()=>""),
      text(base+"/audits").catch(()=>""),
      text(base+"/evolution").catch(()=>"")
    ]);
    state.report=report;state.inspect=inspect;state.policy=policy;
    state.contexts=contexts.contexts||[];state.memory=memory;
    state.decisions=parseLines(decisions);state.audits=parseLines(audits);
    state.evolution=parseLines(evolution);state.graphSeed++;
    renderAll();
  }catch(e){setError(e.message)}
}

function renderInventory(){
  const q=($("tree-filter").value||"").toLowerCase();
  if(!state.inventory.length){$("capsule-list").innerHTML='<div class="empty">Connect to load capsules</div>';return}
  let html="";
  for(const t of state.inventory){
    const jobs=t.jobs.map(j=>({...j,capsules:j.capsules.filter(c=>(t.tenant+"/"+j.id+"/"+c).toLowerCase().includes(q))})).filter(j=>j.capsules.length||!q);
    if(!jobs.length)continue;
    html+='<div class="tenant-block"><div class="tenant-name"><span>'+esc(t.tenant)+'</span><span class="pill">'+jobs.reduce((n,j)=>n+j.capsules.length,0)+'</span></div>';
    for(const job of jobs){
      html+='<div class="job-name">'+esc(job.name||job.id)+'</div>';
      for(const cap of job.capsules){
        const active=state.selected&&state.selected.tenant===t.tenant&&state.selected.job===job.id&&state.selected.capsule===cap;
        html+='<button class="cap-item '+(active?'active':'')+'" data-tenant="'+esc(t.tenant)+'" data-job="'+esc(job.id)+'" data-capsule="'+esc(cap)+'"><div class="cap-label">'+esc(cap)+'</div><div class="cap-path">'+esc(t.tenant)+'/'+esc(job.id)+'</div></button>';
      }
    }
    html+='</div>';
  }
  $("capsule-list").innerHTML=html||'<div class="empty">No matches</div>';
  document.querySelectorAll(".cap-item").forEach(b=>b.onclick=()=>loadCapsule({tenant:b.dataset.tenant,job:b.dataset.job,capsule:b.dataset.capsule}));
}

function renderAll(){renderOverview();renderStrategies();renderDecisions();renderPolicy();renderTimeline("audits",state.audits,"audit-count-side");renderTimeline("evolution",state.evolution,"evolution-count");renderSelected();if(state.activeTab==="overview")renderGraph();applyTab()}

function renderOverview(){
  const strategies=state.report?.strategies||[];
  $("k-strategies").textContent=strategies.length;
  $("hash-pill").textContent="hash "+short(state.report?.hash);
  let best=null,totalActivations=0;
  for(const st of strategies){totalActivations+=Number(st.activations||0);for(const o of st.options||[]){if(!best||o.weight>best.weight)best={...o,node_id:st.node_id}}}
  $("k-confidence").textContent=best?pct(best.weight):"-";
  $("winner-pill").textContent=best?"node "+best.node_id+" / opt "+best.option:"-";
  $("activation-pill").textContent=totalActivations+" activations";
  $("k-decisions").textContent=state.decisions.length;
  $("decision-count").textContent=state.decisions.length+" logged";
  $("audit-count").textContent=state.audits.length+" events";
  $("audit-summary").textContent=state.audits.length;
}

function renderStrategies(){
  const strategies=state.report?.strategies||[];
  if(!strategies.length){$("strategies").innerHTML='<div class="empty">No strategy nodes</div>';return}
  $("strategies").innerHTML=strategies.map(st=>{
    const opts=st.options||[];const winner=opts.reduce((a,o)=>!a||o.weight>a.weight?o:a,null);
    return '<div class="strategy"><div class="strategy-head"><div><div class="strategy-id">Strategy #'+esc(st.node_id)+'</div><div class="strategy-meta">'+esc(st.n_options||opts.length)+' options &middot; '+esc(st.activations||0)+' fires</div></div><span class="pill ok">winner '+esc(winner?.option??"-")+'</span></div>'
    +opts.map(o=>{const isWin=winner&&winner.option===o.option;return '<div class="option"><div class="option-name '+(isWin?'win':'')+'">Opt '+esc(o.option)+'</div><div class="bar-bg"><div class="bar '+(isWin?'win':'')+'" style="width:'+Math.max((+o.weight||0)*100,1)+'%"></div></div><div><span class="cell-label">Weight</span><span class="cell-value">'+pct(o.weight)+'</span></div><div><span class="cell-label">Tries</span><span class="cell-value">'+esc(o.tries??0)+'</span></div><div><span class="cell-label">Avg</span><span class="cell-value">'+Number(o.avg_ms||0).toFixed(3)+'ms</span></div></div>'}).join("")+'</div>';
  }).join("");
}

function renderDecisions(){
  const rows=state.decisions.slice(-20).reverse();
  if(!rows.length){$("decisions").innerHTML='<div class="empty">No decisions</div>';return}
  let html='<table class="table"><thead><tr><th>ID</th><th>Mode</th><th>Selected</th><th>Confidence</th><th>Context</th></tr></thead><tbody>';
  for(const ev of rows){const d=ev.decisions?.[0]||{};const learned=ev.learned===true||ev.learned==="true";
    html+='<tr><td class="mono">'+esc(short(ev.id))+'</td><td>'+(learned?'<span class="pill warn">learn</span>':'<span class="pill peri">read</span>')+'</td><td class="mono">#'+esc(d.node_id??"-")+' &rarr; '+esc(d.chosen_option??"-")+'</td><td>'+pct(d.confidence)+'</td><td class="mono">'+esc(ev.contextKey||"default")+'</td></tr>'}
  $("decisions").innerHTML=html+'</tbody></table>';
}

function renderPolicy(){
  const p=state.policy||{};
  const fields=[["stdout",p.allow_stdout],["stdin",p.allow_stdin],["file read",p.allow_file_read],["file write",p.allow_file_write],["network",p.allow_network]];
  $("policy").innerHTML=fields.map(([n,on])=>'<span class="pill '+(on?'ok':'bad')+'">'+esc(n)+' '+(on?'&#10003;':'&#10005;')+'</span>').join(" ")
    +(p.file_root?'<span class="pill peri" style="margin-left:4px">root: '+esc(p.file_root)+'</span>':'')
    +(Array.isArray(p.allowed_hosts)&&p.allowed_hosts.length?'<span class="pill peri" style="margin-left:4px">hosts: '+esc(p.allowed_hosts.join(", "))+'</span>':'');
  $("policy-mode").textContent=p.error?"error":"enforced";
}

function renderTimeline(id,items,countId){
  $(countId).textContent=items.length+" events";
  if(!items.length){$(id).innerHTML='<div class="empty">No events</div>';return}
  $(id).innerHTML=items.slice(-15).reverse().map(ev=>{
    const action=ev.event||ev.action||"event";const lower=action.toLowerCase();
    const tag=lower.includes("reject")?"bad":lower.includes("accept")||action==="feedback"?"ok":"peri";
    const bits=[ev.job?("job: "+ev.job):"",ev.decisionId,ev.nodeId?("node "+ev.nodeId):"",ev.option!=null?("opt "+ev.option):"",ev.reward!=null?("reward "+ev.reward):""].filter(Boolean).join(" &middot; ");
    return '<div class="event"><div class="event-main"><b>'+esc(action)+'</b><span class="pill '+tag+'">'+esc(ev.timestamp??"-")+'</span></div><div class="event-meta">'+esc(bits||ev.reason||ev.raw||JSON.stringify(ev))+'</div></div>'
  }).join("");
}

function renderCapabilities(){
  const groups={};for(const c of state.capabilities){const pkg=c.package||"other";(groups[pkg]||(groups[pkg]=[])).push(c)}
  $("capabilities").innerHTML=Object.keys(groups).sort().map(pkg=>'<div class="cap-group"><b>'+esc(pkg)+' <span class="mono">('+groups[pkg].length+')</span></b><div>'+groups[pkg].map(c=>esc(c.name)).join("<br>")+'</div></div>').join("")||'<div class="empty">No capabilities</div>';
}

function renderSelected(){
  const sel=state.selected||{};const r=state.report||{};const last=state.decisions[state.decisions.length-1]||{};
  $("selected-detail").innerHTML='<table class="table"><tbody>'
    +'<tr><th>Tenant</th><td class="mono">'+esc(sel.tenant||"-")+'</td></tr>'
    +'<tr><th>Job</th><td class="mono">'+esc(sel.job||"default")+'</td></tr>'
    +'<tr><th>Capsule</th><td class="mono">'+esc(sel.capsule||"-")+'</td></tr>'
    +'<tr><th>Graph hash</th><td class="mono">'+esc(r.hash||"-")+'</td></tr>'
    +'<tr><th>Last decision</th><td class="mono">'+esc(last.id||"-")+'</td></tr>'
    +'<tr><th>Learned</th><td>'+((last.learned===true||last.learned==="true")?'<span class="pill warn">yes</span>':'<span class="pill peri">no</span>')+'</td></tr>'
    +'</tbody></table>'
    +'<div style="display:flex;gap:8px;margin-top:14px">'
    +'<button class="btn btn-danger btn-sm" id="btn-purge-logs">Purge Logs</button>'
    +'<button class="btn btn-danger btn-sm" id="btn-delete-capsule">Delete Capsule</button>'
    +'</div>';
  const base=capsuleBase(sel);
  $("btn-purge-logs").onclick=async()=>{
    if(!confirm("Purge all logs for "+sel.capsule+"?"))return;
    try{const r=await fetch(base+"/logs",{method:"DELETE",headers:auth()});const j=await r.json();if(j.ok){setError("");await loadCapsule(sel)}else setError(j.error||"failed")}catch(e){setError(e.message)}
  };
  $("btn-delete-capsule").onclick=async()=>{
    if(!confirm("DELETE "+sel.tenant+"/"+sel.job+"/"+sel.capsule+"? This is permanent."))return;
    try{const r=await fetch(base,{method:"DELETE",headers:auth()});const j=await r.json();if(j.ok){state.selected=null;await loadInventory();renderInventory();$("selected-detail").innerHTML='<div class="empty">Deleted</div>'}else setError(j.error||"failed")}catch(e){setError(e.message)}
  };
}

/* Graph visualization */
function hashNum(v){let h=2166136261;const s=String(v);for(let i=0;i<s.length;i++){h^=s.charCodeAt(i);h=Math.imul(h,16777619)}return(h>>>0)/4294967295}
function graphKind(op,wk){if(op==="Strategy"||op==="AdaptiveChoice"||wk==="Strategy"||wk==="Adaptive")return"strategy";if(op==="Capability")return"capability";if(/^Const|LoadVar/.test(op))return"input";if(/Print|Return|Halt/.test(op))return"output";return"compute"}
function graphColor(k){return{input:"#94a3b8",compute:"#7dd3fc",strategy:"#facc15",capability:"#c084fc",output:"#fb7185",context:"#34d399"}[k]||"#7dd3fc"}
function makeGraphModel(w,h){
  const inspect=state.inspect||{};const raw=inspect.nodeList||[];const report=state.report||{};const strategies=report.strategies||[];
  const strategyIds=new Set(strategies.map(s=>Number(s.node_id)));const hotIds=new Set(raw.filter(n=>Number(n.activationCount||0)>0).map(n=>Number(n.id)));
  const maxN=120;let step=Math.max(1,Math.ceil(raw.length/maxN));
  let keep=raw.filter((n,i)=>strategyIds.has(Number(n.id))||hotIds.has(Number(n.id))||n.op==="Capability"||i%step===0);
  if(!keep.length)keep=raw.slice(0,maxN);
  const ids=new Set(keep.map(n=>Number(n.id)));const maxId=Math.max(1,...keep.map(n=>Number(n.id)));
  const px=54,py=52;const lane={input:.12,compute:.38,capability:.58,strategy:.74,output:.9};
  const nodes=keep.map(n=>{
    const id=Number(n.id);const kind=graphKind(n.op,n.weightKind);const j=(hashNum(id+"-"+state.graphSeed)-.5);
    const bx=px+(w-px*2)*(id/maxId);const lx=(w-px*2)*(lane[kind]||.42)+px;
    const x=bx*.42+lx*.58+Math.sin(id*.71)*18;const y=py+(h-py*2)*(hashNum("y"+id)*.84+.08)+j*18;
    const hot=Number(n.activationCount||0)>0;const strat=strategies.find(s=>Number(s.node_id)===id);
    const winner=strat?.options?.reduce((a,o)=>!a||o.weight>a.weight?o:a,null);
    return{id,op:n.op,kind,x,y,r:kind==="strategy"?10:kind==="capability"?8:hot?7:4.5,hot,strategy:!!strat,winner,weights:n.weights||[],activation:Number(n.activationCount||0)};
  });
  const byId=new Map(nodes.map(n=>[n.id,n]));let edges=[];
  for(const n of keep){const to=Number(n.id);for(const from of n.operandRefs||[]){if(ids.has(Number(from))&&ids.has(to))edges.push({from:Number(from),to,kind:"operand",weight:.45})}}
  for(const e of inspect.edgeList||[]){if(ids.has(Number(e.from))&&ids.has(Number(e.to)))edges.push({from:Number(e.from),to:Number(e.to),kind:"edge",weight:Number(e.weight||.35)})}
  if(edges.length<Math.max(8,nodes.length*.35)){for(let i=1;i<nodes.length;i++){if(hashNum("link"+i+state.graphSeed)>.38)edges.push({from:nodes[i-1].id,to:nodes[i].id,kind:"flow",weight:.18})}}
  const contexts=(state.contexts||[]).slice(0,18).map((c,i)=>{
    const anchor=byId.get(Number(c.nodeId))||nodes.find(n=>n.strategy)||nodes[0];const angle=(Math.PI*2*i)/Math.max(1,state.contexts.length);
    const weights=c.weights||[];const best=weights.length?Math.max(...weights):0;
    return{id:"ctx-"+i,kind:"context",label:c.contextKey||"context",x:(anchor?.x||w*.5)+Math.cos(angle)*(52+best*38),y:(anchor?.y||h*.5)+Math.sin(angle)*(38+best*28),r:5+best*7,anchor,best,tries:c.totalTries||0};
  });
  return{nodes,edges,contexts,byId,started:performance.now()};
}
function drawGraph(model,t){
  const canvas=$("capsule-graph");const shell=$("graph-shell");if(!canvas||!shell)return;
  const dpr=window.devicePixelRatio||1;const rect=shell.getBoundingClientRect();const w=Math.max(320,rect.width),h=Math.max(300,rect.height);
  if(canvas.width!==Math.floor(w*dpr)||canvas.height!==Math.floor(h*dpr)){canvas.width=Math.floor(w*dpr);canvas.height=Math.floor(h*dpr);canvas.style.width=w+"px";canvas.style.height=h+"px"}
  const ctx=canvas.getContext("2d");ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,w,h);
  const pulse=(Math.sin(t/620)+1)/2;
  for(const e of model.edges){const a=model.byId.get(e.from),b=model.byId.get(e.to);if(!a||!b)continue;const hot=a.hot||b.hot;ctx.beginPath();ctx.moveTo(a.x,a.y);const mx=(a.x+b.x)/2,my=(a.y+b.y)/2-18*Math.sin((a.id+b.id+t/900)%6);ctx.quadraticCurveTo(mx,my,b.x,b.y);ctx.strokeStyle=hot?"rgba(124,131,255,.32)":"rgba(100,110,140,.1)";ctx.lineWidth=hot?1.4:.75;ctx.stroke()}
  for(const c of model.contexts){if(c.anchor){ctx.beginPath();ctx.moveTo(c.anchor.x,c.anchor.y);ctx.lineTo(c.x,c.y);ctx.strokeStyle="rgba(52,211,153,.24)";ctx.lineWidth=1;ctx.stroke()}}
  for(const c of model.contexts){ctx.beginPath();ctx.arc(c.x,c.y,c.r+pulse*1.8,0,Math.PI*2);ctx.fillStyle="rgba(52,211,153,.78)";ctx.shadowColor="#34d399";ctx.shadowBlur=12;ctx.fill();ctx.shadowBlur=0}
  for(const n of model.nodes){const color=graphColor(n.kind);ctx.beginPath();ctx.arc(n.x,n.y,n.r+(n.hot?pulse*2.2:0),0,Math.PI*2);ctx.fillStyle=color;ctx.shadowColor=n.kind==="strategy"?"#7C83FF":color;ctx.shadowBlur=n.kind==="strategy"?20:n.hot?12:3;ctx.fill();ctx.shadowBlur=0;
    if(n.kind==="strategy"){ctx.lineWidth=2;ctx.strokeStyle="rgba(124,131,255,.7)";ctx.stroke();if(n.weights?.length){let start=-Math.PI/2;for(const weight of n.weights){const end=start+Math.PI*2*Number(weight||0);ctx.beginPath();ctx.arc(n.x,n.y,n.r+7,start,end);ctx.strokeStyle=weight>.5?"#34d399":"rgba(124,131,255,.55)";ctx.lineWidth=3;ctx.stroke();start=end}}}}
  ctx.font="11px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif";ctx.textBaseline="middle";
  const labelNodes=[...model.nodes.filter(n=>n.strategy||n.hot),...model.nodes.filter(n=>n.kind==="capability").slice(0,4)].filter((n,i,a)=>a.findIndex(x=>x.id===n.id)===i).slice(0,14);
  for(const n of labelNodes){ctx.fillStyle="rgba(226,228,240,.8)";ctx.fillText(n.kind==="strategy"?"#"+n.id:n.op,n.x+n.r+8,n.y)}
}
function renderGraph(){
  if(state.graphFrame)cancelAnimationFrame(state.graphFrame);
  const inspect=state.inspect||{};const strategies=state.report?.strategies||[];
  $("g-nodes").textContent=inspect.nodes??"-";$("g-edges").textContent=inspect.edges??"-";$("g-strategies").textContent=strategies.length;$("g-contexts").textContent=(state.contexts||[]).length;
  $("graph-mode").textContent=(inspect.nodes||0)+" nodes";
  $("graph-note").textContent=strategies.length?strategies.length+" strategy node"+(strategies.length===1?"":"s")+", "+(state.contexts||[]).length+" context"+(((state.contexts||[]).length===1)?"":"s"):"Graph view — strategy nodes glow with periwinkle when learnable.";
  const shell=$("graph-shell");if(!shell||!state.inspect)return;
  const rect=shell.getBoundingClientRect();const model=makeGraphModel(Math.max(320,rect.width),Math.max(300,rect.height));
  const frame=t=>{drawGraph(model,t);state.graphFrame=requestAnimationFrame(frame)};
  state.graphFrame=requestAnimationFrame(frame);
}

async function createJob(){
  const tenant=$("job-tenant").value.trim();const id=$("job-id").value.trim();const name=$("job-name").value.trim();const desc=$("job-desc").value.trim();
  if(!tenant||!id){setError("Tenant and job id required");return}
  try{
    const r=await fetch("/tenants/"+enc(tenant)+"/jobs",{method:"POST",headers:auth(),body:JSON.stringify({id,name,description:desc,metadata:{source:"console"}})});
    if(!r.ok)throw new Error("create job -> HTTP "+r.status);
    await loadInventory();renderInventory();setError("");
  }catch(e){setError(e.message)}
}

function applyTab(){
  document.querySelectorAll(".tab").forEach(b=>b.classList.toggle("active",b.dataset.tab===state.activeTab));
  document.querySelectorAll("[data-panel]").forEach(p=>p.classList.toggle("hide",p.dataset.panel!==state.activeTab));
  if(state.activeTab==="overview"&&state.inspect)renderGraph();
  else if(state.graphFrame){cancelAnimationFrame(state.graphFrame);state.graphFrame=null}
}

/* Wire up */
$("connect").onclick=connect;
$("refresh-main").onclick=async()=>{if(state.selected)await loadCapsule(state.selected)};
$("create-job").onclick=createJob;
$("tree-filter").oninput=renderInventory;
document.querySelectorAll(".tab").forEach(b=>b.onclick=()=>{state.activeTab=b.dataset.tab;applyTab()});
$("key").addEventListener("keydown",e=>{if(e.key==="Enter")connect()});
const saved=sessionStorage.getItem("lycanKey");if(saved){$("key").value=saved;connect()}
applyTab();
window.addEventListener("resize",()=>{if(state.activeTab==="overview"&&state.inspect)renderGraph()});
</script>
</body>
</html>
"##;
