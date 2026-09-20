//! Regression tests for the 2026-09-07 bug review:
//! - BUG-1: invalid feedback must not advance the warmup lifecycle
//! - BUG-3: decisionId lookup must match the exact `id`, not a substring
//! - BUG-4: read-scoped tokens must not mutate policy via `?learn=true`
//! - BUG-5 (2026-09-08): feedback target must resolve to a bound choice node

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ROUTER_LYC: &[u8] = include_bytes!("../examples/demo_llm_model_router.lyc");

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
            "syntra-bugfix-{label}-{}-{}",
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

fn pick_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static SEQ: AtomicU16 = AtomicU16::new(0);
    19_500 + (std::process::id() as u16 % 200) * 10 + SEQ.fetch_add(1, Ordering::Relaxed) % 10
}

fn boot_server(label: &str) -> Server {
    let store = TempDir::new(label);
    let admin_key = format!(
        "test-admin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    for _ in 0..10 {
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("serve")
            .arg("--addr")
            .arg(&addr)
            .arg("--store")
            .arg(store.path())
            .arg("--admin-key")
            .arg(&admin_key)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn syntra");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut ready = false;
        while Instant::now() < deadline {
            if let Ok(resp) = ureq::get(&format!("http://{addr}/health")).call() {
                if resp.status() == 200 {
                    ready = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        if ready {
            return Server {
                child,
                addr,
                admin_key,
                _store: store,
            };
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not bind syntra to any test port");
}

fn url(srv: &Server, path: &str) -> String {
    format!("http://{}{}", srv.addr, path)
}

fn install_router(srv: &Server) {
    let resp = ureq::post(&url(srv, "/tenants/demo/jobs"))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .send_string(r#"{"id":"bug","name":"bug","description":"repro"}"#)
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = ureq::post(&url(srv, "/tenants/demo/jobs/bug/capsules/router/install"))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .send(ROUTER_LYC)
        .unwrap();
    assert_eq!(resp.status(), 200);
}

fn issue_token(srv: &Server, scope: serde_json::Value) -> String {
    let resp = ureq::post(&url(srv, "/admin/tokens"))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .send_string(&serde_json::json!({ "scope": scope }).to_string())
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["token"].as_str().unwrap().to_string()
}

fn post_status(srv: &Server, path: &str, body: &str, token: &str) -> u16 {
    match ureq::post(&url(srv, path))
        .set("Authorization", &format!("Bearer {token}"))
        .send_string(body)
    {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(e) => panic!("transport error: {e}"),
    }
}

#[test]
fn bogus_feedback_does_not_advance_warmup() {
    let srv = boot_server("warmup-bug1");
    install_router(&srv);

    // 30 feedbacks with decisionIds that do not exist — every one 404s.
    for i in 0..30 {
        let code = post_status(
            &srv,
            "/tenants/demo/jobs/bug/capsules/router/feedback",
            &format!(r#"{{"decisionId":"dec_bogus_{i}","reward":1.0}}"#),
            &srv.admin_key,
        );
        assert_eq!(code, 404, "bogus decisionId must 404");
    }

    let resp = ureq::get(&url(&srv, "/tenants/demo/jobs/bug/capsules/router/report"))
        .set("Authorization", &format!("Bearer {}", srv.admin_key))
        .call()
        .unwrap();
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let warmup = &v["warmup"];
    assert_eq!(
        warmup["state"], "warmup",
        "invalid feedback must not advance lifecycle; got {warmup}"
    );
    assert_eq!(
        warmup["collected"], 0,
        "no feedback should have been recorded"
    );
}

#[test]
fn read_token_cannot_mutate_policy_via_learn() {
    let srv = boot_server("read-learn-bug4");
    install_router(&srv);

    let tok = issue_token(
        &srv,
        serde_json::json!({
            "kind": "read", "tenant": "demo", "job": "bug", "capsule": "router"
        }),
    );

    // Read token + learn=true must be coerced to learn=false.
    let resp = ureq::post(&url(
        &srv,
        "/tenants/demo/jobs/bug/capsules/router/decide?learn=true",
    ))
    .set("Authorization", &format!("Bearer {tok}"))
    .send_string("{}")
    .unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["learned"], false,
        "read-scoped token must not be able to learn; response: {v}"
    );

    // Admin token + learn=true still learns.
    let resp = ureq::post(&url(
        &srv,
        "/tenants/demo/jobs/bug/capsules/router/decide?learn=true",
    ))
    .set("Authorization", &format!("Bearer {}", srv.admin_key))
    .send_string("{}")
    .unwrap();
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["learned"], true, "admin token must keep learn=true");
}

#[test]
fn find_decision_matches_exact_id_not_substring() {
    let store_dir = TempDir::new("store-bug3");
    let store = syntra::store::LycanStore::init(store_dir.path().to_str().unwrap()).unwrap();
    store.create_tenant("t").unwrap();
    store
        .create_job("t", "j", "j", "", &serde_json::json!({}))
        .unwrap();

    // Newest first: the long id is written last, so a substring scan (old
    // behavior) would return it for the short id.
    std::fs::create_dir_all(store.capsule_dir_in_job("t", "j", "c").unwrap()).unwrap();
    store
        .append_decision_log_in_job("t", "j", "c", r#"{"id":"dec_abc","x":1}"#)
        .unwrap();
    store
        .append_decision_log_in_job("t", "j", "c", r#"{"id":"dec_abcdef12345678","x":2}"#)
        .unwrap();

    let found = store
        .find_decision_in_job("t", "j", "c", "dec_abc")
        .unwrap()
        .expect("exact id must resolve");
    assert!(
        found.contains(r#""id":"dec_abc""#),
        "must return the exact-id decision, got: {found}"
    );

    // A non-existent id resolves to None.
    assert!(
        store
            .find_decision_in_job("t", "j", "c", "dec_zzz")
            .unwrap()
            .is_none()
    );
}

// BUG-5: `(feedback name reward)` targeting a name that is not bound to a
// choice/strategy node compiled to a bare LoadVar reference, and the runtime
// silently dropped the credit (fail-open). The compiler must refuse such
// programs (learning-semantics §4.2: Feedback targets AdaptiveChoice/Strategy
// nodes only).
#[test]
fn feedback_target_must_resolve_to_choice_node() {
    fn compile(src: &str) -> Result<syntra::graph::NeuralGraph, String> {
        let tokens = syntra::lexer::Lexer::new(src).tokenize().expect("tokenize");
        let program = syntra::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse");
        syntra::graph_compiler::GraphCompiler::new().compile(&program)
    }

    // Never-bound name → compile error.
    let err = compile("(feedback zzz 1.0)").expect_err("unbound feedback target must not compile");
    assert!(
        err.contains("'zzz' is not bound"),
        "unexpected error: {err}"
    );

    // Bound to a non-choice value → compile error.
    let err = compile("($ x 5)\n(feedback x 1.0)\nx")
        .expect_err("non-choice feedback target must not compile");
    assert!(
        err.contains("'x' is bound to a non-choice value"),
        "unexpected error: {err}"
    );

    // A `$`-bound choice remains the working pattern.
    compile("($ c (choice 0 1 2))\n(feedback c 1.0)\nc").expect("bound choice target must compile");
}
