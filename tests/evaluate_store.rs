//! `syntra evaluate --store`: off-policy evaluation straight from a
//! server's event store, read-only.
//!
//! - It reads a live store (WAL present) and a crashed one.
//! - Gates decide the exit code: a better candidate passes
//!   `lift.dr.lower`, a worse one fails it.
//! - It never writes: after a clean shutdown the database file is
//!   byte-identical and no `-wal`/`-shm` files appear.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct TempDir(PathBuf);
impl TempDir {
    fn new(label: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "syntra-eval-{label}-{}-{}",
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

const KEY: &str = "eval-store-key";

fn boot(store: &Path) -> (Child, String) {
    for _ in 0..10 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
            .args(["serve", "--addr", &addr, "--store"])
            .arg(store)
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
                return (child, addr);
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not boot syntra");
}

fn call(addr: &str, method: &str, tail: &str, body: Value) -> Value {
    let url = format!("http://{addr}/v1/tenants/t/jobs/j/capsules/c/{tail}");
    let resp = ureq::request(method, &url)
        .set("Authorization", &format!("Bearer {KEY}"))
        .send_string(&body.to_string())
        .unwrap_or_else(|e| panic!("{method} {tail}: {e}"));
    let mut text = String::new();
    resp.into_reader().read_to_string(&mut text).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Log `n` explored decisions whose reward is 1.0 for `b`, 0.5 for `a`
/// and 0.0 for `c`, with a little deterministic noise.
fn log_traffic(addr: &str, n: usize) {
    call(
        addr,
        "PUT",
        "spec",
        json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}],
               "exploration": {"kind": "epsilonGreedy", "epsilon": 0.6}}),
    );
    for i in 0..n {
        let d = call(addr, "POST", "decide", json!({"context": {"u": i % 4}}));
        let base = match d["action"].as_str().unwrap() {
            "b" => 1.0,
            "a" => 0.5,
            _ => 0.0,
        };
        let noise = ((i * 7919) % 101) as f64 / 1000.0 - 0.05;
        call(
            addr,
            "POST",
            "reward",
            json!({"decisionId": d["decisionId"], "reward": base + noise}),
        );
    }
}

struct Run {
    code: i32,
    report: Value,
    stderr: String,
}

fn evaluate(store: &Path, capsule: &str, policy: &str, gates: Option<&Path>) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_syntra"));
    cmd.arg("evaluate").arg("--store").arg(store).args([
        "--capsule",
        capsule,
        "--policy",
        policy,
        "--bootstrap",
        "200",
    ]);
    if let Some(g) = gates {
        cmd.arg("--gates").arg(g).arg("--fail-on-gate");
    }
    let out = cmd.output().expect("run syntra evaluate");
    Run {
        code: out.status.code().unwrap_or(-1),
        report: serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn gates_file(dir: &Path, name: &str, gates: &[&str]) -> PathBuf {
    let p = dir.join(format!("{name}.json"));
    std::fs::write(&p, json!({ "gates": gates }).to_string()).unwrap();
    p
}

#[test]
fn evaluates_a_live_store_and_gates_promotion() {
    let tmp = TempDir::new("live");
    let store = tmp.0.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let (mut server, addr) = boot(&store);
    log_traffic(&addr, 600);

    // While the server runs (WAL in use).
    let better = gates_file(&tmp.0, "better", &["lift.dr.lower >= 0.05", "ess >= 50"]);
    let run = evaluate(&store, "t/j/c", "constant:b", Some(&better));
    assert_eq!(run.code, 0, "{}\n{}", run.stderr, run.report);
    assert_eq!(run.report["data"]["rows"], 600, "{}", run.report);
    let dr = run.report["estimators"]["dr"]["estimate"].as_f64().unwrap();
    assert!((dr - 1.0).abs() < 0.1, "DR estimate of constant:b is {dr}");
    assert_eq!(run.report["gatesPassed"], true);

    let worse = gates_file(&tmp.0, "worse", &["lift.dr.lower >= 0"]);
    let run = evaluate(&store, "t/j/c", "constant:c", Some(&worse));
    assert_eq!(run.code, 1, "constant:c must fail its gate: {}", run.report);
    assert_eq!(run.report["gatesPassed"], false);

    // After a crash (WAL left behind).
    server.kill().unwrap();
    server.wait().unwrap();
    let run = evaluate(&store, "t/j/c", "constant:b", Some(&better));
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.report["data"]["rows"], 600);

    // Unknown capsule, bad capsule syntax, missing store.
    let run = evaluate(&store, "t/j/nope", "logged", None);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("no logged decisions"), "{}", run.stderr);
    let run = evaluate(&store, "t/j", "logged", None);
    assert_eq!(run.code, 2);
    let run = evaluate(&tmp.0.join("missing"), "t/j/c", "logged", None);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("does not exist"), "{}", run.stderr);
}

#[test]
fn evaluation_never_writes_to_the_store() {
    let tmp = TempDir::new("ro");
    let store = tmp.0.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let (mut server, addr) = boot(&store);
    log_traffic(&addr, 200);
    // A clean shutdown checkpoints and removes the WAL.
    let status = Command::new("kill")
        .args(["-TERM", &server.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    let db = store.join("syntra.db");
    let before = std::fs::read(&db).unwrap();
    assert!(!store.join("syntra.db-wal").exists());

    let run = evaluate(&store, "t/j/c", "logged", None);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.report["data"]["rows"], 200);

    assert_eq!(std::fs::read(&db).unwrap(), before, "syntra.db changed");
    assert!(
        !store.join("syntra.db-wal").exists(),
        "a -wal file appeared"
    );
    assert!(
        !store.join("syntra.db-shm").exists(),
        "a -shm file appeared"
    );
}
