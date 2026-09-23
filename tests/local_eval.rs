//! Local evaluation: SDKs decide in-process against a published model and
//! upload their decisions, which the server verifies by replay.
//!
//! - Uploaded decisions are logged with their propensities and learned from.
//! - A decision whose pmf, choice or probability was altered, or that names
//!   a model the server never published, is refused and audited.
//! - Retried uploads (decisions and rewards) are idempotent.
//! - A spec change publishes a new model tag even though the model version
//!   is unchanged, and decisions made under the old tag still verify.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use syntra::client::{LocalDecider, base64_decode};
use syntra::decision::{DecideInput, DecisionSpec, Engine};

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new(label: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "syntra-local-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
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
    key: String,
    _store: TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn boot(label: &str) -> Server {
    let store = TempDir::new(label);
    let key = format!("local-admin-{}-{label}", std::process::id());
    for _ in 0..10 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
            .args(["serve", "--addr", &addr, "--store"])
            .arg(&store.0)
            .args(["--admin-key", &key])
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
                    key,
                    _store: store,
                };
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not boot syntra");
}

impl Server {
    fn url(&self, capsule: &str, tail: &str) -> String {
        format!(
            "http://{}/v1/tenants/t/jobs/j/capsules/{capsule}/{tail}",
            self.addr
        )
    }

    fn call(&self, method: &str, url: &str, body: Option<Value>) -> (u16, Value) {
        let req = ureq::request(method, url).set("Authorization", &format!("Bearer {}", self.key));
        let res = match body {
            Some(b) => req.send_string(&b.to_string()),
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
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn create(&self, capsule: &str, spec: Value) {
        let (st, body) = self.call("PUT", &self.url(capsule, "spec"), Some(spec));
        assert!(st == 200 || st == 201, "spec PUT: {st} {body}");
    }

    fn decider(&self, capsule: &str) -> LocalDecider {
        LocalDecider::connect(
            &format!("http://{}", self.addr),
            &self.key,
            "t",
            "j",
            capsule,
        )
        .expect("connect")
    }
}

fn three_actions() -> Value {
    json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}]})
}

#[test]
fn local_decisions_are_verified_logged_and_learned() {
    let srv = boot("happy");
    srv.create("router", three_actions());
    let decider = srv.decider("router");
    assert_eq!(decider.model_version(), 0);

    let mut ids = Vec::new();
    for i in 0..200 {
        let d = decider
            .decide(json!({"user": i % 7, "tier": "pro"}))
            .unwrap();
        assert!(d.probability > 0.0 && d.probability <= 1.0);
        let reward = if d.action == "b" { 1.0 } else { 0.0 };
        decider.reward(&d.decision_id, reward).unwrap();
        ids.push(d.decision_id);
    }
    assert_eq!(decider.pending(), 400);
    let report = decider.flush().unwrap();
    assert_eq!(report.decisions_accepted, 200, "{report:?}");
    assert_eq!(report.decisions_rejected, 0, "{report:?}");
    assert_eq!(report.rewards_applied, 200, "{report:?}");
    assert_eq!(report.rewards_failed, 0, "{report:?}");
    assert_eq!(decider.pending(), 0);

    // The server learned from every reward...
    let (st, model) = srv.call("GET", &srv.url("router", "model"), None);
    assert_eq!(st, 200);
    assert_eq!(model["modelVersion"], 200, "{model}");

    // ...and logged each decision with its propensity and reward.
    let (st, d) = srv.call(
        "GET",
        &srv.url("router", &format!("decisions/{}", ids[17])),
        None,
    );
    assert_eq!(st, 200, "{d}");
    let p = d["probability"].as_f64().expect("propensity logged");
    assert!(p > 0.0 && p <= 1.0, "{d}");
    assert_eq!(d["pmf"].as_array().map(|a| a.len()), Some(3), "{d}");
    assert_eq!(d["rewards"].as_array().map(|a| a.len()), Some(1), "{d}");

    // Syncing picks up the learned model (a new version is published at
    // most once a second).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !decider.sync().unwrap() {
        assert!(Instant::now() < deadline, "no new model was published");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(decider.model_version(), 200);

    // It prefers the rewarded action, and its decisions still verify.
    let mut b = 0;
    for i in 0..200 {
        let d = decider
            .decide(json!({"user": i % 7, "tier": "pro"}))
            .unwrap();
        assert_eq!(d.model_version, 200);
        if d.action == "b" {
            b += 1;
        }
    }
    assert!(b > 120, "learned model chose b only {b}/200 times");
    let report = decider.flush().unwrap();
    assert_eq!(report.decisions_accepted, 200, "{report:?}");
}

/// A real upload item for `capsule`, made the way an SDK makes one.
fn genuine_upload(srv: &Server, capsule: &str, id: &str, seed: u64) -> Value {
    let (st, m) = srv.call("GET", &srv.url(capsule, "model?snapshot=true"), None);
    assert_eq!(st, 200, "{m}");
    let spec = DecisionSpec::from_decide_json(&m["decide"]).unwrap();
    let bytes = base64_decode(m["snapshot"].as_str().unwrap()).unwrap();
    let engine = Engine::restore(spec, &bytes).unwrap();
    let context = json!({"user": 3});
    let input = DecideInput {
        context: context.clone(),
        ..DecideInput::default()
    };
    let d = engine.decide(&input, seed).unwrap();
    json!({
        "decisionId": id,
        "tsMs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        "modelTag": m["modelTag"],
        "modelVersion": m["modelVersion"],
        "seed": seed.to_string(),
        "input": {"context": context},
        "chosenIndex": d.chosen,
        "probability": d.probability,
        "pmf": d.pmf,
        "eligible": d.eligible,
    })
}

fn upload(srv: &Server, capsule: &str, items: Vec<Value>) -> Value {
    let (st, v) = srv.call(
        "POST",
        &srv.url(capsule, "decisions:batch"),
        Some(json!({ "decisions": items })),
    );
    assert_eq!(st, 200, "{v}");
    v
}

fn rejection(v: &Value) -> String {
    v["rejected"][0]["error"].as_str().unwrap_or("").to_string()
}

#[test]
fn altered_or_unpublished_uploads_are_refused() {
    let srv = boot("tamper");
    srv.create("c1", three_actions());

    let good = genuine_upload(&srv, "c1", "loc_good", 42);
    let v = upload(&srv, "c1", vec![good.clone()]);
    assert_eq!(v["accepted"], 1, "{v}");
    assert_eq!(v["duplicates"], 0, "{v}");

    // A retried upload of the same decision is accepted once.
    let v = upload(&srv, "c1", vec![good.clone()]);
    assert_eq!(v["accepted"], 1, "{v}");
    assert_eq!(v["duplicates"], 1, "{v}");

    // Reusing the id for different content is refused.
    let mut reuse = genuine_upload(&srv, "c1", "loc_good", 43);
    reuse["input"]["context"] = json!({"user": 4});
    let v = upload(&srv, "c1", vec![reuse]);
    assert!(rejection(&v).contains("already used"), "{v}");

    let base = genuine_upload(&srv, "c1", "loc_t1", 7);
    let chosen = base["chosenIndex"].as_u64().unwrap() as usize;

    // Inflated propensity for the chosen action.
    let mut pmf = base.clone();
    pmf["decisionId"] = json!("loc_pmf");
    let mut probs: Vec<f64> = serde_json::from_value(pmf["pmf"].clone()).unwrap();
    let other = (chosen + 1) % probs.len();
    probs[chosen] += 0.01;
    probs[other] -= 0.01;
    pmf["pmf"] = json!(probs);
    pmf["probability"] = json!(probs[chosen]);

    // A different action than the seed draws.
    let mut pick = base.clone();
    pick["decisionId"] = json!("loc_pick");
    pick["chosenIndex"] = json!(other);
    pick["probability"] = json!(base["pmf"][other]);

    // A model the server never published.
    let mut tag = base.clone();
    tag["decisionId"] = json!("loc_tag");
    tag["modelTag"] = json!("0123456789abcdef");

    // A decision dated a month ago.
    let mut old = base.clone();
    old["decisionId"] = json!("loc_old");
    old["tsMs"] = json!(base["tsMs"].as_i64().unwrap() - 30 * 24 * 3600 * 1000);

    let v = upload(&srv, "c1", vec![pmf, pick, tag, old, base]);
    assert_eq!(
        v["accepted"], 1,
        "only the unaltered decision is accepted: {v}"
    );
    let errors: Vec<(String, String)> = v["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["decisionId"].as_str().unwrap().to_string(),
                r["error"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let error_for = |id: &str| {
        errors
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, e)| e.clone())
            .unwrap_or_else(|| panic!("{id} was not rejected: {v}"))
    };
    assert!(error_for("loc_pmf").contains("does not replay"));
    assert!(error_for("loc_pick").contains("does not replay"));
    assert!(error_for("loc_tag").contains("not a published model"));
    assert!(error_for("loc_old").contains("tsMs"));

    // Refusals are audited.
    let (_, audits) = srv.call("GET", &srv.url("c1", "audits"), None);
    assert!(
        audits.to_string().contains("upload_rejected"),
        "no upload_rejected audit: {audits}"
    );
    // Stored decisions carry the replayed propensity.
    let (st, d) = srv.call("GET", &srv.url("c1", "decisions/loc_t1"), None);
    assert_eq!(st, 200, "{d}");
    let seed = d["seed"]
        .as_str()
        .map(String::from)
        .or_else(|| d["seed"].as_u64().map(|n| n.to_string()));
    assert_eq!(seed.as_deref(), Some("7"), "{d}");
}

#[test]
fn a_spec_change_publishes_a_new_tag_and_old_tags_still_verify() {
    let srv = boot("spec");
    srv.create("c2", three_actions());
    let decider = srv.decider("c2");
    let before = decider.model_tag();
    let d_old = decider.decide(json!({"x": 1})).unwrap();

    // Same model version, different exploration: a new tag at once.
    let (st, body) = srv.call(
        "PUT",
        &srv.url("c2", "spec"),
        Some(json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}],
                    "exploration": {"kind": "epsilonGreedy", "epsilon": 0.3}})),
    );
    assert!(st == 200 || st == 201, "{st} {body}");
    assert!(decider.sync().unwrap(), "spec change was not published");
    assert_ne!(decider.model_tag(), before);
    assert_eq!(decider.model_version(), 0);
    let d_new = decider.decide(json!({"x": 1})).unwrap();

    let report = decider.flush().unwrap();
    assert_eq!(report.decisions_accepted, 2, "{report:?}");
    for id in [&d_old.decision_id, &d_new.decision_id] {
        let (st, d) = srv.call("GET", &srv.url("c2", &format!("decisions/{id}")), None);
        assert_eq!(st, 200, "{d}");
    }
    let (_, d) = srv.call(
        "GET",
        &srv.url("c2", &format!("decisions/{}", d_new.decision_id)),
        None,
    );
    assert_eq!(d["mode"], "learner", "{d}");
}

#[test]
fn reward_uploads_are_idempotent() {
    let srv = boot("rewards");
    // `sum`: every distinct reward counts, retries must not.
    srv.create(
        "c3",
        json!({"actions": [{"id": "a"}, {"id": "b"}], "rewards": "sum"}),
    );
    let decider = srv.decider("c3");
    let d = decider.decide(json!({})).unwrap();
    decider.reward(&d.decision_id, 0.25).unwrap();
    decider.reward(&d.decision_id, 0.5).unwrap();
    let report = decider.flush().unwrap();
    assert_eq!(report.rewards_applied, 2, "{report:?}");

    // A client retrying a batch whose response it lost.
    let item = json!({"decisionId": d.decision_id, "reward": 1.0, "idempotencyKey": "k-1"});
    for expect_applied in [true, false] {
        let (st, v) = srv.call(
            "POST",
            &srv.url("c3", "rewards:batch"),
            Some(json!({"rewards": [item.clone()]})),
        );
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["results"][0]["ok"], true, "{v}");
        assert_eq!(v["results"][0]["applied"], expect_applied, "{v}");
    }
    let (_, model) = srv.call("GET", &srv.url("c3", "model"), None);
    assert_eq!(model["modelVersion"], 3, "{model}");

    // `first`: one reward per decision whatever key the caller sends.
    srv.create("c4", json!({"actions": [{"id": "a"}, {"id": "b"}]}));
    let decider = srv.decider("c4");
    let d = decider.decide(json!({})).unwrap();
    decider
        .reward_with(&d.decision_id, 1.0, Some("x"), None)
        .unwrap();
    decider
        .reward_with(&d.decision_id, 0.0, Some("y"), None)
        .unwrap();
    let report = decider.flush().unwrap();
    assert_eq!(report.rewards_applied, 2, "both answered ok: {report:?}");
    let (_, model) = srv.call("GET", &srv.url("c4", "model"), None);
    assert_eq!(
        model["modelVersion"], 1,
        "second reward must not apply: {model}"
    );
}

#[test]
fn a_decider_keeps_deciding_while_the_server_is_down() {
    let srv = boot("outage");
    srv.create("c5", three_actions());
    let decider = srv.decider("c5");
    let addr = srv.addr.clone();
    drop(srv);
    // No server: decisions still take microseconds, uploads fail and stay
    // queued.
    let started = Instant::now();
    for _ in 0..1000 {
        decider.decide(json!({"k": 1})).unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(decider.flush().is_err(), "{addr} should be unreachable");
    assert_eq!(decider.pending(), 1000);
}
