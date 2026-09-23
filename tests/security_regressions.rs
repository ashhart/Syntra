//! Server-level security and state-integrity regressions.
//!
//! - A tenant-scoped admin token must not be able to widen its capsule's
//!   file sandbox beyond the capsule's `data/` directory (absolute or
//!   escaping `file_root`), read another tenant's data, or open the
//!   capsule to private networks.
//! - Policy writes are strict (unknown fields rejected) and audited.
//! - Concurrent `/decide` calls must not overwrite `/feedback` updates.
//! - `--dev-mode` without an admin key refuses non-loopback binds.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAB_LYC: &[u8] =
    include_bytes!("../examples/lycan-internals/benchmarks/syntra_vs_vw_mab/mab_2arm.lyc");

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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn syntra");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(r) = ureq::get(&format!("http://{addr}/health")).call() {
                if r.status() == 200 {
                    return Server {
                        child,
                        addr,
                        admin_key,
                        store,
                    };
                }
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

#[test]
fn tenant_admin_cannot_widen_the_file_sandbox() {
    let srv = boot("fileroot");
    std::fs::write(srv.store.path().join("secret-marker.txt"), MARKER).unwrap();
    let token = tenant_admin_token(&srv, "evil");

    let reader = compile(
        "reader",
        "($ path (!cap \"runtime.inputGet\" \"path\"))\n(!cap \"file.readText\" path)\n",
    );
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
        "policy.json".to_string(),
        "../../../../../../tokens.json".to_string(),
    ] {
        let req = serde_json::json!({"path": path}).to_string();
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
    let req = serde_json::json!({"path": "ok.txt"}).to_string();
    let (st, body) = call(
        "POST",
        &capsule_url(&srv, "evil", "reader", "decide"),
        &token,
        Some(req.as_bytes()),
    );
    assert_eq!(st, 200, "in-sandbox read: {body}");
    assert!(
        body.contains("inside-data"),
        "expected file contents in result: {body}"
    );
}

#[test]
fn only_the_operator_may_open_private_networks() {
    let srv = boot("privnet");
    let token = tenant_admin_token(&srv, "acme");
    let (st, body) = call(
        "POST",
        &capsule_url(&srv, "acme", "c", "install"),
        &token,
        Some(MAB_LYC),
    );
    assert_eq!(st, 200, "install: {body}");
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
}

#[test]
fn policy_writes_are_strict_and_audited() {
    let srv = boot("strict");
    let token = tenant_admin_token(&srv, "acme");
    let (st, body) = call(
        "POST",
        &capsule_url(&srv, "acme", "c", "install"),
        &token,
        Some(MAB_LYC),
    );
    assert_eq!(st, 200, "install: {body}");
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
    let audit = std::fs::read_to_string(
        srv.store
            .path()
            .join("tenants/acme/jobs/j/capsules/c/audit.jsonl"),
    )
    .unwrap_or_default();
    assert!(
        audit.contains("policy_updated"),
        "policy change must be audited: {audit}"
    );
}

/// Sum of per-option `tries` across the discrete context buckets in
/// memory.json — every applied feedback increments exactly one.
fn applied_feedback_count(srv: &Server, tenant: &str, capsule: &str) -> f64 {
    let path = srv.store.path().join(format!(
        "tenants/{tenant}/jobs/j/capsules/{capsule}/memory.json"
    ));
    let mem: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("memory.json")).unwrap();
    let mut total = 0.0;
    if let Some(strategies) = mem["strategies"].as_object() {
        for strat in strategies.values() {
            if let Some(contexts) = strat["contexts"].as_object() {
                for bucket in contexts.values() {
                    for stat in bucket["stats"].as_array().into_iter().flatten() {
                        total += stat["tries"].as_f64().unwrap_or(0.0);
                    }
                }
            }
        }
    }
    total
}

#[test]
fn concurrent_decides_do_not_drop_feedback_updates() {
    let srv = boot("lostupdate");
    let (st, body) = call(
        "POST",
        &capsule_url(&srv, "acme", "mab", "install"),
        &srv.admin_key,
        Some(MAB_LYC),
    );
    assert_eq!(st, 200, "install: {body}");

    const N: usize = 120;
    let mut ids = Vec::with_capacity(N);
    for _ in 0..N {
        let (st, body) = call(
            "POST",
            &format!("{}?learn=true", capsule_url(&srv, "acme", "mab", "decide")),
            &srv.admin_key,
            Some(b"{}"),
        );
        assert_eq!(st, 200, "decide: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        ids.push(v["decisionId"].as_str().unwrap().to_string());
    }
    let before = applied_feedback_count(&srv, "acme", "mab");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hammers: Vec<_> = (0..4)
        .map(|_| {
            let stop = stop.clone();
            let url = capsule_url(&srv, "acme", "mab", "decide");
            let key = srv.admin_key.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = call("POST", &url, &key, Some(b"{}"));
                }
            })
        })
        .collect();
    for id in &ids {
        let fb = serde_json::json!({"decisionId": id, "reward": 1.0}).to_string();
        let (st, body) = call(
            "POST",
            &capsule_url(&srv, "acme", "mab", "feedback"),
            &srv.admin_key,
            Some(fb.as_bytes()),
        );
        assert_eq!(st, 200, "feedback: {body}");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for h in hammers {
        h.join().unwrap();
    }
    let after = applied_feedback_count(&srv, "acme", "mab");
    assert_eq!(
        after - before,
        N as f64,
        "every acknowledged feedback must survive concurrent decides (before {before}, after {after})"
    );
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
