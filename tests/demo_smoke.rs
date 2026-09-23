//! Demo smoke tests: the security and deployment demos must run end to end
//! and print their headline results, so a broken demo path fails CI.

use std::process::Command;

/// The containment matrix, TLS gateway and agent governor demos must each
/// exit 0 and print their headline results. Markers are chosen to fail if
/// the *substance* disappears (the audited denial, the rejected handshake,
/// the rail outvoting the learner), not just if the script crashes.
#[test]
#[ignore = "demo scripts are being ported to the v2 decide/reward API"]
fn frontier_demos_prove_their_claims() {
    // Containment matrix: fast, deterministic.
    for (script, markers) in [
        (
            "scripts/demo-containment.py",
            &[
                "SCORE: 24/24 checks passed",
                "execution_denied",
                "max_execution_ms",
                "plain http:// is denied",
                "absolute file_root is refused",
            ] as &[&str],
        ),
        // TLS gateway: asserts a real handshake happened (TLSv1.x), that a
        // wrong CA and a wrong hostname are REJECTED (verification is on),
        // and the honest scope note is printed.
        (
            "scripts/demo-tls-gateway.py",
            &[
                "SCORE: 8/8 checks passed",
                "TLSv1.",
                "REJECTED",
                "hostname mismatch rejected",
                "demo-grade",
            ],
        ),
    ] {
        let output = Command::new(if script.ends_with(".py") {
            "python3"
        } else {
            "bash"
        })
        .arg(script)
        .output()
        .expect("spawn demo");
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
            assert!(combined.contains(m), "{script} missing {m:?}");
        }
    }

    // Agent governor: ~1900 decisions, ~100s — CI-serial budget.
    let output = Command::new("python3")
        .arg("scripts/demo-agent-governor.py")
        .output()
        .expect("spawn governor demo");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "governor exited non-zero\n{combined}"
    );
    for m in [
        "rail trips",
        ">= 0.70",
        "sha256",
        "persisted state identical: true",
        "SCORE: 10/10",
    ] {
        assert!(combined.contains(m), "governor missing {m:?}");
    }
}
