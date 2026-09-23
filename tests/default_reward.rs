//! Default rewards: a decision left without a reward past
//! `reward.waitSeconds` receives `reward.default`, once, and a late real
//! reward is ignored under `first` but still counts under `sum`.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const KEY: &str = "default-reward-key";

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
        "syntra-default-{}-{}",
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
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("http://{}{path}", self.addr);
        let req = ureq::request(method, &url).set("Authorization", &format!("Bearer {KEY}"));
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
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    fn capsule(&self, c: &str, method: &str, tail: &str, body: Option<Value>) -> (u16, Value) {
        self.call(
            method,
            &format!("/v1/tenants/t/jobs/j/capsules/{c}/{tail}"),
            body,
        )
    }

    fn model_version(&self, c: &str) -> u64 {
        self.capsule(c, "GET", "model", None).1["modelVersion"]
            .as_u64()
            .unwrap()
    }

    fn wait_for_version(&self, c: &str, want: u64) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.model_version(c) < want {
            assert!(
                Instant::now() < deadline,
                "model version stuck at {} (want {want})",
                self.model_version(c)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn unrewarded_decisions_get_the_default_once() {
    let srv = boot();
    let spec = json!({"actions": [{"id": "a"}, {"id": "b"}],
                      "reward": {"range": [0, 1], "default": 0, "waitSeconds": 1}});
    let (st, body) = srv.capsule("clicks", "PUT", "spec", Some(spec));
    assert!(st == 200 || st == 201, "{body}");
    assert_eq!(body["reward"]["waitSeconds"], 1);

    let mut ids = Vec::new();
    for i in 0..20 {
        let (_, d) = srv.capsule(
            "clicks",
            "POST",
            "decide",
            Some(json!({"context": {"i": i}})),
        );
        ids.push(d["decisionId"].as_str().unwrap().to_string());
    }
    // Half are clicked.
    for id in ids.iter().step_by(2) {
        let (st, _) = srv.capsule(
            "clicks",
            "POST",
            "reward",
            Some(json!({"decisionId": id, "reward": 1})),
        );
        assert_eq!(st, 200);
    }
    // The other half receive the default after the wait.
    srv.wait_for_version("clicks", 20);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        srv.model_version("clicks"),
        20,
        "each decision is rewarded once"
    );

    let (_, d) = srv.capsule("clicks", "GET", &format!("decisions/{}", ids[1]), None);
    let rewards = d["rewards"].as_array().unwrap();
    assert_eq!(rewards.len(), 1, "{d}");
    assert_eq!(rewards[0]["reward"], 0.0);
    assert_eq!(rewards[0]["detail"]["default"], true);
    let (_, d) = srv.capsule("clicks", "GET", &format!("decisions/{}", ids[0]), None);
    assert_eq!(d["rewards"][0]["reward"], 1.0, "{d}");

    // A click reported after the wait is a duplicate under `first`.
    let (st, r) = srv.capsule(
        "clicks",
        "POST",
        "reward",
        Some(json!({"decisionId": ids[1], "reward": 1})),
    );
    assert_eq!(st, 200);
    assert_eq!(r["applied"], false, "{r}");
    assert_eq!(srv.model_version("clicks"), 20);

    let (_, metrics) = srv.call("GET", "/metrics", None);
    assert!(
        metrics
            .as_str()
            .unwrap_or("")
            .contains("syntra_default_rewards_total 10"),
        "{metrics}"
    );
}

#[test]
fn under_sum_a_late_reward_still_counts() {
    let srv = boot();
    let spec = json!({"actions": [{"id": "a"}], "rewards": "sum",
                      "reward": {"default": 0.25, "waitSeconds": 1}});
    let (st, _) = srv.capsule("sum", "PUT", "spec", Some(spec));
    assert!(st == 200 || st == 201);
    let (_, d) = srv.capsule("sum", "POST", "decide", Some(json!({"context": {}})));
    let id = d["decisionId"].as_str().unwrap().to_string();
    srv.wait_for_version("sum", 1);
    let (_, r) = srv.capsule(
        "sum",
        "POST",
        "reward",
        Some(json!({"decisionId": id, "reward": 1})),
    );
    assert_eq!(r["applied"], true, "{r}");
    assert_eq!(srv.model_version("sum"), 2);
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(srv.model_version("sum"), 2, "the default applies once");
}

#[test]
fn reward_wait_is_validated() {
    let srv = boot();
    for bad in [
        json!({"actions": [{"id": "a"}], "reward": {"waitSeconds": 0}}),
        json!({"actions": [{"id": "a"}], "reward": {"waitSeconds": 999999999}}),
        json!({"actions": [{"id": "a"}], "reward": {"default": "zero"}}),
    ] {
        let (st, body) = srv.capsule("v", "PUT", "spec", Some(bad.clone()));
        assert_eq!(st, 400, "{bad} -> {body}");
    }
}
