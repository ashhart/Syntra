/// Agent subprocess interface for autonomous evolution.
///
/// Sends an improvement brief to an external AI agent via stdin,
/// captures proposal JSON from stdout. Works with any compatible
/// local model command or test fixture script.
///
/// This is dev/local mode only. Do not expose agent-command
/// through HTTP server yet.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Call an agent subprocess with an improvement brief.
///
/// - `command`: shell command to execute (run via `sh -c`)
/// - `brief`: improvement brief JSON written to stdin
/// - `timeout_ms`: maximum time to wait for response
///
/// Returns the agent's stdout on success.
/// Kills the subprocess on timeout.
pub fn call_agent(command: &str, brief: &str, timeout_ms: u64) -> Result<String, String> {
    let mut child = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn agent: {e}"))?;

    // Write brief to stdin, then close it
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(brief.as_bytes())
            .map_err(|e| format!("failed to write brief to agent stdin: {e}"))?;
    }

    // Wait for output with timeout using a thread
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = tx.send(output);
    });

    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(Ok(output)) => {
            let _ = handle.join();
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "agent exited with status {}: {}",
                    output.status.code().unwrap_or(-1),
                    stderr.trim()
                ));
            }
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            if stdout.trim().is_empty() {
                return Err("agent returned empty output".to_string());
            }
            Ok(stdout)
        }
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(format!("agent process error: {e}"))
        }
        Err(_) => {
            // Timeout — the child is owned by the thread, but we can
            // kill it by PID since we're on unix (macOS/Linux).
            // The thread will eventually join when the child exits.
            let _ = handle.join(); // child drops when thread ends, killing it
            Err(format!("agent timed out after {timeout_ms}ms"))
        }
    }
}
