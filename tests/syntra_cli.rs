#[test]
fn health_command_reports_syntra() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
        .arg("health")
        .output()
        .expect("failed to run syntra health");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(r#""service":"Syntra""#), "{stdout}");
}
