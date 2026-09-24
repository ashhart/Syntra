//! `syntra doctor`, `syntra backup` and `syntra restore` as an operator
//! runs them: against stopped, crashed and live stores.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use common::*;
use serde_json::{Value, json};

const KEY: &str = "doctor-suite-operator-key";

const PROGRAM: &str = r#"
($ seg (!cap "runtime.inputGet" "segment"))
(!cap "runtime.publish" "features.vip" (? (== seg "vip") 1.0 0.0))
"#;

/// The capsules every store in this file gets.
const CAPSULES: [(&str, &str, &str); 3] = [
    ("acme", "j", "router"),
    ("acme", "j", "ranker"),
    ("globex", "j", "other"),
];

fn capsule_dir(root: &Path, (t, j, c): (&str, &str, &str)) -> PathBuf {
    root.join(format!("tenants/{t}/jobs/{j}/capsules/{c}"))
}

/// Specs, a program and a policy, decisions and rewards, and a scoped
/// token, through the API.
fn populate_app(app: &App) {
    app.put_spec(
        "acme",
        "j",
        "router",
        json!({"actions": [{"id": "small"}, {"id": "large"}], "learner": {"bits": 12}, "snapshotEvery": 7}),
    );
    app.put_spec(
        "acme",
        "j",
        "ranker",
        json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "learner": {"bits": 12}, "rewards": "sum"}),
    );
    app.put_spec(
        "globex",
        "j",
        "other",
        json!({"actions": [{"id": "x"}, {"id": "y"}]}),
    );
    let (st, v) = app.call_bytes(
        "POST",
        &cap("acme", "j", "router", "/install"),
        &compile_lycan(PROGRAM),
    );
    assert_eq!(st, 200, "{v}");
    app.ok(
        "PUT",
        &cap("acme", "j", "router", "/policy"),
        Some(json!({"allow_stdout": false, "max_execution_ms": 500})),
        200,
    );
    for (t, j, c) in CAPSULES {
        for i in 0..12 {
            let d = app.decide(
                t,
                j,
                c,
                json!({"context": {"segment": if i % 3 == 0 { "vip" } else { "std" }, "i": i}}),
            );
            app.reward(
                t,
                j,
                c,
                json!({"decisionId": d["decisionId"], "reward": (i % 2) as f64}),
            );
        }
    }
    app.issue_token(
        json!({"kind": "read", "tenant": "acme", "job": "j", "capsule": "router"}),
        None,
    );
}

/// A populated store at `root`, closed cleanly (no `-wal`, no `-shm`).
fn build_store(root: &Path) {
    {
        let app = App::at(root, Some(KEY));
        populate_app(&app);
    }
    assert!(!root.join("syntra.db-wal").exists());
    assert!(!root.join("syntra.db-shm").exists());
}

fn populate_server(srv: &Server) {
    srv.ok(
        "PUT",
        &cap("acme", "j", "router", "/spec"),
        Some(json!({"actions": [{"id": "small"}, {"id": "large"}], "learner": {"bits": 12}, "snapshotEvery": 7})),
        201,
    );
    srv.ok(
        "PUT",
        &cap("acme", "j", "ranker", "/spec"),
        Some(json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "learner": {"bits": 12}, "rewards": "sum"})),
        201,
    );
    srv.ok(
        "PUT",
        &cap("globex", "j", "other", "/spec"),
        Some(json!({"actions": [{"id": "x"}, {"id": "y"}]})),
        201,
    );
    let r = srv.http(
        "POST",
        &cap("acme", "j", "router", "/install"),
        Some(&compile_lycan(PROGRAM)),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    for (t, j, c) in CAPSULES {
        for i in 0..12 {
            let d = srv.ok(
                "POST",
                &cap(t, j, c, "/decide"),
                Some(json!({"context": {"segment": "vip", "i": i}})),
                200,
            );
            srv.ok(
                "POST",
                &cap(t, j, c, "/reward"),
                Some(json!({"decisionId": d["decisionId"], "reward": 1, "durable": i == 11})),
                200,
            );
        }
    }
}

/// Wait until nothing in the store changes for longer than the event
/// store's checkpoint interval (1 s), so a live server is idle.
fn wait_quiescent(root: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut last = fs_snapshot(root);
    loop {
        std::thread::sleep(Duration::from_millis(1300));
        let now = fs_snapshot(root);
        if now == last {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "store never went quiet"
        );
        last = now;
    }
}

fn sidecars(root: &Path) -> (bool, bool) {
    (
        root.join("syntra.db-wal").exists(),
        root.join("syntra.db-shm").exists(),
    )
}

// ───────────────────────────── doctor ────────────────────────────────────

#[test]
fn doctor_passes_a_clean_store_stopped_by_sigterm() {
    let work = TempDir::new("doctor-clean");
    let store = work.join("store");
    let mut srv = Server::start(&store, Some(KEY));
    populate_server(&srv);
    assert_eq!(srv.term(), Some(0), "graceful stop exits 0");
    assert!(
        !store.join("server.pid").exists(),
        "server.pid removed on stop"
    );
    assert_eq!(sidecars(&store), (false, false));

    let out = run(SYNTRA, &["doctor", "--store", store.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    assert_eq!(stdout(&out), "3 capsules checked: no problems found.\n");
    let out = run(
        SYNTRA,
        &["doctor", "--store", store.to_str().unwrap(), "--json"],
    );
    let lines: Vec<Value> = stdout(&out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines,
        vec![json!({"summary": true, "capsules": 3, "errors": 0, "warnings": 0})]
    );
    // The findings alone: none.
    let (code, findings) = doctor(&store);
    assert_eq!((code, findings.len()), (0, 0), "{findings:?}");
}

/// The contract in `src/doctor.rs`: nothing is created, written, renamed
/// or deleted, whether the store is live, crashed or stopped.
#[test]
fn doctor_is_read_only() {
    let work = TempDir::new("doctor-ro");
    let store = work.join("store");
    let mut srv = Server::start(&store, Some(KEY));
    populate_server(&srv);

    let check = |state: &str| {
        let before = fs_snapshot(&store);
        let (code, findings) = doctor(&store);
        assert_eq!(code, 0, "{state}: {findings:?}");
        let after = fs_snapshot(&store);
        let changed: Vec<_> = before
            .iter()
            .filter(|(p, meta)| after.get(*p) != Some(meta))
            .map(|(p, _)| p.clone())
            .chain(after.keys().filter(|p| !before.contains_key(*p)).cloned())
            .collect();
        assert!(changed.is_empty(), "{state}: doctor changed {changed:?}");
    };

    // Live: the server holds the database open (with -wal and -shm).
    wait_quiescent(&store);
    assert_eq!(sidecars(&store), (true, true));
    check("live");

    // Crashed: SIGKILL leaves the log, its index and a stale server.pid.
    srv.kill9();
    assert_eq!(sidecars(&store), (true, true));
    assert!(store.join("server.pid").exists());
    check("crashed");

    // A crashed store copied without its -shm (a common backup exclusion):
    // reading the log would create an index, so doctor says so instead.
    let copy = work.join("copy");
    copy_tree(&store, &copy);
    std::fs::remove_file(copy.join("syntra.db-shm")).unwrap();
    let before = fs_snapshot(&copy);
    let (code, findings) = doctor(&copy);
    assert_eq!(code, 1, "{findings:?}");
    assert_eq!(codes(&findings), vec!["event_store_unchecked"]);
    assert_eq!(fs_snapshot(&copy), before);
    assert!(!copy.join("syntra.db-shm").exists());

    // Stopped: a graceful stop checkpoints and removes the log.
    srv.restart();
    srv.ok(
        "POST",
        &cap("acme", "j", "router", "/decide"),
        Some(json!({"context": {}, "durable": true})),
        200,
    );
    assert_eq!(srv.term(), Some(0));
    assert_eq!(sidecars(&store), (false, false));
    check("stopped");
    assert_eq!(
        sidecars(&store),
        (false, false),
        "doctor created a -wal or -shm"
    );
}

type Damage = Box<dyn Fn(&Path)>;
/// (severity, code, path) of one expected finding.
type Want = (&'static str, &'static str, String);

/// Every kind of damage doctor knows, applied to copies of one pristine
/// store: exit 1 and exactly the expected finding codes (with the path of
/// each finding).
#[test]
fn doctor_reports_each_kind_of_damage() {
    let work = TempDir::new("doctor-damage");
    let pristine = work.join("pristine");
    build_store(&pristine);
    let router = |root: &Path| capsule_dir(root, CAPSULES[0]);
    let ranker = |root: &Path| capsule_dir(root, CAPSULES[1]);
    let r = "tenants/acme/jobs/j/capsules/router";
    let write = |p: PathBuf, text: &[u8]| std::fs::write(p, text).unwrap();

    let cases: Vec<(&str, Damage, Vec<Want>)> = vec![
        (
            "spec is not JSON",
            Box::new(move |root| write(router(root).join("spec.json"), b"{\"actions\": [")),
            vec![("error", "spec_unparseable", format!("{r}/spec.json"))],
        ),
        (
            "spec out of range",
            Box::new(move |root| {
                write(
                    router(root).join("spec.json"),
                    br#"{"actions":[{"id":"a"}],"exploration":{"floor":0}}"#,
                )
            }),
            vec![("error", "spec_invalid", format!("{r}/spec.json"))],
        ),
        (
            "spec with an unknown field",
            Box::new(move |root| {
                write(
                    router(root).join("spec.json"),
                    br#"{"actions":[{"id":"a"}],"explore":{}}"#,
                )
            }),
            vec![("error", "spec_invalid", format!("{r}/spec.json"))],
        ),
        (
            "spec missing",
            Box::new(move |root| std::fs::remove_file(ranker(root).join("spec.json")).unwrap()),
            vec![(
                "error",
                "spec_missing",
                "tenants/acme/jobs/j/capsules/ranker".to_string(),
            )],
        ),
        (
            "policy invalid",
            Box::new(move |root| {
                write(
                    router(root).join("policy.json"),
                    br#"{"allow_netwrok": true}"#,
                )
            }),
            vec![("error", "policy_invalid", format!("{r}/policy.json"))],
        ),
        (
            "program replaced by another valid program",
            Box::new(move |root| {
                write(
                    router(root).join("current.lyc"),
                    &compile_lycan("(!cap \"runtime.publish\" \"reason\" \"swapped\")"),
                )
            }),
            vec![("error", "program_hash_mismatch", format!("{r}/current.lyc"))],
        ),
        (
            "program is garbage",
            Box::new(move |root| write(router(root).join("current.lyc"), b"LYCNgarbage")),
            vec![
                ("error", "program_invalid", format!("{r}/current.lyc")),
                ("error", "program_hash_mismatch", format!("{r}/current.lyc")),
            ],
        ),
        (
            "orphan temp files",
            Box::new(move |root| {
                write(router(root).join("spec.tmp-4242-1f2e"), b"{}");
                write(root.join("store.tmp-4242-99"), b"{}");
            }),
            vec![
                ("warn", "orphan_temp_file", "store.tmp-4242-99".to_string()),
                (
                    "warn",
                    "orphan_temp_file",
                    format!("{r}/spec.tmp-4242-1f2e"),
                ),
            ],
        ),
        (
            "a b-tree page of syntra.db overwritten",
            Box::new(|root| {
                let p = root.join("syntra.db");
                let mut b = std::fs::read(&p).unwrap();
                assert!(b.len() >= 4 * 4096);
                for (i, byte) in b[2 * 4096..3 * 4096].iter_mut().enumerate() {
                    *byte = (i * 37 + 11) as u8;
                }
                std::fs::write(&p, b).unwrap();
            }),
            vec![("error", "event_store_corrupt", "syntra.db".to_string())],
        ),
        (
            "syntra.db header overwritten",
            Box::new(|root| {
                let p = root.join("syntra.db");
                let mut b = std::fs::read(&p).unwrap();
                b[..16].copy_from_slice(b"not a database!!");
                std::fs::write(&p, b).unwrap();
            }),
            vec![("error", "event_store_unreadable", "syntra.db".to_string())],
        ),
        (
            "tokens.json is not JSON",
            Box::new(move |root| write(root.join("tokens.json"), b"{\"v\":1,")),
            vec![("error", "tokens_unparseable", "tokens.json".to_string())],
        ),
        (
            "job.json missing",
            Box::new(|root| {
                std::fs::remove_file(root.join("tenants/globex/jobs/j/job.json")).unwrap()
            }),
            vec![(
                "warn",
                "job_meta_missing",
                "tenants/globex/jobs/j".to_string(),
            )],
        ),
        (
            "preserved corrupt evidence",
            Box::new(move |root| write(root.join("tokens.json.corrupt-1700000000"), b"x")),
            vec![(
                "warn",
                "corrupt_evidence",
                "tokens.json.corrupt-1700000000".to_string(),
            )],
        ),
    ];

    // Control: the pristine store is clean.
    assert_eq!(doctor(&pristine), (0, vec![]));
    for (i, (what, damage, want)) in cases.into_iter().enumerate() {
        let copy = work.join(&format!("case-{i}"));
        copy_tree(&pristine, &copy);
        damage(&copy);
        let (code, findings) = doctor(&copy);
        assert_eq!(code, 1, "{what}: {findings:?}");
        let mut got: Vec<(String, String, String)> = findings
            .iter()
            .map(|f| {
                (
                    f["severity"].as_str().unwrap().to_string(),
                    f["code"].as_str().unwrap().to_string(),
                    f["path"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        got.sort();
        let mut want: Vec<(String, String, String)> = want
            .into_iter()
            .map(|(s, c, p)| (s.to_string(), c.to_string(), p))
            .collect();
        want.sort();
        assert_eq!(got, want, "{what}: {findings:?}");
        assert!(
            findings
                .iter()
                .all(|f| !f["detail"].as_str().unwrap().is_empty()),
            "{what}: {findings:?}"
        );
        // The summary counts them, in JSON and in the readable output.
        let out = run(
            SYNTRA,
            &["doctor", "--store", copy.to_str().unwrap(), "--json"],
        );
        let summary: Value = serde_json::from_str(stdout(&out).lines().last().unwrap()).unwrap();
        let errors = want.iter().filter(|w| w.0 == "error").count();
        assert_eq!(summary["errors"], json!(errors), "{what}: {summary}");
        assert_eq!(summary["warnings"], json!(want.len() - errors), "{what}");
        let text = stdout(&run(SYNTRA, &["doctor", "--store", copy.to_str().unwrap()]));
        let last = text.lines().last().unwrap();
        assert!(last.contains(" checked: "), "{what}: {text}");
        let finding_lines = text
            .lines()
            .filter(|l| l.starts_with("error ") || l.starts_with("warn "))
            .count();
        assert_eq!(
            finding_lines,
            want.len(),
            "{what}: one line per finding:\n{text}"
        );
    }
}

#[test]
fn doctor_exits_2_when_there_is_no_store() {
    let work = TempDir::new("doctor-nostore");
    let empty = work.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let file = work.join("file");
    std::fs::write(&file, "x").unwrap();
    let old = work.join("old");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("store.json"), r#"{"format": 1}"#).unwrap();
    let garbled = work.join("garbled");
    std::fs::create_dir_all(&garbled).unwrap();
    std::fs::write(garbled.join("store.json"), "][").unwrap();
    let missing = work.join("missing");
    for (args, want) in [
        (
            vec!["doctor", "--store", missing.to_str().unwrap()],
            "is not a directory",
        ),
        (
            vec!["doctor", "--store", file.to_str().unwrap()],
            "is not a directory",
        ),
        (
            vec!["doctor", "--store", empty.to_str().unwrap()],
            "is not a Syntra store",
        ),
        (
            vec!["doctor", "--store", old.to_str().unwrap()],
            "store format 1",
        ),
        (
            vec!["doctor", "--store", garbled.to_str().unwrap()],
            "unreadable store marker",
        ),
        (vec!["doctor"], "--store <root> is required"),
        (
            vec!["doctor", "--store", empty.to_str().unwrap(), "--fix"],
            "unknown argument",
        ),
    ] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(want), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
    assert!(!missing.exists(), "doctor never creates the store");
    let out = run(SYNTRA, &["doctor", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("Usage: syntra doctor"));
}

// ───────────────────────────── backup / restore ──────────────────────────

/// Everything the API reports about one capsule's history.
#[derive(Debug, Clone, PartialEq)]
struct CapsuleState {
    spec: Value,
    program: Value,
    policy: Value,
    model_version: u64,
    decisions: u64,
    rewards: u64,
    /// decision id -> its rewards (seq, value, key).
    history: BTreeMap<String, Vec<(i64, f64, String)>>,
}

fn capsule_state(srv: &Server, (t, j, c): (&str, &str, &str)) -> CapsuleState {
    let info = srv.ok("GET", &cap(t, j, c, ""), None, 200);
    let mut history = BTreeMap::new();
    let mut after: Option<String> = None;
    loop {
        let q = match &after {
            Some(a) => format!("/decisions?limit=1000&after={a}"),
            None => "/decisions?limit=1000".to_string(),
        };
        let page = srv.ok("GET", &cap(t, j, c, &q), None, 200);
        for d in page["decisions"].as_array().unwrap() {
            let id = d["decisionId"].as_str().unwrap().to_string();
            let full = srv.ok("GET", &cap(t, j, c, &format!("/decisions/{id}")), None, 200);
            let rewards = full["rewards"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    (
                        r["seq"].as_i64().unwrap(),
                        r["reward"].as_f64().unwrap(),
                        r["idempotencyKey"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            history.insert(id, rewards);
        }
        match page["next"].as_str() {
            Some(n) => after = Some(n.to_string()),
            None => break,
        }
    }
    CapsuleState {
        spec: info["spec"].clone(),
        program: info["program"]["programSha256"].clone(),
        policy: srv.ok("GET", &cap(t, j, c, "/policy"), None, 200),
        model_version: info["modelVersion"].as_u64().unwrap(),
        decisions: info["stats"]["decisions"].as_u64().unwrap(),
        rewards: info["stats"]["rewards"].as_u64().unwrap(),
        history,
    }
}

fn syntra_json(args: &[&str]) -> Value {
    let out = run(SYNTRA, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: {}{}",
        stdout(&out),
        stderr(&out)
    );
    serde_json::from_str(stdout(&out).trim()).unwrap()
}

#[test]
fn backup_of_a_live_store_under_load_restores_everything() {
    let work = TempDir::new("backup-live");
    let src = work.join("src");
    let srv = Arc::new(Server::start(&src, Some(KEY)));
    populate_server(&srv);
    let token = srv.ok(
        "POST",
        "/v1/admin/tokens",
        Some(json!({"scope": {"kind": "read", "tenant": "acme", "job": "j", "capsule": "router"}})),
        200,
    )["token"]
        .as_str()
        .unwrap()
        .to_string();
    let before: Vec<CapsuleState> = CAPSULES.iter().map(|c| capsule_state(&srv, *c)).collect();

    // Keep deciding and rewarding on every capsule while the backup runs.
    let stop = Arc::new(AtomicBool::new(false));
    let served = Arc::new(AtomicU64::new(0));
    let load: Vec<_> = (0..3)
        .map(|n| {
            let (srv, stop, served) = (srv.clone(), stop.clone(), served.clone());
            std::thread::spawn(move || -> Result<(), String> {
                let mut i = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let (t, j, c) = CAPSULES[((n + i) % 3) as usize];
                    let body = json!({"context": {"segment": "std", "n": n, "i": i}}).to_string();
                    let d = srv.try_http("POST", &cap(t, j, c, "/decide"), Some(body.as_bytes()))?;
                    if d.status != 200 {
                        return Err(format!("decide: {} {}", d.status, d.body));
                    }
                    let rb = json!({"decisionId": d.json()["decisionId"], "reward": (i % 2) as f64, "durable": i.is_multiple_of(5)});
                    let r = srv.try_http("POST", &cap(t, j, c, "/reward"), Some(rb.to_string().as_bytes()))?;
                    if r.status != 200 {
                        return Err(format!("reward: {} {}", r.status, r.body));
                    }
                    served.fetch_add(1, Ordering::Relaxed);
                    i += 1;
                }
                Ok(())
            })
        })
        .collect();
    assert!(wait_until(Duration::from_secs(10), || served
        .load(Ordering::Relaxed)
        > 30));

    let bak = work.join("bak");
    let out = syntra_json(&[
        "backup",
        "--store",
        src.to_str().unwrap(),
        "--out",
        bak.to_str().unwrap(),
    ]);
    let during = served.load(Ordering::Relaxed);
    assert!(wait_until(Duration::from_secs(10), || served
        .load(Ordering::Relaxed)
        > during + 30));
    stop.store(true, Ordering::Relaxed);
    for h in load {
        h.join().unwrap().unwrap();
    }
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["eventStore"], json!(true));
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(bak.join("manifest.json")).unwrap()).unwrap();
    let listed: BTreeSet<&str> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for skipped in [
        "server.pid",
        "syntra.db",
        "syntra.db-wal",
        "syntra.db-shm",
        ".readiness_probe",
    ] {
        assert!(!listed.contains(skipped), "{skipped} must not be copied");
    }
    assert!(listed.contains("tokens.json") && listed.contains("store.json"));

    // Restore into a new root and serve it.
    let dst = work.join("dst");
    let v = syntra_json(&[
        "restore",
        "--from",
        bak.to_str().unwrap(),
        "--into",
        dst.to_str().unwrap(),
    ]);
    assert_eq!(v["ok"], json!(true));
    assert!(v["previousStore"].is_null());
    assert!(!dst.join("server.pid").exists());
    let srv2 = Server::start(&dst, Some(KEY));
    let all = |s: &Server| s.ok("GET", "/v1/admin/capsules", None, 200)["capsules"].clone();
    assert_eq!(all(&srv2), all(&srv), "every capsule comes back");
    for (c, old) in CAPSULES.iter().zip(&before) {
        let new = capsule_state(&srv2, *c);
        assert_eq!(new.spec, old.spec, "{c:?} spec");
        assert_eq!(new.program, old.program, "{c:?} program");
        assert_eq!(new.policy, old.policy, "{c:?} policy");
        for (id, rewards) in &old.history {
            assert_eq!(new.history.get(id), Some(rewards), "{c:?} decision {id}");
        }
        assert!(
            new.decisions >= old.decisions && new.rewards >= old.rewards,
            "{c:?}"
        );
        // The model is rebuilt from exactly the log that was backed up.
        let rewards_in_log: u64 = new.history.values().map(|r| r.len() as u64).sum();
        assert_eq!(new.rewards, rewards_in_log, "{c:?}");
        assert_eq!(
            new.model_version, new.rewards,
            "{c:?}: model version == learned rewards"
        );
        assert!(new.model_version >= old.model_version, "{c:?}");
    }
    // Tokens come back too.
    let a = agent();
    let who = try_http(
        &a,
        "GET",
        &srv2.url("/v1/auth/whoami"),
        &[("Authorization", &format!("Bearer {token}"))],
        None,
    )
    .unwrap();
    assert_eq!(who.status, 200);
    let (code, findings) = doctor(&dst);
    assert_eq!(code, 0, "{findings:?}");

    // With the load stopped, a backup of the (still live) store restores
    // the model bit for bit.
    srv.ok(
        "POST",
        &cap("acme", "j", "router", "/decide"),
        Some(json!({"context": {}, "durable": true})),
        200,
    );
    let bak2 = work.join("bak2");
    syntra_json(&[
        "backup",
        "--store",
        src.to_str().unwrap(),
        "--out",
        bak2.to_str().unwrap(),
    ]);
    let dst2 = work.join("dst2");
    syntra_json(&[
        "restore",
        "--from",
        bak2.to_str().unwrap(),
        "--into",
        dst2.to_str().unwrap(),
    ]);
    let srv3 = Server::start(&dst2, Some(KEY));
    for (t, j, c) in CAPSULES {
        let live = srv.ok("GET", &cap(t, j, c, "/model?snapshot=true"), None, 200);
        let restored = srv3.ok("GET", &cap(t, j, c, "/model?snapshot=true"), None, 200);
        assert_eq!(restored["modelVersion"], live["modelVersion"], "{c}");
        assert_eq!(
            restored["snapshot"], live["snapshot"],
            "{c}: model differs after restore"
        );
        assert_eq!(
            capsule_state(&srv3, (t, j, c)),
            capsule_state(&srv, (t, j, c)),
            "{c}"
        );
    }
}

#[test]
fn backup_refuses_bad_arguments() {
    let work = TempDir::new("backup-args");
    let store = work.join("store");
    build_store(&store);
    let full = work.join("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("x"), "x").unwrap();
    for (args, code, want) in [
        (
            vec!["backup", "--store", store.to_str().unwrap()],
            2,
            "Usage",
        ),
        (vec!["backup", "--out", full.to_str().unwrap()], 2, "Usage"),
        (
            vec![
                "backup",
                "--store",
                store.to_str().unwrap(),
                "--out",
                full.to_str().unwrap(),
            ],
            1,
            "not empty",
        ),
        (
            vec![
                "backup",
                "--store",
                full.to_str().unwrap(),
                "--out",
                work.join("o").to_str().unwrap(),
            ],
            1,
            "not a Syntra store",
        ),
        (
            vec!["restore", "--from", full.to_str().unwrap()],
            2,
            "Usage",
        ),
        (
            vec![
                "restore",
                "--from",
                full.to_str().unwrap(),
                "--into",
                work.join("r").to_str().unwrap(),
            ],
            1,
            "not a backup",
        ),
    ] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(code), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(want), "{args:?}: {}", stderr(&out));
    }
    assert!(!work.join("r").exists());
}

#[test]
fn restore_refuses_a_live_root_unless_forced() {
    let work = TempDir::new("restore-live");
    let src = work.join("src");
    build_store(&src);
    let bak = work.join("bak");
    syntra_json(&[
        "backup",
        "--store",
        src.to_str().unwrap(),
        "--out",
        bak.to_str().unwrap(),
    ]);

    // A running server's root.
    let live = work.join("live");
    let srv = Server::start(&live, Some(KEY));
    srv.ok(
        "PUT",
        &cap("live", "j", "c", "/spec"),
        Some(json!({"actions": [{"id": "a"}]})),
        201,
    );
    let out = run(
        SYNTRA,
        &[
            "restore",
            "--from",
            bak.to_str().unwrap(),
            "--into",
            live.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("refusing restore into live root")
            && err.contains(&srv.pid().to_string())
            && err.contains("--force"),
        "{err}"
    );
    assert_eq!(
        srv.ok("GET", "/v1/tenants", None, 200)["tenants"],
        json!(["live"])
    );
    assert!(
        capsule_dir(&live, ("live", "j", "c"))
            .join("spec.json")
            .exists()
    );
    drop(srv);

    // A root whose server.pid names a live process (this test) is refused
    // without --force and replaced with it; the old root is kept aside.
    let target = work.join("target");
    build_store(&target);
    std::fs::write(target.join("marker.txt"), "old root").unwrap();
    std::fs::write(target.join("server.pid"), std::process::id().to_string()).unwrap();
    let before = fs_snapshot(&target);
    let out = run(
        SYNTRA,
        &[
            "restore",
            "--from",
            bak.to_str().unwrap(),
            "--into",
            target.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        fs_snapshot(&target),
        before,
        "a refused restore changes nothing"
    );
    let v = syntra_json(&[
        "restore",
        "--from",
        bak.to_str().unwrap(),
        "--into",
        target.to_str().unwrap(),
        "--force",
    ]);
    let aside = PathBuf::from(v["previousStore"].as_str().unwrap());
    assert!(aside.starts_with(work.path()), "{aside:?}");
    assert_eq!(
        std::fs::read_to_string(aside.join("marker.txt")).unwrap(),
        "old root"
    );
    assert!(!target.join("marker.txt").exists());
    assert!(!target.join("server.pid").exists());
    assert_eq!(doctor(&target), (0, vec![]));

    // A stale server.pid (a process that has exited) does not block.
    let stale = work.join("stale");
    std::fs::create_dir_all(&stale).unwrap();
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    std::fs::write(stale.join("server.pid"), pid.to_string()).unwrap();
    syntra_json(&[
        "restore",
        "--from",
        bak.to_str().unwrap(),
        "--into",
        stale.to_str().unwrap(),
    ]);
    assert!(stale.join("store.json").exists());
}

#[test]
fn restore_refuses_a_tampered_backup() {
    let work = TempDir::new("restore-tamper");
    let src = work.join("src");
    build_store(&src);
    let bak = work.join("bak");
    syntra_json(&[
        "backup",
        "--store",
        src.to_str().unwrap(),
        "--out",
        bak.to_str().unwrap(),
    ]);
    let spec_rel = "files/tenants/acme/jobs/j/capsules/router/spec.json";

    let edit_manifest = |root: &Path, f: &dyn Fn(&mut Value)| {
        let p = root.join("manifest.json");
        let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        f(&mut m);
        std::fs::write(&p, m.to_string()).unwrap();
    };
    let cases: Vec<(&str, Damage, &str)> = vec![
        (
            "a spec edited after the backup",
            Box::new(move |b| {
                let p = b.join(spec_rel);
                let mut s = std::fs::read(&p).unwrap();
                s.push(b' ');
                std::fs::write(p, s).unwrap();
            }),
            "checksum mismatch",
        ),
        (
            "one byte of syntra.db flipped",
            Box::new(|b| {
                let p = b.join("syntra.db");
                let mut s = std::fs::read(&p).unwrap();
                let mid = s.len() / 2;
                s[mid] ^= 0x40;
                std::fs::write(p, s).unwrap();
            }),
            "syntra.db: checksum mismatch",
        ),
        (
            "a listed file missing",
            Box::new(|b| std::fs::remove_file(b.join("files/store.json")).unwrap()),
            "store.json",
        ),
        (
            "a manifest path escaping the store",
            Box::new(move |b| {
                edit_manifest(b, &|m| {
                    m["files"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"path": "../escape", "sha256": "00", "bytes": 0}))
                })
            }),
            "escapes the store",
        ),
        (
            "an unknown backup format",
            Box::new(move |b| edit_manifest(b, &|m| m["format"] = json!(99))),
            "backup format",
        ),
    ];
    // An existing root that a refused restore must leave alone.
    let existing = work.join("existing");
    build_store(&existing);
    for (i, (what, tamper, want)) in cases.into_iter().enumerate() {
        let copy = work.join(&format!("bak-{i}"));
        copy_tree(&bak, &copy);
        tamper(&copy);
        let fresh = work.join(&format!("fresh-{i}"));
        let out = run(
            SYNTRA,
            &[
                "restore",
                "--from",
                copy.to_str().unwrap(),
                "--into",
                fresh.to_str().unwrap(),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{what}");
        assert!(stderr(&out).contains(want), "{what}: {}", stderr(&out));
        assert!(!fresh.exists(), "{what}: nothing is created");
        let before = fs_snapshot(&existing);
        let out = run(
            SYNTRA,
            &[
                "restore",
                "--from",
                copy.to_str().unwrap(),
                "--into",
                existing.to_str().unwrap(),
                "--force",
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{what}");
        assert_eq!(
            fs_snapshot(&existing),
            before,
            "{what}: existing root untouched"
        );
    }
    // No staging directories are left behind.
    let leftovers: Vec<String> = std::fs::read_dir(work.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".restore-") || n.contains(".pre-restore-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // Control: the untouched backup restores.
    syntra_json(&[
        "restore",
        "--from",
        bak.to_str().unwrap(),
        "--into",
        work.join("ok").to_str().unwrap(),
    ]);
    assert_eq!(doctor(&work.join("ok")), (0, vec![]));
}
