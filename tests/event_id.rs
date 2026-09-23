//! `eventId` idempotency under concurrency: many simultaneous decides with
//! one id log exactly one decision and all return it; a different request
//! with a used id is a 409.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const KEY: &str = "event-id-key";

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
        "syntra-eventid-{}-{}",
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

fn call(addr: &str, method: &str, tail: &str, body: Option<&Value>) -> (u16, Value) {
    let url = format!("http://{addr}/v1/tenants/t/jobs/j/capsules/c/{tail}");
    let req = ureq::request(method, &url).set("Authorization", &format!("Bearer {KEY}"));
    let res = match body {
        Some(b) => req.send_string(&b.to_string()),
        None => req.call(),
    };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => panic!("{method} {tail}: {e}"),
    };
    let status = resp.status();
    let mut text = String::new();
    resp.into_reader().read_to_string(&mut text).unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[test]
fn concurrent_decides_with_one_event_id_log_one_decision() {
    let srv = boot();
    let actions: Vec<Value> = (0..8).map(|i| json!({"id": format!("a{i}")})).collect();
    let (st, _) = call(&srv.addr, "PUT", "spec", Some(&json!({"actions": actions})));
    assert!(st == 200 || st == 201);

    let addr = Arc::new(srv.addr.clone());
    for round in 0..30 {
        let event_id = format!("evt-{round}");
        let body = Arc::new(json!({"eventId": event_id, "context": {"round": round}}));
        let handles: Vec<_> = (0..12)
            .map(|_| {
                let (addr, body) = (addr.clone(), body.clone());
                std::thread::spawn(move || call(&addr, "POST", "decide", Some(&body)))
            })
            .collect();
        let results: Vec<(u16, Value)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let first = &results[0].1;
        for (st, v) in &results {
            assert_eq!(*st, 200, "{v}");
            assert_eq!(v["decisionId"], first["decisionId"]);
            assert_eq!(
                v["action"], first["action"],
                "every caller sees the logged action"
            );
        }
        let (st, stored) = call(&srv.addr, "GET", &format!("decisions/{event_id}"), None);
        assert_eq!(st, 200);
        assert_eq!(stored["action"], first["action"]);
    }
    let (_, list) = call(&srv.addr, "GET", "decisions?limit=1000", None);
    assert_eq!(list["decisions"].as_array().unwrap().len(), 30);

    // Same id, different request: exactly one wins.
    let a = json!({"eventId": "evt-x", "context": {"v": 1}});
    let b = json!({"eventId": "evt-x", "context": {"v": 2}});
    let (addr_a, addr_b) = (addr.clone(), addr.clone());
    let ha = std::thread::spawn(move || call(&addr_a, "POST", "decide", Some(&a)).0);
    let hb = std::thread::spawn(move || call(&addr_b, "POST", "decide", Some(&b)).0);
    let mut codes = [ha.join().unwrap(), hb.join().unwrap()];
    codes.sort();
    assert_eq!(codes, [200, 409]);
}
