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

    /// Best-effort read of a capsule's `manifest.json`. Returns `None` if the
    /// capsule directory or manifest file is missing, or if the manifest is
    /// malformed. Used by `/admin/capsules` to surface a friendly capsule
    /// name when one is sidecarred next to the binary.
    pub fn read_manifest_in_job(
        &self,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Option<serde_json::Value> {
        let dir = self.capsule_dir_in_job(tenant, job, capsule).ok()?;
        let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Enumerate every (tenant, job, capsule) tuple known to this store.
    /// Used by /metrics to surface lifecycle and meta-bandit gauges at
    /// scrape time. Best-effort: missing tenants/jobs are silently skipped.
    pub fn list_all_capsules(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        let tenants = match self.list_tenants() {
            Ok(t) => t,
            Err(_) => return out,
        };
        for tenant in tenants {
            let jobs_dir = match self.tenant_dir(&tenant) {
                Ok(d) => d.join("jobs"),
                Err(_) => continue,
            };
            if !jobs_dir.exists() { continue; }
            let jobs = match list_subdirs(&jobs_dir) {
                Ok(j) => j,
                Err(_) => continue,
            };
            for job in jobs {
                let caps = self.list_capsules_in_job(&tenant, &job).unwrap_or_default();
                for capsule in caps {
                    out.push((tenant.clone(), job.clone(), capsule));
                }
            }
        }
        out
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

    pub fn load_reward_spec_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Option<serde_json::Value> {
        let path = self.capsule_dir_in_job(tenant, job, capsule).ok()?.join("reward_spec.json");
        let text = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_reward_spec_in_job(&self, tenant: &str, job: &str, capsule: &str, spec: &serde_json::Value) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("reward_spec.json");
        self.write_atomic(&path, spec.to_string().as_bytes())
    }

    pub fn load_warmup_state_in_job(&self, tenant: &str, job: &str, capsule: &str) -> Option<crate::warmup::WarmupState> {
        let path = self.capsule_dir_in_job(tenant, job, capsule).ok()?.join("warmup.json");
        let text = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_warmup_state_in_job(&self, tenant: &str, job: &str, capsule: &str, w: &crate::warmup::WarmupState) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?.join("warmup.json");
        let json = serde_json::to_string_pretty(w).map_err(|e| format!("serialize warmup: {e}"))?;
        self.write_atomic(&path, json.as_bytes())
    }

    // ── Hierarchical bandits (step 5 of roadmap.md hierarchical wiring) ──
    //
    // Two distinct sidecars, both read-by-runtime:
    //   * hierarchical_spec.json — the tree shape, written once at install
    //     time by Syntra's capsule_compiler. Read-only here.
    //   * hierarchical_state.json — the per-HierState bandit buckets,
    //     mutated by /decide and /feedback. Read + atomic-write.
    //
    // Both follow the same Option-returning pattern as the warmup helpers
    // above: a missing or malformed sidecar reads as None (callers then
    // either fall back to flat-AdaptiveChoice behaviour or treat the
    // capsule as cold). On save we write atomically and propagate I/O
    // errors as String.

    /// Read the hierarchical-tree spec written by capsule_compiler at
    /// install time. Returns None when the capsule was installed as flat
    /// (no `hierarchical_options` declared) or when the sidecar is
    /// missing / unparseable.
    pub fn load_hierarchical_spec_in_job(
        &self, tenant: &str, job: &str, capsule: &str,
    ) -> Option<crate::hierarchical::HierarchicalSpec> {
        let path = self.capsule_dir_in_job(tenant, job, capsule).ok()?
            .join("hierarchical_spec.json");
        let text = std::fs::read_to_string(&path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        crate::hierarchical::HierarchicalSpec::from_json(&value).ok()
    }

    /// Read the persisted per-HierState bandit state. Returns None when
    /// no state has been persisted yet (capsule is freshly installed) or
    /// when the sidecar is missing / unparseable. The runtime's decide
    /// path uses this in tandem with `load_hierarchical_spec_in_job` —
    /// the spec is the immutable tree shape; the state is the mutable
    /// bandit history keyed by HierState.
    pub fn load_hierarchical_state_in_job(
        &self, tenant: &str, job: &str, capsule: &str,
    ) -> Option<crate::hierarchical_state::HierarchicalCapsuleState> {
        let path = self.capsule_dir_in_job(tenant, job, capsule).ok()?
            .join("hierarchical_state.json");
        let text = std::fs::read_to_string(&path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        crate::hierarchical_state::HierarchicalCapsuleState::from_json(&value).ok()
    }

    /// Atomically persist the hierarchical bandit state. Same atomic-write
    /// guarantee as the other sidecar helpers.
    pub fn save_hierarchical_state_in_job(
        &self, tenant: &str, job: &str, capsule: &str,
        state: &crate::hierarchical_state::HierarchicalCapsuleState,
    ) -> Result<(), String> {
        let path = self.capsule_dir_in_job(tenant, job, capsule)?
            .join("hierarchical_state.json");
        let json = serde_json::to_string_pretty(&state.to_json())
            .map_err(|e| format!("serialize hierarchical_state: {e}"))?;
        self.write_atomic(&path, json.as_bytes())
    }

    /// Atomically persist the hierarchical-tree spec. Called from the
    /// `PUT /hierarchical_spec` install-side endpoint so an operator
    /// who compiled the capsule out-of-band can upload the sidecar
    /// into the runtime store. `capsule_compiler` writes the same
    /// file at compile-output time; this method is the upload
    /// counterpart for the install path.
    pub fn save_hierarchical_spec_in_job(
        &self, tenant: &str, job: &str, capsule: &str,
        spec: &crate::hierarchical::HierarchicalSpec,
    ) -> Result<(), String> {
        spec.validate()?;
        let path = self.capsule_dir_in_job(tenant, job, capsule)?
            .join("hierarchical_spec.json");
        let json = serde_json::to_string_pretty(&spec.to_json())
            .map_err(|e| format!("serialize hierarchical_spec: {e}"))?;
        self.write_atomic(&path, json.as_bytes())
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

#[cfg(test)]
mod hierarchical_sidecar_tests {
    use super::*;
    use crate::hierarchical::{HierarchicalOption, HierarchicalSpec, RewardKind, RewardSpec};
    use crate::hierarchical_state::HierarchicalCapsuleState;

    fn cont_reward() -> RewardSpec {
        RewardSpec { kind: RewardKind::Continuous, range: Some([-1.0, 1.0]) }
    }

    fn build_2x2_spec() -> HierarchicalSpec {
        HierarchicalSpec {
            options: vec![
                HierarchicalOption::Branch {
                    name: "us".to_string(),
                    sub_capsule: Box::new(HierarchicalSpec {
                        options: vec![
                            HierarchicalOption::Leaf { name: "us_a".into() },
                            HierarchicalOption::Leaf { name: "us_b".into() },
                        ],
                        reward: cont_reward(),
                        reward_propagation: None,
                    }),
                },
                HierarchicalOption::Branch {
                    name: "eu".to_string(),
                    sub_capsule: Box::new(HierarchicalSpec {
                        options: vec![
                            HierarchicalOption::Leaf { name: "eu_a".into() },
                            HierarchicalOption::Leaf { name: "eu_b".into() },
                        ],
                        reward: cont_reward(),
                        reward_propagation: None,
                    }),
                },
            ],
            reward: cont_reward(),
            reward_propagation: None,
        }
    }

    fn fresh_store() -> (LycanStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "lycan-store-hier-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = LycanStore::init(&dir.to_string_lossy()).unwrap();
        (store, dir)
    }

    #[test]
    fn hierarchical_spec_round_trips_when_present() {
        // Simulate what capsule_compiler writes at install time, then read
        // it back through the store's helper. Sidecar at capsule-dir level
        // means we need a real capsule directory; an empty placeholder .lyc
        // is enough to materialise it.
        let (store, root) = fresh_store();
        store.install_capsule_bytes_in_job("t", "j", "c", b"placeholder").unwrap();

        let spec = build_2x2_spec();
        let path = store.capsule_dir_in_job("t", "j", "c").unwrap()
            .join("hierarchical_spec.json");
        std::fs::write(&path, serde_json::to_string_pretty(&spec.to_json()).unwrap()).unwrap();

        let loaded = store.load_hierarchical_spec_in_job("t", "j", "c")
            .expect("spec must load");
        assert_eq!(loaded.max_depth(), 2);
        assert_eq!(loaded.count_leaves(), 4);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hierarchical_state_save_load_round_trip() {
        let (store, root) = fresh_store();
        store.install_capsule_bytes_in_job("t", "j", "c", b"placeholder").unwrap();

        // Build a tiny state by simulating a few decides + feedbacks.
        let spec = build_2x2_spec();
        let mut state = HierarchicalCapsuleState::new(spec);
        // Drive ~10 rounds against a deterministic RNG so the state has
        // some buckets and weights to round-trip.
        let mut rng_state: u64 = 12345;
        let mut next = || -> f64 {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng_state as u32) as f64 / (u32::MAX as f64 + 1.0)
        };
        for _ in 0..10 {
            let pair = (next(), next());
            let decision = state.select_path(|| pair).expect("select_path");
            state.apply_feedback(&decision.path, &decision.path, 0.7);
        }

        store.save_hierarchical_state_in_job("t", "j", "c", &state)
            .expect("save");
        let loaded = store.load_hierarchical_state_in_job("t", "j", "c")
            .expect("load");

        // Round-trip preserves the tree shape and per-bucket weights.
        // (Exact-bit equality through serde_json isn't guaranteed for f64
        // — a value like 0.46448049816388076 can round-trip to
        // 0.4644804981638808 (1 ULP). The round-trip is structurally
        // exact and within float precision; that's the right assertion
        // here, not byte-for-byte string equality.)
        assert_eq!(loaded.spec.count_leaves(), state.spec.count_leaves());
        assert_eq!(loaded.spec.max_depth(), state.spec.max_depth());

        let loaded_buckets = loaded.to_json().get("buckets").cloned().unwrap_or_default();
        let state_buckets = state.to_json().get("buckets").cloned().unwrap_or_default();
        let loaded_keys: std::collections::BTreeSet<String> = loaded_buckets.as_object()
            .map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let state_keys: std::collections::BTreeSet<String> = state_buckets.as_object()
            .map(|m| m.keys().cloned().collect()).unwrap_or_default();
        assert_eq!(loaded_keys, state_keys,
                   "bucket key set must round-trip exactly");

        // Spot-check one bucket's weights survive within 1e-9 of original.
        for key in &loaded_keys {
            let lw = loaded_buckets[key]["weights"].as_array().unwrap();
            let sw = state_buckets[key]["weights"].as_array().unwrap();
            assert_eq!(lw.len(), sw.len(),
                       "bucket {key} weight length must match");
            for (a, b) in lw.iter().zip(sw.iter()) {
                let av = a.as_f64().unwrap();
                let bv = b.as_f64().unwrap();
                assert!((av - bv).abs() < 1e-9,
                        "bucket {key} weight mismatch: {av} vs {bv}");
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hierarchical_spec_returns_none_when_absent() {
        // Pre-existing flat capsule has no hierarchical sidecar — the
        // loader returns None, not an error. This is the path
        // `do_decide` will use to detect "treat as flat".
        let (store, root) = fresh_store();
        store.install_capsule_bytes_in_job("t", "j", "c", b"placeholder").unwrap();
        assert!(store.load_hierarchical_spec_in_job("t", "j", "c").is_none());
        assert!(store.load_hierarchical_state_in_job("t", "j", "c").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
