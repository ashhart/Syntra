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

#[test]
fn author_out_dir_emits_capsule_directory() {
    let root = std::env::temp_dir().join(format!(
        "syntra-author-dir-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&root).expect("create temp dir");

    let spec = root.join("router.yaml");
    let out_dir = root.join("capsule");
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
reward:
  type: continuous
  range: [-1.0, 1.0]
  components:
    - { name: quality,    weight: 0.6,  normalize: minmax, range: [0.0, 1.0] }
    - { name: latency_ms, weight: -0.2, normalize: budget, budget: 2000 }
    - { name: cost_usd,   weight: -0.2, normalize: budget, budget: 0.05 }
"#,
    )
    .expect("write spec");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
        .arg("author")
        .arg(&spec)
        .arg("--out-dir")
        .arg(&out_dir)
        .output()
        .expect("run syntra author --out-dir");

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for f in &[
        "program.lyc",
        "program.lycs",
        "learning.json",
        "reward_spec.json",
        "context_schema.json",
        "manifest.json",
    ] {
        assert!(out_dir.join(f).exists(), "missing {f}");
    }

    let reward_spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("reward_spec.json")).unwrap())
            .unwrap();
    assert_eq!(reward_spec["components"].as_array().unwrap().len(), 3);

    let _ = std::fs::remove_dir_all(root);
}

mod e2e {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    fn pick_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    fn http(
        addr: &str,
        method: &str,
        path: &str,
        admin_key: Option<&str>,
        body: &[u8],
        content_type: &str,
    ) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

        let mut req = String::new();
        req.push_str(&format!("{method} {path} HTTP/1.1\r\n"));
        req.push_str(&format!("Host: {addr}\r\n"));
        req.push_str(&format!("Content-Type: {content_type}\r\n"));
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        if let Some(k) = admin_key {
            req.push_str(&format!("Authorization: Bearer {k}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");

        stream.write_all(req.as_bytes()).unwrap();
        if !body.is_empty() {
            stream.write_all(body).unwrap();
        }
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).unwrap();

        let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(buf.len());
        let head = String::from_utf8_lossy(&buf[..split]).to_string();
        let body_start = split + 4;
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, buf[body_start..].to_vec())
    }

    fn json_body(b: &[u8]) -> serde_json::Value {
        serde_json::from_slice(b).expect("json body")
    }

    fn wait_for_health(addr: &str) {
        for _ in 0..100 {
            if let Ok(mut s) = TcpStream::connect(addr) {
                s.set_read_timeout(Some(Duration::from_millis(500))).ok();
                let req = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
                if s.write_all(req.as_bytes()).is_ok() {
                    let mut buf = [0u8; 16];
                    if s.read(&mut buf).is_ok() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not start at {addr}");
    }

    #[test]
    fn full_round_trip_author_install_decide_feedback_components() {
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let admin_key = "test-admin-key";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-e2e-{}-{}",
            std::process::id(),
            super::unique_suffix()
        ));
        std::fs::create_dir_all(&store_root).unwrap();

        let server_addr = addr.clone();
        let server_store = store_root.to_string_lossy().to_string();
        std::thread::spawn(move || {
            lycan::server::run_server(lycan::server::ServerConfig {
                addr: server_addr,
                store_path: server_store,
                admin_key: Some(admin_key.to_string()),
                service_name: Some("Syntra".to_string()),
            });
        });
        wait_for_health(&addr);

        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let spec = work.join("router.yaml");
        let out_dir = work.join("capsule");
        std::fs::write(
            &spec,
            r#"
name: e2e-router
options: [cheap_fast, balanced, expensive_accurate]
contexts: [task_type]
reward:
  type: continuous
  range: [-1.0, 1.0]
  components:
    - { name: quality,    weight: 0.6,  normalize: minmax, range: [0.0, 1.0] }
    - { name: latency_ms, weight: -0.2, normalize: budget, budget: 2000 }
    - { name: cost_usd,   weight: -0.2, normalize: budget, budget: 0.05 }
"#,
        )
        .unwrap();

        let out = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("author")
            .arg(&spec)
            .arg("--out-dir")
            .arg(&out_dir)
            .output()
            .expect("syntra author");
        assert!(out.status.success(), "author failed: {}", String::from_utf8_lossy(&out.stderr));

        let lyc_bytes = std::fs::read(out_dir.join("program.lyc")).unwrap();
        let reward_spec_bytes = std::fs::read(out_dir.join("reward_spec.json")).unwrap();

        let tenant = "acme";
        let job = "default";
        let capsule = "router";

        let (s, _) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"),
            Some(admin_key),
            &lyc_bytes,
            "application/octet-stream",
        );
        assert_eq!(s, 200, "install status");

        let (s, _) = http(
            &addr,
            "PUT",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/reward_spec"),
            Some(admin_key),
            &reward_spec_bytes,
            "application/json",
        );
        assert_eq!(s, 200, "install reward_spec status");

        let (s, body) = http(
            &addr,
            "GET",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/reward_spec"),
            Some(admin_key),
            &[],
            "application/json",
        );
        assert_eq!(s, 200);
        let fetched = json_body(&body);
        assert_eq!(fetched["components"].as_array().unwrap().len(), 3);

        let decide_body = serde_json::json!({"inputs": {"task_type": "summary"}});
        let (s, body) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide"),
            Some(admin_key),
            decide_body.to_string().as_bytes(),
            "application/json",
        );
        assert_eq!(s, 200, "decide status; body={}", String::from_utf8_lossy(&body));
        let decision = json_body(&body);
        assert!(decision.get("decisionId").is_some(), "decide must return decisionId: {decision}");
        let decision_id = decision["decisionId"].as_str().unwrap().to_string();

        let feedback_body = serde_json::json!({
            "decisionId": decision_id,
            "components": {"quality": 0.85, "latency_ms": 1240, "cost_usd": 0.018},
        });
        let (s, body) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback"),
            Some(admin_key),
            feedback_body.to_string().as_bytes(),
            "application/json",
        );
        assert_eq!(s, 200, "feedback status; body={}", String::from_utf8_lossy(&body));
        let fb = json_body(&body);
        let r = fb["reward"].as_f64().expect("reward must be a number");
        assert!((r - 0.314).abs() < 1e-3, "expected ~0.314, got {r}");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn feedback_components_without_reward_spec_returns_400() {
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let admin_key = "test-admin-key-2";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-e2e-noreward-{}-{}",
            std::process::id(),
            super::unique_suffix()
        ));
        std::fs::create_dir_all(&store_root).unwrap();

        let server_addr = addr.clone();
        let server_store = store_root.to_string_lossy().to_string();
        std::thread::spawn(move || {
            lycan::server::run_server(lycan::server::ServerConfig {
                addr: server_addr,
                store_path: server_store,
                admin_key: Some(admin_key.to_string()),
                service_name: Some("Syntra".to_string()),
            });
        });
        wait_for_health(&addr);

        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let spec = work.join("router.yaml");
        let out_dir = work.join("capsule");
        std::fs::write(
            &spec,
            r#"
name: e2e-router2
options: [a, b]
contexts: [tier]
reward:
  type: continuous
  range: [0.0, 1.0]
  components:
    - { name: score, weight: 1.0, normalize: minmax, range: [0.0, 1.0] }
"#,
        )
        .unwrap();

        let out = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("author")
            .arg(&spec)
            .arg("--out-dir")
            .arg(&out_dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        let lyc_bytes = std::fs::read(out_dir.join("program.lyc")).unwrap();

        let tenant = "acme";
        let job = "default";
        let capsule = "noreward";
        let (s, _) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"),
            Some(admin_key),
            &lyc_bytes,
            "application/octet-stream",
        );
        assert_eq!(s, 200);

        let decide_body = serde_json::json!({"inputs": {"tier": "gold"}});
        let (s, body) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide"),
            Some(admin_key),
            decide_body.to_string().as_bytes(),
            "application/json",
        );
        assert_eq!(s, 200, "decide; body={}", String::from_utf8_lossy(&body));
        let decision_id = json_body(&body)["decisionId"].as_str().unwrap().to_string();

        let feedback_body = serde_json::json!({
            "decisionId": decision_id,
            "components": {"score": 0.7},
        });
        let (s, body) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback"),
            Some(admin_key),
            feedback_body.to_string().as_bytes(),
            "application/json",
        );
        assert_eq!(s, 400, "feedback without rewardSpec should 400; body={}", String::from_utf8_lossy(&body));

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn warmup_transitions_after_30_feedbacks() {
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let admin_key = "test-admin-warmup";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-e2e-warmup-{}-{}",
            std::process::id(),
            super::unique_suffix()
        ));
        std::fs::create_dir_all(&store_root).unwrap();

        let server_addr = addr.clone();
        let server_store = store_root.to_string_lossy().to_string();
        std::thread::spawn(move || {
            lycan::server::run_server(lycan::server::ServerConfig {
                addr: server_addr,
                store_path: server_store,
                admin_key: Some(admin_key.to_string()),
                service_name: Some("Syntra".to_string()),
            });
        });
        wait_for_health(&addr);

        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let spec = work.join("router.yaml");
        let out_dir = work.join("capsule");
        std::fs::write(
            &spec,
            r#"
name: warmup-test
options: [a, b, c]
contexts: [tier]
reward: { type: bernoulli }
"#,
        )
        .unwrap();

        let out = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("author")
            .arg(&spec)
            .arg("--out-dir")
            .arg(&out_dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        let lyc_bytes = std::fs::read(out_dir.join("program.lyc")).unwrap();

        let tenant = "acme";
        let job = "default";
        let capsule = "warmcap";
        let (s, _) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"),
            Some(admin_key),
            &lyc_bytes,
            "application/octet-stream",
        );
        assert_eq!(s, 200);

        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        let (s, body) = http(
            &addr,
            "POST",
            &decide_path,
            Some(admin_key),
            br#"{"inputs":{"tier":"gold"}}"#,
            "application/json",
        );
        assert_eq!(s, 200);
        let initial = json_body(&body);
        assert_eq!(initial["warmup"]["state"], "warmup", "initial decide should report warmup state");
        assert_eq!(initial["warmup"]["collected"], 0);
        assert_eq!(initial["warmup"]["target"], 30);

        let mut transitioned_round: Option<usize> = None;
        for i in 1..=30 {
            let (s, body) = http(
                &addr,
                "POST",
                &decide_path,
                Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#,
                "application/json",
            );
            assert_eq!(s, 200, "decide #{i}; body={}", String::from_utf8_lossy(&body));
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();

            let reward = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, body) = http(
                &addr,
                "POST",
                &feedback_path,
                Some(admin_key),
                fb_body.as_bytes(),
                "application/json",
            );
            assert_eq!(s, 200, "feedback #{i}; body={}", String::from_utf8_lossy(&body));
            let fb = json_body(&body);
            if fb["warmupTransitioned"].as_bool().unwrap_or(false) {
                transitioned_round = Some(i);
                assert_eq!(fb["warmup"]["state"], "active", "transition should report active state");
            }
        }
        assert_eq!(transitioned_round, Some(30), "warmup should transition on the 30th feedback");

        let (s, body) = http(
            &addr,
            "POST",
            &decide_path,
            Some(admin_key),
            br#"{"inputs":{"tier":"gold"}}"#,
            "application/json",
        );
        assert_eq!(s, 200);
        let after = json_body(&body);
        assert_eq!(after["warmup"]["state"], "active", "post-warmup decide should report active");
        let algo = after["warmup"]["algorithm"].as_str().unwrap_or("");
        assert!(algo.contains("Thompson"), "binary rewards should pick Thompson, got: {algo}");

        let warmup_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("warmup.json");
        assert!(warmup_path.exists(), "warmup.json should be persisted in capsule dir");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    fn drive_to_active(addr: &str, admin_key: &str, decide_path: &str, feedback_path: &str) {
        for i in 1..=30 {
            let (s, body) = http(addr, "POST", decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#, "application/json");
            assert_eq!(s, 200, "decide #{i}; {}", String::from_utf8_lossy(&body));
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let reward = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, _) = http(addr, "POST", feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }
    }

    fn spawn_server(store_root: &std::path::Path, admin_key: &str) -> String {
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let server_addr = addr.clone();
        let server_store = store_root.to_string_lossy().to_string();
        let key = admin_key.to_string();
        std::thread::spawn(move || {
            lycan::server::run_server(lycan::server::ServerConfig {
                addr: server_addr,
                store_path: server_store,
                admin_key: Some(key),
                service_name: Some("Syntra".to_string()),
            });
        });
        wait_for_health(&addr);
        addr
    }

    fn install_simple_bernoulli(addr: &str, admin_key: &str, work: &std::path::Path,
        tenant: &str, job: &str, capsule: &str)
    {
        let spec = work.join(format!("{capsule}.yaml"));
        let out_dir = work.join(format!("{capsule}-cap"));
        std::fs::write(
            &spec,
            r#"
name: meta-test
options: [a, b, c]
contexts: [tier]
reward: { type: bernoulli }
"#,
        )
        .unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("author").arg(&spec).arg("--out-dir").arg(&out_dir).output().unwrap();
        assert!(out.status.success());
        let lyc = std::fs::read(out_dir.join("program.lyc")).unwrap();
        let (s, _) = http(addr, "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"),
            Some(admin_key), &lyc, "application/octet-stream");
        assert_eq!(s, 200);
    }

    #[test]
    fn meta_bandit_active_after_warmup() {
        let admin_key = "test-admin-mb1";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-mb1-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "metacap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);

        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        drive_to_active(&addr, admin_key, &decide_path, &feedback_path);

        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        assert_eq!(resp["warmup"]["state"], "active");

        let decisions = resp["decisions"].as_array().expect("decisions array");
        assert!(!decisions.is_empty(), "must have decisions");
        let candidate_id = decisions[0]["candidateId"].as_str()
            .expect("decisions[0] must carry candidateId in Active state");
        let valid = ["Thompson", "Ucb", "Weighted", "EpsilonGreedy", "Greedy"];
        assert!(valid.contains(&candidate_id), "unexpected candidateId: {candidate_id}");

        let did = resp["decisionId"].as_str().unwrap().to_string();
        let fb_body = format!(r#"{{"decisionId":"{did}","reward":1.0}}"#);
        let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
            fb_body.as_bytes(), "application/json");
        assert_eq!(s, 200);

        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        let strategies = mem["strategies"].as_object().expect("strategies obj");
        let any_with_meta = strategies.values().any(|s| s.get("metaBandit").map(|v| !v.is_null()).unwrap_or(false));
        let any_with_cand_ctx = strategies.values().any(|s| {
            s.get("candidateContexts").and_then(|v| v.as_object()).map(|o| !o.is_empty()).unwrap_or(false)
        });
        assert!(any_with_meta, "memory.json must contain a metaBandit after Active feedback");
        assert!(any_with_cand_ctx, "memory.json must contain candidateContexts after Active feedback");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn meta_bandit_records_trials_over_long_horizon() {
        let admin_key = "test-admin-mb2";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-mb2-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "longhorizon");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        drive_to_active(&addr, admin_key, &decide_path, &feedback_path);

        for i in 0..200 {
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let reward = if i % 2 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        let strategies = mem["strategies"].as_object().unwrap();
        let mut total_trials_recorded: f64 = 0.0;
        for s in strategies.values() {
            if let Some(mb) = s.get("metaBandit") {
                if let Some(arr) = mb.get("candidates").and_then(|v| v.as_array()) {
                    for c in arr {
                        total_trials_recorded += c["trials"].as_f64().unwrap_or(0.0);
                    }
                }
            }
        }
        // 200 records × forgetting 0.999 converges to ~1/(1-0.999) = 1000 effective trials max,
        // but linearly accumulates early. Conservatively require a measurable accumulation.
        assert!(total_trials_recorded >= 100.0,
            "meta-bandit must record effective trials over 200 Active feedbacks, got {total_trials_recorded}");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn change_detection_resets_meta_bandit() {
        let admin_key = "test-admin-mb3";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-mb3-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "drift");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        for i in 1..=30 {
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let r = if (i * 7919) % 10 < 1 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{r}}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        let mut change_round: Option<usize> = None;
        for i in 0..400 {
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let r = if (i * 7919) % 10 < 9 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{r}}}"#);
            let (s, body) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
            if json_body(&body)["changeDetected"].as_bool().unwrap_or(false) {
                change_round = Some(i);
                break;
            }
        }
        assert!(change_round.is_some(), "change should be detected after regime shift");

        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        let strategies = mem["strategies"].as_object().unwrap();
        for s in strategies.values() {
            if let Some(mb) = s.get("metaBandit") {
                if mb.is_null() { continue; }
                let total_rounds = mb["totalRounds"].as_u64().unwrap_or(0);
                assert_eq!(total_rounds, 0,
                    "meta-bandit must reset on change detection; got totalRounds={total_rounds}");
            }
        }

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn per_context_change_detection_resets_only_drifted_context() {
        let admin_key = "test-admin-mb4";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-mb4-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "twoctx");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Drive to Active state. Default warmup target is 30. Use the same
        // context for warmup (avoids a partial warmup contributing different
        // amounts per context).
        for i in 1..=30 {
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"a"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let r = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{r}}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Build candidate state for BOTH contexts at low reward (Bernoulli ~0.1).
        // Each context independently; ~20 feedbacks each.
        for ctx in &["tier_a", "tier_b"] {
            for i in 1..=20 {
                let body_req = format!(r#"{{"contextKey":"{ctx}","inputs":{{"tier":"{ctx}"}}}}"#);
                let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                    body_req.as_bytes(), "application/json");
                let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
                let r = if (i * 7919) % 10 < 1 { 1.0 } else { 0.0 };
                let fb_body = format!(r#"{{"decisionId":"{did}","reward":{r}}}"#);
                let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                    fb_body.as_bytes(), "application/json");
                assert_eq!(s, 200);
            }
        }

        // Snapshot memory: both ctx_a and ctx_b should have candidate state now.
        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem_before: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        let cands_before: Vec<String> = mem_before["strategies"].as_object().unwrap()
            .values()
            .filter_map(|s| s.get("candidateContexts").and_then(|v| v.as_object()))
            .flat_map(|o| o.keys().cloned().collect::<Vec<_>>())
            .collect();
        assert!(cands_before.iter().any(|k| k.ends_with("|tier_a")),
            "tier_a should have candidate state before drift; got: {cands_before:?}");
        assert!(cands_before.iter().any(|k| k.ends_with("|tier_b")),
            "tier_b should have candidate state before drift; got: {cands_before:?}");

        // Drift tier_a only: sustained reward ~0.9. Interleave tier_b feedback
        // at the old low rate so the capsule-level aggregate signal stays near
        // its baseline — only tier_a's per-context detector sees pure drift.
        let mut fired = false;
        for i in 0..500 {
            // Tier A: high reward
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"contextKey":"tier_a","inputs":{"tier":"tier_a"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let r = if (i * 7919) % 10 < 9 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{r}}}"#);
            let (s, body) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
            let resp = json_body(&body);
            if resp["contextChangeDetected"].as_bool().unwrap_or(false)
                && resp["warmup"]["state"] == "active"
            {
                fired = true;
                break;
            }
            // Tier B: low reward, keeps capsule-level signal stable
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"contextKey":"tier_b","inputs":{"tier":"tier_b"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let r = if (i * 7919) % 10 < 1 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{r}}}"#);
            let (s, body) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
            // If capsule-level fires, the test premise fails — bail with the data so we know.
            if json_body(&body)["changeDetected"].as_bool().unwrap_or(false) {
                panic!("capsule-level fired during interleaved drift at iteration {i}; the test premise needs tuning");
            }
        }
        assert!(fired, "per-context change detector should fire on tier_a drift while capsule stays Active");

        // Verify post-drift state.
        let mem_after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        for (_, s) in mem_after["strategies"].as_object().unwrap() {
            let cc = s.get("candidateContexts").and_then(|v| v.as_object());
            if let Some(cc_map) = cc {
                // tier_a's candidate state must be cleared (no keys ending in "|tier_a")
                let has_tier_a = cc_map.keys().any(|k| k.ends_with("|tier_a"));
                assert!(!has_tier_a, "tier_a candidate state should be reset");
                // tier_b's candidate state must still exist
                let has_tier_b = cc_map.keys().any(|k| k.ends_with("|tier_b"));
                assert!(has_tier_b, "tier_b candidate state must survive");
            }
        }

        // Capsule lifecycle must still be Active — not Warmup.
        let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"tier_a","inputs":{"tier":"tier_a"}}"#, "application/json");
        let resp = json_body(&body);
        assert_eq!(resp["warmup"]["state"], "active",
            "capsule must remain in Active state after per-context drift");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn long_horizon_run_keeps_option_state_bounded_under_forgetting() {
        let admin_key = "test-admin-c3";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-c3-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "longopt");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        drive_to_active(&addr, admin_key, &decide_path, &feedback_path);

        // Drive 1500 rounds with steady binary signal. Default forgetting=0.999
        // gives a steady-state effective sample size of ~1000, so total_reward
        // and Beta α should not grow without bound.
        for i in 0..1500 {
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#, "application/json");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let reward = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();

        // Find the candidate bucket option states — pull max α, max β, and assert
        // they stay below the no-decay growth ceiling.
        let mut max_alpha: f64 = 0.0;
        let mut max_beta: f64 = 0.0;
        let mut max_ucb_tries: f64 = 0.0;
        for s in mem["strategies"].as_object().unwrap().values() {
            if let Some(cc) = s.get("candidateContexts").and_then(|v| v.as_object()) {
                for bucket in cc.values() {
                    if let Some(states) = bucket.get("optionStates").and_then(|v| v.as_array()) {
                        for st in states {
                            if st.get("kind").and_then(|v| v.as_str()) == Some("betaBernoulli") {
                                let a = st["alpha"].as_f64().unwrap_or(0.0);
                                let b = st["beta"].as_f64().unwrap_or(0.0);
                                if a > max_alpha { max_alpha = a; }
                                if b > max_beta { max_beta = b; }
                            }
                            if st.get("kind").and_then(|v| v.as_str()) == Some("ucb") {
                                let t = st["tries"].as_f64().unwrap_or(0.0);
                                if t > max_ucb_tries { max_ucb_tries = t; }
                            }
                        }
                    }
                }
            }
        }

        // Without forgetting, α would grow by +1 per relevant feedback for the
        // dominant candidate, reaching ~500 over 1500 rounds. With forgetting=0.999,
        // it should plateau around 1 + (success_rate * 1/(1-f)) ≈ 1 + ~334 = ~335 max.
        // Allow some headroom; the asserts here are that no value drifted into the
        // no-decay regime (i.e., over 600+).
        assert!(max_alpha < 600.0, "Beta α exceeded forgetting ceiling: {max_alpha}");
        assert!(max_beta < 600.0, "Beta β exceeded forgetting ceiling: {max_beta}");
        assert!(max_ucb_tries < 1500.0, "UCB tries didn't decay: {max_ucb_tries}");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    fn install_feature_context_capsule(
        addr: &str, admin_key: &str, work: &std::path::Path,
        tenant: &str, job: &str, capsule: &str,
    ) {
        // Author a minimal Bernoulli capsule (3 options) using the existing schema.
        let spec = work.join(format!("{capsule}.yaml"));
        let out_dir = work.join(format!("{capsule}-cap"));
        std::fs::write(
            &spec,
            r#"
name: feature-test
options: [a, b, c]
contexts: [tier]
reward: { type: bernoulli }
"#,
        )
        .unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_syntra"))
            .arg("author").arg(&spec).arg("--out-dir").arg(&out_dir).output().unwrap();
        assert!(out.status.success());
        let lyc = std::fs::read(out_dir.join("program.lyc")).unwrap();
        let (s, _) = http(addr, "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"),
            Some(admin_key), &lyc, "application/octet-stream");
        assert_eq!(s, 200);

        // PUT a learning config carrying a Features contextSpec.
        let cfg = serde_json::json!({
            "contextSpec": {
                "type": "features",
                "features": [
                    {"name": "age", "type": {"kind": "continuous", "range": [0.0, 100.0]}},
                    {"name": "tier", "type": {"kind": "categorical", "values": ["free", "gold"]}}
                ]
            }
        });
        let (s, _) = http(addr, "PUT",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/learning"),
            Some(admin_key), cfg.to_string().as_bytes(), "application/json");
        assert_eq!(s, 200);
    }

    fn drive_to_active_with_features(
        addr: &str, admin_key: &str, decide_path: &str, feedback_path: &str,
    ) {
        for i in 1..=30 {
            let body_req = r#"{"features":{"age":30.0,"tier":"gold"}}"#;
            let (s, body) = http(addr, "POST", decide_path, Some(admin_key),
                body_req.as_bytes(), "application/json");
            assert_eq!(s, 200, "decide #{i}; {}", String::from_utf8_lossy(&body));
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let reward = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, _) = http(addr, "POST", feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }
    }

    #[test]
    fn linucb_capsule_routes_and_learns() {
        let admin_key = "test-admin-d3a";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-d3a-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "linucbcap");
        install_feature_context_capsule(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        drive_to_active_with_features(&addr, admin_key, &decide_path, &feedback_path);

        // Hit decide many times with varied feature vectors.
        let mut linucb_chosen = 0;
        let mut linucb_decision_id: Option<String> = None;
        for i in 0..200 {
            let age = (i % 80) as f64 + 10.0;
            let tier = if i % 2 == 0 { "gold" } else { "free" };
            let body_req = format!(r#"{{"features":{{"age":{age},"tier":"{tier}"}}}}"#);
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req.as_bytes(), "application/json");
            assert_eq!(s, 200);
            let resp = json_body(&body);
            let decisions = resp["decisions"].as_array().unwrap();
            if let Some(first) = decisions.first() {
                if first["candidateId"].as_str() == Some("LinUcb") {
                    linucb_chosen += 1;
                    if linucb_decision_id.is_none() {
                        linucb_decision_id = Some(resp["decisionId"].as_str().unwrap().to_string());
                        assert!(first["featureVector"].as_array().is_some(),
                            "LinUcb decision must include featureVector");
                    }
                }
            }
        }
        assert!(linucb_chosen > 0, "LinUcb should be selected at least once over 200 decides");

        // Send feedback on a LinUcb decision and verify the LinUcb bucket gets state.
        if let Some(did) = linucb_decision_id {
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":1.0}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Inspect memory.json — verify LinUcb option_states exist with non-trivial b.
        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        let mut found_linucb_state = false;
        let mut meta_candidate_count = 0;
        for s in mem["strategies"].as_object().unwrap().values() {
            if let Some(mb) = s.get("metaBandit") {
                if let Some(arr) = mb.get("candidates").and_then(|v| v.as_array()) {
                    meta_candidate_count = arr.len();
                }
            }
            if let Some(cc) = s.get("candidateContexts").and_then(|v| v.as_object()) {
                for (k, bucket) in cc {
                    if !k.starts_with("LinUcb|") { continue; }
                    if let Some(states) = bucket.get("optionStates").and_then(|v| v.as_array()) {
                        for st in states {
                            if st["kind"].as_str() == Some("linucb") {
                                found_linucb_state = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(found_linucb_state, "memory must contain LinUcb option_states");
        assert_eq!(meta_candidate_count, 7, "feature-context capsule must register 7 candidates (5 discrete + LinUcb + LinTs)");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn discrete_capsule_does_not_include_linucb_candidate() {
        let admin_key = "test-admin-d3b";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-d3b-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "discretecap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        drive_to_active(&addr, admin_key, &decide_path, &feedback_path);

        for _ in 0..200 {
            let (_, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                br#"{"inputs":{"tier":"gold"}}"#, "application/json");
            let resp = json_body(&body);
            let decisions = resp["decisions"].as_array().unwrap();
            if let Some(first) = decisions.first() {
                assert_ne!(first["candidateId"].as_str(), Some("LinUcb"),
                    "discrete capsule must never select LinUcb");
            }
        }

        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        for s in mem["strategies"].as_object().unwrap().values() {
            if let Some(mb) = s.get("metaBandit") {
                if let Some(arr) = mb.get("candidates").and_then(|v| v.as_array()) {
                    assert_eq!(arr.len(), 5, "discrete capsule must register 5 candidates");
                    for c in arr {
                        assert_ne!(c["id"].as_str(), Some("LinUcb"),
                            "discrete meta-bandit must not contain LinUcb");
                    }
                }
            }
        }

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn linucb_capsule_rejects_invalid_features() {
        let admin_key = "test-admin-d3c";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-d3c-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "invalid");
        install_feature_context_capsule(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");

        // Missing required feature
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"features":{"age":30.0}}"#, "application/json");
        assert_eq!(s, 400, "missing feature should 400; got body={}", String::from_utf8_lossy(&body));

        // Wrong type — number where category expected
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"features":{"age":30.0,"tier":99}}"#, "application/json");
        assert_eq!(s, 400, "type mismatch should 400; got body={}", String::from_utf8_lossy(&body));

        // Unknown category value
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"features":{"age":30.0,"tier":"platinum"}}"#, "application/json");
        assert_eq!(s, 400, "unknown category should 400; got body={}", String::from_utf8_lossy(&body));

        // No features at all
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"some_key"}"#, "application/json");
        assert_eq!(s, 400, "feature-context capsule should reject discrete contextKey; got body={}", String::from_utf8_lossy(&body));

        // Valid features still work
        let (s, _) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"features":{"age":30.0,"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn discrete_capsule_ood_score_appears_in_response() {
        let admin_key = "test-admin-ood-discrete";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ood-d-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "oodcap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Drive with a single known contextKey so the OOD detector accumulates it.
        for i in 1..=60 {
            let body_req = br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#;
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req, "application/json");
            assert_eq!(s, 200, "decide #{i}");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let reward = if i % 3 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Known key → low OOD score.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        let known_score = resp["oodScore"].as_f64().unwrap();
        assert!(known_score < 0.1, "known key oodScore = {known_score} (expected < 0.1)");

        // Novel key → maximal OOD score.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"never_seen","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        let novel_score = resp["oodScore"].as_f64().unwrap();
        assert!((novel_score - 1.0).abs() < 1e-9,
            "novel key oodScore = {novel_score} (expected 1.0)");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    fn set_refusal_config(addr: &str, admin_key: &str, tenant: &str, job: &str, capsule: &str,
        refusal_json: serde_json::Value, context_spec: Option<serde_json::Value>)
    {
        let mut cfg = serde_json::json!({ "refusal": refusal_json });
        if let Some(spec) = context_spec {
            cfg["contextSpec"] = spec;
        }
        let (s, _) = http(addr, "PUT",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/learning"),
            Some(admin_key), cfg.to_string().as_bytes(), "application/json");
        assert_eq!(s, 200);
    }

    #[test]
    fn refusal_disabled_capsule_never_refuses() {
        let admin_key = "test-admin-ref-off";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ref-off-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "refoff");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Drive through warmup with one contextKey.
        for i in 1..=60 {
            let body_req = br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#;
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req, "application/json");
            assert_eq!(s, 200, "decide #{i}");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{}}}"#,
                if i % 3 == 0 { 1.0 } else { 0.0 });
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Novel contextKey — refusal disabled, so should still decide.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"never_seen","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        assert_eq!(resp["refused"].as_bool(), Some(false));
        assert!(resp["decisions"].as_array().unwrap().len() > 0,
            "decisions array should be non-empty");
        let conf = &resp["confidence"];
        assert!((conf["oodScore"].as_f64().unwrap() - 1.0).abs() < 1e-9,
            "OOD score should be 1.0 for novel key");
        assert_eq!(conf["refused"].as_bool(), Some(false));

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn refusal_enabled_capsule_refuses_on_ood() {
        let admin_key = "test-admin-ref-ood";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ref-ood-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "refoodcap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        // Loose interval threshold so OOD is the only refusal trigger.
        set_refusal_config(&addr, admin_key, tenant, job, capsule,
            serde_json::json!({
                "enabled": true,
                "coverage": 0.95,
                "maxIntervalWidth": 10.0,
                "oodThreshold": 0.5,
            }),
            None,
        );
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        for i in 1..=60 {
            let body_req = br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#;
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req, "application/json");
            assert_eq!(s, 200, "decide #{i}");
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{}}}"#,
                if i % 3 == 0 { 1.0 } else { 0.0 });
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Novel key → OOD score 1.0 ≥ 0.5 threshold → refuse.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"never_seen","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        assert_eq!(resp["refused"].as_bool(), Some(true));
        assert_eq!(resp["confidence"]["refusalReason"].as_str(), Some("ood"));
        assert_eq!(resp["decisions"].as_array().unwrap().len(), 0,
            "decisions should be empty when refused");

        // Audit log should contain a decision_refused event.
        let audit_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("audit.jsonl");
        let audit_text = std::fs::read_to_string(&audit_path).unwrap();
        assert!(audit_text.contains("decision_refused"),
            "audit log missing decision_refused event: {audit_text}");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn refusal_enabled_capsule_refuses_on_wide_interval() {
        let admin_key = "test-admin-ref-wide";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ref-wide-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "refwidecap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        // Very strict interval width; OOD threshold loose so it's not the trigger.
        set_refusal_config(&addr, admin_key, tenant, job, capsule,
            serde_json::json!({
                "enabled": true,
                "coverage": 0.95,
                "maxIntervalWidth": 0.05,
                "oodThreshold": 1.5,
            }),
            None,
        );
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Drive past warmup with noisy rewards so the calibrator builds a wide interval.
        // Residuals are partitioned across candidate buckets by the meta-bandit, so we
        // need enough iterations for every chosen candidate to have ≥30 residuals.
        for i in 1..=300 {
            let body_req = br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#;
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req, "application/json");
            assert_eq!(s, 200);
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            // Noisy: alternates 0 and 1 → residuals ~0.5 once the mean settles.
            let reward = if i % 2 == 0 { 1.0 } else { 0.0 };
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{reward}}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        assert_eq!(resp["refused"].as_bool(), Some(true),
            "expected refused; got {resp}");
        assert_eq!(resp["confidence"]["refusalReason"].as_str(), Some("interval_too_wide"));

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn refusal_enabled_capsule_decides_when_confident() {
        let admin_key = "test-admin-ref-ok";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ref-ok-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "refokcap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        set_refusal_config(&addr, admin_key, tenant, job, capsule,
            serde_json::json!({
                "enabled": true,
                "coverage": 0.95,
                "maxIntervalWidth": 5.0,
                "oodThreshold": 5.0,
            }),
            None,
        );
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Drive enough iterations so every chosen-candidate bucket exceeds 30 residuals
        // (calibrator min_samples), else the response would refuse with
        // insufficient_calibration_data.
        for i in 1..=300 {
            let body_req = br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#;
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req, "application/json");
            assert_eq!(s, 200);
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{}}}"#,
                if i % 3 == 0 { 1.0 } else { 0.0 });
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let resp = json_body(&body);
        assert_eq!(resp["refused"].as_bool(), Some(false),
            "thresholds were loose; should not refuse: {resp}");
        assert!(resp["decisions"].as_array().unwrap().len() > 0);

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn feedback_on_refused_decision_only_updates_ood() {
        let admin_key = "test-admin-ref-fb";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ref-fb-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "reffbcap");
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, capsule);
        set_refusal_config(&addr, admin_key, tenant, job, capsule,
            serde_json::json!({
                "enabled": true,
                "coverage": 0.95,
                "maxIntervalWidth": 10.0,
                "oodThreshold": 0.5,
            }),
            None,
        );
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Warmup
        for i in 1..=60 {
            let body_req = br#"{"contextKey":"known","inputs":{"tier":"gold"}}"#;
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req, "application/json");
            assert_eq!(s, 200);
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":{}}}"#,
                if i % 3 == 0 { 1.0 } else { 0.0 });
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Snapshot memory.json before triggering the refused decision.
        let mem_path = store_root.join("tenants").join(tenant)
            .join("jobs").join(job).join("capsules").join(capsule).join("memory.json");
        let mem_before: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();

        // Trigger refusal.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"contextKey":"brand_new","inputs":{"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let refused_resp = json_body(&body);
        assert_eq!(refused_resp["refused"].as_bool(), Some(true));
        let refused_did = refused_resp["decisionId"].as_str().unwrap().to_string();

        // Snapshot meta-bandit + candidate_contexts immediately after the refused decide
        // so we compare apples to apples (decide already persists OOD updates).
        let mem_after_refused_decide: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();

        // Feedback on the refused decision.
        let fb_body = format!(r#"{{"decisionId":"{refused_did}","reward":1.0}}"#);
        let (s, body) = http(&addr, "POST", &feedback_path, Some(admin_key),
            fb_body.as_bytes(), "application/json");
        assert_eq!(s, 200);
        let fb_resp = json_body(&body);
        assert_eq!(fb_resp["ok"].as_bool(), Some(true));
        assert!(fb_resp["noted"].as_str().unwrap().contains("refused"),
            "expected 'refused' in noted message: {fb_resp}");

        // After refused-feedback, meta-bandit + candidate_contexts should be unchanged
        // versus the post-refused-decide snapshot.
        let mem_after_fb: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mem_path).unwrap()).unwrap();
        assert_eq!(
            mem_after_refused_decide["strategies"], mem_after_fb["strategies"],
            "feedback on refused decision must not mutate meta-bandit or candidate state",
        );

        // Sanity: original mem_before is from before the novel contextKey was even seen,
        // so it differs (OOD detector now tracks brand_new).
        assert_ne!(mem_before["strategies"], mem_after_fb["strategies"]);

        let _ = std::fs::remove_dir_all(&store_root);
    }

    #[test]
    fn feature_capsule_ood_score_appears_in_response() {
        let admin_key = "test-admin-ood-feature";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-ood-f-{}-{}", std::process::id(), super::unique_suffix()));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);
        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let (tenant, job, capsule) = ("acme", "default", "oodfeat");
        install_feature_context_capsule(&addr, admin_key, &work, tenant, job, capsule);
        let decide_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide");
        let feedback_path = format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback");

        // Drive past warmup with vectors in a narrow band: age ∈ [20,40], tier=gold.
        drive_to_active_with_features(&addr, admin_key, &decide_path, &feedback_path);
        for i in 0..200 {
            let age = 20.0 + (i % 21) as f64;
            let body_req = format!(r#"{{"features":{{"age":{age},"tier":"gold"}}}}"#);
            let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
                body_req.as_bytes(), "application/json");
            assert_eq!(s, 200);
            let did = json_body(&body)["decisionId"].as_str().unwrap().to_string();
            let fb_body = format!(r#"{{"decisionId":"{did}","reward":1.0}}"#);
            let (s, _) = http(&addr, "POST", &feedback_path, Some(admin_key),
                fb_body.as_bytes(), "application/json");
            assert_eq!(s, 200);
        }

        // Near the seen distribution.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"features":{"age":30.0,"tier":"gold"}}"#, "application/json");
        assert_eq!(s, 200);
        let near_score = json_body(&body)["oodScore"].as_f64().unwrap();

        // Far from the seen distribution: out-of-range age + opposite tier.
        // The encoded vector lands in a region the detector hasn't trained on.
        let (s, body) = http(&addr, "POST", &decide_path, Some(admin_key),
            br#"{"features":{"age":99.0,"tier":"free"}}"#, "application/json");
        assert_eq!(s, 200);
        let far_score = json_body(&body)["oodScore"].as_f64().unwrap();

        assert!(far_score > near_score,
            "far vector oodScore ({far_score}) should exceed near vector oodScore ({near_score})");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    /// Installing a capsule whose binary contains an `OpCode::Strategy` node
    /// must still succeed (the warning is non-blocking) and the resulting
    /// stored graph must still report the Strategy node when re-read. The
    /// stderr warning itself is emitted via `tracing::warn!` as a side effect
    /// and not captured here; manual smoke verifies the log line.
    #[test]
    fn install_with_strategy_node_succeeds_and_is_non_blocking() {
        // Build a tiny .lyc with a `(strategy ...)` node entirely in process,
        // using the lycan parser + graph compiler. This avoids any reliance
        // on a pre-built demo .lyc and exercises the same byte-format the
        // install endpoint sees in production.
        let src = "(strategy (1) (2) (3))";
        let tokens = lycan::lexer::Lexer::new(src).tokenize().expect("tokenize");
        let program = lycan::parser::Parser::new(tokens).parse_program().expect("parse");
        let graph = lycan::graph_compiler::GraphCompiler::new().compile(&program);
        let strategy_count = graph
            .nodes
            .iter()
            .filter(|n| matches!(n.op, lycan::graph::OpCode::Strategy))
            .count();
        assert!(
            strategy_count > 0,
            "test fixture should contain at least one Strategy node"
        );
        let lyc_bytes = graph.to_bytes();

        // Bring up a Syntra server backed by lycan's HTTP layer.
        let port = pick_port();
        let addr = format!("127.0.0.1:{port}");
        let admin_key = "test-admin-key-strategy-warn";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-strategy-warn-{}-{}",
            std::process::id(),
            super::unique_suffix()
        ));
        std::fs::create_dir_all(&store_root).unwrap();

        let server_addr = addr.clone();
        let server_store = store_root.to_string_lossy().to_string();
        std::thread::spawn(move || {
            lycan::server::run_server(lycan::server::ServerConfig {
                addr: server_addr,
                store_path: server_store,
                admin_key: Some(admin_key.to_string()),
                service_name: Some("Syntra".to_string()),
            });
        });
        wait_for_health(&addr);

        let tenant = "warn-tenant";
        let job = "default";
        let capsule = "warn-capsule";

        let (status, body) = http(
            &addr,
            "POST",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install"),
            Some(admin_key),
            &lyc_bytes,
            "application/octet-stream",
        );
        assert_eq!(
            status,
            200,
            "install must succeed even when the capsule contains Strategy nodes; body={}",
            String::from_utf8_lossy(&body)
        );

        // The install response shape must stay unchanged — no warning surfaced
        // in the JSON, only via tracing on the server side.
        let resp = json_body(&body);
        assert_eq!(resp["ok"], true, "install response shape unchanged: {resp}");
        assert!(resp.get("warning").is_none(), "warning must NOT be in the HTTP response");

        // Sanity: the bytes we stored decode back to a graph that still has
        // the Strategy node — the warning does not strip it.
        let stored = store_root
            .join("tenants")
            .join(tenant)
            .join("jobs")
            .join(job)
            .join("capsules")
            .join(capsule)
            .join("current.lyc");
        let stored_bytes = std::fs::read(&stored).expect("read stored .lyc");
        let stored_graph =
            lycan::graph::NeuralGraph::from_bytes(&stored_bytes).expect("valid stored .lyc");
        assert!(
            stored_graph
                .nodes
                .iter()
                .any(|n| matches!(n.op, lycan::graph::OpCode::Strategy)),
            "stored capsule must preserve Strategy nodes; install is non-mutating"
        );

        let _ = std::fs::remove_dir_all(&store_root);
    }

    /// Covers `GET /admin/capsules` (the dashboard's capsule-switcher feed):
    /// installs two capsules in the same store — one defaulted to
    /// meta-bandit, one switched to shared-state by writing
    /// `learning.json` with `sharedState.enabled = true` and an
    /// `optionFeatures` map. Asserts the endpoint reports each capsule's
    /// scoring mode, surfaces shared-state option labels from
    /// `optionFeatures` keys (sorted), and emits a stable path-sorted
    /// array.
    #[test]
    fn admin_capsules_lists_installed_with_correct_scoring_mode() {
        let admin_key = "test-admin-caps";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-admin-caps-{}-{}",
            std::process::id(),
            super::unique_suffix()
        ));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);

        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        // ── Capsule A: vanilla meta-bandit (no learning.json overrides).
        let tenant = "acme";
        let job = "default";
        let cap_meta = "meta-cap";
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, cap_meta);

        // ── Capsule B: shared-state-linucb. Install the same binary
        // but overwrite learning.json with sharedState.enabled = true
        // and an optionFeatures map keyed on labels we'll assert on.
        let cap_shared = "shared-cap";
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, cap_shared);

        let shared_learning = serde_json::json!({
            "sharedState": {
                "enabled": true,
                "dContext": 2,
                "dOption": 2,
                "lambda": 1.0,
                "alpha": 1.0,
                "scoreKind": "ucb",
                "optionFeatures": {
                    "alpha":   [1.0, 0.0],
                    "bravo":   [0.0, 1.0],
                    "charlie": [0.5, 0.5],
                }
            }
        });
        let (s, _) = http(
            &addr,
            "PUT",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{cap_shared}/learning"),
            Some(admin_key),
            shared_learning.to_string().as_bytes(),
            "application/json",
        );
        assert_eq!(
            s, 200,
            "PUT learning.json for shared-state capsule should succeed"
        );

        // ── Hit the endpoint.
        let (s, body) = http(
            &addr,
            "GET",
            "/admin/capsules",
            Some(admin_key),
            &[],
            "application/json",
        );
        assert_eq!(
            s,
            200,
            "GET /admin/capsules; body={}",
            String::from_utf8_lossy(&body)
        );
        let resp = json_body(&body);
        let arr = resp["capsules"]
            .as_array()
            .expect("capsules must be an array");
        assert_eq!(arr.len(), 2, "expected exactly two capsules: {resp}");

        // Stable sort by path: acme/default/meta-cap < acme/default/shared-cap.
        assert_eq!(arr[0]["path"], format!("{tenant}/{job}/{cap_meta}"));
        assert_eq!(arr[1]["path"], format!("{tenant}/{job}/{cap_shared}"));

        // Meta-bandit row.
        assert_eq!(
            arr[0]["scoringMode"], "meta-bandit",
            "first capsule must report meta-bandit; row={}",
            arr[0]
        );
        // Without optionFeatures we fall back to option_0..option_{n-1}
        // counted from the .lyc AdaptiveChoice node — the spec installs
        // three options (a, b, c).
        let meta_opts: Vec<String> = arr[0]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            meta_opts,
            vec!["option_0", "option_1", "option_2"],
            "meta-bandit row should fall back to numbered options: {meta_opts:?}"
        );

        // Shared-state row.
        assert_eq!(
            arr[1]["scoringMode"], "shared-state-linucb",
            "second capsule must report shared-state-linucb; row={}",
            arr[1]
        );
        let shared_opts: Vec<String> = arr[1]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        // BTreeMap iterates in key-sorted order; the labels match what we
        // wrote into optionFeatures above.
        assert_eq!(
            shared_opts,
            vec![
                "alpha".to_string(),
                "bravo".to_string(),
                "charlie".to_string(),
            ],
            "shared-state row should surface optionFeatures keys sorted: {shared_opts:?}"
        );

        // Auth: no Bearer header → 401.
        let (s, _) = http(
            &addr,
            "GET",
            "/admin/capsules",
            None,
            &[],
            "application/json",
        );
        assert_eq!(s, 401, "missing admin key must yield 401");

        let _ = std::fs::remove_dir_all(&store_root);
    }

    /// `/admin/capsules` must detect hierarchical capsules — those that
    /// have a `hierarchical_spec.json` sidecar — and report
    /// `scoringMode: "hierarchical"` along with the *real* leaf names
    /// from `enumerate_paths().map(resolve_path)`. This is one notch
    /// better than the flat-meta-bandit fallback (`option_0..N`)
    /// because the tree carries real labels.
    #[test]
    fn admin_capsules_reports_hierarchical_scoring_mode_with_real_leaf_labels() {
        let admin_key = "test-admin-caps-hier";
        let store_root = std::env::temp_dir().join(format!(
            "syntra-admin-caps-hier-{}-{}",
            std::process::id(),
            super::unique_suffix()
        ));
        std::fs::create_dir_all(&store_root).unwrap();
        let addr = spawn_server(&store_root, admin_key);

        let work = store_root.join("work");
        std::fs::create_dir_all(&work).unwrap();

        // Install a capsule, then PUT a 2x2 hierarchical_spec sidecar.
        let tenant = "acme";
        let job = "default";
        let cap = "hier-cap";
        install_simple_bernoulli(&addr, admin_key, &work, tenant, job, cap);

        let hier_spec = serde_json::json!({
            "options": [
                {"name": "us", "subCapsule": {
                    "options": [{"name":"us_small"},{"name":"us_medium"}],
                    "reward": {"type":"continuous","range":[-1,1]}
                }},
                {"name": "eu", "subCapsule": {
                    "options": [{"name":"eu_small"},{"name":"eu_medium"}],
                    "reward": {"type":"continuous","range":[-1,1]}
                }}
            ],
            "reward": {"type":"continuous","range":[-1,1]}
        });
        let (s, _) = http(
            &addr,
            "PUT",
            &format!("/tenants/{tenant}/jobs/{job}/capsules/{cap}/hierarchical_spec"),
            Some(admin_key),
            hier_spec.to_string().as_bytes(),
            "application/json",
        );
        assert_eq!(s, 200, "PUT /hierarchical_spec should succeed");

        let (s, body) = http(
            &addr,
            "GET",
            "/admin/capsules",
            Some(admin_key),
            &[],
            "application/json",
        );
        assert_eq!(s, 200, "GET /admin/capsules; body={}", String::from_utf8_lossy(&body));
        let resp = json_body(&body);
        let arr = resp["capsules"].as_array().expect("capsules must be an array");
        let row = arr.iter()
            .find(|r| r["path"] == format!("{tenant}/{job}/{cap}"))
            .expect("our hierarchical capsule must appear in the list");

        assert_eq!(
            row["scoringMode"], "hierarchical",
            "capsule with a hierarchical_spec sidecar must report hierarchical; row={row}"
        );

        // Real leaf labels from the tree, NOT option_0..N placeholders.
        let opts: Vec<String> = row["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            opts,
            vec![
                "us_small".to_string(),
                "us_medium".to_string(),
                "eu_small".to_string(),
                "eu_medium".to_string(),
            ],
            "hierarchical row must surface enumerate_paths().map(resolve_path) order, \
             got {opts:?}"
        );

        let _ = std::fs::remove_dir_all(&store_root);
    }
}
