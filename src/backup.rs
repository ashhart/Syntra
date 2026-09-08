//! Backup/restore for the Syntra store root.
//! Format: versioned JSON document with base64-encoded file contents.
//! Restore is atomic via stage-then-rename.

use serde::{Deserialize, Serialize};
use std::path::Path;

const BACKUP_VERSION: u32 = 1;
/// Files we refuse to back up — they're either ephemeral or transient.
const SKIP_NAMES: &[&str] = &[".readiness_probe"];

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub path: String,
    pub content_b64: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    pub v: u32,
    pub created_at: u64,
    pub files: Vec<BackupFile>,
}

pub fn serialize_store(root: &Path) -> Result<Vec<u8>, String> {
    let mut files: Vec<BackupFile> = Vec::new();
    walk_collect(root, root, &mut files)?;
    let backup = Backup {
        v: BACKUP_VERSION,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0),
        files,
    };
    serde_json::to_vec(&backup)
        .map_err(|e| format!("serialize backup: {e}"))
}

pub fn restore_store(root: &Path, body: &[u8]) -> Result<usize, String> {
    let backup: Backup = serde_json::from_slice(body)
        .map_err(|e| format!("malformed backup: {e}"))?;
    if backup.v != BACKUP_VERSION {
        return Err(format!(
            "backup version {} not supported by this server (expected {})",
            backup.v, BACKUP_VERSION
        ));
    }

    // Reject any path that could escape the staging root.
    for f in &backup.files {
        if f.path.is_empty() || f.path.starts_with('/') || f.path.contains("..") {
            return Err(format!("refusing unsafe path in backup: {:?}", f.path));
        }
    }

    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos()).unwrap_or(0),
    );
    let parent = root.parent().ok_or_else(|| "store root has no parent".to_string())?;
    let leaf = root.file_name().ok_or_else(|| "store root has no name".to_string())?
        .to_string_lossy().to_string();
    let staging = parent.join(format!("{leaf}.restore-staging-{suffix}"));
    let rollback = parent.join(format!("{leaf}.restore-backup-{suffix}"));

    // Materialise into staging.
    if staging.exists() { std::fs::remove_dir_all(&staging).ok(); }
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("create staging dir: {e}"))?;
    let mut written = 0usize;
    for f in &backup.files {
        let full = staging.join(&f.path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create dir {parent:?}: {e}"))?;
        }
        let bytes = b64_decode(&f.content_b64)
            .map_err(|e| format!("decode {}: {e}", f.path))?;
        std::fs::write(&full, &bytes)
            .map_err(|e| format!("write {}: {e}", full.display()))?;
        written += 1;
    }

    // Atomic swap: live → rollback, staging → live.
    if root.exists() {
        std::fs::rename(root, &rollback)
            .map_err(|e| format!("move live store aside: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&staging, root) {
        // Try to put the old store back so we don't leave the server
        // pointing at a missing directory.
        if rollback.exists() {
            let _ = std::fs::rename(&rollback, root);
        }
        return Err(format!("install restored store: {e}"));
    }
    Ok(written)
}

fn walk_collect(root: &Path, dir: &Path, out: &mut Vec<BackupFile>) -> Result<(), String> {
    if !dir.exists() { return Ok(()); }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {dir:?}: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if SKIP_NAMES.iter().any(|s| *s == name_str) { continue; }
        if name_str.starts_with('.') && name_str != ".tmp" { /* allow .tmp; skip other dotfiles */ }
        // Skip restore-staging and rollback dirs left behind by prior runs.
        if name_str.starts_with("restore-staging-") || name_str.starts_with("restore-backup-")
            || name_str.contains(".restore-staging-") || name_str.contains(".restore-backup-") {
            continue;
        }
        let meta = entry.metadata().map_err(|e| format!("metadata: {e}"))?;
        if meta.is_dir() {
            walk_collect(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root)
                .map_err(|e| format!("strip prefix: {e}"))?
                .to_string_lossy().to_string();
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("read {path:?}: {e}"))?;
            out.push(BackupFile {
                path: rel,
                content_b64: b64_encode(&bytes),
            });
        }
    }
    Ok(())
}

// ── Hand-rolled standard base64 (no padding stripping, no external dep) ──

const B64_ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let chunks = bytes.chunks_exact(3);
    let rem = chunks.remainder();
    for chunk in chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[((n >>  6) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[( n        & 0x3F) as usize] as char);
    }
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >>  6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn dec(c: u8) -> Result<u32, String> {
        Ok(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("invalid base64 byte: {c}")),
        })
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return Err("base64 length not divisible by 4".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad0 = chunk[2] == b'=';
        let pad1 = chunk[3] == b'=';
        let a = dec(chunk[0])?;
        let b = dec(chunk[1])?;
        let c = if pad0 { 0 } else { dec(chunk[2])? };
        let d = if pad1 { 0 } else { dec(chunk[3])? };
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        out.push(((n >> 16) & 0xFF) as u8);
        if !pad0 { out.push(((n >> 8) & 0xFF) as u8); }
        if !pad1 { out.push((n & 0xFF) as u8); }
    }
    Ok(out)
}

// ── CLI wrappers (`syntra backup` / `syntra restore`) ──
//
// The HTTP admin routes keep their existing behavior; these are the
// operator-CLI counterparts with two CLI-only guarantees:
//   * backup fsyncs the bundle before exiting (the HTTP path hands bytes
//     straight to the socket and cannot control the client's durability);
//   * restore refuses to clobber a LIVE root unless `--force`.

pub fn cli_backup(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: syntra backup --store <root> --out <file.json>");
        eprintln!("Serializes the whole store root to one versioned JSON bundle");
        eprintln!("(base64 file contents, format v1 — same as POST /admin/backup).");
        eprintln!("The bundle is fsynced before exit.");
        eprintln!("For a CONSISTENT snapshot quiesce the server first (`syntra stop`):");
        eprintln!("the walk takes no lock, so a serving store mixes file generations.");
        eprintln!("Exit codes: 0 ok, 1 backup failed, 2 bad usage.");
        return;
    }
    let mut store: Option<String> = None;
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--store" => { store = args.get(i + 1).cloned(); i += 1; }
            "--out" => { out = args.get(i + 1).cloned(); i += 1; }
            _ => {}
        }
        i += 1;
    }
    let (Some(store), Some(out_path)) = (store, out) else {
        eprintln!("Usage: syntra backup --store <root> --out <file.json>");
        std::process::exit(2);
    };
    let root = std::path::Path::new(&store);
    if !root.is_dir() {
        eprintln!(r#"{{"ok":false,"reason":"store root not found: {store}"}}"#);
        std::process::exit(2);
    }
    let bytes = match serialize_store(root) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(r#"{{"ok":false,"reason":"{}"}}"#, e.replace('"', "'"));
            std::process::exit(1);
        }
    };
    if let Err(e) = write_fsynced(std::path::Path::new(&out_path), &bytes) {
        eprintln!(r#"{{"ok":false,"reason":"{}"}}"#, e.replace('"', "'"));
        std::process::exit(1);
    }
    let bundle: Backup = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Backup { v: 0, created_at: 0, files: vec![] });
    println!(
        "{}",
        serde_json::json!({
            "ok": true, "out": out_path, "files": bundle.files.len(),
            "bytes": bytes.len(), "v": bundle.v,
        })
    );
}

pub fn cli_restore(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: syntra restore --bundle <file.json> --into <root> [--force]");
        eprintln!("Installs a backup bundle at <root> via atomic stage-then-rename");
        eprintln!("(existing root is renamed to <root>.restore-backup-<n> first).");
        eprintln!("REFUSES a live root unless --force: restore renames the live root");
        eprintln!("OUT FROM UNDER a serving server — path-based writes then fail into");
        eprintln!("the void and a later boot can silently create an EMPTY store while");
        eprintln!("the real data strands as a .restore-backup-* sibling.");
        eprintln!("Liveness = <root>/.readiness_probe exists, or a .evolve.lock names a");
        eprintln!("live pid. Stop the server (`syntra stop`) and run `syntra doctor`");
        eprintln!("afterwards. Exit codes: 0 ok, 1 refused/failed, 2 bad usage.");
        return;
    }
    let mut bundle: Option<String> = None;
    let mut into: Option<String> = None;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bundle" => { bundle = args.get(i + 1).cloned(); i += 1; }
            "--into" => { into = args.get(i + 1).cloned(); i += 1; }
            "--force" => force = true,
            _ => {}
        }
        i += 1;
    }
    let (Some(bundle_path), Some(into_root)) = (bundle, into) else {
        eprintln!("Usage: syntra restore --bundle <file.json> --into <root> [--force]");
        std::process::exit(2);
    };
    let body = match std::fs::read(&bundle_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(r#"{{"ok":false,"reason":"cannot read bundle: {e}"}}"#);
            std::process::exit(1);
        }
    };
    let root = std::path::Path::new(&into_root);
    if !force {
        if let Some(reason) = restore_target_is_live(root) {
            eprintln!(
                r#"{{"ok":false,"reason":"refusing restore into live root: {reason}","hint":"stop the server first, or pass --force (restore renames the live root out from under a serving server — see syntra restore --help)"}}"#,
                reason = reason.replace('"', "'"),
            );
            std::process::exit(1);
        }
    }
    match restore_store(root, &body) {
        Ok(files) => println!(
            "{}",
            serde_json::json!({ "ok": true, "into": into_root, "files": files, "forced": force })
        ),
        Err(e) => {
            eprintln!(r#"{{"ok":false,"reason":"{}"}}"#, e.replace('"', "'"));
            std::process::exit(1);
        }
    }
}

/// Write + fsync before exit: a backup that evaporates on power loss is
/// not a backup.
fn write_fsynced(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::File::create(path).map_err(|e| format!("create {path:?}: {e}"))?;
    f.write_all(bytes).map_err(|e| format!("write {path:?}: {e}"))?;
    f.sync_all().map_err(|e| format!("fsync {path:?}: {e}"))
}

/// Detects a store root that a server is serving (or crashed serving).
/// Mirrors `syntra doctor`'s SERVE_PROBE_PRESENT / LOCK_STALE evidence.
fn restore_target_is_live(root: &std::path::Path) -> Option<String> {
    if root.join(".readiness_probe").exists() {
        return Some(".readiness_probe present (server serving, or killed mid-/ready)".to_string());
    }
    // Any .evolve.lock naming a live pid proves a process is working in
    // this store right now.
    let tenants = root.join("tenants");
    for tenant in crate::doctor::sub_dirs(&tenants) {
        for job in crate::doctor::sub_dirs(&tenant.join("jobs")) {
            for cap in crate::doctor::sub_dirs(&job.join("capsules")) {
                let lock = cap.join(".evolve.lock");
                if let Ok(text) = std::fs::read_to_string(&lock) {
                    if let Ok(pid) = text.trim().parse::<u32>() {
                        if crate::doctor::pid_alive(pid) {
                            return Some(format!("{} held by live pid {pid}", lock.display()));
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "syntra-backup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap().as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn b64_roundtrip() {
        for input in &[
            b"" as &[u8],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            b"hello, world",
            &[0u8, 1, 2, 3, 255, 254, 253, 252][..],
        ] {
            let enc = b64_encode(input);
            let dec = b64_decode(&enc).unwrap();
            assert_eq!(&dec[..], *input);
        }
    }

    #[test]
    fn backup_restore_roundtrip_preserves_files() {
        let src = tmpdir();
        std::fs::create_dir_all(src.join("tenants/acme/jobs/main/capsules/router")).unwrap();
        std::fs::write(src.join("tenants/acme/jobs/main/capsules/router/current.lyc"),
                       b"LYCNbinary").unwrap();
        std::fs::write(src.join("tenants/acme/jobs/main/capsules/router/memory.json"),
                       b"{\"v\":7}").unwrap();
        std::fs::write(src.join("tokens.json"), b"{\"v\":1,\"tokens\":{}}").unwrap();

        let bundle = serialize_store(&src).unwrap();

        let dst = tmpdir();
        let n = restore_store(&dst, &bundle).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            std::fs::read(dst.join("tenants/acme/jobs/main/capsules/router/current.lyc")).unwrap(),
            b"LYCNbinary"
        );
        assert_eq!(
            std::fs::read(dst.join("tokens.json")).unwrap(),
            b"{\"v\":1,\"tokens\":{}}"
        );
        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn restore_rejects_traversal_paths() {
        let dst = tmpdir();
        let bad = serde_json::json!({
            "v": 1,
            "createdAt": 0,
            "files": [
                {"path": "../escape.txt", "contentB64": "QQ=="}
            ]
        });
        let err = restore_store(&dst, bad.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("unsafe path"), "got: {err}");
        std::fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn restore_rejects_wrong_version() {
        let dst = tmpdir();
        let body = serde_json::json!({
            "v": 999, "createdAt": 0, "files": []
        });
        let err = restore_store(&dst, body.to_string().as_bytes()).unwrap_err();
        assert!(err.contains("version"));
        std::fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn restore_rejects_malformed_json() {
        let dst = tmpdir();
        let err = restore_store(&dst, b"not json").unwrap_err();
        assert!(err.contains("malformed"));
        std::fs::remove_dir_all(&dst).ok();
    }
}
