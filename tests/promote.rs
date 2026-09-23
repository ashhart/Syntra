//! `POST .../evaluate` and `POST .../promote`: OPE over HTTP, and spec
//! changes that apply only when their gates pass.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const KEY: &str = "promote-test-key";

struct Server {
    child: Child,
    addr: String,
    store: std::path::PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.store);
    }
}

fn boot() -> Server {
    let store = std::env::temp_dir().join(format!(
        "syntra-promote-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&store).unwrap();
    for _ in 0..10 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
            .args(["serve", "--addr", &addr, "--store"])
            .arg(&store)
            .args(["--admin-key", KEY])
            .env("SYNTRA_RATE_LIMIT_RPS", "10000000")
            .env("SYNTRA_RATE_LIMIT_BURST", "10000000")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn syntra");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(r) = ureq::get(&format!("http://{addr}/health")).call()
                && r.status() == 200
            {
                return Server { child, addr, store };
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not boot syntra");
}

impl Server {
    fn call(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("http://{}{path}", self.addr);
        let req = ureq::request(method, &url).set("Authorization", &format!("Bearer {token}"));
        let res = match body {
            Some(b) => req.send_string(&b.to_string()),
            None => req.call(),
        };
        let resp = match res {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("{method} {path}: {e}"),
        };
        let status = resp.status();
        let mut text = String::new();
        resp.into_reader().read_to_string(&mut text).unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn capsule(&self, token: &str, method: &str, tail: &str, body: Option<Value>) -> (u16, Value) {
        self.call(
            token,
            method,
            &format!("/v1/tenants/t/jobs/j/capsules/c/{tail}"),
            body,
        )
    }
}

/// b pays 1.0, a 0.5, c 0.0; heavy exploration so every action is logged.
fn log_traffic(srv: &Server, n: usize) {
    let (st, body) = srv.capsule(
        KEY,
        "PUT",
        "spec",
        Some(json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}],
                    "exploration": {"kind": "epsilonGreedy", "epsilon": 0.6}})),
    );
    assert!(st == 200 || st == 201, "{st} {body}");
    for i in 0..n {
        let (_, d) = srv.capsule(
            KEY,
            "POST",
            "decide",
            Some(json!({"context": {"u": i % 4}})),
        );
        let reward = match d["action"].as_str().unwrap() {
            "b" => 1.0,
            "a" => 0.5,
            _ => 0.0,
        };
        let (st, _) = srv.capsule(
            KEY,
            "POST",
            "reward",
            Some(json!({"decisionId": d["decisionId"], "reward": reward})),
        );
        assert_eq!(st, 200);
    }
}

fn audit_events(srv: &Server) -> Vec<String> {
    let (_, v) = srv.capsule(KEY, "GET", "audits", None);
    v["audits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn evaluate_and_gated_promotion() {
    let srv = boot();
    log_traffic(&srv, 500);

    let (st, report) = srv.capsule(
        KEY,
        "POST",
        "evaluate",
        Some(json!({"policy": "constant:b", "gates": ["lift.dr.lower >= 0.05"], "bootstrap": 200})),
    );
    assert_eq!(st, 200, "{report}");
    assert_eq!(report["data"]["rows"], 500);
    assert_eq!(report["gatesPassed"], true, "{}", report["verdict"]);

    // Policies that would read server files are refused.
    for p in ["spec:/etc/passwd", "target-column"] {
        let (st, _) = srv.capsule(KEY, "POST", "evaluate", Some(json!({"policy": p})));
        assert_eq!(st, 400, "{p}");
    }
    let (st, _) = srv.capsule(
        KEY,
        "POST",
        "evaluate",
        Some(json!({"policy": "logged", "nope": 1})),
    );
    assert_eq!(st, 400);

    // A gate that cannot pass: 409, spec untouched, refusal audited.
    let (st, v) = srv.capsule(
        KEY,
        "POST",
        "promote",
        Some(json!({"spec": {"exploration": {"epsilon": 0.1}},
                    "gates": ["lift.dr.lower >= 5"], "bootstrap": 200})),
    );
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["promoted"], false);
    let (_, spec) = srv.capsule(KEY, "GET", "spec", None);
    assert_eq!(spec["exploration"]["epsilon"], 0.6);
    assert!(audit_events(&srv).contains(&"promotion_refused".to_string()));

    // The learned policy beats heavy exploration: promoted and audited.
    let (st, v) = srv.capsule(
        KEY,
        "POST",
        "promote",
        Some(json!({"spec": {"exploration": {"epsilon": 0.1}},
                    "gates": ["lift.dr.lower >= 0"], "bootstrap": 200})),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["promoted"], true);
    assert_eq!(v["spec"]["exploration"]["epsilon"], 0.1);
    let (_, spec) = srv.capsule(KEY, "GET", "spec", None);
    assert_eq!(spec["exploration"]["epsilon"], 0.1);
    assert!(audit_events(&srv).contains(&"spec_promoted".to_string()));

    // Promotion needs gates.
    let (st, _) = srv.capsule(
        KEY,
        "POST",
        "promote",
        Some(json!({"spec": {"exploration": {"epsilon": 0.2}}})),
    );
    assert_eq!(st, 400);
}

#[test]
fn data_tokens_may_neither_evaluate_nor_promote() {
    let srv = boot();
    log_traffic(&srv, 120);
    let (st, tok) = srv.call(
        KEY,
        "POST",
        "/v1/admin/tokens",
        Some(
            json!({"scope": {"kind": "read", "tenant": "t", "job": "j", "capsule": "c"},
                    "label": "sdk"}),
        ),
    );
    assert_eq!(st, 200, "{tok}");
    let token = tok["token"].as_str().unwrap().to_string();

    // Evaluation is expensive; data-plane keys cannot start one.
    let (st, _) = srv.capsule(
        &token,
        "POST",
        "evaluate",
        Some(json!({"policy": "logged", "bootstrap": 0})),
    );
    assert_eq!(st, 403);
    let (st, _) = srv.capsule(
        &token,
        "POST",
        "promote",
        Some(json!({"spec": {"exploration": {"epsilon": 0.1}}, "gates": ["lift.dr.lower >= 0"]})),
    );
    assert_eq!(st, 403);
}
