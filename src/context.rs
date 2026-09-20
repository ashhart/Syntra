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
    /// Root directory for file capabilities. Paths resolved relative to this.
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

impl ExecutionPolicy {
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
