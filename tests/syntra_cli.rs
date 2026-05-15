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

#[test]
fn author_command_compiles_yaml_bandit_spec() {
    let root = std::env::temp_dir().join(format!(
        "syntra-author-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&root).expect("create temp dir");

    let spec = root.join("router.yaml");
    let lyc = root.join("router.lyc");
    let lycs = root.join("router.lycs");
    std::fs::write(
        &spec,
        r#"
name: llm-router
options:
  - cheap_fast
  - balanced
  - expensive_accurate
contexts:
  - task_type
  - customer_tier
  - urgency
reward:
  quality: 0.6
  latency: -0.2
  cost: -0.2
"#,
    )
    .expect("write spec");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
        .arg("author")
        .arg(&spec)
        .arg("--out")
        .arg(&lyc)
        .arg("--source-out")
        .arg(&lycs)
        .output()
        .expect("run syntra author");

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("author output is json");
    assert_eq!(stdout["ok"], true);
    assert_eq!(stdout["options"], 3);

    let source = std::fs::read_to_string(&lycs).expect("read generated source");
    assert!(source.contains("($ selected_option (choice 0 1 2))"));
    assert!(source.contains("runtime.inputGet"));

    let bytes = std::fs::read(&lyc).expect("read generated lyc");
    let graph = lycan::graph::NeuralGraph::from_bytes(&bytes).expect("valid lyc graph");
    assert!(
        graph
            .nodes
            .iter()
            .any(|n| matches!(n.op, lycan::graph::OpCode::AdaptiveChoice))
    );

    let _ = std::fs::remove_dir_all(root);
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
}
