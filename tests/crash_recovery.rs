//! Crash recovery: three SIGKILLs at seeded pseudo-random points while
//! several clients decide and reward concurrently (some rewards
//! `durable: true`). After each crash and restart:
//!
//! - `syntra doctor` is clean, on the crashed store and on the live one;
//! - each capsule's model version equals the rewards committed to its log
//!   (every reward is learned in learner mode), so nothing is half-applied;
//! - every reward acknowledged with `durable: true`, in this cycle or an
//!   earlier one, is in the log;
//! - retried rewards for decisions caught by the crash answer 200 or 404,
//!   never a server error;
//! - decisions keep flowing, and each new reward moves the model by one.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};
use syntra::decision::SplitMix64;

const KEY: &str = "crash-suite-operator-key";
const T: &str = "acme";
const J: &str = "prod";
/// `often` snapshots every 25 updates (restart = snapshot + short replay);
/// `rarely` keeps the default 1000 (restart = replay of the whole log).
const CAPSULES: [&str; 2] = ["often", "rarely"];
const WORKERS: u64 = 4;
const CYCLES: usize = 3;
const MASTER_SEED: u64 = 0x5EED_C4A5;

/// What the clients saw before the crash.
#[derive(Default)]
struct Observed {
    /// (capsule, decision id) of rewards acknowledged with `durable: true`.
    durable: Vec<(&'static str, String)>,
    /// Decisions acknowledged whose reward was not acknowledged.
    unrewarded: Vec<(&'static str, String)>,
}

struct Load {
    stop: Arc<AtomicBool>,
    rewarded: Arc<AtomicU64>,
    observed: Arc<Mutex<Observed>>,
    workers: Vec<std::thread::JoinHandle<Result<(), String>>>,
}

fn request(
    agent: &ureq::Agent,
    addr: &str,
    target: &str,
    body: &Value,
) -> Result<HttpResponse, String> {
    try_http(
        agent,
        "POST",
        &format!("http://{addr}{target}"),
        &[("Authorization", &format!("Bearer {KEY}"))],
        Some(body.to_string().as_bytes()),
    )
}

/// Start `WORKERS` clients that decide and reward until stopped or until
/// the server goes away.
fn start_load(addr: &str, seed: u64) -> Load {
    let stop = Arc::new(AtomicBool::new(false));
    let rewarded = Arc::new(AtomicU64::new(0));
    let observed = Arc::new(Mutex::new(Observed::default()));
    let workers = (0..WORKERS)
        .map(|w| {
            let (stop, rewarded, observed) = (stop.clone(), rewarded.clone(), observed.clone());
            let addr = addr.to_string();
            let agent = agent();
            std::thread::spawn(move || -> Result<(), String> {
                let mut rng = SplitMix64::new(seed ^ (w + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                // Transport errors are expected once the server is killed;
                // the kill sets `stop` first, so any error seen while `stop`
                // is clear is a failure.
                let fail = |e: String| {
                    if stop.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err(e)
                    }
                };
                while !stop.load(Ordering::SeqCst) {
                    let c = CAPSULES[(rng.next_u64() % 2) as usize];
                    let ctx =
                        json!({"segment": rng.next_u64() % 3, "load": rng.next_f64(), "worker": w});
                    let d = match request(
                        &agent,
                        &addr,
                        &cap(T, J, c, "/decide"),
                        &json!({"context": ctx}),
                    ) {
                        Ok(r) if r.status == 200 => r.json(),
                        Ok(r) => return fail(format!("decide: {} {}", r.status, r.body)),
                        Err(e) => return fail(e),
                    };
                    let id = d["decisionId"].as_str().unwrap().to_string();
                    let durable = rng.next_u64().is_multiple_of(5);
                    let reward = if rng.next_f64() < 0.5 { 1.0 } else { 0.0 };
                    let body = json!({"decisionId": id, "reward": reward, "durable": durable});
                    match request(&agent, &addr, &cap(T, J, c, "/reward"), &body) {
                        Ok(r) if r.status == 200 => {
                            let v = r.json();
                            if v["applied"] != json!(true) {
                                return Err(format!("first reward not applied: {v}"));
                            }
                            rewarded.fetch_add(1, Ordering::SeqCst);
                            if durable {
                                observed.lock().unwrap().durable.push((c, id));
                            }
                        }
                        Ok(r) => return fail(format!("reward: {} {}", r.status, r.body)),
                        Err(e) => {
                            observed.lock().unwrap().unrewarded.push((c, id));
                            return fail(e);
                        }
                    }
                }
                Ok(())
            })
        })
        .collect();
    Load {
        stop,
        rewarded,
        observed,
        workers,
    }
}

fn capsule_info(srv: &Server, c: &str) -> (u64, u64, u64) {
    let v = srv.ok("GET", &cap(T, J, c, ""), None, 200);
    (
        v["modelVersion"].as_u64().unwrap(),
        v["stats"]["rewards"].as_u64().unwrap(),
        v["stats"]["decisions"].as_u64().unwrap(),
    )
}

fn assert_doctor_clean(store: &std::path::Path, when: &str) {
    let (code, findings) = doctor(store);
    assert_eq!(code, 0, "doctor {when}: {findings:?}");
    assert!(findings.is_empty(), "doctor {when}: {findings:?}");
}

#[test]
fn sigkill_under_load_never_loses_acknowledged_durable_rewards() {
    let work = TempDir::new("crash");
    let store = work.join("store");
    let mut srv = Server::start(&store, Some(KEY));
    srv.ok(
        "PUT",
        &cap(T, J, "often", "/spec"),
        Some(json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "learner": {"bits": 14}, "snapshotEvery": 25})),
        201,
    );
    srv.ok(
        "PUT",
        &cap(T, J, "rarely", "/spec"),
        Some(json!({"actions": [{"id": "x", "features": {"cost": 1}}, {"id": "y", "features": {"cost": 2}}], "learner": {"bits": 14}})),
        201,
    );

    let mut rng = SplitMix64::new(MASTER_SEED);
    let mut durable_acked: Vec<(&'static str, String)> = Vec::new();
    let mut acked_total = 0u64;
    for cycle in 0..CYCLES {
        // Seeded kill point: after this many acknowledged rewards, plus a
        // few milliseconds so the kill lands anywhere in a commit batch.
        let kill_after = 80 + rng.next_u64() % 160;
        let extra_ms = rng.next_u64() % 12;
        let load = start_load(&srv.addr, rng.next_u64());
        let deadline = Instant::now() + Duration::from_secs(60);
        while load.rewarded.load(Ordering::SeqCst) < kill_after {
            assert!(
                Instant::now() < deadline,
                "cycle {cycle}: load stalled at {} rewards",
                load.rewarded.load(Ordering::SeqCst)
            );
            if load.workers.iter().any(|w| w.is_finished()) {
                break; // a worker failed; surfaced by the join below
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(extra_ms));
        load.stop.store(true, Ordering::SeqCst);
        srv.kill9();
        for w in load.workers {
            w.join()
                .unwrap()
                .unwrap_or_else(|e| panic!("cycle {cycle}: {e}"));
        }
        let observed = std::mem::take(&mut *load.observed.lock().unwrap());
        acked_total += load.rewarded.load(Ordering::SeqCst);
        durable_acked.extend(observed.durable);
        assert!(
            store.join("syntra.db-wal").exists(),
            "cycle {cycle}: SIGKILL should leave the write-ahead log behind"
        );

        // The crashed store is consistent before anything touches it.
        assert_doctor_clean(&store, &format!("after crash {cycle}"));

        srv.restart();
        assert_doctor_clean(&store, &format!("after restart {cycle}"));

        let mut committed_total = 0;
        for c in CAPSULES {
            let (version, rewards, decisions) = capsule_info(&srv, c);
            assert_eq!(
                version, rewards,
                "cycle {cycle} {c}: model version must equal the learned rewards in the log"
            );
            assert!(rewards <= decisions, "cycle {cycle} {c}");
            committed_total += rewards;
        }
        // At most one in-flight reward per worker per cycle can be committed
        // without having been acknowledged.
        assert!(
            committed_total <= acked_total + WORKERS * (cycle as u64 + 1),
            "cycle {cycle}: {committed_total} rewards committed, {acked_total} acknowledged"
        );
        assert!(committed_total >= durable_acked.len() as u64);

        // Every durable acknowledgement ever given is still in the log.
        for (c, id) in &durable_acked {
            let (st, v) = srv.call("GET", &cap(T, J, c, &format!("/decisions/{id}")), None);
            assert_eq!(
                st, 200,
                "cycle {cycle}: durable reward's decision {c}/{id} lost: {v}"
            );
            assert_eq!(
                v["rewards"].as_array().map(Vec::len),
                Some(1),
                "cycle {cycle}: durable reward for {c}/{id} lost: {v}"
            );
        }

        // Rewards for decisions the crash interrupted: applied, duplicate
        // or 404 (the decision was in the lost tail); never a 5xx.
        for (c, id) in observed.unrewarded {
            let before = capsule_info(&srv, c).0;
            let (st, v) = srv.call(
                "POST",
                &cap(T, J, c, "/reward"),
                Some(json!({"decisionId": id, "reward": 1, "durable": true})),
            );
            match st {
                200 => {
                    let applied = v["applied"] == json!(true);
                    assert_eq!(capsule_info(&srv, c).0, before + u64::from(applied), "{v}");
                    acked_total += u64::from(applied);
                    durable_acked.push((c, id));
                }
                404 => {}
                _ => panic!("cycle {cycle}: retried reward for {c}/{id}: {st} {v}"),
            }
        }

        // Decisions keep flowing, and each reward moves the model by one.
        for c in CAPSULES {
            let (version, _, _) = capsule_info(&srv, c);
            for i in 0..5 {
                let d = srv.ok(
                    "POST",
                    &cap(T, J, c, "/decide"),
                    Some(json!({"context": {"segment": i, "load": 0.5}})),
                    200,
                );
                let r = srv.ok(
                    "POST",
                    &cap(T, J, c, "/reward"),
                    Some(json!({"decisionId": d["decisionId"], "reward": 1, "durable": i == 4})),
                    200,
                );
                assert_eq!(
                    r["modelVersion"],
                    json!(version + i + 1),
                    "cycle {cycle} {c}"
                );
            }
            acked_total += 5;
        }
    }
    println!(
        "{acked_total} rewards acknowledged over {CYCLES} crashes; {} durable",
        durable_acked.len()
    );

    // A graceful stop after the last cycle leaves a clean, stopped store.
    let versions: Vec<u64> = CAPSULES.iter().map(|c| capsule_info(&srv, c).0).collect();
    assert_eq!(srv.term(), Some(0));
    assert!(!store.join("syntra.db-wal").exists());
    assert_doctor_clean(&store, "after the final graceful stop");
    srv.restart();
    let after: Vec<u64> = CAPSULES.iter().map(|c| capsule_info(&srv, c).0).collect();
    assert_eq!(after, versions, "a graceful restart keeps every model");
}
