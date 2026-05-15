/// Execution context — the runtime boundary for Lycan programs.
///
/// Carries policy constraints, injected input, working directory for
/// file sandboxing, and (future) audit/resource metadata.

use std::path::PathBuf;
use crate::capabilities::CapValue;

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

/// Runtime context passed through the execution stack.
#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub policy: Option<ExecutionPolicy>,
    pub input: Option<CapValue>,
    /// Working directory for file sandbox. Set to capsule dir in server mode.
    pub working_dir: Option<PathBuf>,
}

impl ExecutionContext {
    #[allow(dead_code)]
    pub fn unrestricted() -> Self {
        Self { policy: None, input: None, working_dir: None }
    }

    pub fn with_policy(policy: ExecutionPolicy) -> Self {
        Self { policy: Some(policy), input: None, working_dir: None }
    }

    pub fn with_input(input: CapValue) -> Self {
        Self { policy: None, input: Some(input), working_dir: None }
    }

    #[allow(dead_code)]
    pub fn full(policy: ExecutionPolicy, input: CapValue) -> Self {
        Self { policy: Some(policy), input: Some(input), working_dir: None }
    }
}
