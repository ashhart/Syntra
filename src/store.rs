/// Lycan persistent store — filesystem-backed capsule registry.
///
/// Container is disposable. Store is sacred.
/// All mutable runtime data lives under a configurable root directory.
/// Tenant, job, and capsule isolation is enforced by path validation.
///
/// Hierarchy: tenant / job / capsule
/// Old API without job maps to job="default".

use sha2::{Sha256, Digest};
use std::path::{Path, PathBuf};
use std::io::Write;

pub struct LycanStore {
    root: PathBuf,
}

/// Validate a tenant, job, or capsule name: [a-zA-Z0-9_-]+ only.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if name.len() > 128 {
        return Err("name too long (max 128 chars)".to_string());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!("name contains path traversal: {name}"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("name must match [a-zA-Z0-9_-]+: {name}"));
    }
    Ok(())
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs()
}

#[allow(dead_code)]
impl LycanStore {
    pub fn open(path: &str) -> Result<Self, String> {
        let root = PathBuf::from(path);
        if !root.exists() {
            return Err(format!("store does not exist: {path}"));
        }
        Ok(Self { root })
    }

    pub fn init(path: &str) -> Result<Self, String> {
        let root = PathBuf::from(path);
        std::fs::create_dir_all(root.join("tenants"))
            .map_err(|e| format!("cannot create store: {e}"))?;
        Ok(Self { root })
    }

    pub fn open_or_init(path: &str) -> Result<Self, String> {
        let root = PathBuf::from(path);
        if root.join("tenants").exists() { Self::open(path) } else { Self::init(path) }
    }

    pub fn root_path(&self) -> &Path { &self.root }

    // ── Path resolution ──

    fn tenant_dir(&self, tenant: &str) -> Result<PathBuf, String> {
        validate_name(tenant)?;
        Ok(self.root.join("tenants").join(tenant))
    }

    pub fn job_dir(&self, tenant: &str, job: &str) -> Result<PathBuf, String> {
        validate_name(tenant)?;
        validate_name(job)?;
        Ok(self.root.join("tenants").join(tenant).join("jobs").join(job))
    }

    pub fn capsule_dir_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<PathBuf, String> {
        validate_name(capsule)?;
        Ok(self.job_dir(tenant, job)?.join("capsules").join(capsule))
    }

    pub fn graph_path_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<PathBuf, String> {
        Ok(self.capsule_dir_in_job(tenant, job, capsule)?.join("current.lyc"))
    }

    fn snapshots_dir_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<PathBuf, String> {
        Ok(self.capsule_dir_in_job(tenant, job, capsule)?.join("snapshots"))
    }

    // Old API: delegate to job="default"
    pub fn capsule_dir(&self, tenant: &str, capsule: &str) -> Result<PathBuf, String> {
        self.capsule_dir_in_job(tenant, "default", capsule)
    }
    pub fn graph_path(&self, tenant: &str, capsule: &str) -> Result<PathBuf, String> {
        self.graph_path_in_job(tenant, "default", capsule)
    }

    // ── Tenant operations ──

    pub fn create_tenant(&self, tenant: &str) -> Result<(), String> {
        let dir = self.tenant_dir(tenant)?;
        std::fs::create_dir_all(dir.join("jobs").join("default").join("capsules"))
            .map_err(|e| format!("cannot create tenant: {e}"))
    }

    pub fn list_tenants(&self) -> Result<Vec<String>, String> {
        list_subdirs(&self.root.join("tenants"))
    }

    // ── Job operations ──

    pub fn create_job(&self, tenant: &str, job_id: &str, name: &str, description: &str, metadata: &serde_json::Value) -> Result<serde_json::Value, String> {
        let dir = self.job_dir(tenant, job_id)?;
        if dir.join("job.json").exists() {
            return Err("job already exists".to_string());
        }
        std::fs::create_dir_all(dir.join("capsules"))
            .map_err(|e| format!("cannot create job: {e}"))?;
        let ts = timestamp_secs();
        let job = serde_json::json!({
            "id": job_id,
            "name": if name.is_empty() { job_id } else { name },
            "description": description,
            "metadata": metadata,
            "createdAt": ts,
            "updatedAt": ts,
        });
        self.write_atomic(&dir.join("job.json"), job.to_string().as_bytes())?;
        Ok(job)
    }

    pub fn list_jobs(&self, tenant: &str) -> Result<Vec<serde_json::Value>, String> {
        validate_name(tenant)?;
        let dir = self.tenant_dir(tenant)?.join("jobs");
        if !dir.exists() { return Ok(vec![]); }
        let mut jobs = Vec::new();
        for name in list_subdirs(&dir)? {
            let job_path = dir.join(&name).join("job.json");
            let mut job: serde_json::Value = if job_path.exists() {
                let text = std::fs::read_to_string(&job_path).unwrap_or_default();
                serde_json::from_str(&text).unwrap_or(serde_json::json!({"id": name}))
            } else {
                serde_json::json!({"id": name})
            };
            // Add capsule count
            let caps = self.list_capsules_in_job(tenant, &name).unwrap_or_default();
            job.as_object_mut().map(|m| m.insert("capsules".into(), serde_json::json!(caps.len())));
            jobs.push(job);
        }
        Ok(jobs)
    }

    pub fn get_job(&self, tenant: &str, job: &str) -> Result<serde_json::Value, String> {
        let dir = self.job_dir(tenant, job)?;
        let job_path = dir.join("job.json");
        let mut j: serde_json::Value = if job_path.exists() {
            let text = std::fs::read_to_string(&job_path).unwrap_or_default();
            serde_json::from_str(&text).unwrap_or(serde_json::json!({"id": job}))
        } else {
            serde_json::json!({"id": job})
        };
        let caps = self.list_capsules_in_job(tenant, job).unwrap_or_default();
        j.as_object_mut().map(|m| m.insert("capsuleList".into(), serde_json::json!(caps)));
        Ok(j)
    }

    fn touch_job(&self, tenant: &str, job: &str) {
        if let Ok(dir) = self.job_dir(tenant, job) {
            let job_path = dir.join("job.json");
            if job_path.exists() {
                if let Ok(text) = std::fs::read_to_string(&job_path) {
                    if let Ok(mut j) = serde_json::from_str::<serde_json::Value>(&text) {
                        j.as_object_mut().map(|m| m.insert("updatedAt".into(), serde_json::json!(timestamp_secs())));
                        std::fs::write(&job_path, j.to_string()).ok();
                    }
                }
            }
        }
    }

    // ── Capsule operations (job-aware) ──

    pub fn list_capsules_in_job(&self, tenant: &str, job: &str) -> Result<Vec<String>, String> {
        let dir = self.capsule_dir_in_job(tenant, job, "placeholder")?.parent().unwrap().to_path_buf();
        if !dir.exists() { return Ok(vec![]); }
        list_subdirs(&dir)
    }

    pub fn list_capsules(&self, tenant: &str) -> Result<Vec<String>, String> {
        self.list_capsules_in_job(tenant, "default")
    }

    pub fn install_capsule_bytes_in_job(&self, tenant: &str, job: &str, capsule: &str, data: &[u8]) -> Result<(), String> {
        self.create_tenant(tenant)?;
        let cap_dir = self.capsule_dir_in_job(tenant, job, capsule)?;
        std::fs::create_dir_all(&cap_dir)
            .map_err(|e| format!("cannot create capsule dir: {e}"))?;
        std::fs::create_dir_all(cap_dir.join("snapshots")).ok();
        self.write_atomic(&cap_dir.join("current.lyc"), data)?;

        let hash = sha256_hex(data);
        let manifest = serde_json::json!({"name": capsule, "tenant": tenant, "job": job, "hash": hash, "installed": timestamp_secs()});
        std::fs::write(cap_dir.join("manifest.json"), manifest.to_string()).ok();

        let policy = r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": true
}"#;
        if !cap_dir.join("policy.json").exists() {
            std::fs::write(cap_dir.join("policy.json"), policy).ok();
        }
        // Ensure job dir exists with job.json
        let job_dir = self.job_dir(tenant, job)?;
        if !job_dir.join("job.json").exists() {
            let j = serde_json::json!({"id": job, "name": job, "createdAt": timestamp_secs(), "updatedAt": timestamp_secs()});
            std::fs::write(job_dir.join("job.json"), j.to_string()).ok();
        }
        self.touch_job(tenant, job);
        Ok(())
    }

    pub fn install_capsule_bytes(&self, tenant: &str, capsule: &str, data: &[u8]) -> Result<(), String> {
        self.install_capsule_bytes_in_job(tenant, "default", capsule, data)
    }

    pub fn install_capsule(&self, tenant: &str, capsule: &str, lyc_path: &str) -> Result<(), String> {
        let data = std::fs::read(lyc_path).map_err(|e| format!("cannot read {lyc_path}: {e}"))?;
        self.install_capsule_bytes(tenant, capsule, &data)
    }

    // ── Graph I/O (job-aware) ──

    pub fn load_graph_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<Vec<u8>, String> {
        let path = self.graph_path_in_job(tenant, job, capsule)?;
        std::fs::read(&path).map_err(|e| format!("cannot read graph: {e}"))
    }
    pub fn load_graph(&self, tenant: &str, capsule: &str) -> Result<Vec<u8>, String> {
        self.load_graph_in_job(tenant, "default", capsule)
    }

    pub fn save_graph_in_job(&self, tenant: &str, job: &str, capsule: &str, data: &[u8]) -> Result<(), String> {
        let path = self.graph_path_in_job(tenant, job, capsule)?;
        self.write_atomic(&path, data)?;
        self.touch_job(tenant, job);
        Ok(())
    }
    pub fn save_graph(&self, tenant: &str, capsule: &str, data: &[u8]) -> Result<(), String> {
        self.save_graph_in_job(tenant, "default", capsule, data)
    }

    // ── Policy (job-aware) ──

    pub fn load_policy_json_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<String, String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("policy.json");
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read policy: {e}"))
    }
    pub fn load_policy_json(&self, tenant: &str, capsule: &str) -> Result<String, String> {
        self.load_policy_json_in_job(tenant, "default", capsule)
    }

    pub fn load_execution_policy_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<crate::context::ExecutionPolicy, String> {
        let text = self.load_policy_json_in_job(tenant, job, capsule)?;
        parse_execution_policy(&text)
    }
    pub fn load_execution_policy(&self, tenant: &str, capsule: &str) -> Result<crate::context::ExecutionPolicy, String> {
        self.load_execution_policy_in_job(tenant, "default", capsule)
    }

    pub fn save_policy_json_in_job(&self, tenant: &str, job: &str, capsule: &str, json: &str) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("policy.json");
        self.write_atomic(&path, json.as_bytes())
    }
    pub fn save_policy_json(&self, tenant: &str, capsule: &str, json: &str) -> Result<(), String> {
        self.save_policy_json_in_job(tenant, "default", capsule, json)
    }

    pub fn capsule_exists_in_job(&self, tenant: &str, job: &str, capsule: &str) -> bool {
        self.graph_path_in_job(tenant, job, capsule).map(|p| p.exists()).unwrap_or(false)
    }
    pub fn capsule_exists(&self, tenant: &str, capsule: &str) -> bool {
        self.capsule_exists_in_job(tenant, "default", capsule)
    }

    // ── Snapshots (job-aware) ──

    pub fn snapshot_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<String, String> {
        let data = self.load_graph_in_job(tenant, job, capsule)?;
        let snap_dir = self.snapshots_dir_in_job(tenant, job, capsule)?;
        std::fs::create_dir_all(&snap_dir).ok();
        let name = format!("{}", timestamp_secs());
        self.write_atomic(&snap_dir.join(format!("{name}.lyc")), &data)?;
        Ok(name)
    }
    pub fn snapshot(&self, tenant: &str, capsule: &str) -> Result<String, String> {
        self.snapshot_in_job(tenant, "default", capsule)
    }

    pub fn list_snapshots_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<Vec<String>, String> {
        let mut snaps = Vec::new();
        let capsule_dir = self.capsule_dir_in_job(tenant, job, capsule)?;
        let dirs = [
            capsule_dir.join("snapshots"),
            capsule_dir.join("current.lyc.snapshots"),
        ];
        for dir in dirs {
            if !dir.exists() { continue; }
            for entry in std::fs::read_dir(&dir).map_err(|e| format!("cannot read snapshots: {e}"))? {
                if let Ok(e) = entry {
                    if let Some(name) = e.file_name().to_str() {
                        if name.ends_with(".lyc") { snaps.push(name.trim_end_matches(".lyc").to_string()); }
                    }
                }
            }
        }
        snaps.sort();
        snaps.dedup();
        Ok(snaps)
    }
    pub fn list_snapshots(&self, tenant: &str, capsule: &str) -> Result<Vec<String>, String> {
        self.list_snapshots_in_job(tenant, "default", capsule)
    }

    // ── Append-only logs (job-aware) ──

    fn append_log_in_job(&self, tenant: &str, job: &str, capsule: &str, filename: &str, entry: &str) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join(filename);
        let mut f = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path)
            .map_err(|e| format!("cannot open {filename}: {e}"))?;
        writeln!(f, "{}", entry).map_err(|e| format!("cannot write {filename}: {e}"))
    }

    fn read_log_in_job(&self, tenant: &str, job: &str, capsule: &str, filename: &str) -> Result<String, String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join(filename);
        if !path.exists() { return Ok(String::new()); }
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read {filename}: {e}"))
    }

    pub fn append_audit_in_job(&self, t: &str, j: &str, c: &str, e: &str) -> Result<(), String> { self.append_log_in_job(t, j, c, "audit.jsonl", e) }
    pub fn append_audit(&self, t: &str, c: &str, e: &str) -> Result<(), String> { self.append_audit_in_job(t, "default", c, e) }

    pub fn append_feedback_log_in_job(&self, t: &str, j: &str, c: &str, e: &str) -> Result<(), String> { self.append_log_in_job(t, j, c, "feedback.jsonl", e) }
    pub fn append_feedback_log(&self, t: &str, c: &str, e: &str) -> Result<(), String> { self.append_feedback_log_in_job(t, "default", c, e) }

    pub fn append_evolution_log_in_job(&self, t: &str, j: &str, c: &str, e: &str) -> Result<(), String> { self.append_log_in_job(t, j, c, "evolution.jsonl", e) }
    pub fn append_evolution_log(&self, t: &str, c: &str, e: &str) -> Result<(), String> { self.append_evolution_log_in_job(t, "default", c, e) }

    pub fn append_decision_log_in_job(&self, t: &str, j: &str, c: &str, e: &str) -> Result<(), String> { self.append_log_in_job(t, j, c, "decision.jsonl", e) }
    pub fn append_decision_log(&self, t: &str, c: &str, e: &str) -> Result<(), String> { self.append_decision_log_in_job(t, "default", c, e) }

    pub fn read_audits_in_job(&self, t: &str, j: &str, c: &str) -> Result<String, String> { self.read_log_in_job(t, j, c, "audit.jsonl") }
    pub fn read_audits(&self, t: &str, c: &str) -> Result<String, String> { self.read_audits_in_job(t, "default", c) }

    pub fn read_feedback_log_in_job(&self, t: &str, j: &str, c: &str) -> Result<String, String> { self.read_log_in_job(t, j, c, "feedback.jsonl") }
    pub fn read_feedback_log(&self, t: &str, c: &str) -> Result<String, String> { self.read_feedback_log_in_job(t, "default", c) }

    pub fn read_evolution_log_in_job(&self, t: &str, j: &str, c: &str) -> Result<String, String> {
        let dir = self.capsule_dir_in_job(t, j, c)?;
        let mut out = String::new();
        for path in [dir.join("evolution.jsonl"), dir.join("current.lyc.evolution.jsonl")] {
            if path.exists() {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read evolution log: {e}"))?;
                out.push_str(&text);
                if !out.ends_with('\n') { out.push('\n'); }
            }
        }
        Ok(out)
    }
    pub fn read_evolution_log(&self, t: &str, c: &str) -> Result<String, String> { self.read_evolution_log_in_job(t, "default", c) }

    pub fn read_decision_log_in_job(&self, t: &str, j: &str, c: &str) -> Result<String, String> { self.read_log_in_job(t, j, c, "decision.jsonl") }
    pub fn read_decision_log(&self, t: &str, c: &str) -> Result<String, String> { self.read_decision_log_in_job(t, "default", c) }

    pub fn find_decision_in_job(&self, tenant: &str, job: &str, capsule: &str, decision_id: &str) -> Result<Option<String>, String> {
        let log = self.read_decision_log_in_job(tenant, job, capsule)?;
        for line in log.lines().rev() {
            if line.contains(decision_id) { return Ok(Some(line.to_string())); }
        }
        Ok(None)
    }
    pub fn find_decision(&self, t: &str, c: &str, id: &str) -> Result<Option<String>, String> { self.find_decision_in_job(t, "default", c, id) }

    // ── Memory sidecar (job-aware) ──

    pub fn load_memory_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<crate::learning::CapsuleMemory, String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("memory.json");
        if !path.exists() { return Ok(crate::learning::CapsuleMemory::default()); }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read memory.json: {e}"))?;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("invalid memory.json: {e}"))?;
        Ok(crate::learning::CapsuleMemory::from_json(&json))
    }
    pub fn load_memory(&self, t: &str, c: &str) -> Result<crate::learning::CapsuleMemory, String> { self.load_memory_in_job(t, "default", c) }

    pub fn save_memory_in_job(&self, tenant: &str, job: &str, capsule: &str, mem: &crate::learning::CapsuleMemory) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("memory.json");
        self.write_atomic(&path, mem.to_json().to_string().as_bytes())
    }
    pub fn save_memory(&self, t: &str, c: &str, m: &crate::learning::CapsuleMemory) -> Result<(), String> { self.save_memory_in_job(t, "default", c, m) }

    pub fn load_learning_config_in_job(&self, tenant: &str, job: &str, capsule: &str) -> crate::learning::LearningConfig {
        let path = match self.capsule_dir_in_job(tenant, job, capsule) {
            Ok(d) => d.join("learning.json"),
            Err(_) => return crate::learning::LearningConfig::default(),
        };
        if !path.exists() { return crate::learning::LearningConfig::default(); }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
        crate::learning::LearningConfig::from_json(&json)
    }
    pub fn load_learning_config(&self, t: &str, c: &str) -> crate::learning::LearningConfig { self.load_learning_config_in_job(t, "default", c) }

    pub fn save_learning_config_in_job(&self, tenant: &str, job: &str, capsule: &str, cfg: &crate::learning::LearningConfig) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("learning.json");
        self.write_atomic(&path, cfg.to_json().to_string().as_bytes())
    }
    pub fn save_learning_config(&self, t: &str, c: &str, cfg: &crate::learning::LearningConfig) -> Result<(), String> { self.save_learning_config_in_job(t, "default", c, cfg) }

    // ── Locking (job-aware) ──

    pub fn lock_capsule_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<PathBuf, String> {
        let lock_path = self.capsule_dir_in_job(tenant, job, capsule)?.join(".evolve.lock");
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) {
            Ok(mut f) => { write!(f, "{}", std::process::id()).ok(); Ok(lock_path) }
            Err(_) => Err("capsule is locked by another operation".to_string()),
        }
    }
    pub fn lock_capsule(&self, t: &str, c: &str) -> Result<PathBuf, String> { self.lock_capsule_in_job(t, "default", c) }

    pub fn unlock_capsule_in_job(&self, tenant: &str, job: &str, capsule: &str) {
        if let Ok(p) = self.capsule_dir_in_job(tenant, job, capsule).map(|d| d.join(".evolve.lock")) {
            let _ = std::fs::remove_file(p);
        }
    }
    pub fn unlock_capsule(&self, t: &str, c: &str) { self.unlock_capsule_in_job(t, "default", c) }

    // ── Deletion (GDPR Art.17 / data erasure) ──

    pub fn delete_capsule_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<(), String> {
        let dir = self.capsule_dir_in_job(tenant, job, capsule)?;
        if !dir.exists() { return Err("capsule not found".into()); }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot delete capsule: {e}"))
    }
    pub fn delete_capsule(&self, t: &str, c: &str) -> Result<(), String> { self.delete_capsule_in_job(t, "default", c) }

    pub fn delete_job(&self, tenant: &str, job: &str) -> Result<(), String> {
        let dir = self.job_dir(tenant, job)?;
        if !dir.exists() { return Err("job not found".into()); }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot delete job: {e}"))
    }

    pub fn delete_tenant(&self, tenant: &str) -> Result<(), String> {
        let dir = self.tenant_dir(tenant)?;
        if !dir.exists() { return Err("tenant not found".into()); }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot delete tenant: {e}"))
    }

    pub fn purge_logs_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Result<u32, String> {
        let dir = self.capsule_dir_in_job(tenant, job, capsule)?;
        let mut count = 0u32;
        for log in ["audit.jsonl", "decision.jsonl", "feedback.jsonl", "evolution.jsonl"] {
            let path = dir.join(log);
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| format!("cannot delete {log}: {e}"))?;
                count += 1;
            }
        }
        Ok(count)
    }

    // ── Atomic write ──

    fn write_atomic(&self, path: &Path, data: &[u8]) -> Result<(), String> {
        let tmp_path = path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("cannot create temp file: {e}"))?;
        f.write_all(data).map_err(|e| format!("cannot write temp file: {e}"))?;
        f.sync_all().map_err(|e| format!("cannot fsync temp file: {e}"))?;
        std::fs::rename(&tmp_path, path).map_err(|e| format!("cannot rename temp to target: {e}"))
    }

    // ── Inspect ──

    pub fn inspect(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("store: {}\n", self.root.display()));
        if let Ok(tenants) = self.list_tenants() {
            out.push_str(&format!("tenants: {}\n", tenants.len()));
            for t in &tenants {
                out.push_str(&format!("  {t}/\n"));
                if let Ok(jobs) = self.list_jobs(t) {
                    for j in &jobs {
                        let jid = j.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        out.push_str(&format!("    {jid}/\n"));
                        if let Ok(caps) = self.list_capsules_in_job(t, jid) {
                            for c in &caps {
                                let marker = if self.capsule_exists_in_job(t, jid, c) { "●" } else { "○" };
                                out.push_str(&format!("      {marker} {c}\n"));
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

fn list_subdirs(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.exists() { return Ok(vec![]); }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("cannot read dir: {e}"))? {
        if let Ok(e) = entry {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() { names.push(name.to_string()); }
            }
        }
    }
    names.sort();
    Ok(names)
}

fn parse_execution_policy(text: &str) -> Result<crate::context::ExecutionPolicy, String> {
    let json: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| format!("invalid policy.json: {e}"))?;
    fn bf(j: &serde_json::Value, k: &str, d: bool) -> bool {
        j.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
    }
    let allowed_hosts = json.get("allowed_hosts")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(crate::context::ExecutionPolicy {
        allow_stdout: bf(&json, "allow_stdout", true),
        allow_stdin: bf(&json, "allow_stdin", false),
        allow_file_read: bf(&json, "allow_file_read", false),
        allow_file_write: bf(&json, "allow_file_write", false),
        allow_network: bf(&json, "allow_network", false),
        file_root: json.get("file_root").and_then(|v| v.as_str()).map(String::from),
        allowed_hosts,
        deny_private_networks: bf(&json, "deny_private_networks", true),
    })
}
