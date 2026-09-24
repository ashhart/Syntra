//! `syntra backup` and `syntra restore`.
//!
//! A backup is a directory: `manifest.json`, the store's files under
//! `files/`, and `syntra.db`, a consistent copy of the event store made with
//! `VACUUM INTO` (safe while the server is running). Every file's SHA-256
//! is in the manifest and checked on restore.
//!
//! Restore stages the backup next to the target and swaps it in with a
//! rename; an existing target is kept as `<root>.pre-restore-<ms>`. It
//! refuses a root whose server is running (it holds `server.lock`, or
//! `server.pid` names a live process) unless `--force`.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::store::{now_ms, sha256_hex};

pub const BACKUP_FORMAT: u64 = 2;

/// Files and directories at the store root that are never copied.
const SKIP_AT_ROOT: &[&str] = &[
    "syntra.db",
    "syntra.db-wal",
    "syntra.db-shm",
    "server.pid",
    "server.lock",
    ".readiness_probe",
];

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if dir == root && SKIP_AT_ROOT.contains(&name.as_str()) {
            continue;
        }
        if name.contains(".tmp-") {
            continue;
        }
        let meta =
            std::fs::symlink_metadata(&p).map_err(|e| format!("stat {}: {e}", p.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("refusing to back up symlink {}", p.display()));
        }
        if meta.is_dir() {
            walk(root, &p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut f =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("fsync {}: {e}", path.display()))
}

/// Copy the store at `root` into the new directory `out`.
pub fn backup(root: &Path, out: &Path) -> Result<serde_json::Value, String> {
    if !root.join("store.json").exists() {
        return Err(format!("{} is not a Syntra store", root.display()));
    }
    if out.exists()
        && std::fs::read_dir(out)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return Err(format!("{} already exists and is not empty", out.display()));
    }
    std::fs::create_dir_all(out.join("files"))
        .map_err(|e| format!("create {}: {e}", out.display()))?;

    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    let mut listed = Vec::new();
    for p in &files {
        let rel = p.strip_prefix(root).unwrap_or(p);
        let bytes = std::fs::read(p).map_err(|e| format!("read {}: {e}", p.display()))?;
        write_synced(&out.join("files").join(rel), &bytes)?;
        listed.push(json!({ "path": rel.to_string_lossy(), "sha256": sha256_hex(&bytes), "bytes": bytes.len() }));
    }

    let db = root.join("syntra.db");
    let db_entry = if db.exists() {
        let dest = out.join("syntra.db");
        let conn = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open event store: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])
            .map_err(|e| format!("copy event store: {e}"))?;
        let bytes = std::fs::read(&dest).map_err(|e| format!("read {}: {e}", dest.display()))?;
        std::fs::File::open(&dest)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Some(json!({ "sha256": sha256_hex(&bytes), "bytes": bytes.len() }))
    } else {
        None
    };

    let manifest = json!({
        "format": BACKUP_FORMAT,
        "createdAtMs": now_ms(),
        "source": root.to_string_lossy(),
        "files": listed,
        "syntraDb": db_entry,
    });
    write_synced(
        &out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap().as_bytes(),
    )?;
    Ok(manifest)
}

/// Why `root` looks live: a server holds its lock, or a `server.pid`
/// names a running process.
pub fn live_server(root: &Path) -> Option<String> {
    if crate::store::root_locked(root) {
        let pid = std::fs::read_to_string(root.join("server.pid"))
            .ok()
            .map(|p| format!(" (pid {})", p.trim()))
            .unwrap_or_default();
        return Some(format!(
            "a running server holds {}{pid}",
            crate::store::LOCK_FILE
        ));
    }
    let pid = std::fs::read_to_string(root.join("server.pid")).ok()?;
    let pid = pid.trim().parse::<u32>().ok()?;
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    alive.then(|| format!("server.pid names running process {pid}"))
}

/// Install the backup at `from` as the store at `into`.
pub fn restore(from: &Path, into: &Path, force: bool) -> Result<serde_json::Value, String> {
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(from.join("manifest.json"))
            .map_err(|e| format!("{}: not a backup ({e})", from.display()))?,
    )
    .map_err(|e| format!("manifest.json: {e}"))?;
    if manifest.get("format").and_then(|f| f.as_u64()) != Some(BACKUP_FORMAT) {
        return Err(format!("backup format is not {BACKUP_FORMAT}"));
    }
    if !force && let Some(why) = live_server(into) {
        return Err(format!(
            "refusing restore into live root {}: {why} (stop it or pass --force)",
            into.display()
        ));
    }
    // Verify everything before touching the target.
    let files = manifest["files"].as_array().cloned().unwrap_or_default();
    for f in &files {
        let rel = f["path"].as_str().ok_or("manifest entry without a path")?;
        if Path::new(rel)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(format!("manifest path {rel:?} escapes the store"));
        }
        let bytes =
            std::fs::read(from.join("files").join(rel)).map_err(|e| format!("{rel}: {e}"))?;
        if f["sha256"].as_str() != Some(sha256_hex(&bytes).as_str()) {
            return Err(format!("{rel}: checksum mismatch"));
        }
    }
    if let Some(db) = manifest.get("syntraDb").filter(|v| !v.is_null()) {
        let bytes = std::fs::read(from.join("syntra.db")).map_err(|e| format!("syntra.db: {e}"))?;
        if db["sha256"].as_str() != Some(sha256_hex(&bytes).as_str()) {
            return Err("syntra.db: checksum mismatch".into());
        }
    }

    let parent = into
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stage = parent.join(format!(
        ".{}.restore-{}",
        into.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store".into()),
        now_ms()
    ));
    let result = (|| {
        for f in &files {
            let rel = f["path"].as_str().unwrap_or_default();
            let bytes = std::fs::read(from.join("files").join(rel)).map_err(|e| e.to_string())?;
            write_synced(&stage.join(rel), &bytes)?;
        }
        if from.join("syntra.db").exists() && manifest.get("syntraDb").is_some_and(|v| !v.is_null())
        {
            let bytes = std::fs::read(from.join("syntra.db")).map_err(|e| e.to_string())?;
            write_synced(&stage.join("syntra.db"), &bytes)?;
        }
        Ok::<(), String>(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(e);
    }
    let previous = if into.exists() {
        let aside = PathBuf::from(format!("{}.pre-restore-{}", into.display(), now_ms()));
        std::fs::rename(into, &aside).map_err(|e| format!("move existing store aside: {e}"))?;
        Some(aside)
    } else {
        None
    };
    std::fs::rename(&stage, into).map_err(|e| format!("install restored store: {e}"))?;
    Ok(json!({
        "ok": true,
        "files": files.len(),
        "eventStore": manifest.get("syntraDb").is_some_and(|v| !v.is_null()),
        "previousStore": previous.map(|p| p.to_string_lossy().into_owned()),
    }))
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

pub fn cli_backup(args: &[String]) {
    const USAGE: &str = "Usage: syntra backup --store <root> --out <new-dir>\n\n\
        A consistent copy of the store, safe while the server runs: the files,\n\
        an online copy of syntra.db, and a manifest of SHA-256 hashes.";
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{USAGE}");
        return;
    }
    let (Some(store), Some(out)) = (arg(args, "--store"), arg(args, "--out")) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match backup(Path::new(&store), Path::new(&out)) {
        Ok(m) => println!(
            "{}",
            json!({ "ok": true, "out": out, "files": m["files"].as_array().map(|a| a.len()), "eventStore": !m["syntraDb"].is_null() })
        ),
        Err(e) => {
            eprintln!("syntra backup: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cli_restore(args: &[String]) {
    const USAGE: &str = "Usage: syntra restore --from <backup-dir> --into <root> [--force]\n\n\
        Checks every hash in the backup, then swaps it in; an existing root is\n\
        kept as <root>.pre-restore-<ms>. Refuses a root a running server holds\n\
        unless --force.";
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{USAGE}");
        return;
    }
    let (Some(from), Some(into)) = (arg(args, "--from"), arg(args, "--into")) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let force = args.iter().any(|a| a == "--force");
    match restore(Path::new(&from), Path::new(&into), force) {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("syntra restore: {e}");
            std::process::exit(1);
        }
    }
}
