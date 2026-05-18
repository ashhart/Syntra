/// Execution context — the runtime boundary for Lycan programs.
///
/// Carries policy constraints, injected input, working directory for
/// file sandboxing, and (future) audit/resource metadata.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use crate::capabilities::CapValue;

/// Shared per-decision buffer that capabilities can write named computed
/// values into. Wrapped in `Rc<RefCell<…>>` so the buffer survives the
/// `ExecutionContext` being moved into the graph executor — the caller
/// (typically the server's `do_decide`) keeps a clone of the `Rc` and can
/// read the contents back after `executor.run()` returns. `BTreeMap` so
/// output order is deterministic (sorted by name).
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
            policy: None, input: None, working_dir: None,
            selection_mode: SelectionMode::Greedy, selection_epsilon: 0.10,
            published: None,
        }
    }

    pub fn with_policy(policy: ExecutionPolicy) -> Self {
        Self {
            policy: Some(policy), input: None, working_dir: None,
            selection_mode: SelectionMode::Greedy, selection_epsilon: 0.10,
            published: None,
        }
    }

    pub fn with_input(input: CapValue) -> Self {
        Self {
            policy: None, input: Some(input), working_dir: None,
            selection_mode: SelectionMode::Greedy, selection_epsilon: 0.10,
            published: None,
        }
    }

    #[allow(dead_code)]
    pub fn full(policy: ExecutionPolicy, input: CapValue) -> Self {
        Self {
            policy: Some(policy), input: Some(input), working_dir: None,
            selection_mode: SelectionMode::Greedy, selection_epsilon: 0.10,
            published: None,
        }
    }
}
