//! Route-level authorization regression tests.
//!
//! Boots `syntra serve` against a fresh tempdir + a fixed admin key,
//! issues scoped tokens via `POST /admin/tokens`, then asserts each
//! scope can only reach the actions allowed by its `Scope::allows`
//! contract. Covers admin, tenant-admin, and read-only tokens across
//! decide / read / mutate / delete / purge surfaces, including the
//! legacy `/tenants/{t}/capsules/{c}/...` default-job routes.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAB_LYC: &[u8] = include_bytes!(
    "../examples/lycan-internals/benchmarks/syntra_vs_vw_mab/mab_2arm.lyc"
);

struct Server {
    child: Child,
    addr: String,
    admin_key: String,
    _store: TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new(label: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "syntra-auth-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &std::path::Path { &self.0 }
}
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn pick_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static SEQ: AtomicU16 = AtomicU16::new(0);
    // Spread across a per-process window so concurrent test runs don't collide.
    19_000 + (std::process::id() as u16 % 200) * 10 + SEQ.fetch_add(1, Ordering::Relaxed) % 10
}

fn boot_server(label: &str) -> Server {
    let store = TempDir::new(label);
    let admin_key = format!("test-admin-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    // Try a few ports until one binds (other tests in the suite take 18000-18999).
    for _ in 0..10 {
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("serve")
            .arg("--addr").arg(&addr)
            .arg("--store").arg(store.path())
            .arg("--admin-key").arg(&admin_key)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn syntra");
        // Wait up to 3s for /health.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut ready = false;
        while Instant::now() < deadline {
            if let Ok(resp) = ureq::get(&format!("http://{addr}/health")).call() {
                if resp.status() == 200 { ready = true; break; }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        if ready {
            return Server { child, addr, admin_key, _store: store };
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not bind syntra to any test port");
}

fn url(srv: &Server, path: &str) -> String { format!("http://{}{}", srv.addr, path) }

fn auth(token: &str) -> (&'static str, String) {
    ("Authorization", format!("Bearer {token}"))
}

fn issue_token(srv: &Server, scope: serde_json::Value) -> String {
    let body = serde_json::json!({"scope": scope, "label": "test"}).to_string();
    let resp = ureq::post(&url(srv, "/admin/tokens"))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .expect("issue token");
    assert_eq!(resp.status(), 200, "admin /tokens should succeed");
    let j: serde_json::Value = resp.into_json().unwrap();
    j["token"].as_str().expect("token field").to_string()
}

fn status_with_token(srv: &Server, method: &str, path: &str, token: &str, body: Option<&[u8]>) -> u16 {
    let url = url(srv, path);
    let req = match method {
        "GET" => ureq::get(&url),
        "POST" => ureq::post(&url),
        "PUT" => ureq::put(&url),
        "DELETE" => ureq::delete(&url),
        _ => panic!("unknown method"),
    }.set("Authorization", &format!("Bearer {token}"));

    let result = match body {
        Some(b) => req.set("Content-Type", "application/octet-stream").send_bytes(b),
        None => req.call(),
    };
    match result {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(e) => panic!("transport error against {url}: {e}"),
    }
}

fn install_via_admin(srv: &Server, tenant: &str, job: &str, capsule: &str) {
    // Create the job first (admin route).
    let _ = ureq::post(&url(srv, &format!("/tenants/{tenant}/jobs")))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .set("Content-Type", "application/json")
        .send_string(&serde_json::json!({"id": job, "name": job}).to_string());
    let path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install");
    let resp = ureq::post(&url(srv, &path))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(MAB_LYC)
        .expect("admin install should succeed");
    assert_eq!(resp.status(), 200, "admin install should succeed");
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[test]
fn admin_token_can_do_anything() {
    let srv = boot_server("admin");
    install_via_admin(&srv, "t1", "j1", "c1");

    assert_eq!(status_with_token(&srv, "GET",  "/tenants",                                                    &srv.admin_key, None), 200);
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/t1/jobs",                                            &srv.admin_key, None), 200);
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/t1/jobs/j1/capsules/c1/report",                      &srv.admin_key, None), 200);
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/t1/jobs/j1/capsules/c1/policy",                      &srv.admin_key, None), 200);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t1/jobs/j1/capsules/c1/logs",                      &srv.admin_key, None), 200);
}

#[test]
fn read_token_can_read_and_decide_only_its_capsule() {
    let srv = boot_server("read-ok");
    install_via_admin(&srv, "ta", "ja", "ca");

    let tok = issue_token(&srv, serde_json::json!({
        "kind": "read", "tenant": "ta", "job": "ja", "capsule": "ca"
    }));

    // ALLOWED: read + decide on the exact tenant/job/capsule.
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/ta/jobs/ja/capsules/ca/report",   &tok, None), 200);
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/ta/jobs/ja/capsules/ca/policy",   &tok, None), 200);
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/ta/jobs/ja/capsules/ca/memory",   &tok, None), 200);
    assert_eq!(status_with_token(&srv, "GET",  "/tenants/ta/jobs/ja/capsules/ca/contexts", &tok, None), 200);
    // /decide accepts an empty JSON body for capsules without features.
    let decide_status = match ureq::post(&url(&srv, "/tenants/ta/jobs/ja/capsules/ca/decide"))
        .set("Authorization", &format!("Bearer {tok}"))
        .set("Content-Type", "application/json")
        .send_string("{}")
    {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(e) => panic!("transport: {e}"),
    };
    // Any 2xx/4xx that isn't 401/403 means auth was accepted (the body may
    // reject for other reasons — e.g. capsule-specific input validation).
    assert!(decide_status != 401 && decide_status != 403,
        "read token should be allowed to /decide, got {decide_status}");
}

#[test]
fn read_token_cannot_mutate_or_delete() {
    let srv = boot_server("read-deny");
    install_via_admin(&srv, "t", "j", "c");

    let tok = issue_token(&srv, serde_json::json!({
        "kind": "read", "tenant": "t", "job": "j", "capsule": "c"
    }));

    // Each of these is a CapsuleMutate / TenantOp / AdminGlobal that
    // a Read scope must be rejected on.
    assert_eq!(status_with_token(&srv, "POST", "/tenants/t/jobs/j/capsules/c/install",  &tok, Some(MAB_LYC)), 403);
    let _ = ureq::post(&url(&srv, "/tenants/t/jobs/j/capsules/c/feedback"))
        .set("Authorization", &format!("Bearer {tok}"))
        .set("Content-Type", "application/json")
        .send_string("{}");  // attempted call; we only care its auth-checked
    let fb_status = match ureq::post(&url(&srv, "/tenants/t/jobs/j/capsules/c/feedback"))
        .set("Authorization", &format!("Bearer {tok}"))
        .set("Content-Type", "application/json")
        .send_string("{}")
    {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => panic!("transport"),
    };
    assert_eq!(fb_status, 403, "read token must not POST /feedback");

    let pol_status = match ureq::put(&url(&srv, "/tenants/t/jobs/j/capsules/c/policy"))
        .set("Authorization", &format!("Bearer {tok}"))
        .set("Content-Type", "application/json")
        .send_string("{}")
    {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => panic!("transport"),
    };
    assert_eq!(pol_status, 403, "read token must not PUT /policy");

    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t/jobs/j/capsules/c",      &tok, None), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t/jobs/j/capsules/c/logs", &tok, None), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t/jobs/j",                 &tok, None), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t",                        &tok, None), 403);
    assert_eq!(status_with_token(&srv, "GET",    "/tenants",                          &tok, None), 403);
}

#[test]
fn read_token_cannot_reach_other_tenant_or_job_or_capsule() {
    let srv = boot_server("read-scope");
    install_via_admin(&srv, "mine", "mine-job", "mine-cap");
    install_via_admin(&srv, "other", "other-job", "other-cap");

    let tok = issue_token(&srv, serde_json::json!({
        "kind": "read", "tenant": "mine", "job": "mine-job", "capsule": "mine-cap"
    }));

    // Different tenant — same job/capsule names: still denied.
    assert_eq!(status_with_token(&srv, "GET", "/tenants/other/jobs/other-job/capsules/other-cap/report", &tok, None), 403);
    // Different job in same tenant — denied.
    assert_eq!(status_with_token(&srv, "GET", "/tenants/mine/jobs/other-job/capsules/mine-cap/report",   &tok, None), 403);
    // Different capsule in same tenant+job — denied.
    assert_eq!(status_with_token(&srv, "GET", "/tenants/mine/jobs/mine-job/capsules/other-cap/report",   &tok, None), 403);
    // Legacy default-job route: same tenant but job=default — denied since read scope is for job=mine-job.
    assert_eq!(status_with_token(&srv, "GET", "/tenants/mine/capsules/mine-cap/report",                  &tok, None), 403);
}

#[test]
fn tenant_admin_cannot_cross_tenants() {
    let srv = boot_server("tenant-admin");
    install_via_admin(&srv, "acme", "ja", "ca");
    install_via_admin(&srv, "other", "jo", "co");

    let tok = issue_token(&srv, serde_json::json!({
        "kind": "tenant_admin", "tenant": "acme"
    }));

    // Allowed: any action within their own tenant.
    assert_eq!(status_with_token(&srv, "GET", "/tenants/acme/jobs",                              &tok, None), 200);
    assert_eq!(status_with_token(&srv, "GET", "/tenants/acme/jobs/ja/capsules/ca/report",        &tok, None), 200);
    assert_eq!(status_with_token(&srv, "POST", "/tenants/acme/jobs/ja/capsules/ca/install",     &tok, Some(MAB_LYC)), 200);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/acme/jobs/ja/capsules/ca/logs",      &tok, None), 200);

    // Denied: anything against another tenant.
    assert_eq!(status_with_token(&srv, "GET",    "/tenants/other/jobs",                              &tok, None), 403);
    assert_eq!(status_with_token(&srv, "GET",    "/tenants/other/jobs/jo/capsules/co/report",        &tok, None), 403);
    assert_eq!(status_with_token(&srv, "POST",   "/tenants/other/jobs/jo/capsules/co/install",       &tok, Some(MAB_LYC)), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/other/jobs/jo/capsules/co/logs",          &tok, None), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/other",                                    &tok, None), 403);

    // Denied: admin-only routes.
    assert_eq!(status_with_token(&srv, "GET", "/tenants", &tok, None), 403);
}

#[test]
fn legacy_default_job_routes_are_scope_checked() {
    // Routes under /tenants/{t}/capsules/{c}/... must authorize against
    // job = "default", consistent with the legacy install path that
    // writes the capsule under default/.
    let srv = boot_server("legacy-default");
    // Install via the legacy admin route to write under job=default.
    let resp = ureq::post(&url(&srv, "/tenants/t/capsules/c/install"))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(MAB_LYC)
        .expect("legacy install ok for admin");
    assert_eq!(resp.status(), 200);

    // A Read token scoped to (t, default, c) can read the legacy route.
    let default_tok = issue_token(&srv, serde_json::json!({
        "kind": "read", "tenant": "t", "job": "default", "capsule": "c"
    }));
    assert_eq!(status_with_token(&srv, "GET", "/tenants/t/capsules/c/report", &default_tok, None), 200);

    // A Read token scoped to (t, some-other-job, c) is rejected on the same legacy route.
    let nondefault_tok = issue_token(&srv, serde_json::json!({
        "kind": "read", "tenant": "t", "job": "some-other-job", "capsule": "c"
    }));
    assert_eq!(status_with_token(&srv, "GET", "/tenants/t/capsules/c/report", &nondefault_tok, None), 403);

    // No token: 401.
    let resp = ureq::get(&url(&srv, "/tenants/t/capsules/c/report")).call();
    let status = match resp {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(e) => panic!("transport: {e}"),
    };
    assert_eq!(status, 401, "no token should 401");

    // Read token cannot mutate (legacy POST /install / DELETE /logs etc.)
    assert_eq!(status_with_token(&srv, "POST",   "/tenants/t/capsules/c/install",  &default_tok, Some(MAB_LYC)), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t/capsules/c/logs",     &default_tok, None), 403);
    assert_eq!(status_with_token(&srv, "DELETE", "/tenants/t/capsules/c",          &default_tok, None), 403);
}

// Unused import shield — `Read` trait is imported above for completeness
// of the std-process interplay; keep the symbol live for the linter.
#[allow(dead_code)]
fn _keep_read_used() {
    let mut s = String::new();
    let _ = std::io::empty().read_to_string(&mut s);
}
