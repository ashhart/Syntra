use crate::capabilities::CapValue;
/// Execution context — the runtime boundary for Lycan programs.
///
/// Carries policy constraints, injected input, working directory for
/// file sandboxing, and (future) audit/resource metadata.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Per-decision buffer for `runtime.publish`. `Rc<RefCell>` so callers
/// can read it back after the executor consumes the `ExecutionContext`;
/// `BTreeMap` for deterministic (sorted) output order.
pub type PublishedBuffer = Rc<RefCell<BTreeMap<String, serde_json::Value>>>;

/// Construct a fresh empty publish buffer.
pub fn new_published_buffer() -> PublishedBuffer {
    Rc::new(RefCell::new(BTreeMap::new()))
}

/// What a program is allowed to do at runtime.
#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    pub allow_stdout: bool,
    pub allow_stdin: bool,
    pub allow_file_read: bool,
    pub allow_file_write: bool,
    pub allow_network: bool,
    /// Permit plain `http://` when the network sandbox is active. Off by
    /// default: sandboxed programs may only use `https://`, so an
    /// allow-listed host cannot be reached over an unencrypted channel.
    pub allow_insecure_http: bool,
    /// Root directory for file capabilities, relative to the execution's
    /// working directory. Never absolute and never containing `..`
    /// (enforced by [`ExecutionPolicy::from_policy_json`]).
    pub file_root: Option<String>,
    /// Allowed HTTP hosts. Empty = deny all outbound HTTP when policy is active.
    pub allowed_hosts: Vec<String>,
    /// Block requests to localhost, RFC1918, link-local, metadata IPs.
    pub deny_private_networks: bool,
    /// Wall-clock budget for one execution, enforced by the graph executor
    /// (checked every 64 node evaluations). `None` = unlimited — only ever
    /// set deliberately (CLI/tests); any policy-backed path that does not
    /// specify a budget gets `DEFAULT_EXECUTION_MS` at load time, so a
    /// policy.json that forgets the field still has a ceiling.
    pub max_execution_ms: Option<u64>,
}

/// Budget applied when a policy.json omits `max_execution_ms`.
pub const DEFAULT_EXECUTION_MS: u64 = 30_000;

/// Upper bound for `max_execution_ms` in a policy.json. One execution
/// occupies a server worker for its whole budget, so an unbounded value
/// lets a single capsule starve every other tenant.
pub const MAX_EXECUTION_MS_LIMIT: u64 = 60_000;

/// Every key a policy.json may contain. Unknown keys are rejected so a
/// typo (`allow_netwrok`) or a field from a newer version can never be
/// silently ignored.
pub const POLICY_KEYS: &[&str] = &[
    "allow_stdout",
    "allow_stdin",
    "allow_file_read",
    "allow_file_write",
    "allow_network",
    "allow_insecure_http",
    "allow_self_modify",
    "file_root",
    "allowed_hosts",
    "deny_private_networks",
    "max_execution_ms",
    "max_memory_bytes",
];

/// Validate a policy `file_root`: a relative path made only of normal
/// components (or `.`). Absolute paths, drive prefixes and `..` are
/// refused because the root must stay inside the execution's working
/// directory — an absolute root is an unrestricted filesystem grant.
pub fn validate_file_root(root: &str) -> Result<(), String> {
    use std::path::Component;
    if root.is_empty() {
        return Err(
            "file_root must not be empty (omit it to use the capsule data directory)".into(),
        );
    }
    if root.len() > 255 || root.contains('\0') {
        return Err("file_root must be at most 255 bytes with no NUL".into());
    }
    if root.starts_with('\\') || root.contains(':') {
        return Err(format!("file_root '{root}' must be a relative path"));
    }
    for component in std::path::Path::new(root).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("file_root '{root}' must not contain '..'"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("file_root '{root}' must be a relative path"));
            }
        }
    }
    Ok(())
}

/// Validate one `allowed_hosts` entry: a DNS name or IP literal, optionally
/// a `*.` wildcard prefix. No scheme, port, path, userinfo or whitespace —
/// those would never match and usually mean the operator meant something else.
pub fn validate_allowed_host(host: &str) -> Result<(), String> {
    let name = host.strip_prefix("*.").unwrap_or(host);
    // A colon is only legal as part of an IPv6 literal; otherwise it is a port.
    let is_ipv6_literal = name.parse::<std::net::Ipv6Addr>().is_ok() && !host.starts_with("*.");
    let valid = is_ipv6_literal
        || (!name.is_empty()
            && name.len() <= 253
            && !name.starts_with('.')
            && !name.ends_with('.')
            && !name.contains("..")
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'));
    if !valid {
        return Err(format!(
            "allowed_hosts entry '{host}' must be a bare host name (e.g. api.example.com or *.example.com) with no scheme, port or path"
        ));
    }
    Ok(())
}

impl ExecutionPolicy {
    /// Parse and validate a policy.json document. Fail-closed: unknown
    /// keys, wrong types, an absolute or escaping `file_root`, malformed
    /// hosts and out-of-range budgets are all errors, never defaults.
    /// Every server-side policy read and write goes through here.
    pub fn from_policy_json(text: &str) -> Result<Self, String> {
        let json: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("invalid policy JSON: {e}"))?;
        Self::from_policy_value(&json)
    }

    /// [`Self::from_policy_json`] for an already-parsed value.
    pub fn from_policy_value(json: &serde_json::Value) -> Result<Self, String> {
        let obj = json
            .as_object()
            .ok_or_else(|| "policy must be a JSON object".to_string())?;
        for key in obj.keys() {
            if !POLICY_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "unknown policy field '{key}' (allowed: {})",
                    POLICY_KEYS.join(", ")
                ));
            }
        }
        let flag = |key: &str, default: bool| -> Result<bool, String> {
            match obj.get(key) {
                None => Ok(default),
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| format!("policy.{key} must be boolean")),
            }
        };
        let file_root = match obj.get("file_root") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => {
                let root = v
                    .as_str()
                    .ok_or_else(|| "policy.file_root must be a string".to_string())?;
                validate_file_root(root)?;
                Some(root.to_string())
            }
        };
        let allowed_hosts = match obj.get("allowed_hosts") {
            None => Vec::new(),
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| {
                    "policy.allowed_hosts must be an array of strings".to_string()
                })?;
                let mut hosts = Vec::with_capacity(arr.len());
                for h in arr {
                    let h = h.as_str().ok_or_else(|| {
                        "policy.allowed_hosts must be an array of strings".to_string()
                    })?;
                    validate_allowed_host(h)?;
                    hosts.push(h.to_ascii_lowercase());
                }
                hosts
            }
        };
        let max_execution_ms = match obj.get("max_execution_ms") {
            None => DEFAULT_EXECUTION_MS,
            Some(v) => {
                let ms = v.as_u64().ok_or_else(|| {
                    "policy.max_execution_ms must be a positive integer".to_string()
                })?;
                if ms == 0 || ms > MAX_EXECUTION_MS_LIMIT {
                    return Err(format!(
                        "policy.max_execution_ms must be between 1 and {MAX_EXECUTION_MS_LIMIT}"
                    ));
                }
                ms
            }
        };
        if let Some(v) = obj.get("max_memory_bytes")
            && v.as_u64().is_none()
        {
            return Err("policy.max_memory_bytes must be a non-negative integer".into());
        }
        // Written by `lycan capsule create` and store installs. Nothing in
        // the runtime reads it, but it must still be a boolean.
        flag("allow_self_modify", true)?;
        Ok(Self {
            allow_stdout: flag("allow_stdout", true)?,
            allow_stdin: flag("allow_stdin", false)?,
            allow_file_read: flag("allow_file_read", false)?,
            allow_file_write: flag("allow_file_write", false)?,
            allow_network: flag("allow_network", false)?,
            allow_insecure_http: flag("allow_insecure_http", false)?,
            file_root,
            allowed_hosts,
            deny_private_networks: flag("deny_private_networks", true)?,
            max_execution_ms: Some(max_execution_ms),
        })
    }

    /// Deny everything, stdio included. Used ONLY where a policy could not
    /// be read and the execution must still proceed (server `/decide`,
    /// evolve endpoint) — there the caller's contract is "nothing may
    /// happen".
    pub fn deny_all() -> Self {
        Self {
            allow_stdout: false,
            allow_stdin: false,
            allow_file_read: false,
            allow_file_write: false,
            allow_network: false,
            allow_insecure_http: false,
            file_root: None,
            allowed_hosts: vec![],
            deny_private_networks: true,
            max_execution_ms: Some(DEFAULT_EXECUTION_MS),
        }
    }
    /// The evolution/verification sandbox: no file, no network, no stdin,
    /// wall-clock budget — but stdout stays ON. The gate must RUN the
    /// host program (to measure its baseline) and the candidate; host
    /// programs report through `!p`/Print, and stdout is not a registry
    /// effect (capability-abi §2 layer 2 only): a proposer gains nothing
    /// from printing. Candidate side effects are what BUG-9 sandboxes.
    pub fn evolve_sandbox() -> Self {
        Self {
            allow_stdout: true,
            ..Self::deny_all()
        }
    }
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            allow_stdout: true,
            allow_stdin: true,
            allow_file_read: true,
            allow_file_write: true,
            allow_network: true,
            allow_insecure_http: true,
            file_root: None,
            allowed_hosts: vec![],
            deny_private_networks: true,
            max_execution_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Greedy,
    Weighted,
    EpsilonGreedy,
}

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub policy: Option<ExecutionPolicy>,
    pub input: Option<CapValue>,
    pub working_dir: Option<PathBuf>,
    pub selection_mode: SelectionMode,
    pub selection_epsilon: f64,
    /// Per-decision buffer for `runtime.publish` values. `None` for CLI,
    /// tests, and any internal caller that doesn't care about journalled
    /// publish output — `runtime.publish` becomes a silent no-op in that
    /// case so the same capsule program runs everywhere unchanged.
    pub published: Option<PublishedBuffer>,
}

impl ExecutionContext {
    #[allow(dead_code)]
    pub fn unrestricted() -> Self {
        Self {
            policy: None,
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    pub fn with_policy(policy: ExecutionPolicy) -> Self {
        Self {
            policy: Some(policy),
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    pub fn with_input(input: CapValue) -> Self {
        Self {
            policy: None,
            input: Some(input),
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    #[allow(dead_code)]
    pub fn full(policy: ExecutionPolicy, input: CapValue) -> Self {
        Self {
            policy: Some(policy),
            input: Some(input),
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn defaults_apply_when_fields_are_absent() {
        let p = ExecutionPolicy::from_policy_json("{}").unwrap();
        assert!(p.allow_stdout && !p.allow_stdin && !p.allow_file_read);
        assert!(!p.allow_file_write && !p.allow_network && !p.allow_insecure_http);
        assert!(p.deny_private_networks && p.allowed_hosts.is_empty() && p.file_root.is_none());
        assert_eq!(p.max_execution_ms, Some(DEFAULT_EXECUTION_MS));
    }

    #[test]
    fn existing_policy_files_still_parse() {
        // Shapes written by `lycan capsule create` and by store installs.
        for text in [
            r#"{"allow_stdout": true, "allow_stdin": false, "allow_file_read": false,
                "allow_file_write": false, "allow_network": false, "allow_self_modify": true,
                "max_execution_ms": 30000, "max_memory_bytes": 268435456}"#,
            r#"{"allow_stdout": true, "allow_file_read": true, "file_root": ".",
                "allowed_hosts": [], "deny_private_networks": true}"#,
        ] {
            ExecutionPolicy::from_policy_json(text).unwrap();
        }
    }

    #[test]
    fn invalid_documents_are_rejected() {
        for (text, why) in [
            (r#"[]"#, "JSON object"),
            (r#"{"allow_netwrok": true}"#, "unknown policy field"),
            (r#"{"allow_network": 1}"#, "must be boolean"),
            (r#"{"file_root": "/etc"}"#, "relative path"),
            (r#"{"file_root": "a/../../b"}"#, "must not contain '..'"),
            (r#"{"file_root": ""}"#, "must not be empty"),
            (r#"{"file_root": "C:\\data"}"#, "relative path"),
            (r#"{"file_root": 7}"#, "must be a string"),
            (r#"{"allowed_hosts": "example.com"}"#, "array of strings"),
            (
                r#"{"allowed_hosts": ["https://example.com"]}"#,
                "bare host name",
            ),
            (
                r#"{"allowed_hosts": ["example.com:443"]}"#,
                "bare host name",
            ),
            (r#"{"allowed_hosts": ["exa mple.com"]}"#, "bare host name"),
            (r#"{"max_execution_ms": 0}"#, "between 1 and"),
            (r#"{"max_execution_ms": 600000}"#, "between 1 and"),
            (r#"{"max_execution_ms": -5}"#, "positive integer"),
            (r#"{"max_memory_bytes": "big"}"#, "non-negative integer"),
        ] {
            let err = ExecutionPolicy::from_policy_json(text).unwrap_err();
            assert!(err.contains(why), "{text}: expected {why:?} in {err:?}");
        }
    }

    #[test]
    fn hosts_are_normalized_and_wildcards_accepted() {
        let p = ExecutionPolicy::from_policy_json(
            r#"{"allowed_hosts": ["API.Example.com", "*.example.org", "::1"]}"#,
        )
        .unwrap();
        assert_eq!(
            p.allowed_hosts,
            vec!["api.example.com", "*.example.org", "::1"]
        );
    }
}
