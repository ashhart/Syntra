//! One-command demo smoke test.
//!
//! `scripts/demo.sh --no-live` must run every proof end to end and exit 0:
//! edge-of-chaos derivation, live/offline Mars decision, governed LLM
//! routing with promotion gates plus the measured bake-off, the adaptive
//! clinical trial, and the Erdos #160 proof-lab refusal — closing with the
//! receipt block. Guards against the broken-demo-path class of bug
//! (showcase scripts referencing files that do not exist).

use std::process::Command;

#[test]
fn demo_runs_all_proofs() {
    let output = Command::new("bash")
        .arg("scripts/demo.sh")
        .arg("--no-live")
        .output()
        .expect("spawn demo");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    assert!(output.status.success(), "demo exited non-zero\n{combined}");
    for marker in [
        "PROOF 1 — DERIVES PHYSICS",
        "PROOF 2 — RUNS A MISSION",
        "PROOF 3 — RUNS THE ECONOMICS",
        "PROOF 4 — RUNS THE TRIAL",
        "PROOF 5 — REFUSES TO LIE",
        "RECEIPTS",
        "DEMO COMPLETE",
    ] {
        assert!(combined.contains(marker), "missing {marker:?}\n{combined}");
    }
    // Proof 1 must show the derived boundary, not just run.
    assert!(combined.contains("Feigenbaum extrapolation"), "{combined}");
    assert!(combined.contains("Known value:"), "{combined}");
    // Proof 3 must show the promotion-gate verdict and the measured latency.
    assert!(combined.contains("Status:"), "{combined}");
    // Proof 4 must identify the TRUE per-subgroup winners (seeded run:
    // mild→A, severe→B, the designed "no single best arm" story) and the
    // trial must beat the fixed control on observed responses.
    assert!(combined.contains("WINNER A at patient"), "{combined}");
    assert!(combined.contains("WINNER B at patient"), "{combined}");
    assert!(
        combined.contains("more patients responded (+"),
        "{combined}"
    );
    // Proof 5 must show the honest refusal.
    assert!(combined.contains("refuses"), "{combined}");
    // Receipts must hash a non-empty decision log.
    assert!(combined.contains("decision log entries:"), "{combined}");
    assert!(combined.contains("decision-log fingerprint"), "{combined}");
}

/// The three frontier demos (agent governor, self-evolution gauntlet,
/// containment matrix) must each exit 0 and print their headline proofs.
/// Markers are chosen to fail if the *substance* disappears (the rail
/// outvoting the learner, the deny-all sandbox note, the audited denial),
/// not just if the script crashes.
#[test]
fn frontier_demos_prove_their_claims() {
    // Containment matrix + self-evolution gauntlet: fast, deterministic.
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
        (
            "scripts/demo-self-evolve.sh",
            &["SCORE: 15/15", "deny-all", "0.3200 -> 0.7500 -> 1.0000"],
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

#[test]
fn science_demos_check_numerical_results() {
    let output = Command::new("python3")
        .args(["scripts/demo-science.py", "--no-build"])
        .output()
        .expect("spawn science demo checks");
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "science demo failed\n{combined}");
    for name in ["mars-lambert", "launch-window", "pandemic", "edge-of-chaos"] {
        assert!(combined.contains(&format!("PASS: {name}")), "{combined}");
    }
}

/// The flagship real-time control demo (closed-loop rendezvous) must dock,
/// stay byte-identical across two seeded runs, fire its Guard fallback,
/// and report µs-class per-tick decisions with bounded allocations.
/// Markers fail if the *substance* disappears (docking, determinism,
/// safe-hold reachability), not just if the binary crashes.
#[test]
fn rt_control_loop_proves_realtime_claims() {
    let output = Command::new("cargo")
        .args(["run", "--release", "--example", "rt_control_loop"])
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .expect("spawn rt_control_loop");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        output.status.success(),
        "rt_control_loop exited non-zero\n{combined}"
    );
    let report = std::fs::read_to_string("target/rt-control.report").expect("flagship report file");
    for marker in [
        "byte-identical traces: PASS",
        "docked: true",
        "safe-hold activations: 1",
        "verifier-gated",
        "µs",          // per-tick decisions are microsecond-class
        "allocs/tick", // allocation accounting is part of the claim
    ] {
        assert!(report.contains(marker), "missing {marker:?}\n{report}");
    }
}
