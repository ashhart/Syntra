//! `syntra doctor` / `syntra backup` / `syntra restore` CLI tests.
//!
//! Fixtures are built with the library (LycanStore) for determinism; the
//! validators themselves run as the real binary (exit codes + stdout are
//! the contract). The corrupt-evidence convention (`<name>.corrupt-<ts>`)
//! is proven through the real store load paths.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAB_LYC: &[u8] =
    include_bytes!("../examples/lycan-internals/benchmarks/syntra_vs_vw_mab/mab_2arm.lyc");

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
}

fn temp_store(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "syntra-doctor-{label}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Healthy fixture: init + one installed capsule (current.lyc, manifest,
/// policy, job.json — exactly what the install write-path produces).
fn healthy_store(label: &str) -> PathBuf {
    let root = temp_store(label);
    let store = syntra::store::LycanStore::init(root.to_str().unwrap()).unwrap();
    store
        .install_capsule_bytes("acme", "cap1", MAB_LYC)
        .unwrap();
    root
}

fn capsule_dir(root: &Path) -> PathBuf {
    root.join("tenants")
        .join("acme")
        .join("jobs")
        .join("default")
        .join("capsules")
        .join("cap1")
}

fn doctor(store: &Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .args(["doctor", "--store"])
        .arg(store)
        .args(extra)
        .output()
        .expect("run syntra doctor");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn find_code(stdout: &str, code: &str) -> Option<serde_json::Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|j| j["code"].as_str() == Some(code))
}

// ── doctor ──

#[test]
fn doctor_healthy_store_exits_zero() {
    let root = healthy_store("healthy");
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 0, "healthy store must exit 0, findings:\n{stdout}");
    assert!(
        stdout.lines().any(|l| l.contains("\"type\":\"summary\"")),
        "summary line required:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_unreadable_store_exits_two() {
    // Missing root.
    let missing = temp_store("missing").join("nope");
    let (code, stdout, _) = doctor(&missing, &[]);
    assert_eq!(code, 2);
    assert!(find_code(&stdout, "STORE_UNREADABLE").is_some());

    // Root exists but is not a store (no tenants/) — the post-failed-restore
    // shape: must never be reported healthy.
    let empty = temp_store("empty");
    let (code, stdout) = (doctor(&empty, &[]).0, doctor(&empty, &[]).1);
    assert_eq!(code, 2, "a non-store root is unreadable, not healthy");
    assert!(find_code(&stdout, "STORE_UNREADABLE").is_some());
    let _ = std::fs::remove_dir_all(&empty);
    let _ = std::fs::remove_dir_all(missing.parent().unwrap());
}

#[test]
fn doctor_reports_corrupt_memory() {
    let root = healthy_store("memcorrupt");
    std::fs::write(
        capsule_dir(&root).join("memory.json"),
        b"{\"version\": 7, \"strate",
    )
    .unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 1);
    let f = find_code(&stdout, "MEMORY_UNPARSEABLE").expect("MEMORY_UNPARSEABLE finding");
    assert_eq!(f["severity"], "error");
    assert!(f["path"].as_str().unwrap().ends_with("memory.json"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corrupt_load_paths_log_and_preserve_evidence_before_reset() {
    // The corrupt-<ts> evidence convention, proven through the REAL store
    // load paths: a corrupt sidecar is copied to `<name>.corrupt-<secs>`
    // before the caller resets it to defaults — bounded to one copy, and
    // the reset itself still happens (availability preserved).
    let root = healthy_store("evidence");
    let cap = capsule_dir(&root);
    let store = syntra::store::LycanStore::open(root.to_str().unwrap()).unwrap();

    std::fs::write(cap.join("memory.json"), b"NOT JSON AT ALL {{{").unwrap();
    assert!(
        store.load_memory_in_job("acme", "default", "cap1").is_err(),
        "corrupt memory.json must stay Err so callers keep their reset path"
    );
    let copies: Vec<_> = std::fs::read_dir(&cap)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("memory.json.corrupt-")
        })
        .collect();
    assert_eq!(
        copies.len(),
        1,
        "exactly one evidence copy after first corrupt read"
    );
    assert_eq!(
        std::fs::read(copies[0].path()).unwrap(),
        b"NOT JSON AT ALL {{{",
        "evidence must be a byte-for-byte copy of the corrupt file"
    );

    // Repeat resets must not accumulate evidence files.
    assert!(store.load_memory_in_job("acme", "default", "cap1").is_err());
    let copies_after: Vec<_> = std::fs::read_dir(&cap)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("memory.json.corrupt-")
        })
        .collect();
    assert_eq!(
        copies_after.len(),
        1,
        "evidence is bounded to one copy per name"
    );

    // hierarchical_state.json takes the same path (its load returns None,
    // the caller re-initializes bandit weights from the spec).
    std::fs::write(cap.join("hierarchical_state.json"), b"{\"spec\": trun").unwrap();
    assert!(
        store
            .load_hierarchical_state_in_job("acme", "default", "cap1")
            .is_none()
    );
    assert!(
        std::fs::read_dir(&cap).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("hierarchical_state.json.corrupt-")),
        "corrupt hierarchical_state.json must leave evidence before reset"
    );

    // Doctor surfaces the evidence as findings.
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 1);
    assert!(find_code(&stdout, "CORRUPT_EVIDENCE").is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_reports_orphan_tmp_files() {
    let root = healthy_store("tmp");
    let cap = capsule_dir(&root);
    std::fs::write(cap.join("current.tmp"), b"half-written").unwrap();
    std::fs::write(cap.join("policy.tmp.4242.0"), b"half-written").unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 1);
    let hits: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("TMP_ORPHAN"))
        .collect();
    assert_eq!(
        hits.len(),
        2,
        "both tmp naming schemes must be flagged:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_detects_torn_jsonl_tail() {
    let root = healthy_store("torn");
    let cap = capsule_dir(&root);
    std::fs::write(
        cap.join("decision.jsonl"),
        b"{\"id\":\"dec_1\",\"ok\":true}\n{\"id\":\"dec_2\",\"part",
    )
    .unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 1);
    let f = find_code(&stdout, "JSONL_TORN_TAIL").expect("JSONL_TORN_TAIL finding");
    assert!(f["path"].as_str().unwrap().ends_with("decision.jsonl"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_detects_stale_evolve_lock() {
    let root = healthy_store("lock");
    // A pid that is guaranteed dead AND reaped: a zombie still answers
    // kill -0, so spawn a no-op child and wait() it before probing.
    let mut dead_child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let dead = dead_child.id();
    dead_child.wait().unwrap();
    std::fs::write(capsule_dir(&root).join(".evolve.lock"), dead.to_string()).unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 1);
    assert!(find_code(&stdout, "LOCK_STALE").is_some(), "{stdout}");

    // A lock naming a LIVE pid (this test process) is not a finding —
    // it means an evolve is genuinely running.
    std::fs::write(
        capsule_dir(&root).join(".evolve.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 0, "live lock must not be a finding:\n{stdout}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_warns_on_memory_version_drift() {
    let root = healthy_store("drift");
    std::fs::write(
        capsule_dir(&root).join("memory.json"),
        r#"{"version":3,"strategies":{}}"#,
    )
    .unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 1, "drift is a finding (fail-closed)");
    let f = find_code(&stdout, "MEMORY_VERSION_DRIFT").expect("drift finding");
    assert_eq!(f["severity"], "warn");
    assert!(f["detail"].as_str().unwrap().contains('3'));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_is_read_only() {
    // Doctor MUST NOT touch the store: every file's mtime+size (and the
    // file set itself) must be identical before and after a run — on a
    // fixture WITH findings, so the write temptation is real.
    let root = healthy_store("readonly");
    let cap = capsule_dir(&root);
    std::fs::write(cap.join("memory.json"), b"garbage{").unwrap();
    std::fs::write(cap.join("current.tmp"), b"x").unwrap();
    std::fs::write(cap.join(".evolve.lock"), b"999999").unwrap();

    fn snapshot(dir: &Path) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let m = e.metadata().unwrap();
                    out.push((p, m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
                }
            }
        }
        out.sort();
        out
    }
    let before = snapshot(&root);
    let (code1, _, _) = doctor(&root, &[]);
    let after = snapshot(&root);
    assert_eq!(code1, 1);
    assert_eq!(before, after, "doctor mutated the store");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn doctor_json_flag_emits_findings_only() {
    let root = healthy_store("jsonflag");
    std::fs::write(capsule_dir(&root).join("memory.json"), b"nope{").unwrap();
    let (code, stdout, _) = doctor(&root, &["--json"]);
    assert_eq!(code, 1);
    for line in stdout.lines() {
        let j: serde_json::Value = serde_json::from_str(line).expect("each --json line is JSON");
        assert_ne!(
            j["type"].as_str(),
            Some("summary"),
            "--json must omit the summary line"
        );
    }
    assert!(!stdout.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

// ── backup / restore CLI ──

fn syntra_json(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_syntra"))
        .args(args)
        .output()
        .expect("run syntra");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}
#[test]
fn backup_restore_round_trip_recovers_a_live_store() {
    let root = temp_store("roundtrip");
    let admin_key = format!("rt-admin-{}", unique_suffix());
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        format!("127.0.0.1:{p}")
    };

    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_syntra"))
            .args(["serve", "--addr", &addr, "--store"])
            .arg(&root)
            .arg("--admin-key")
            .arg(&admin_key)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };

    let boot = || {
        let child = spawn();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(r) = ureq::get(&format!("http://{addr}/health")).call() {
                if r.status() == 200 {
                    return child;
                }
            }
            assert!(std::time::Instant::now() < deadline, "server did not boot");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    let auth = |r: ureq::Request| r.set("Authorization", &format!("Bearer {admin_key}"));

    // Phase 1: install + learn so memory.json (v7) + decision logs exist.
    let srv = boot();
    let base = format!("http://{addr}/tenants/acme/jobs/default/capsules/rtcap");
    auth(ureq::post(&format!("{base}/install")))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(MAB_LYC)
        .expect("install");
    let d = auth(ureq::post(&format!("{base}/decide?learn=true")))
        .set("Content-Type", "application/json")
        .send_string(r#"{"inputs":{"x":1}}"#)
        .expect("decide")
        .into_json::<serde_json::Value>()
        .unwrap();
    let did = d["decisionId"].as_str().unwrap().to_string();
    auth(ureq::post(&format!("{base}/feedback")))
        .set("Content-Type", "application/json")
        .send_string(&format!(r#"{{"decisionId":"{did}","reward":1.0}}"#))
        .expect("feedback");
    drop(srv); // Drop kills the child; writes completed before responses returned.
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Backup.
    let bundle = temp_store("bundle").join("bundle.json");
    let (code, out, err) = syntra_json(&[
        "backup",
        "--store",
        root.to_str().unwrap(),
        "--out",
        bundle.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "backup failed: {err}");
    let j: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(j["ok"], true);
    assert!(
        j["files"].as_u64().unwrap() > 5,
        "bundle should hold the store: {j}"
    );
    let body = std::fs::read(&bundle).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["v"],
        1
    );

    // Mutate: wipe the capsule tree entirely.
    std::fs::remove_dir_all(root.join("tenants").join("acme")).unwrap();
    let (code, stdout, _) = doctor(&root, &[]);
    // tenants/acme gone — store still walks; either way it is NOT 2.
    assert_ne!(code, 2, "doctored empty store: {stdout}");

    // Restore + verify recovery.
    let (code, out, err) = syntra_json(&[
        "restore",
        "--bundle",
        bundle.to_str().unwrap(),
        "--into",
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "restore failed: {err}");
    let j: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(j["ok"], true);

    let (code, stdout, _) = doctor(&root, &[]);
    assert_eq!(code, 0, "doctor must be clean after restore:\n{stdout}");

    // decide works again.
    let srv = boot();
    let d = auth(ureq::post(&format!("{base}/decide?learn=true")))
        .set("Content-Type", "application/json")
        .send_string(r#"{"inputs":{"x":1}}"#)
        .expect("decide after restore")
        .into_json::<serde_json::Value>()
        .unwrap();
    assert!(d["decisionId"].as_str().is_some());
    drop(srv);

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(bundle.parent().unwrap());
}

#[test]
fn restore_refuses_a_live_root_without_force() {
    let root = healthy_store("live");
    let bundle = temp_store("livebundle").join("b.json");
    let (code, _, err) = syntra_json(&[
        "backup",
        "--store",
        root.to_str().unwrap(),
        "--out",
        bundle.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");

    // Evidence 1: readiness probe present => serving (or crashed mid-/ready).
    std::fs::write(root.join(".readiness_probe"), b"").unwrap();
    let (code, _, err) = syntra_json(&[
        "restore",
        "--bundle",
        bundle.to_str().unwrap(),
        "--into",
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "restore into a probed root must be refused");
    assert!(err.contains("refusing restore into live root"), "{err}");
    std::fs::remove_file(root.join(".readiness_probe")).unwrap();

    // Evidence 2: an .evolve.lock naming a live pid (this process).
    std::fs::write(
        capsule_dir(&root).join(".evolve.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    let (code, _, err) = syntra_json(&[
        "restore",
        "--bundle",
        bundle.to_str().unwrap(),
        "--into",
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "restore into a lock-held root must be refused");
    assert!(err.contains("live pid"), "{err}");

    // --force overrides.
    let (code, out, err) = syntra_json(&[
        "restore",
        "--bundle",
        bundle.to_str().unwrap(),
        "--into",
        root.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(code, 0, "--force restore must proceed: {err}");
    assert!(out.contains("\"ok\":true"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(bundle.parent().unwrap());
}
