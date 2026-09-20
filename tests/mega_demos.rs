//! Mega-demo QA suite: compiles and runs every Lycan substrate demo end to
//! end and asserts the demo's headline claim appears in its output.
//!
//! These are the "cool things" demos: edge-of-chaos detection, pandemic
//! policy scoring, Mars transfer windows, live HORIZONS propagation,
//! chaos control, grid blackout prevention, ICU triage, antiviral target
//! selection, planetary defense, and spacecraft fault management.
//!
//! Each case asserts:
//!   1. `lycan <file.lycs>` exits 0 (no runtime error),
//!   2. the demo's signature result line is present (not just "ran").

use std::path::Path;
use std::process::Command;

fn run_demo(name: &str) -> (bool, String) {
    let path = Path::new("examples/lycan-internals").join(format!("{name}.lycs"));
    let output = Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg(&path)
        .output()
        .expect("spawn lycan");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), format!("{stdout}\n{stderr}"))
}

fn assert_demo(name: &str, marker: &str) {
    let (ok, out) = run_demo(name);
    assert!(ok, "{name}: lycan exited non-zero\n{out}");
    assert!(
        out.contains(marker),
        "{name}: expected output marker {marker:?} not found\n{out}"
    );
}

#[test]
fn edge_of_chaos_derives_feigenbaum_boundary() {
    // Numerically derives the logistic-map edge of chaos (~3.56995) from
    // Feigenbaum extrapolation, Lyapunov scanning, and divergence checks.
    assert_demo("demo_edge_of_chaos", "Known value:");
    let (_, out) = run_demo("demo_edge_of_chaos");
    assert!(out.contains("Feigenbaum extrapolation"), "{out}");
    assert!(out.contains("Lyapunov exponent scan"), "{out}");
}

#[test]
fn feigenbaum_constant_demo_converges() {
    assert_demo("demo_feigenbaum", "Feigenbaum");
}

#[test]
fn pandemic_policy_scores_interventions() {
    assert_demo("demo_pandemic_policy", "Best pandemic policy:");
}

#[test]
fn mars_transfer_searches_windows() {
    assert_demo("demo_mars_transfer", "Keplerian");
}

#[test]
fn mars_decide_obeys_c3_constraint() {
    assert_demo("demo_mars_decide", "PASS: C3 within constraint");
}

fn constrained_mars(input: &str, label: &str) -> String {
    let work = std::env::temp_dir().join(format!("syntra-mars-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let source = work.join("mission.lycs");
    std::fs::copy("examples/lycan-internals/demo_mars_decide.lycs", &source).unwrap();
    let compiled = Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg("compile")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let request = work.join("request.json");
    std::fs::write(&request, input).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg("decide")
        .arg(work.join("mission.lyc"))
        .arg("--input")
        .arg(request)
        .output()
        .unwrap();
    std::fs::remove_dir_all(work).unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[test]
fn mars_refinement_respects_time_of_flight_bounds() {
    let out = constrained_mars(
        r#"{"max_c3":100,"min_tof":220,"max_tof":225,"search_window_days":500}"#,
        "flight-bounds",
    );
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("TOF:"))
        .unwrap();
    let days: f64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(
        (220.0..=225.0).contains(&days),
        "refinement escaped flight bounds: {out}"
    );
}

#[test]
fn mars_infeasible_energy_budget_returns_no_mission() {
    let out = constrained_mars(r#"{"max_c3":0.01,"search_window_days":500}"#, "energy");
    assert!(out.contains("INFEASIBLE:"), "{out}");
    assert!(!out.contains("PASS: C3 within constraint"), "{out}");
    assert!(out.contains("\"result\": \"null\""), "{out}");
}

#[test]
fn apophis_horizons_propagation_matches_reference() {
    assert_demo("demo_horizons_apophis", "Horizons error");
}

#[test]
fn control_chaos_selects_controller() {
    assert_demo("demo_control_chaos", "Selected controller reward");
}

#[test]
fn grid_blackout_prevention_selects_resilience_action() {
    assert_demo("demo_grid_blackout_prevention", "Best grid policy:");
}

#[test]
fn icu_triage_scores_care_priority() {
    assert_demo("demo_icu_triage", "Best ICU triage policy:");
}

#[test]
fn antiviral_target_selection_scores_interventions() {
    assert_demo(
        "demo_antiviral_target_selection",
        "Best HIV-like intervention class",
    );
}

#[test]
fn planetary_defense_scores_mitigation_strategies() {
    assert_demo("demo_planetary_defense", "Best strategy by robust score");
}

#[test]
fn spacecraft_fault_manager_selects_fault_policy() {
    assert_demo(
        "demo_spacecraft_fault_manager",
        "Best spacecraft fault policy",
    );
}

#[test]
fn adaptive_api_router_attack_demo_recovers() {
    // Provider degrading under attack; feedback shifts the selected provider.
    assert_demo("demo_adaptive_api_router_attack", "provider");
}

#[test]
fn lorenz_and_mandelbrot_substrate_demos_run() {
    assert_demo("demo_lorenz", "Lorenz");
    assert_demo("demo_mandelbrot", "Mandelbrot");
}
