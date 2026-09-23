//! Demo smoke tests: the security and deployment demos must run end to end
//! and print their headline results, so a broken demo path fails CI.

use std::process::Command;

/// The containment matrix, TLS gateway and agent governor demos must each
/// exit 0 and print their headline results. Markers are chosen to fail if
/// the *substance* disappears (the audited denial, the rejected handshake,
/// the rail outvoting the learner), not just if the script crashes.
///
/// The scripts run against the binaries cargo built for this test run
/// (`SYNTRA_BIN`, `LYCAN_BIN`), never a stale release build.
#[test]
fn frontier_demos_prove_their_claims() {
    let root = env!("CARGO_MANIFEST_DIR");
    for (script, markers) in [
        // Containment matrix: every denial is a 500 with the guard's own
        // error text, audited with its request id, and the canary planted
        // outside the sandbox never shows up in a response.
        (
            "scripts/demo-containment.py",
            &[
                "SCORE: 24/24 checks passed",
                "execution_denied",
                "denials audited before their 500",
                "outside canary in 0 of",
                "max_execution_ms",
                "plain http:// is denied",
                "absolute file_root is refused",
                "path escapes sandbox",
            ] as &[&str],
        ),
        // TLS gateway: a real handshake happened (TLSv1.x), a wrong CA and
        // a wrong hostname are REJECTED (verification is on), and the
        // honest scope note is printed.
        (
            "scripts/demo-tls-gateway.py",
            &[
                "SCORE: 8/8 checks passed",
                "TLSv1.",
                "REJECTED",
                "hostname mismatch rejected",
                "24/24 rewards applied",
                "demo-grade",
            ],
        ),
        // Agent governor: the rail returns block with probability 1, the
        // learner separates rogue from coder, the model survives a restart
        // byte for byte, and tenants are isolated.
        (
            "scripts/demo-agent-governor.py",
            &[
                "SCORE: 10/10 checks passed",
                "rail trips",
                "returned block with p=1.00",
                ">= 0.70",
                "persisted state identical: true",
                "cross-tenant read 403",
                "sha256",
            ],
        ),
    ] {
        let output = Command::new("python3")
            .arg(script)
            .current_dir(root)
            .env("SYNTRA_BIN", env!("CARGO_BIN_EXE_syntra"))
            .env("LYCAN_BIN", env!("CARGO_BIN_EXE_lycan"))
            .output()
            .expect("spawn python3");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "{script} exited non-zero\n{combined}"
        );
        for m in markers {
            assert!(combined.contains(m), "{script} missing {m:?}\n{combined}");
        }
    }
}
