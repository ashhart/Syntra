//! Server-level security and state-integrity regressions.
//!
//! - A tenant-scoped admin token must not be able to widen its capsule's
//!   file sandbox beyond the capsule's `data/` directory (absolute or
//!   escaping `file_root`), read another tenant's data, or open the
//!   capsule to private networks.
//! - Policy writes are strict (unknown fields rejected) and audited.
//! - Concurrent decides and rewards lose no model updates, across a restart.
//! - `--dev-mode` without an admin key refuses non-loopback binds.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new(label: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "syntra-sec-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server {
    child: Child,
    addr: String,
    admin_key: String,
    store: TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn boot(label: &str) -> Server {
    let store = TempDir::new(label);
    let admin_key = format!("sec-admin-{}-{label}", std::process::id());
    for _ in 0..10 {
        let addr = format!("127.0.0.1:{}", free_port());
        let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
            .args(["serve", "--addr", &addr, "--store"])
            .arg(store.path())
            .args(["--admin-key", &admin_key])
            .env("SYNTRA_RATE_LIMIT_RPS", "10000000")
            .env("SYNTRA_RATE_LIMIT_BURST", "10000000")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn syntra");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(r) = ureq::get(&format!("http://{addr}/health")).call()
                && r.status() == 200
            {
                return Server {
                    child,
                    addr,
                    admin_key,
                    store,
                };
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not boot syntra");
}

/// Send a request; returns (status, body) for both success and HTTP errors.
fn call(method: &str, url: &str, token: &str, body: Option<&[u8]>) -> (u16, String) {
    let req = ureq::request(method, url).set("Authorization", &format!("Bearer {token}"));
    let res = match body {
        Some(b) => req.send_bytes(b),
        None => req.call(),
    };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("transport error for {method} {url}: {e}"),
    };
    let status = resp.status();
    let mut text = String::new();
    resp.into_reader().read_to_string(&mut text).unwrap();
    (status, text)
}

fn tenant_admin_token(srv: &Server, tenant: &str) -> String {
    let (status, body) = call(
        "POST",
        &format!("http://{}/admin/tokens", srv.addr),
        &srv.admin_key,
        Some(
            serde_json::json!({"scope": {"kind": "tenant_admin", "tenant": tenant}, "label": "sec-test"})
                .to_string()
                .as_bytes(),
        ),
    );
    assert_eq!(status, 200, "token issue failed: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["token"].as_str().expect("token field").to_string()
}

/// Compile Lycan source with the `lycan` binary and return the `.lyc` bytes.
fn compile(label: &str, source: &str) -> Vec<u8> {
    let dir = TempDir::new(label);
    let src = dir.path().join("program.lycs");
    std::fs::write(&src, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg("compile")
        .arg(&src)
        .output()
        .expect("run lycan compile");
    assert!(
        out.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(dir.path().join("program.lyc")).expect("compiled .lyc")
}

fn capsule_url(srv: &Server, tenant: &str, capsule: &str, tail: &str) -> String {
    format!(
        "http://{}/v1/tenants/{tenant}/jobs/j/capsules/{capsule}/{tail}",
        srv.addr
    )
}

const MARKER: &str = "TOP-SECRET-STORE-MARKER";

/// Create a capsule with a spec (the v2 way to create one).
fn create_capsule(srv: &Server, token: &str, tenant: &str, capsule: &str) {
    let (st, body) = call(
        "PUT",
        &capsule_url(srv, tenant, capsule, "spec"),
        token,
        Some(br#"{"actions":[{"id":"a"},{"id":"b"}]}"#),
    );
    assert!(st == 200 || st == 201, "spec PUT: {st} {body}");
}

#[test]
fn tenant_admin_cannot_widen_the_file_sandbox() {
    let srv = boot("fileroot");
    std::fs::write(srv.store.path().join("secret-marker.txt"), MARKER).unwrap();
    let token = tenant_admin_token(&srv, "evil");

    // A feature program that reads the file named in the request and
    // publishes its contents as the decision's `reason`, which the decide
    // response echoes. Any successful escape would show up there.
    let reader = compile(
        "reader",
        "($ path (!cap \"runtime.inputGet\" \"path\"))\n(!cap \"runtime.publish\" \"reason\" (!cap \"file.readText\" path))\n",
    );
    create_capsule(&srv, &token, "evil", "reader");
    let (st, body) = call(
        "POST",
        &capsule_url(&srv, "evil", "reader", "install"),
        &token,
        Some(&reader),
    );
    assert_eq!(st, 200, "install: {body}");

    let policy = |root: serde_json::Value| {
        serde_json::json!({
            "allow_stdout": true, "allow_file_read": true, "allow_file_write": true,
            "file_root": root
        })
        .to_string()
    };
    let store_root = srv.store.path().to_string_lossy().to_string();
    for bad in [
        serde_json::json!(store_root),
        serde_json::json!("/"),
        serde_json::json!("../../../../.."),
        serde_json::json!("data/../../.."),
    ] {
        for key in [&token, &srv.admin_key] {
            let (st, body) = call(
                "PUT",
                &capsule_url(&srv, "evil", "reader", "policy"),
                key,
                Some(policy(bad.clone()).as_bytes()),
            );
            assert_eq!(st, 400, "file_root {bad} must be rejected: {body}");
        }
    }

    // A legitimate relative root works, but cannot reach outside data/.
    let (st, body) = call(
        "PUT",
        &capsule_url(&srv, "evil", "reader", "policy"),
        &token,
        Some(policy(serde_json::json!(".")).as_bytes()),
    );
    assert_eq!(st, 200, "relative root: {body}");
    for path in [
        "../../../../../../secret-marker.txt".to_string(),
        format!("{store_root}/secret-marker.txt"),
        "../policy.json".to_string(),
        "../spec.json".to_string(),
        "policy.json".to_string(),
        "../../../../../../tokens.json".to_string(),
        "../../../../../../syntra.db".to_string(),
    ] {
        let req = serde_json::json!({ "context": { "path": path } }).to_string();
        let (_, body) = call(
            "POST",
            &capsule_url(&srv, "evil", "reader", "decide"),
            &token,
            Some(req.as_bytes()),
        );
        assert!(
            !body.contains(MARKER),
            "read escaped the sandbox via {path}: {body}"
        );
        assert!(
            !body.contains("allow_file_read"),
            "capsule read its own policy via {path}: {body}"
        );
        assert!(
            !body.contains("\"actions\""),
            "capsule read its own spec via {path}: {body}"
        );
        assert!(
            !body.contains("tokenHash") && !body.contains("\"scope\""),
            "tokens leaked via {path}: {body}"
        );
    }

    // Positive control: a file inside the capsule's data/ dir is readable.
    let data_dir = srv
        .store
        .path()
        .join("tenants/evil/jobs/j/capsules/reader/data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("ok.txt"), "inside-data").unwrap();
    let req = serde_json::json!({ "context": { "path": "ok.txt" } }).to_string();
    let (st, body) = call(
        "POST",
        &capsule_url(&srv, "evil", "reader", "decide"),
        &token,
        Some(req.as_bytes()),
    );
    assert_eq!(st, 200, "in-sandbox read: {body}");
    assert!(
        body.contains("inside-data"),
        "expected the file contents as the reason: {body}"
    );
}

#[test]
fn only_global_admins_may_open_private_networks() {
    let srv = boot("privnet");
    let token = tenant_admin_token(&srv, "acme");
    create_capsule(&srv, &token, "acme", "c");
    let open = serde_json::json!({
        "allow_network": true, "allowed_hosts": ["internal.example"], "deny_private_networks": false
    })
    .to_string();
    let (st, body) = call(
        "PUT",
        &capsule_url(&srv, "acme", "c", "policy"),
        &token,
        Some(open.as_bytes()),
    );
    assert_eq!(
        st, 403,
        "tenant admin must not disable deny_private_networks: {body}"
    );
    let (st, body) = call(
        "PUT",
        &capsule_url(&srv, "acme", "c", "policy"),
        &srv.admin_key,
        Some(open.as_bytes()),
    );
    assert_eq!(
        st, 200,
        "operator may disable deny_private_networks: {body}"
    );
    // An admin-scope token is as powerful as the operator key (it can
    // issue itself more admin tokens), and gets the same answer.
    let (status, issued) = call(
        "POST",
        &format!("http://{}/admin/tokens", srv.addr),
        &srv.admin_key,
        Some(br#"{"scope": {"kind": "admin"}, "label": "sec-test-admin"}"#),
    );
    assert_eq!(status, 200, "token issue failed: {issued}");
    let admin: serde_json::Value = serde_json::from_str(&issued).unwrap();
    let (st, body) = call(
        "PUT",
        &capsule_url(&srv, "acme", "c", "policy"),
        admin["token"].as_str().unwrap(),
        Some(open.as_bytes()),
    );
    assert_eq!(
        st, 200,
        "an admin token may disable deny_private_networks: {body}"
    );
}

#[test]
fn policy_writes_are_strict_and_audited() {
    let srv = boot("strict");
    let token = tenant_admin_token(&srv, "acme");
    create_capsule(&srv, &token, "acme", "c");
    for (bad, why) in [
        (r#"{"allow_netwrok": true}"#, "unknown policy field"),
        (r#"{"allow_network": "yes"}"#, "must be boolean"),
        (
            r#"{"allowed_hosts": ["https://api.example.com/"]}"#,
            "bare host name",
        ),
        (r#"{"max_execution_ms": 3600000}"#, "max_execution_ms"),
        (r#"[]"#, "JSON object"),
    ] {
        let (st, body) = call(
            "PUT",
            &capsule_url(&srv, "acme", "c", "policy"),
            &token,
            Some(bad.as_bytes()),
        );
        assert_eq!(st, 400, "{bad} must be rejected");
        assert!(
            body.contains(why),
            "error for {bad} should mention {why:?}: {body}"
        );
    }
    let good = r#"{"allow_stdout": true, "allow_network": false}"#;
    let (st, body) = call(
        "PUT",
        &capsule_url(&srv, "acme", "c", "policy"),
        &token,
        Some(good.as_bytes()),
    );
    assert_eq!(st, 200, "valid policy: {body}");
    let (st, audit) = call(
        "GET",
        &capsule_url(&srv, "acme", "c", "audits"),
        &token,
        None,
    );
    assert_eq!(st, 200, "audits: {audit}");
    assert!(
        audit.contains("policy_updated"),
        "policy change must be audited: {audit}"
    );
    assert!(
        audit.contains("policySha256"),
        "audit must carry the policy hash: {audit}"
    );
}

fn model_version(srv: &Server, tenant: &str, capsule: &str) -> u64 {
    let (st, body) = call(
        "GET",
        &capsule_url(srv, tenant, capsule, "model"),
        &srv.admin_key,
        None,
    );
    assert_eq!(st, 200, "model: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["modelVersion"].as_u64().expect("modelVersion")
}

fn stats(srv: &Server, tenant: &str, capsule: &str) -> serde_json::Value {
    let (st, body) = call(
        "GET",
        &capsule_url(srv, tenant, capsule, ""),
        &srv.admin_key,
        None,
    );
    assert_eq!(st, 200, "capsule: {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["stats"].clone()
}

fn restart(srv: &mut Server, signal: &str) {
    let pid = srv.child.id().to_string();
    if signal == "TERM" {
        Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    } else {
        let _ = srv.child.kill();
    }
    let _ = srv.child.wait();
    let addr = format!("127.0.0.1:{}", free_port());
    srv.child = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .args(["serve", "--addr", &addr, "--store"])
        .arg(srv.store.path())
        .args(["--admin-key", &srv.admin_key])
        .env("SYNTRA_RATE_LIMIT_RPS", "10000000")
        .env("SYNTRA_RATE_LIMIT_BURST", "10000000")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("respawn");
    srv.addr = addr;
    let deadline = Instant::now() + Duration::from_secs(5);
    while ureq::get(&format!("http://{}/health", srv.addr))
        .call()
        .is_err()
    {
        assert!(Instant::now() < deadline, "server did not come back");
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Decide `n` times and reward each decision while other threads keep
/// deciding; returns once every reward is acknowledged.
fn decide_and_reward_under_load(srv: &Server, capsule: &str, n: usize, durable_every: usize) {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let req = serde_json::json!({ "context": { "i": i } }).to_string();
        let (st, body) = call(
            "POST",
            &capsule_url(srv, "acme", capsule, "decide"),
            &srv.admin_key,
            Some(req.as_bytes()),
        );
        assert_eq!(st, 200, "decide: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        ids.push(v["decisionId"].as_str().unwrap().to_string());
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hammers: Vec<_> = (0..4)
        .map(|_| {
            let stop = stop.clone();
            let url = capsule_url(srv, "acme", capsule, "decide");
            let key = srv.admin_key.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = call("POST", &url, &key, Some(br#"{"context":{}}"#));
                }
            })
        })
        .collect();
    for (i, id) in ids.iter().enumerate() {
        let durable = durable_every > 0 && i % durable_every == 0;
        let rb =
            serde_json::json!({ "decisionId": id, "reward": (i % 2) as f64, "durable": durable })
                .to_string();
        let (st, body) = call(
            "POST",
            &capsule_url(srv, "acme", capsule, "reward"),
            &srv.admin_key,
            Some(rb.as_bytes()),
        );
        assert_eq!(st, 200, "reward: {body}");
        if i % 50 == 0 {
            let (st, body) = call(
                "POST",
                &capsule_url(srv, "acme", capsule, "reward"),
                &srv.admin_key,
                Some(rb.as_bytes()),
            );
            assert_eq!(st, 200, "retry: {body}");
            assert!(
                body.contains("\"duplicate\":true"),
                "a retried reward must be a duplicate: {body}"
            );
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for h in hammers {
        h.join().unwrap();
    }
}

/// Every acknowledged reward updates the model exactly once, concurrent
/// decides never erase an update, and a graceful restart (SIGTERM flushes
/// the write-behind log) rebuilds the same model.
#[test]
fn concurrent_decides_and_rewards_lose_no_updates() {
    let mut srv = boot("lostupdate");
    create_capsule(&srv, &srv.admin_key.clone(), "acme", "router");
    const N: usize = 200;
    decide_and_reward_under_load(&srv, "router", N, 0);
    assert_eq!(
        model_version(&srv, "acme", "router"),
        N as u64,
        "every reward applied exactly once"
    );
    restart(&mut srv, "TERM");
    assert_eq!(
        model_version(&srv, "acme", "router"),
        N as u64,
        "graceful restart must keep every acknowledged reward"
    );
    assert_eq!(
        stats(&srv, "acme", "router")["rewards"].as_u64(),
        Some(N as u64)
    );
}

/// After a hard crash the rebuilt model matches the reward log exactly:
/// rewards in the uncommitted tail may be lost, but none is half-applied.
/// A reward sent with `"durable": true` is committed before it is
/// acknowledged, so it always survives.
#[test]
fn hard_crash_leaves_model_and_log_consistent() {
    let mut srv = boot("crash");
    create_capsule(&srv, &srv.admin_key.clone(), "acme", "router");
    const N: usize = 200;
    decide_and_reward_under_load(&srv, "router", N, 10);
    restart(&mut srv, "KILL");
    let committed = stats(&srv, "acme", "router")["rewards"].as_u64().unwrap();
    assert!(committed <= N as u64);
    // Every 10th reward was durable; the last durable one (index 190) is
    // committed together with everything queued before it.
    assert!(
        committed >= 191,
        "durable rewards and everything before them must survive: {committed}"
    );
    assert_eq!(
        model_version(&srv, "acme", "router"),
        committed,
        "model must match the reward log exactly"
    );
}

#[test]
fn a_second_server_on_one_store_refuses_to_start() {
    let srv = boot("twoservers");
    let mut second = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .args([
            "serve",
            "--addr",
            &format!("127.0.0.1:{}", free_port()),
            "--store",
        ])
        .arg(srv.store.path())
        .args(["--admin-key", &srv.admin_key])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn a second server");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = second.kill();
            let _ = second.wait();
            panic!("a second server started on a store already in use");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
    let mut stderr = String::new();
    second
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("another syntra server"), "{stderr}");
    // The first server is untouched.
    let (st, _) = call("GET", &format!("http://{}/health", srv.addr), "", None);
    assert_eq!(st, 200);
}

#[test]
fn dev_mode_refuses_non_loopback_bind_without_opt_in() {
    let store = TempDir::new("devmode");
    let out = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .args(["serve", "--dev-mode", "--addr", "0.0.0.0:0", "--store"])
        .arg(store.path())
        .env_remove("LYCAN_ADMIN_KEY")
        .output()
        .expect("run syntra");
    assert!(
        !out.status.success(),
        "dev mode on 0.0.0.0 must refuse to start"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--dev-mode-allow-remote"),
        "error should name the opt-in flag: {stderr}"
    );
}
