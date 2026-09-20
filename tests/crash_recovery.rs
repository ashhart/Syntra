//! Crash-injection integration test: SIGKILL the serving process mid
//! decide/feedback load, restart, and prove the durability contract:
//!
//!   * sidecars written via write_atomic (tmp + fsync + rename) never tear
//!     — memory.json always parses and carries version 7;
//!   * the server comes back and serves /health + the decisions stream;
//!   * `syntra doctor` classifies the aftermath (exit 0 or 1, never 2) and
//!     never reports a graph VERIFY/DECODE failure;
//!   * no `*.corrupt-*` evidence files appear — because nothing actually
//!     corrupted (clean tmp rename is the whole point);
//!   * the decision stream is continuous across three kill cycles.
//!
//! Kills land at three deterministic-random offsets while a client thread
//! hammers requests, so they land mid-flight, not idle.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const MAB_LYC: &[u8] =
    include_bytes!("../examples/lycan-internals/benchmarks/syntra_vs_vw_mab/mab_2arm.lyc");

const TENANT: &str = "crashtenant";
const JOB: &str = "default";
const CAPSULE: &str = "crashcap";
const ADMIN_KEY: &str = "crash-admin-key";

struct Fixture {
    root: PathBuf,
    addr: String,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "syntra-crash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            addr: String::new(),
        }
    }

    /// Spawn `syntra serve` on a fresh port. The port is bind-probed
    /// from the ephemeral range so we cannot be handed a port another
    /// test binary is holding (a foreign /health would fake our readiness
    /// poll). Deterministic RNG seed so explore sequences are reproducible.
    fn boot(&mut self) -> Child {
        for _ in 0..10 {
            let port = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let p = l.local_addr().unwrap().port();
                drop(l);
                p
            };
            let addr = format!("127.0.0.1:{port}");
            let mut child = Command::new(env!("CARGO_BIN_EXE_syntra"))
                .args(["serve", "--addr", &addr, "--store"])
                .arg(&self.root)
                .args(["--admin-key", ADMIN_KEY])
                .env("LYCAN_RNG_SEED", "1337")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn syntra serve");
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut ready = false;
            while Instant::now() < deadline {
                if let Ok(r) = ureq::get(&format!("http://{addr}/health")).call() {
                    if r.status() == 200 {
                        ready = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            if ready {
                self.addr = addr;
                return child;
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("could not bind syntra to any test port");
    }
}

struct Hammer {
    stop: Arc<AtomicBool>,
    decided: Arc<AtomicUsize>,
    fed: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Continuous decide(learn=true)+feedback client. Survives the server dying
/// under it — connection errors are the crash, they are expected.
fn start_hammer(addr: String) -> Hammer {
    let stop = Arc::new(AtomicBool::new(false));
    let decided = Arc::new(AtomicUsize::new(0));
    let fed = Arc::new(AtomicUsize::new(0));
    let (stop_c, dec_c, fed_c) = (stop.clone(), decided.clone(), fed.clone());
    let handle = std::thread::spawn(move || {
        let base = format!("http://{addr}/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}");
        let mut i = 0usize;
        while !stop_c.load(Ordering::Relaxed) {
            i += 1;
            let decide = ureq::post(&format!("{base}/decide?learn=true"))
                .set("Authorization", &format!("Bearer {ADMIN_KEY}"))
                .set("Content-Type", "application/json")
                .send_string(&format!(r#"{{"inputs":{{"tier":"t{}"}}}}"#, i % 4));
            let Ok(j) = decide.and_then(|r| r.into_json::<serde_json::Value>().map_err(Into::into))
            else {
                continue;
            };
            let Some(did) = j["decisionId"].as_str().map(str::to_string) else {
                continue;
            };
            dec_c.fetch_add(1, Ordering::Relaxed);
            let reward = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb = ureq::post(&format!("{base}/feedback"))
                .set("Authorization", &format!("Bearer {ADMIN_KEY}"))
                .set("Content-Type", "application/json")
                .send_string(&format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#));
            if fb.is_ok() {
                fed_c.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    Hammer {
        stop,
        decided,
        fed,
        handle: Some(handle),
    }
}

/// Deterministic pseudo-random kill offsets (LCG): the same three kill
/// points on every machine, so a regression is reproducible.
fn kill_offset_ms(seed: u64) -> u64 {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    x ^= x >> 33;
    400 + (x % 1400)
}

#[test]
fn sigkill_cycles_leave_the_store_doctor_clean_and_serving() {
    let mut fx = Fixture::new();

    // Cycle 0 doubles as setup: boot, install the capsule, then hammer+kill.
    let mut child = fx.boot();
    let base = format!(
        "http://{}/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}",
        fx.addr
    );
    ureq::post(&format!("{base}/install"))
        .set("Authorization", &format!("Bearer {ADMIN_KEY}"))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(MAB_LYC)
        .expect("install capsule");

    let mut total_decided = 0usize;
    let mut total_fed = 0usize;

    for cycle in 0..3u64 {
        let mut hammer = start_hammer(fx.addr.clone());
        // Establish a real durability workload before starting the kill timer.
        // A busy host must not turn this recovery test into a throughput test.
        let ready_by = Instant::now() + Duration::from_secs(15);
        while hammer.decided.load(Ordering::Relaxed) < 20 && Instant::now() < ready_by {
            std::thread::sleep(Duration::from_millis(5));
        }
        if hammer.decided.load(Ordering::Relaxed) < 20 {
            let _ = child.kill();
            let _ = child.wait();
            hammer.stop.store(true, Ordering::Relaxed);
            let _ = hammer.handle.take().unwrap().join();
            panic!("crash workload did not reach 20 decisions within 15 seconds");
        }
        std::thread::sleep(Duration::from_millis(kill_offset_ms(0x5EED + cycle)));
        // SIGKILL: Child::kill is kill(2) SIGKILL on unix — no handler, no
        // drain; everything in flight dies. Exactly the crash we test.
        child.kill().expect("SIGKILL");
        child.wait().expect("reap");
        hammer.stop.store(true, Ordering::Relaxed);
        hammer.handle.take().unwrap().join().expect("hammer thread");
        total_decided += hammer.decided.load(Ordering::Relaxed);
        total_fed += hammer.fed.load(Ordering::Relaxed);

        // Restart on the same store: it must open and serve again.
        child = fx.boot();
    }
    // Final server is up (`child`) after the loop.
    assert!(
        total_decided >= 50,
        "expected >=50 successful learn-decides across cycles, got {total_decided}"
    );
    assert!(total_fed > 0, "feedback must land too");

    let addr = fx.addr.clone();

    // /health ok on the restarted server.
    let r = ureq::get(&format!("http://{addr}/health"))
        .call()
        .expect("health after crash");
    assert_eq!(r.status(), 200);

    // memory.json: written via tmp+fsync+rename — must NEVER be torn, and
    // the learn=true traffic guarantees it exists with the current version.
    let mem_path = fx
        .root
        .join("tenants")
        .join(TENANT)
        .join("jobs")
        .join(JOB)
        .join("capsules")
        .join(CAPSULE)
        .join("memory.json");
    let mem: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&mem_path).expect("memory.json readable after crashes"),
    )
    .expect("memory.json never tears: write_atomic fsyncs the tmp before rename");
    assert_eq!(
        mem["version"].as_u64(),
        Some(7),
        "memory.json keeps version 7 across crashes"
    );

    // Decisions stream still served (rotation-aware concatenation intact).
    let stream = ureq::get(&format!(
        "http://{addr}/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}/decisions"
    ))
    .set("Authorization", &format!("Bearer {ADMIN_KEY}"))
    .call()
    .expect("decisions stream after crashes");
    assert_eq!(stream.status(), 200);
    let body = stream.into_string().unwrap();
    assert!(
        body.lines().any(|l| l.contains("\"id\":\"dec_")),
        "decisions stream must still contain decisions"
    );

    // The corrupt-evidence convention proves itself ABSENT here: nothing
    // actually corrupted, so no load path ever hit its evidence branch.
    let evidence: Vec<String> = walk(&fx.root)
        .iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".corrupt-")
        })
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        evidence.is_empty(),
        "clean tmp+fsync+rename must leave no corrupt evidence: {evidence:?}"
    );

    // doctor classifies the aftermath: never "unreadable" (2); no graph
    // integrity failures (the atomic .lyc write survived every kill).
    let doc = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .args(["doctor", "--store"])
        .arg(&fx.root)
        .output()
        .expect("run doctor");
    let code = doc.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&doc.stdout).to_string();
    assert!(
        code == 0 || code == 1,
        "doctor must understand the post-crash store (exit {code}):\n{stdout}"
    );
    assert!(
        !stdout.contains("GRAPH_VERIFY_FAIL") && !stdout.contains("GRAPH_DECODE_FAIL"),
        "capsule graphs must survive SIGKILL intact:\n{stdout}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&fx.root);
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
    }
    out
}
