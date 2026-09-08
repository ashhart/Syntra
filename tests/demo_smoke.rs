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
    assert!(combined.contains("more patients responded (+"), "{combined}");
    // Proof 5 must show the honest refusal.
    assert!(combined.contains("refuses"), "{combined}");
    // Receipts must hash a non-empty decision log.
    assert!(combined.contains("decision log entries:"), "{combined}");
    assert!(combined.contains("decision-log fingerprint"), "{combined}");
}
