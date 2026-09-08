//! `syntra doctor` — read-only store validator.
//!
//! READ-ONLY CONTRACT: this module MUST NOT create, write, rename, or
//! delete anything. Every open is a read; there is no `fs::write`,
//! `fs::create`, or `fs::remove` call on any doctor path, and the store is
//! opened with `LycanStore::open` (never `open_or_init`, which would
//! materialise `tenants/`). The contract is pinned by
//! `tests/doctor_cli.rs::doctor_is_read_only` (mtime+size snapshot of the
//! whole store before/after a doctor run).
//!
//! Doctor *covers* crash recovery (orphan tmps, torn JSONL tails, stale
//! locks, corrupt-evidence files, stranded restore dirs) but *cleans
//! nothing*: cleanup stays a deliberate operator action (rm the named
//! file). See `docs/store-retention.md` § "doctor, backup/restore and
//! crash semantics".
//!
//! Output: one JSON line per finding on stdout —
//! `{"severity","path","code","detail"}` — plus a final summary line
//! (`--json` suppresses the summary). Exit codes are fail-closed:
//! 0 = no findings, 1 = findings, 2 = store unreadable / not a store.

use serde_json::json;
use std::path::Path;

use crate::graph::NeuralGraph;
use crate::store::LycanStore;
use crate::verifier;

/// `memory.json` version written by the current build
/// (`crate::learning::CapsuleMemory::to_json`). Anything else on disk is
/// either legacy or hand-edited drift.
const EXPECTED_MEMORY_VERSION: u64 = 7;

/// JSONL log names checked for rotation sanity (plus the legacy
/// evolution-journal name).
const LOG_NAMES: &[&str] = &[
    "decision.jsonl",
    "feedback.jsonl",
    "audit.jsonl",
    "evolution.jsonl",
    "current.lyc.evolution.jsonl",
];

/// Per-capsule JSON sidecars that must parse when present (memory.json and
/// policy.json get dedicated checks).
const PARSE_ONLY_JSON: &[&str] = &[
    "manifest.json",
    "reward_spec.json",
    "context_schema.json",
    "warmup.json",
    "learning.json",
    "hierarchical_spec.json",
    "hierarchical_state.json",
];

#[derive(Debug)]
pub struct Finding {
    pub severity: &'static str, // "error" | "warn"
    pub path: String,
    pub code: String,
    pub detail: String,
}

impl Finding {
    fn new(severity: &'static str, path: impl Into<String>, code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { severity, path: path.into(), code: code.into(), detail: detail.into() }
    }
    fn line(&self) -> String {
        json!({
            "severity": self.severity,
            "path": self.path,
            "code": self.code,
            "detail": self.detail,
        })
        .to_string()
    }
}

#[derive(Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub files_scanned: u64,
    pub bytes_total: u64,
}

impl Report {
    fn push(&mut self, f: Finding) {
        self.findings.push(f);
    }
    fn errors(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == "error").count()
    }
    fn warnings(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == "warn").count()
    }
}

// ── CLI ──

pub fn cli_doctor(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: syntra doctor --store <root> [--json]");
        eprintln!("Read-only store validator. Walks tenants/jobs/capsules and reports");
        eprintln!("findings as JSON lines on stdout, then a summary line (--json omits it).");
        eprintln!("Exit codes: 0 healthy, 1 findings, 2 store unreadable.");
        eprintln!("Checks: current.lyc decode + verify (reported separately), sidecar JSON");
        eprintln!("parse (manifest/reward_spec/context_schema/warmup/learning/hier spec+state/");
        eprintln!("policy/job/tokens), memory.json parse + version drift, orphan .tmp files,");
        eprintln!("torn JSONL tails, .1 rotation sanity, stale .evolve.lock (dead pid),");
        eprintln!("tenant/job dir orphans, corrupt-evidence files, stranded restore dirs,");
        eprintln!("and log bytes vs retention.json when configured.");
        eprintln!("Doctor NEVER writes or deletes: remediation is manual (rm the named file,");
        eprintln!("or restore from backup). See docs/store-retention.md.");
        return;
    }
    let mut store: Option<String> = None;
    let mut json_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--store" => {
                store = args.get(i + 1).cloned();
                i += 1;
            }
            "--json" => json_only = true,
            _ => {}
        }
        i += 1;
    }
    let Some(store) = store else {
        eprintln!("Usage: syntra doctor --store <root> [--json]");
        std::process::exit(2);
    };
    let root = Path::new(&store);
    let report = match validate(root) {
        Ok(r) => r,
        Err(e) => {
            // Store unreadable is itself fail-closed: never report healthy.
            println!("{}", Finding::new("error", store, "STORE_UNREADABLE", e).line());
            std::process::exit(2);
        }
    };
    for f in &report.findings {
        println!("{}", f.line());
    }
    let exit_code = if report.findings.is_empty() { 0 } else { 1 };
    if !json_only {
        println!(
            "{}",
            json!({
                "type": "summary",
                "store": store,
                "scannedFiles": report.files_scanned,
                "totalBytes": report.bytes_total,
                "errors": report.errors(),
                "warnings": report.warnings(),
                "exit": exit_code,
            })
        );
    }
    std::process::exit(exit_code);
}

// ── Validation ──

/// Walk the store read-only and collect findings. `Err` means the store
/// could not be opened/validated at all (exit 2); a successfully returned
/// report may still contain error-severity findings (exit 1).
pub fn validate(root: &Path) -> Result<Report, String> {
    if !root.exists() {
        return Err(format!("store root does not exist: {}", root.display()));
    }
    // open (not open_or_init — that would create tenants/). Invalid
    // retention.json fails closed here, same posture as server startup.
    let store = LycanStore::open(&root.display().to_string())?;
    let retention = store.retention_config();
    if !root.join("tenants").is_dir() {
        return Err(format!(
            "no tenants/ directory under {} — not a Syntra store (a failed restore can strand the real root as a .restore-backup-* sibling)",
            root.display()
        ));
    }

    let mut rep = Report::default();

    // Generic sweep: byte totals, orphan tmps, corrupt-evidence copies,
    // torn JSONL tails — anywhere in the tree, including snapshots/.
    sweep_dir(root, root, retention.max_log_bytes, &mut rep);

    // Store-root siblings left by an interrupted restore (they live OUTSIDE
    // root, so the sweep above never sees them).
    check_stranded_restore_dirs(root, &mut rep);

    // Root-level files.
    check_root_file(root, &mut rep);

    // Structured tenant → job → capsule walk.
    let tenants_dir = root.join("tenants");
    for tenant in sub_dirs(&tenants_dir) {
        let jobs_dir = tenant.join("jobs");
        if !jobs_dir.is_dir() {
            rep.push(Finding::new("warn", rel(root, &tenant), "TENANT_ORPHAN",
                "tenant directory has no jobs/ (create_tenant mkdir interrupted?)"));
            continue;
        }
        for job_dir in sub_dirs(&jobs_dir) {
            check_job(root, &job_dir, &mut rep);
        }
    }
    Ok(rep)
}

/// Generic recursive sweep over every file under `dir`.
fn sweep_dir(root: &Path, dir: &Path, max_log_bytes: u64, rep: &mut Report) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            sweep_dir(root, &path, max_log_bytes, rep);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        rep.files_scanned += 1;
        rep.bytes_total += meta.len();
        let rel = rel(root, &path);

        if is_tmp_name(&name) {
            rep.push(Finding::new("warn", rel.clone(), "TMP_ORPHAN",
                "orphaned atomic-write temp file — rename never completed (crash mid-write); safe to delete manually"));
        }
        if name.contains(".corrupt-") {
            rep.push(Finding::new("error", rel.clone(), "CORRUPT_EVIDENCE",
                "corruption evidence preserved by a load path before resetting the sidecar; inspect/diff it, then delete manually"));
        }
        if name.ends_with(".jsonl") || name.ends_with(".jsonl.1") {
            check_jsonl_tail(root, &path, &rel, rep);
            if max_log_bytes > 0 && meta.len() > max_log_bytes.saturating_mul(2).saturating_add(4096) {
                rep.push(Finding::new("warn", rel.clone(), "RETENTION_BYTES_EXCEEDED",
                    format!("{} is {} bytes, > 2x retention maxLogBytes ({max_log_bytes}) — rotation is not bounding this log",
                        name, meta.len())));
            }
        }
    }
}

/// A JSONL file is torn when its last non-whitespace line does not parse:
/// appends are buffered and never fsynced, so a crash can leave a partial
/// tail (docs/store-retention.md, "what is NOT durable").
fn check_jsonl_tail(_root: &Path, path: &Path, rel: &str, rep: &mut Report) {
    let Ok(bytes) = std::fs::read(path) else { return };
    let text = String::from_utf8_lossy(&bytes).to_string();
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return;
    }
    let last = trimmed.lines().next_back().unwrap_or("");
    if serde_json::from_str::<serde_json::Value>(last).is_err() {
        rep.push(Finding::new("error", rel.to_string(), "JSONL_TORN_TAIL",
            format!("last line of {} is not valid JSON ({} bytes, likely a lost buffered append mid-crash); the API serves it raw and /feedback cannot resolve it",
                name_of(path), last.len())));
    }
}

fn check_root_file(root: &Path, rep: &mut Report) {
    let tokens = root.join("tokens.json");
    if tokens.exists() {
        let ok = std::fs::read_to_string(&tokens)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some();
        if !ok {
            rep.push(Finding::new("error", rel(root, &tokens), "TOKENS_UNPARSEABLE",
                "tokens.json does not parse — server startup drops ALL scoped tokens silently (evidence is copied to tokens.json.corrupt-<ts> on boot); tokens must be reissued"));
        }
    }
    if root.join(".readiness_probe").exists() {
        rep.push(Finding::new("warn", rel(root, &root.join(".readiness_probe")), "SERVE_PROBE_PRESENT",
            ".readiness_probe present — a server is serving this store now, or was killed mid-/ready; a live server makes backup/restore unsafe"));
    }
}

/// `restore_store` materialises the bundle at `<leaf>.restore-staging-*`,
/// renames the live root to `<leaf>.restore-backup-*`, then renames
/// staging into place. Semantics of the siblings:
///   * a surviving `.restore-staging-*` = the restore never completed
///     (crash mid-swap or failed second rename);
///   * a `.restore-backup-*` is the INTENTIONALLY RETAINED rollback copy
///     every successful restore leaves — not a finding while the live
///     root has content. The dangerous shape is live-root-with-empty-
///     tenants/ next to a rollback: that means the swap half-failed and a
///     later boot silently created an EMPTY store while the real data
///     strands in the rollback copy.
fn check_stranded_restore_dirs(root: &Path, rep: &mut Report) {
    let (Some(parent), Some(leaf)) = (root.parent(), root.file_name()) else { return };
    let Some(leaf) = leaf.to_str() else { return };
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    let live_tenants_empty = match std::fs::read_dir(root.join("tenants")) {
        Ok(d) => d.count() == 0,
        Err(_) => true,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&format!("{leaf}.restore-staging-")) {
            rep.push(Finding::new("error", entry.path().to_string_lossy().to_string(), "RESTORE_STRANDED",
                format!("an interrupted/failed restore left staging dir {name} beside the store root; reconcile the three copies before trusting {}", root.display())));
        } else if name.starts_with(&format!("{leaf}.restore-backup-")) && live_tenants_empty {
            rep.push(Finding::new("error", entry.path().to_string_lossy().to_string(), "RESTORE_ROLLBACK_WITH_EMPTY_LIVE",
                format!("live root has an EMPTY tenants/ while restore rollback {name} holds data — a failed swap likely let a later boot create an empty store; the real data is probably in {name}")));
        }
    }
}

fn check_job(root: &Path, job_dir: &Path, rep: &mut Report) {
    let job_json = job_dir.join("job.json");
    if !job_json.exists() {
        rep.push(Finding::new("warn", rel(root, &job_json), "JOB_NO_MANIFEST",
            "job directory without job.json (mkdir ran before the write; the API synthesizes {\"id\":<name>} from the directory name)"));
    } else if !parses_json(&job_json) {
        rep.push(Finding::new("error", rel(root, &job_json), "JOB_UNPARSEABLE",
            "job.json does not parse (touch_job rewrites it non-atomically — it can tear)"));
    }
    let caps_dir = job_dir.join("capsules");
    if !caps_dir.is_dir() {
        rep.push(Finding::new("warn", rel(root, &caps_dir), "JOB_NO_CAPSULES_DIR",
            "job directory without capsules/"));
        return;
    }
    for cap_dir in sub_dirs(&caps_dir) {
        check_capsule(root, &cap_dir, rep);
    }
}

fn check_capsule(root: &Path, dir: &Path, rep: &mut Report) {
    // current.lyc: decode and verify are reported SEPARATELY — a file that
    // decodes but fails verification is a different failure (corrupt graph
    // structure) from one that fails the header/magic decode (torn write).
    let lyc = dir.join("current.lyc");
    if !lyc.exists() {
        rep.push(Finding::new("error", rel(root, &lyc), "GRAPH_MISSING",
            "capsule directory has no current.lyc — /decide 404s"));
    } else {
        match std::fs::read(&lyc) {
            Ok(bytes) => match NeuralGraph::from_bytes(&bytes) {
                Ok(graph) => {
                    if let Err(e) = verifier::verify(&graph) {
                        rep.push(Finding::new("error", rel(root, &lyc), "GRAPH_VERIFY_FAIL",
                            format!("graph decodes but fails verification: {e}")));
                    }
                }
                Err(e) => rep.push(Finding::new("error", rel(root, &lyc), "GRAPH_DECODE_FAIL",
                    format!("graph fails NeuralGraph::from_bytes: {e} — repair source: snapshots/"))),
            },
            Err(e) => rep.push(Finding::new("error", rel(root, &lyc), "GRAPH_UNREADABLE",
                format!("cannot read current.lyc: {e}"))),
        }
    }

    for name in PARSE_ONLY_JSON {
        let p = dir.join(name);
        if p.exists() && !parses_json(&p) {
            rep.push(Finding::new("error", rel(root, &p),
                format!("{}_UNPARSEABLE", name.trim_end_matches(".json").to_uppercase()),
                format!("{name} exists but does not parse — the load path treats it as absent and silently resets to defaults (memory.json and hierarchical_state.json also preserve a {name}.corrupt-<ts> evidence copy on read)")));
        }
    }

    // memory.json: parse + version drift. Corrupt memory.json is the worst
    // silent-loss case: every caller unwrap_or_default()s it and the next
    // learn=true write persists the empty state.
    let mem = dir.join("memory.json");
    if mem.exists() {
        match std::fs::read_to_string(&mem)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            None => rep.push(Finding::new("error", rel(root, &mem), "MEMORY_UNPARSEABLE",
                "memory.json does not parse — callers reset to default and the next save overwrites: silent total learning loss unless restored from backup or the corrupt-<ts> evidence")),
            Some(v) => {
                let version = v.get("version").and_then(|x| x.as_u64());
                if version != Some(EXPECTED_MEMORY_VERSION) {
                    rep.push(Finding::new("warn", rel(root, &mem), "MEMORY_VERSION_DRIFT",
                        format!("memory.json version is {:?}, current build writes {EXPECTED_MEMORY_VERSION} — legacy/hand-edited file; from_json never gates on version, so drift is invisible until the next save rewrites it", version)));
                }
            }
        }
    }

    // policy.json: missing/malformed fails closed to deny-all at decide time.
    let policy = dir.join("policy.json");
    if !policy.exists() {
        rep.push(Finding::new("warn", rel(root, &policy), "POLICY_MISSING",
            "no policy.json — execution fails closed to deny-all"));
    } else if !parses_json(&policy) {
        rep.push(Finding::new("error", rel(root, &policy), "POLICY_UNPARSEABLE",
            "policy.json does not parse — execution fails closed to deny-all"));
    }

    // Stale evolve lock: a dead pid means the lock will block every future
    // evolve run on this capsule forever (manual rm).
    let lock = dir.join(".evolve.lock");
    if lock.exists() {
        match std::fs::read_to_string(&lock).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(pid) if pid_alive(pid) => {}
            Some(pid) => rep.push(Finding::new("error", rel(root, &lock), "LOCK_STALE",
                format!(".evolve.lock names pid {pid}, which is not running — evolve on this capsule is blocked; delete manually"))),
            None => rep.push(Finding::new("error", rel(root, &lock), "LOCK_STALE",
                ".evolve.lock exists but carries no parsable pid — treat as stale; delete manually")),
        }
    }

    // Rotation sanity: the rotated generation may only exist alongside a
    // live base. A crash between remove_file(.1) and rename(base→.1) — or
    // between rename and the next append — strands `.1` with no base.
    for name in LOG_NAMES {
        let base = dir.join(name);
        let rotated = std::path::PathBuf::from(format!("{}.1", base.display()));
        if rotated.exists() {
            let base_ok = base.exists() && base.metadata().map(|m| m.len() > 0).unwrap_or(false);
            if !base_ok {
                rep.push(Finding::new("warn", rel(root, &rotated), "ROT_MISSING_BASE",
                    format!("{name}.1 exists but {name} is missing/empty — the rotated stream still reads, but a new append or a purge will not re-materialise the base cleanly")));
            }
        }
    }
}

// ── Small helpers ──


fn parses_json(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some()
}

/// Orphan-temp naming: the legacy shared `<stem>.tmp` and the current
/// unique `<stem>.tmp.<pid>.<counter>` scheme (and `tokens.json.tmp`).
pub(crate) fn is_tmp_name(name: &str) -> bool {
    name.ends_with(".tmp") || name.contains(".tmp.")
}

pub(crate) fn sub_dirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string())
}

/// Liveness probe via `kill -0` — the same `/bin/kill` idiom `syntra stop`
/// already uses. If `kill` is unavailable we report "alive" so doctor never
/// false-flags a lock as stale; a live-but-other-user pid may read as
/// dead (ESRCH vs EPERM are indistinguishable through exit status).
pub(crate) fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(true)
}
