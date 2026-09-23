//! Filesystem layout for tenants, jobs and capsule artifacts.
//!
//! ```text
//! <root>/
//!   store.json                          format marker
//!   syntra.db                           event store: decisions, rewards, models, audit
//!   tokens.json                         hashed API tokens
//!   tenants/<t>/jobs/<j>/job.json
//!   tenants/<t>/jobs/<j>/capsules/<c>/
//!     spec.json                         decision spec (actions, exploration, learner, mode)
//!     policy.json                       execution policy for the feature program
//!     current.lyc                       compiled feature program (optional)
//!     manifest.json                     install metadata
//!     data/                             file-capability sandbox root
//! ```
//!
//! Artifacts change rarely (installs, spec and policy edits), so every write
//! is atomic and durable: temp file, fsync, rename, fsync of the directory.
//! Events never touch these files; they live in `syntra.db`.

use std::path::{Path, PathBuf};

use crate::decision::DecisionSpec;

/// On-disk format written to `store.json`. v1 stores (JSONL logs and
/// `memory.json` per capsule) are detected and refused with a pointer to
/// `syntra migrate`.
pub const STORE_FORMAT: u64 = 2;

/// Policy written for a new capsule: nothing is allowed until an operator
/// grants it.
pub const DEFAULT_POLICY: &str = r#"{
  "allow_stdout": false,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false
}"#;

/// Tenant, job and capsule names: 1-128 characters from `[A-Za-z0-9_.-]`,
/// not starting with `.`. Names become path components, so nothing that
/// could traverse or hide is allowed.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid name {name:?}: use 1-128 characters from A-Z a-z 0-9 _ - . not starting with ."
        ))
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write `data` to `path` atomically and durably.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = path.with_extension(format!(
        "tmp-{}-{:x}",
        std::process::id(),
        crate::decision::random_seed()
    ));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("create temp file: {e}"))?;
        f.write_all(data)
            .map_err(|e| format!("write temp file: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync temp file: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename into place: {e}"))?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Handle to one store root.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open a store, creating it when the directory is empty or missing.
    /// Refuses a v1 store rather than misreading it.
    pub fn open_or_init(path: &str) -> Result<Self, String> {
        let root = PathBuf::from(path);
        std::fs::create_dir_all(root.join("tenants"))
            .map_err(|e| format!("cannot create store at {path}: {e}"))?;
        let marker = root.join("store.json");
        match std::fs::read_to_string(&marker) {
            Ok(text) => {
                let v: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("{}: invalid JSON: {e}", marker.display()))?;
                let format = v.get("format").and_then(|f| f.as_u64()).unwrap_or(0);
                if format != STORE_FORMAT {
                    return Err(format!(
                        "{} declares store format {format}; this build reads format {STORE_FORMAT}",
                        marker.display()
                    ));
                }
            }
            Err(_) => {
                if Self::looks_like_v1(&root) {
                    return Err(format!(
                        "{path} is a v1 store (per-capsule memory.json and JSONL logs). \
                         Run `syntra migrate --from {path} --to <new-root>` to import it."
                    ));
                }
                write_atomic(
                    &marker,
                    serde_json::json!({ "format": STORE_FORMAT, "createdAtMs": now_ms() })
                        .to_string()
                        .as_bytes(),
                )?;
            }
        }
        Ok(Store { root })
    }

    /// A v1 store has capsule directories containing `memory.json` or
    /// `decision.jsonl` and no `store.json`.
    fn looks_like_v1(root: &Path) -> bool {
        let tenants = root.join("tenants");
        let Ok(ts) = std::fs::read_dir(&tenants) else {
            return false;
        };
        for t in ts.flatten() {
            let jobs = t.path().join("jobs");
            for j in std::fs::read_dir(&jobs).into_iter().flatten().flatten() {
                for c in std::fs::read_dir(j.path().join("capsules"))
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let p = c.path();
                    if p.join("memory.json").exists() || p.join("decision.jsonl").exists() {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    /// Path of the SQLite event store.
    pub fn events_path(&self) -> PathBuf {
        self.root.join("syntra.db")
    }

    pub fn tokens_path(&self) -> PathBuf {
        self.root.join("tokens.json")
    }

    pub fn tenant_dir(&self, tenant: &str) -> Result<PathBuf, String> {
        validate_name(tenant)?;
        Ok(self.root.join("tenants").join(tenant))
    }

    pub fn job_dir(&self, tenant: &str, job: &str) -> Result<PathBuf, String> {
        validate_name(job)?;
        Ok(self.tenant_dir(tenant)?.join("jobs").join(job))
    }

    pub fn capsule_dir(&self, tenant: &str, job: &str, capsule: &str) -> Result<PathBuf, String> {
        validate_name(capsule)?;
        Ok(self.job_dir(tenant, job)?.join("capsules").join(capsule))
    }

    /// The capsule's file-capability sandbox root.
    pub fn data_dir(&self, tenant: &str, job: &str, capsule: &str) -> Result<PathBuf, String> {
        Ok(self.capsule_dir(tenant, job, capsule)?.join("data"))
    }

    // ── Tenants and jobs ────────────────────────────────────────────────

    fn list_dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .filter(|n| validate_name(n).is_ok())
            .collect();
        names.sort();
        names
    }

    pub fn list_tenants(&self) -> Vec<String> {
        Self::list_dir_names(&self.root.join("tenants"))
    }

    /// Create a job; returns false when it already existed.
    pub fn create_job(&self, tenant: &str, job: &str, name: Option<&str>) -> Result<bool, String> {
        let dir = self.job_dir(tenant, job)?;
        let meta = dir.join("job.json");
        if meta.exists() {
            return Ok(false);
        }
        std::fs::create_dir_all(dir.join("capsules"))
            .map_err(|e| format!("create job {tenant}/{job}: {e}"))?;
        let doc = serde_json::json!({
            "id": job,
            "name": name.unwrap_or(job),
            "createdAtMs": now_ms(),
        });
        write_atomic(&meta, doc.to_string().as_bytes())?;
        Ok(true)
    }

    pub fn list_jobs(&self, tenant: &str) -> Result<Vec<serde_json::Value>, String> {
        let dir = self.tenant_dir(tenant)?.join("jobs");
        Ok(Self::list_dir_names(&dir)
            .into_iter()
            .map(|job| {
                self.get_job(tenant, &job)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| serde_json::json!({ "id": job }))
            })
            .collect())
    }

    pub fn get_job(&self, tenant: &str, job: &str) -> Result<Option<serde_json::Value>, String> {
        let dir = self.job_dir(tenant, job)?;
        if !dir.is_dir() {
            return Ok(None);
        }
        let mut doc = std::fs::read_to_string(dir.join("job.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .unwrap_or_else(|| serde_json::json!({ "id": job }));
        doc["capsules"] = serde_json::json!(self.list_capsules(tenant, job)?);
        Ok(Some(doc))
    }

    // ── Capsules ────────────────────────────────────────────────────────

    pub fn list_capsules(&self, tenant: &str, job: &str) -> Result<Vec<String>, String> {
        let dir = self.job_dir(tenant, job)?.join("capsules");
        Ok(Self::list_dir_names(&dir)
            .into_iter()
            .filter(|c| dir.join(c).join("spec.json").exists())
            .collect())
    }

    /// Every capsule in the store as (tenant, job, capsule).
    pub fn list_all_capsules(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for t in self.list_tenants() {
            let Ok(jobs_dir) = self.tenant_dir(&t).map(|d| d.join("jobs")) else {
                continue;
            };
            for j in Self::list_dir_names(&jobs_dir) {
                for c in self.list_capsules(&t, &j).unwrap_or_default() {
                    out.push((t.clone(), j.clone(), c));
                }
            }
        }
        out
    }

    /// A capsule exists once it has a spec.
    pub fn capsule_exists(&self, tenant: &str, job: &str, capsule: &str) -> bool {
        self.capsule_dir(tenant, job, capsule)
            .map(|d| d.join("spec.json").exists())
            .unwrap_or(false)
    }

    /// Load a capsule's spec; `Ok(None)` when the capsule does not exist.
    pub fn load_spec(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<Option<DecisionSpec>, String> {
        let path = self.capsule_dir(tenant, job, capsule)?.join("spec.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("{}: invalid JSON: {e}", path.display()))?;
        DecisionSpec::from_json(&value)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Write a spec, creating the capsule (with a deny-all policy and the
    /// job) when it does not exist yet.
    pub fn save_spec(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
        spec: &DecisionSpec,
    ) -> Result<(), String> {
        spec.validate()?;
        let dir = self.capsule_dir(tenant, job, capsule)?;
        if !dir.join("spec.json").exists() {
            self.create_job(tenant, job, None)?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("create capsule dir: {e}"))?;
            if !dir.join("policy.json").exists() {
                write_atomic(&dir.join("policy.json"), DEFAULT_POLICY.as_bytes())?;
            }
        }
        let text = serde_json::to_string_pretty(&spec.to_json()).map_err(|e| e.to_string())?;
        write_atomic(&dir.join("spec.json"), text.as_bytes())
    }

    /// The policy document as stored, or `None` when absent.
    pub fn load_policy_text(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<Option<String>, String> {
        let path = self.capsule_dir(tenant, job, capsule)?.join("policy.json");
        match std::fs::read_to_string(&path) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    /// The parsed execution policy. A missing policy is the deny-all
    /// default; an invalid one is an error (callers run deny-all).
    pub fn load_execution_policy(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<crate::context::ExecutionPolicy, String> {
        let text = self
            .load_policy_text(tenant, job, capsule)?
            .unwrap_or_else(|| DEFAULT_POLICY.to_string());
        crate::context::ExecutionPolicy::from_policy_json(&text)
    }

    /// Store a policy document. Callers validate it first.
    pub fn save_policy_text(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
        text: &str,
    ) -> Result<(), String> {
        let path = self.capsule_dir(tenant, job, capsule)?.join("policy.json");
        write_atomic(&path, text.as_bytes())
    }

    /// The compiled feature program, if the capsule has one.
    pub fn load_program(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let path = self.capsule_dir(tenant, job, capsule)?.join("current.lyc");
        match std::fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    pub fn save_program(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        let dir = self.capsule_dir(tenant, job, capsule)?;
        write_atomic(&dir.join("current.lyc"), bytes)?;
        let manifest = serde_json::json!({
            "programSha256": sha256_hex(bytes),
            "programBytes": bytes.len(),
            "installedAtMs": now_ms(),
        });
        write_atomic(&dir.join("manifest.json"), manifest.to_string().as_bytes())
    }

    /// Remove the feature program; returns false when there was none.
    pub fn delete_program(&self, tenant: &str, job: &str, capsule: &str) -> Result<bool, String> {
        let dir = self.capsule_dir(tenant, job, capsule)?;
        let removed = match std::fs::remove_file(dir.join("current.lyc")) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("remove program: {e}")),
        };
        let _ = std::fs::remove_file(dir.join("manifest.json"));
        Ok(removed)
    }

    pub fn read_manifest(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Option<serde_json::Value> {
        let path = self
            .capsule_dir(tenant, job, capsule)
            .ok()?
            .join("manifest.json");
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    pub fn delete_capsule(&self, tenant: &str, job: &str, capsule: &str) -> Result<bool, String> {
        remove_dir(&self.capsule_dir(tenant, job, capsule)?)
    }

    pub fn delete_job(&self, tenant: &str, job: &str) -> Result<bool, String> {
        remove_dir(&self.job_dir(tenant, job)?)
    }

    pub fn delete_tenant(&self, tenant: &str) -> Result<bool, String> {
        remove_dir(&self.tenant_dir(tenant)?)
    }
}

fn remove_dir(dir: &Path) -> Result<bool, String> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("remove {}: {e}", dir.display())),
    }
}

/// Keep a corrupt file next to the original for forensics.
pub fn write_corrupt_evidence(path: &Path) {
    let stamp = now_ms();
    let evidence = path.with_extension(format!("corrupt-{stamp}"));
    let _ = std::fs::copy(path, evidence);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "syntra-store-{label}-{}-{:x}",
            std::process::id(),
            crate::decision::random_seed()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn names_are_path_safe() {
        for good in ["acme", "a.b", "job_1", "X-2"] {
            validate_name(good).unwrap();
        }
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "a\\b",
            "sp ace",
            &"x".repeat(129),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn spec_roundtrip_creates_capsule_with_deny_all_policy() {
        let root = temp_root("spec");
        let store = Store::open_or_init(root.to_str().unwrap()).unwrap();
        assert!(!store.capsule_exists("acme", "prod", "router"));
        let mut spec = DecisionSpec::default();
        spec.actions = vec![
            crate::decision::ActionSpec::new("small"),
            crate::decision::ActionSpec::new("large"),
        ];
        store.save_spec("acme", "prod", "router", &spec).unwrap();
        assert!(store.capsule_exists("acme", "prod", "router"));
        assert_eq!(
            store.load_spec("acme", "prod", "router").unwrap(),
            Some(spec)
        );
        let policy = store
            .load_execution_policy("acme", "prod", "router")
            .unwrap();
        assert!(!policy.allow_file_read && !policy.allow_network && !policy.allow_stdout);
        assert_eq!(store.list_capsules("acme", "prod").unwrap(), vec!["router"]);
        assert_eq!(
            store.list_all_capsules(),
            vec![("acme".into(), "prod".into(), "router".into())]
        );
        assert!(store.delete_capsule("acme", "prod", "router").unwrap());
        assert!(!store.capsule_exists("acme", "prod", "router"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v1_stores_are_refused() {
        let root = temp_root("v1");
        let cap = root.join("tenants/t/jobs/j/capsules/c");
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::write(cap.join("memory.json"), "{}").unwrap();
        let err = Store::open_or_init(root.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("v1 store") && err.contains("syntra migrate"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
