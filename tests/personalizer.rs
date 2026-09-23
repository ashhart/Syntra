//! Azure Personalizer-compatible API: rank, reward, activate and service
//! configuration, spoken exactly as Personalizer clients speak them.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const KEY: &str = "personalizer-test-key";

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
        "syntra-personalizer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&store).unwrap();
    boot_on(&store)
}

fn boot_on(store: &std::path::Path) -> Server {
    let store = store.to_path_buf();
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
    /// A request with Personalizer's key header.
    fn px(&self, key: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("http://{}{path}", self.addr);
        let req = ureq::request(method, &url)
            .set("Ocp-Apim-Subscription-Key", key)
            .set("Content-Type", "application/json");
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

    fn syntra(&self, method: &str, tail: &str, body: Option<Value>) -> (u16, Value) {
        self.px(
            KEY,
            method,
            &format!("/v1/tenants/t/jobs/j/capsules/news/{tail}"),
            body,
        )
    }

    /// A key bound to the capsule, as a Personalizer resource key.
    fn capsule_key(&self) -> String {
        let (st, v) = self.px(
            KEY,
            "POST",
            "/v1/admin/tokens",
            Some(
                json!({"scope": {"kind": "read", "tenant": "t", "job": "j", "capsule": "news"},
                        "label": "personalizer"}),
            ),
        );
        assert_eq!(st, 200, "{v}");
        v["token"].as_str().unwrap().to_string()
    }
}

fn rank_body(event_id: &str) -> Value {
    json!({
        "contextFeatures": [
            {"user": {"profileType": "AnonymousUser", "latLong": "47.6,-122.1"}},
            {"environment": {"dayOfMonth": "28", "monthOfYear": "8", "weather": "Sunny"}},
            {"device": {"mobile": true, "Windows": true}}
        ],
        "actions": [
            {"id": "NewsArticle", "features": [{"type": "News"}]},
            {"id": "SportsArticle", "features": [{"type": "Sports"}]},
            {"id": "EntertainmentArticle", "features": [{"type": "Entertainment"}]}
        ],
        "excludedActions": ["SportsArticle"],
        "eventId": event_id
    })
}

#[test]
fn rank_reward_and_activate_like_personalizer() {
    let srv = boot();
    let (st, _) = srv.syntra(
        "PUT",
        "spec",
        Some(json!({"actions": [], "exploration": {"kind": "epsilonGreedy", "epsilon": 0.2}})),
    );
    assert!(st == 200 || st == 201);
    let key = srv.capsule_key();

    // Rank.
    let (st, r) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/rank",
        Some(rank_body("75269AD0-BFEE-4598-8196-C57383D38E10")),
    );
    assert_eq!(st, 201, "{r}");
    assert_eq!(r["eventId"], "75269AD0-BFEE-4598-8196-C57383D38E10");
    let ranking = r["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 3, "{r}");
    assert_eq!(ranking[0]["id"], r["rewardActionId"]);
    assert_ne!(r["rewardActionId"], "SportsArticle");
    let last = ranking.last().unwrap();
    assert_eq!(last["id"], "SportsArticle");
    assert_eq!(last["probability"], 0.0);
    let total: f64 = ranking
        .iter()
        .map(|x| x["probability"].as_f64().unwrap())
        .sum();
    assert!((total - 1.0).abs() < 1e-9, "{r}");

    // The decision is logged with its propensity and the merged features.
    let (st, d) = srv.syntra(
        "GET",
        "decisions/75269AD0-BFEE-4598-8196-C57383D38E10",
        None,
    );
    assert_eq!(st, 200, "{d}");
    assert_eq!(d["context"]["environment"]["weather"], "Sunny");
    assert_eq!(d["context"]["device"]["mobile"], true);

    // Retrying rank with the same eventId returns the same answer.
    let (st, again) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/rank",
        Some(rank_body("75269AD0-BFEE-4598-8196-C57383D38E10")),
    );
    assert_eq!(st, 201);
    assert_eq!(again["rewardActionId"], r["rewardActionId"]);

    // Reward: 204 and learned.
    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/75269AD0-BFEE-4598-8196-C57383D38E10/reward",
        Some(json!({"value": 1.0})),
    );
    assert_eq!(st, 204);
    let (_, model) = srv.syntra("GET", "model", None);
    assert_eq!(model["modelVersion"], 1);

    // Unknown event: Personalizer-shaped 404.
    let (st, e) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/nope/reward",
        Some(json!({"value": 1.0})),
    );
    assert_eq!(st, 404);
    assert_eq!(e["error"]["code"], "ResourceNotFound", "{e}");

    // Deferred activation: not logged, rewards held, applied on activate.
    let mut body = rank_body("deferred-1");
    body["deferActivation"] = json!(true);
    let (st, _) = srv.px(&key, "POST", "/personalizer/v1.0/rank", Some(body));
    assert_eq!(st, 201);
    let (st, _) = srv.syntra("GET", "decisions/deferred-1", None);
    assert_eq!(st, 404, "a deferred event is not logged before activation");
    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/deferred-1/reward",
        Some(json!({"value": 0.5})),
    );
    assert_eq!(st, 204);
    let (_, model) = srv.syntra("GET", "model", None);
    assert_eq!(model["modelVersion"], 1, "a held reward is not learned");
    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/deferred-1/activate",
        None,
    );
    assert_eq!(st, 204);
    let (st, d) = srv.syntra("GET", "decisions/deferred-1", None);
    assert_eq!(st, 200, "{d}");
    assert_eq!(d["rewards"][0]["reward"], 0.5, "{d}");
    let (_, model) = srv.syntra("GET", "model", None);
    assert_eq!(model["modelVersion"], 2);
    // Activating twice is harmless; an unknown event is a 404.
    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/deferred-1/activate",
        None,
    );
    assert_eq!(st, 204);
    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/never-ranked/activate",
        None,
    );
    assert_eq!(st, 404);

    // Bad input, in Personalizer's error shape.
    let (st, e) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/rank",
        Some(json!({"actions": []})),
    );
    assert_eq!(st, 400);
    assert_eq!(e["error"]["code"], "BadArgument", "{e}");
    let (st, e) = srv.px(
        "wrong-key",
        "POST",
        "/personalizer/v1.0/rank",
        Some(rank_body("x")),
    );
    assert_eq!(st, 401);
    assert!(e["error"]["code"].is_string(), "{e}");

    // The operator key is not bound to a capsule: use the capsule path.
    let (st, e) = srv.px(KEY, "POST", "/personalizer/v1.0/rank", Some(rank_body("y")));
    assert_eq!(st, 400, "{e}");
    let (st, r) = srv.syntra("POST", "personalizer/v1.0/rank", Some(rank_body("y")));
    assert_eq!(st, 201, "{r}");
}

#[test]
fn service_configuration_maps_onto_the_spec() {
    let srv = boot();
    let (st, _) = srv.syntra(
        "PUT",
        "spec",
        Some(json!({"actions": [{"id": "a"}, {"id": "b"}]})),
    );
    assert!(st == 200 || st == 201);

    let (st, c) = srv.syntra("GET", "personalizer/v1.0/configurations/service", None);
    assert_eq!(st, 200, "{c}");
    assert_eq!(c["rewardWaitTime"], "PT10M");
    assert_eq!(c["rewardAggregation"], "earliest");
    assert_eq!(c["learningMode"], "Online");
    assert!(c["defaultReward"].is_null());

    let (st, c) = srv.syntra(
        "PUT",
        "personalizer/v1.0/configurations/service",
        Some(
            json!({"rewardWaitTime": "PT30S", "defaultReward": 0, "rewardAggregation": "earliest",
                    "explorationPercentage": 0.3, "learningMode": "Apprentice",
                    "modelExportFrequency": "PT5M", "logRetentionDays": 90}),
        ),
    );
    assert_eq!(st, 200, "{c}");
    assert_eq!(c["rewardWaitTime"], "PT30S");
    assert_eq!(c["defaultReward"], 0.0);
    assert_eq!(c["learningMode"], "Apprentice");
    let (_, spec) = srv.syntra("GET", "spec", None);
    assert_eq!(spec["reward"]["waitSeconds"], 30);
    assert_eq!(spec["reward"]["default"], 0.0);
    assert_eq!(spec["exploration"]["kind"], "epsilonGreedy");
    assert_eq!(spec["exploration"]["epsilon"], 0.3);
    assert_eq!(spec["mode"], "baselineExplore");

    // Apprentice mode: the first action is the baseline, served ~90% of
    // the time (baselineEpsilon 0.1).
    let mut first = 0;
    for i in 0..200 {
        let (st, r) = srv.syntra(
            "POST",
            "personalizer/v1.0/rank",
            Some(json!({"actions": [{"id": "b"}, {"id": "a"}], "contextFeatures": [{"i": i}]})),
        );
        assert_eq!(st, 201, "{r}");
        if r["rewardActionId"] == "b" {
            first += 1;
        }
    }
    assert!(first > 160, "baseline served {first}/200 times");

    for bad in [
        json!({"rewardWaitTime": "10 minutes"}),
        json!({"rewardAggregation": "average"}),
        json!({"learningMode": "LoggingOnly"}),
        json!({"explorationPercentage": 2}),
    ] {
        let (st, e) = srv.syntra(
            "PUT",
            "personalizer/v1.0/configurations/service",
            Some(bad.clone()),
        );
        assert_eq!(st, 400, "{bad} -> {e}");
        assert_eq!(e["error"]["code"], "BadArgument");
    }
}

#[test]
fn deferred_events_survive_a_graceful_restart() {
    let mut srv = boot();
    let (st, _) = srv.syntra(
        "PUT",
        "spec",
        Some(json!({"actions": [], "exploration": {"kind": "epsilonGreedy", "epsilon": 0.2}})),
    );
    assert!(st == 200 || st == 201);
    let key = srv.capsule_key();
    let mut body = rank_body("deferred-across-restart");
    body["deferActivation"] = json!(true);
    let (st, _) = srv.px(&key, "POST", "/personalizer/v1.0/rank", Some(body));
    assert_eq!(st, 201);
    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/deferred-across-restart/reward",
        Some(json!({"value": 0.75})),
    );
    assert_eq!(st, 204);

    // Graceful stop (SIGTERM), then a new process on the same store.
    let status = Command::new("kill")
        .args(["-TERM", &srv.child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    while srv.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    let store = srv.store.clone();
    let restarted = boot_on(&store);
    // The new server owns the store now; the old one (already exited)
    // must not remove it on drop.
    let old = std::mem::replace(&mut srv, restarted);
    std::mem::forget(old);

    let (st, _) = srv.px(
        &key,
        "POST",
        "/personalizer/v1.0/events/deferred-across-restart/activate",
        None,
    );
    assert_eq!(st, 204);
    let (st, d) = srv.syntra("GET", "decisions/deferred-across-restart", None);
    assert_eq!(st, 200, "{d}");
    assert_eq!(d["rewards"][0]["reward"], 0.75, "{d}");
}
