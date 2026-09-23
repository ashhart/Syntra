//! `syntra doctor`: read-only store validator.
//!
//! READ-ONLY CONTRACT: nothing here creates, writes, renames or deletes a
//! file. The event store is opened with SQLite's read-only flag, so not even
//! a `-wal` or `-shm` file appears. Pinned by
//! `tests/doctor_cli.rs::doctor_is_read_only` (an mtime and size snapshot of
//! the whole store before and after a run).
//!
//! Output: one JSON line per finding on stdout,
//! `{"severity","path","code","detail"}`, then a summary line (suppressed by
//! `--json`). Exit codes: 0 no findings, 1 findings, 2 the store is
//! unreadable or not a store.

use std::path::Path;

use serde_json::json;

use crate::decision::DecisionSpec;
use crate::store::{STORE_FORMAT, sha256_hex, validate_name};

#[derive(Debug)]
pub struct Finding {
    pub severity: &'static str,
    pub path: String,
    pub code: String,
    pub detail: String,
}

impl Finding {
    fn line(&self) -> String {
        json!({ "severity": self.severity, "path": self.path, "code": self.code, "detail": self.detail })
            .to_string()
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub capsules: usize,
}

impl Report {
    fn add(
        &mut self,
        severity: &'static str,
        path: impl Into<String>,
        code: &str,
        detail: impl Into<String>,
    ) {
        self.findings.push(Finding {
            severity,
            path: path.into(),
            code: code.to_string(),
            detail: detail.into(),
        });
    }
}

pub fn cli_doctor(args: &[String]) {
    let mut store: Option<String> = None;
    let mut json_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--store" => {
                i += 1;
                store = args.get(i).cloned();
            }
            "--json" => json_only = true,
            "--help" | "-h" => {
                eprintln!("Usage: syntra doctor --store <root> [--json]");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(store) = store else {
        eprintln!("syntra doctor: --store <root> is required");
        std::process::exit(2);
    };
    match validate(Path::new(&store)) {
        Ok(report) => {
            for f in &report.findings {
                println!("{}", f.line());
            }
            if !json_only {
                println!(
                    "{}",
                    json!({
                        "summary": true,
                        "capsules": report.capsules,
                        "errors": report.findings.iter().filter(|f| f.severity == "error").count(),
                        "warnings": report.findings.iter().filter(|f| f.severity == "warn").count(),
                    })
                );
            }
            std::process::exit(if report.findings.is_empty() { 0 } else { 1 });
        }
        Err(e) => {
            eprintln!("syntra doctor: {e}");
            std::process::exit(2);
        }
    }
}

/// Validate a store without modifying it.
pub fn validate(root: &Path) -> Result<Report, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    let marker = root.join("store.json");
    let text = std::fs::read_to_string(&marker).map_err(|e| {
        format!(
            "{} is not a Syntra store ({}: {e})",
            root.display(),
            marker.display()
        )
    })?;
    let format = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("format").and_then(|f| f.as_u64()))
        .ok_or_else(|| format!("{}: unreadable store marker", marker.display()))?;
    if format != STORE_FORMAT {
        return Err(format!(
            "store format {format}; this build reads {STORE_FORMAT}"
        ));
    }
    let mut rep = Report::default();

    if root.join("tokens.json").exists() && !parses_json(&root.join("tokens.json")) {
        rep.add(
            "error",
            "tokens.json",
            "tokens_unparseable",
            "tokens.json is not valid JSON",
        );
    }
    check_event_store(root, &mut rep);
    check_temp_files(root, root, &mut rep);

    let tenants = root.join("tenants");
    for t in dir_entries(&tenants) {
        let tn = name_of(&t);
        if validate_name(&tn).is_err() {
            rep.add(
                "warn",
                rel(root, &t),
                "bad_name",
                "directory name is not a valid tenant name",
            );
            continue;
        }
        for j in dir_entries(&t.join("jobs")) {
            if !j.join("job.json").exists() {
                rep.add(
                    "warn",
                    rel(root, &j),
                    "job_meta_missing",
                    "job directory has no job.json",
                );
            } else if !parses_json(&j.join("job.json")) {
                rep.add(
                    "error",
                    rel(root, &j.join("job.json")),
                    "job_meta_unparseable",
                    "job.json is not valid JSON",
                );
            }
            for c in dir_entries(&j.join("capsules")) {
                rep.capsules += 1;
                check_capsule(root, &c, &mut rep);
            }
        }
    }
    Ok(rep)
}

fn check_capsule(root: &Path, dir: &Path, rep: &mut Report) {
    let spec_path = dir.join("spec.json");
    match std::fs::read_to_string(&spec_path) {
        Err(_) => rep.add(
            "error",
            rel(root, dir),
            "spec_missing",
            "capsule directory has no spec.json",
        ),
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Err(e) => rep.add(
                "error",
                rel(root, &spec_path),
                "spec_unparseable",
                e.to_string(),
            ),
            Ok(v) => {
                if let Err(e) = DecisionSpec::from_json(&v) {
                    rep.add("error", rel(root, &spec_path), "spec_invalid", e);
                }
            }
        },
    }
    let policy_path = dir.join("policy.json");
    match std::fs::read_to_string(&policy_path) {
        Err(_) => rep.add(
            "warn",
            rel(root, dir),
            "policy_missing",
            "no policy.json; the capsule runs deny-all",
        ),
        Ok(text) => {
            if let Err(e) = crate::context::ExecutionPolicy::from_policy_json(&text) {
                rep.add(
                    "error",
                    rel(root, &policy_path),
                    "policy_invalid",
                    format!("{e}; the capsule runs deny-all"),
                );
            }
        }
    }
    let program = dir.join("current.lyc");
    if let Ok(bytes) = std::fs::read(&program) {
        if let Err(e) = crate::server::runtime::FeatureProgram::load(&bytes) {
            rep.add("error", rel(root, &program), "program_invalid", e);
        }
        match std::fs::read_to_string(dir.join("manifest.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        {
            None => rep.add(
                "warn",
                rel(root, dir),
                "manifest_missing",
                "program installed without a readable manifest.json",
            ),
            Some(m) => {
                let want = m
                    .get("programSha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if want != sha256_hex(&bytes) {
                    rep.add(
                        "error",
                        rel(root, &program),
                        "program_hash_mismatch",
                        "current.lyc does not match the hash in manifest.json",
                    );
                }
            }
        }
    }
    check_temp_files(root, dir, rep);
}

/// Integrity check of `syntra.db`, opened read-only.
fn check_event_store(root: &Path, rep: &mut Report) {
    let db = root.join("syntra.db");
    if !db.exists() {
        rep.add(
            "warn",
            "syntra.db",
            "event_store_missing",
            "no event store yet (created on first server start)",
        );
        return;
    }
    // With no -wal/-shm present (server stopped, log checkpointed), open the
    // file as immutable: SQLite then takes no locks and creates no files.
    // With them present (a server is running), read through the existing
    // shared-memory index; that creates nothing either.
    let wal_present = root.join("syntra.db-wal").exists() || root.join("syntra.db-shm").exists();
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let uri = if wal_present {
        format!("file:{}?mode=ro", db.display())
    } else {
        format!("file:{}?immutable=1", db.display())
    };
    let conn = match rusqlite::Connection::open_with_flags(&uri, flags) {
        Ok(c) => c,
        Err(e) => {
            rep.add(
                "error",
                "syntra.db",
                "event_store_unopenable",
                e.to_string(),
            );
            return;
        }
    };
    match conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)) {
        Ok(s) if s == "ok" => {}
        Ok(s) => rep.add("error", "syntra.db", "event_store_corrupt", s),
        Err(e) => rep.add(
            "error",
            "syntra.db",
            "event_store_unreadable",
            e.to_string(),
        ),
    }
}

/// Leftover temp files from interrupted atomic writes.
fn check_temp_files(root: &Path, dir: &Path, rep: &mut Report) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = name_of(&p);
        if p.is_file() && name.contains(".tmp-") {
            rep.add(
                "warn",
                rel(root, &p),
                "orphan_temp_file",
                "leftover from an interrupted write; safe to delete when the server is stopped",
            );
        }
        if p.is_file() && name.contains(".corrupt-") {
            rep.add(
                "warn",
                rel(root, &p),
                "corrupt_evidence",
                "a corrupt file was preserved here for inspection",
            );
        }
    }
}

fn dir_entries(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn parses_json(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
