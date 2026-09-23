/// Lycan integration tests — validates the full pipeline works correctly.
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_id() -> String {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}_{}", std::process::id(), n)
}

// Helper: run Lycan source and capture stdout
fn run_lycan(src: &str) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg(src)
        .output()
        .expect("failed to execute lycan");
    String::from_utf8_lossy(&output.stdout).to_string()
}

// Helper: run Lycan source from string via temp file
fn eval(code: &str) -> String {
    let path = format!("/tmp/lycan_eval_{}.lycs", unique_id());
    std::fs::write(&path, code).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg(&path)
        .output()
        .expect("failed to execute lycan");
    std::fs::remove_file(&path).ok();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// Helper: compile to .lyc and run the binary
fn compile_and_run(code: &str) -> String {
    let uid = unique_id();
    let src_path = format!("/tmp/lycan_cr_{}.lycs", uid);
    let bin_path = format!("/tmp/lycan_cr_{}.lyc", uid);
    std::fs::write(&src_path, code).unwrap();

    // Compile
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src_path])
        .output()
        .expect("failed to compile");

    // Run binary
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg(&bin_path)
        .output()
        .expect("failed to run binary");

    std::fs::remove_file(&src_path).ok();
    std::fs::remove_file(&bin_path).ok();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// ── Arithmetic ──

#[test]
fn test_addition() {
    assert_eq!(eval("(!p (+ 2 3))"), "5");
}

#[test]
fn test_subtraction() {
    assert_eq!(eval("(!p (- 10 4))"), "6");
}

#[test]
fn test_multiplication() {
    assert_eq!(eval("(!p (* 7 8))"), "56");
}

#[test]
fn test_division_exact() {
    assert_eq!(eval("(!p (/ 20 4))"), "5");
}

#[test]
fn test_division_float() {
    assert_eq!(eval("(!p (/ 7 2))"), "3.5");
}

#[test]
fn test_modulo() {
    assert_eq!(eval("(!p (% 17 5))"), "2");
}

#[test]
fn test_nested_arithmetic() {
    assert_eq!(eval("(!p (+ (* 3 4) (- 10 5)))"), "17");
}

#[test]
fn test_negative_numbers() {
    assert_eq!(eval("(!p (+ -5 3))"), "-2");
}

// ── Comparison ──

#[test]
fn test_equality() {
    assert_eq!(eval("(!p (== 5 5))"), "true");
    assert_eq!(eval("(!p (== 5 3))"), "false");
}

#[test]
fn test_not_equal() {
    assert_eq!(eval("(!p (!= 5 3))"), "true");
}

#[test]
fn test_less_than() {
    assert_eq!(eval("(!p (< 3 5))"), "true");
    assert_eq!(eval("(!p (< 5 3))"), "false");
}

#[test]
fn test_greater_than() {
    assert_eq!(eval("(!p (> 5 3))"), "true");
}

#[test]
fn test_lte_gte() {
    assert_eq!(eval("(!p (<= 5 5))"), "true");
    assert_eq!(eval("(!p (>= 3 5))"), "false");
}

// ── Logic ──

#[test]
fn test_and() {
    assert_eq!(eval("(!p (&& true true))"), "true");
    assert_eq!(eval("(!p (&& true false))"), "false");
}

#[test]
fn test_or() {
    assert_eq!(eval("(!p (|| false true))"), "true");
    assert_eq!(eval("(!p (|| false false))"), "false");
}

#[test]
fn test_not() {
    assert_eq!(eval("(!p (not true))"), "false");
    assert_eq!(eval("(!p (not false))"), "true");
}

// ── Strings ──

#[test]
fn test_string_concat() {
    assert_eq!(eval(r#"(!p (+ "hello" " world"))"#), "hello world");
}

#[test]
fn test_string_with_number() {
    assert_eq!(eval(r#"(!p (+ "value: " 42))"#), "value: 42");
}

// ── Variables ──

#[test]
fn test_immutable_binding() {
    assert_eq!(eval("($ x 42) (!p x)"), "42");
}

#[test]
fn test_mutable_binding() {
    assert_eq!(eval("($! x 1) (= x 2) (!p x)"), "2");
}

#[test]
fn test_multiple_bindings() {
    assert_eq!(eval("($ a 10) ($ b 20) (!p (+ a b))"), "30");
}

// ── Functions ──

#[test]
fn test_named_function() {
    assert_eq!(eval("(F double (x) (* x 2)) (!p (double 21))"), "42");
}

#[test]
fn test_recursive_function() {
    assert_eq!(
        eval(
            "
        (F fib (n)
          (? (<= n 1) n
            (+ (fib (- n 1)) (fib (- n 2)))))
        (!p (fib 10))
    "
        ),
        "55"
    );
}

#[test]
fn test_lambda() {
    assert_eq!(eval("($ f (\\ (x) (* x x))) (!p (f 7))"), "49");
}

#[test]
fn test_higher_order() {
    assert_eq!(
        eval(
            "
        (F apply (f x) (f x))
        (!p (apply (\\ (n) (* n 10)) 5))
    "
        ),
        "50"
    );
}

// ── Control Flow ──

#[test]
fn test_if_true() {
    assert_eq!(eval("(!p (? true 1 0))"), "1");
}

#[test]
fn test_if_false() {
    assert_eq!(eval("(!p (? false 1 0))"), "0");
}

#[test]
fn test_if_chain() {
    assert_eq!(
        eval(
            "
        ($ x 15)
        (!p (? (> x 20) \"high\"
             (? (> x 10) \"medium\"
                \"low\")))
    "
        ),
        "medium"
    );
}

#[test]
fn test_while_loop() {
    assert_eq!(
        eval(
            "
        ($! i 0)
        ($! sum 0)
        (W (< i 5) (= sum (+ sum i)) (= i (+ i 1)))
        (!p sum)
    "
        ),
        "10"
    );
}

#[test]
fn test_for_each() {
    assert_eq!(
        eval(
            "
        ($! sum 0)
        (each x (A 1 2 3 4 5) (= sum (+ sum x)))
        (!p sum)
    "
        ),
        "15"
    );
}

#[test]
fn test_repeat() {
    assert_eq!(
        eval(
            "
        ($! count 0)
        (# 10 (= count (+ count 1)))
        (!p count)
    "
        ),
        "10"
    );
}

// ── Collections ──

#[test]
fn test_array_literal() {
    assert_eq!(eval("(!p (A 1 2 3))"), "(A 1 2 3)");
}

#[test]
fn test_array_index() {
    assert_eq!(eval("(!p (I (A 10 20 30) 1))"), "20");
}

#[test]
fn test_range() {
    assert_eq!(eval("(!p (.. 1 5))"), "(A 1 2 3 4)");
}

#[test]
fn test_array_length() {
    assert_eq!(eval("(!p (!len (A 1 2 3 4 5)))"), "5");
}

#[test]
fn test_array_concat() {
    assert_eq!(eval("(!p (+ (A 1 2) (A 3 4)))"), "(A 1 2 3 4)");
}

// ── Pipelines ──

#[test]
fn test_pipe_map() {
    assert_eq!(eval("(!p (|* (A 1 2 3) (\\ (x) (* x 2))))"), "(A 2 4 6)");
}

#[test]
fn test_pipe_filter() {
    assert_eq!(eval("(!p (|? (A 1 2 3 4 5) (\\ (x) (> x 3))))"), "(A 4 5)");
}

#[test]
fn test_pipe_reduce() {
    assert_eq!(eval("(!p (|+ (A 1 2 3 4 5) (\\ (a b) (+ a b)) 0))"), "15");
}

#[test]
fn test_pipe_chain() {
    // Filter evens, double them, sum
    assert_eq!(
        eval(
            "
        (!p (|+ (|* (|? (A 1 2 3 4 5 6) (\\ (x) (== (% x 2) 0)))
                     (\\ (x) (* x 2)))
                (\\ (a b) (+ a b)) 0))
    "
        ),
        "24"
    );
}

// ── Builtins ──

#[test]
fn test_split() {
    assert_eq!(eval(r#"(!p (!len (!split "a b c d" " ")))"#), "4");
}

#[test]
fn test_num_parse() {
    assert_eq!(eval(r#"(!p (+ (!num "42") 8))"#), "50");
}

#[test]
fn test_str_convert() {
    assert_eq!(eval(r#"(!p (!str 123))"#), "123");
}

#[test]
fn test_math_builtins() {
    assert_eq!(
        eval("(!p (!abs -42) (!round (* (!sin 1.0) 1000000.0)) (!sqrt 144.0))"),
        "42 841471 12"
    );
    assert_eq!(
        compile_and_run("(!p (!round (* (!cos 0.0) 1000000.0)))"),
        "1000000"
    );
}

#[test]
fn test_capability_registry_command_lists_metadata() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg("capabilities")
        .output()
        .expect("failed to run capabilities command");
    assert!(
        output.status.success(),
        "capabilities command should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("\"name\": \"stats.percentile\""),
        "registry should include stats.percentile: {stdout}"
    );
    assert!(
        stdout.contains("\"name\": \"http.get\""),
        "registry should include http.get: {stdout}"
    );
    assert!(
        stdout.contains("\"name\": \"sql.sqliteQuery\""),
        "registry should include SQLite capability: {stdout}"
    );
    assert!(
        stdout.contains("\"name\": \"http.get\""),
        "registry should include HTTP capability: {stdout}"
    );
    assert!(
        !stdout.contains("ephemeris_state"),
        "public capability names should use camelCase, not snake_case: {stdout}"
    );
    assert!(
        stdout.contains("\"effects\": [\"file_read\"]"),
        "registry should expose file_read effects: {stdout}"
    );
    assert!(
        stdout.contains("\"effects\": [\"network\"]"),
        "registry should expose network effects: {stdout}"
    );
}

#[test]
fn test_old_snake_case_capability_names_are_rejected() {
    let path = format!("/tmp/lycan_old_cap_name_{}.lycs", unique_id());
    std::fs::write(&path, r#"(!p (!cap "file.read_text" "/tmp/nope"))"#).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg(&path)
        .output()
        .expect("failed to run old capability name check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "old snake_case capability name should fail"
    );
    assert!(
        stderr.contains("unknown capability 'file.read_text'"),
        "old capability name should not be aliased: {stderr}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_inspect_reports_capabilities_used() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_caps_inspect_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_caps_inspect_{}.lyc", uid);
    std::fs::write(
        &src,
        r#"
        ($ m (!cap "stats.mean" (A 1.0 2.0 3.0)))
        (!p m)
    "#,
    )
    .unwrap();

    let compile = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .expect("failed to compile capability inspect program");
    assert!(
        compile.status.success(),
        "capability inspect program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let inspect = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect capability program");
    let stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(
        stdout.contains("\"capabilities_used\""),
        "inspect should include capabilities_used: {stdout}"
    );
    assert!(
        stdout.contains("\"name\": \"stats.mean\""),
        "inspect should include capability metadata for stats.mean: {stdout}"
    );
    assert!(
        stdout.contains("\"purity\": \"pure\""),
        "inspect should include capability purity metadata: {stdout}"
    );

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_platform_capability_pack_file_json_stats_ops_source_and_binary() {
    let uid = unique_id();
    let json_path = format!("/tmp/lycan_cap_pack_{}.json", uid);
    let write_path = format!("/tmp/lycan_cap_pack_write_{}.txt", uid);
    std::fs::write(
        &json_path,
        r#"{"shop":"Bento Labs","orders":[18,22,40,80],"weather":{"rain":true}}"#,
    )
    .unwrap();

    let code = format!(
        r#"
        ($ wrote (!cap "file.writeText" "{write_path}" "ok"))
        ($ text (!cap "file.readText" "{json_path}"))
        ($ orders (!cap "json.get" text "orders"))
        (!p "exists:" (!cap "file.exists" "{write_path}"))
        (!p "shop:" (!cap "json.get" text "shop"))
        (!p "rain:" (!cap "json.get" text "weather.rain"))
        (!p "orders:" (!len orders))
        (!p "mean:" (!cap "stats.mean" orders))
        (!p "p95:" (!round (!cap "stats.percentile" orders 95.0)))
        (!p "forecast:" (!round (!cap "series.ewmaForecast" orders 0.5)))
        (!p "instances:" (!cap "ops.autoScaleRecommend" (!cap "series.ewmaForecast" orders 0.5) 25.0 1 10))
    "#
    );

    let source = eval(&code);
    assert!(
        source.contains("exists: true"),
        "source should write/read files: {source}"
    );
    assert!(
        source.contains("shop: Bento Labs"),
        "source should read JSON strings: {source}"
    );
    assert!(
        source.contains("rain: true"),
        "source should read JSON booleans: {source}"
    );
    assert!(
        source.contains("orders: 4"),
        "source should return JSON arrays: {source}"
    );
    assert!(
        source.contains("mean: 40"),
        "source should compute stats.mean: {source}"
    );
    assert!(
        source.contains("p95: 74"),
        "source should compute percentile interpolation: {source}"
    );
    assert!(
        source.contains("forecast: 55"),
        "source should compute EWMA forecast: {source}"
    );
    assert!(
        source.contains("instances: 3"),
        "source should recommend autoscale count: {source}"
    );

    let binary = compile_and_run(&code);
    assert!(
        binary.contains("exists: true"),
        "binary should write/read files: {binary}"
    );
    assert!(
        binary.contains("shop: Bento Labs"),
        "binary should read JSON strings: {binary}"
    );
    assert!(
        binary.contains("instances: 3"),
        "binary should recommend autoscale count: {binary}"
    );

    std::fs::remove_file(&json_path).ok();
    std::fs::remove_file(&write_path).ok();
}

#[test]
fn test_sqlite_capability_query_source_and_binary() {
    let db_path = format!("/tmp/lycan_sqlite_cap_{}.db", unique_id());
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "create table shops (name text not null, orders integer not null)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into shops (name, orders) values ('Friday', 120)",
            [],
        )
        .unwrap();
        conn.execute("insert into shops (name, orders) values ('Monday', 34)", [])
            .unwrap();
    }

    let code = format!(
        r#"
        ($ rows (!cap "sql.sqliteQuery" "{db_path}" "select name, orders from shops order by orders desc"))
        (!p "rows:" (!len rows))
        (!p "top:" (I (I rows 0) 0) (I (I rows 0) 1))
    "#
    );

    let source = eval(&code);
    assert!(
        source.contains("rows: 2"),
        "source should query SQLite rows: {source}"
    );
    assert!(
        source.contains("top: Friday 120"),
        "source should preserve row values: {source}"
    );

    let binary = compile_and_run(&code);
    assert!(
        binary.contains("rows: 2"),
        "binary should query SQLite rows: {binary}"
    );
    assert!(
        binary.contains("top: Friday 120"),
        "binary should preserve row values: {binary}"
    );

    std::fs::remove_file(&db_path).ok();
}

#[test]
fn test_http_get_capability_source_and_binary() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = r#"{"status":"ready","orders":42}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        }
    });

    let code = format!(
        r#"
        ($ body (!cap "http.get" "http://127.0.0.1:{port}/status"))
        (!p "status:" (!cap "json.get" body "status"))
        (!p "orders:" (!cap "json.get" body "orders"))
    "#
    );

    let source = eval(&code);
    assert!(
        source.contains("status: ready"),
        "source should fetch HTTP body: {source}"
    );
    assert!(
        source.contains("orders: 42"),
        "source should parse fetched JSON: {source}"
    );

    let binary = compile_and_run(&code);
    assert!(
        binary.contains("status: ready"),
        "binary should fetch HTTP body: {binary}"
    );
    assert!(
        binary.contains("orders: 42"),
        "binary should parse fetched JSON: {binary}"
    );

    server.join().unwrap();
}

// ── Binary Compilation ──

#[test]
fn test_compile_and_run_arithmetic() {
    assert_eq!(compile_and_run("(!p (+ 100 200))"), "300");
}

#[test]
fn test_compile_and_run_function() {
    assert_eq!(
        compile_and_run(
            "
        (F square (x) (* x x))
        (!p (square 9))
    "
        ),
        "81"
    );
}

#[test]
fn test_compile_and_run_loop() {
    assert_eq!(
        compile_and_run(
            "
        ($! sum 0)
        (each x (.. 1 6) (= sum (+ sum x)))
        (!p sum)
    "
        ),
        "15"
    );
}

#[test]
fn test_compile_and_run_pipeline() {
    assert_eq!(
        compile_and_run(
            "
        (!p (|+ (|* (A 1 2 3) (\\(x)(* x x))) (\\(a b)(+ a b)) 0))
    "
        ),
        "14"
    );
}

#[test]
fn test_compile_and_run_conditional() {
    assert_eq!(
        compile_and_run(
            "
        (F abs (x) (? (< x 0) (- 0 x) x))
        (!p (abs -42))
    "
        ),
        "42"
    );
}

// ── Example Programs ──

#[test]
fn test_example_hello() {
    let out = run_lycan("examples/lycan/hello.lycs");
    assert!(out.contains("hello from lycan"));
}

#[test]
fn test_example_fibonacci() {
    let out = run_lycan("examples/lycan/fibonacci.lycs");
    assert!(out.contains("55"));
}

#[test]
fn test_example_fizzbuzz() {
    let out = run_lycan("examples/lycan/fizzbuzz.lycs");
    assert!(out.contains("FizzBuzz"));
    assert!(out.contains("Fizz"));
    assert!(out.contains("Buzz"));
}

// ── Evolution ──

#[test]
fn test_neural_binary_preserves_behavior() {
    // Same program should produce same output from source and binary
    let code = "
        (F fact (n) (? (<= n 1) 1 (* n (fact (- n 1)))))
        (!p (fact 10))
    ";
    let source_output = eval(code);
    let binary_output = compile_and_run(code);
    assert_eq!(source_output, binary_output);
}

#[test]
fn test_calculator_empty_input() {
    // Run a COPY: executing a .lyc persists learned weights back into the
    // source (enforced by tests/fixture_drift.rs).
    let tmp = std::env::temp_dir().join(format!("syntra-calc-{}.lyc", std::process::id()));
    std::fs::copy("examples/lycan/calculator.lyc", &tmp).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg(tmp.to_str().unwrap())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    std::fs::remove_file(&tmp).ok();
    // Should exit cleanly, not crash
    assert!(
        output.status.success() || output.status.code() == Some(0),
        "calculator should handle empty input gracefully"
    );
}

// ── Improvement Protocol ──

// ── Delayed Feedback ──

// ── Edge of Chaos Validation ──

// ── Chaos Control Demo ──

// ── Planetary Defense Demo ──

// ── JPL Horizons Astrodynamics Validation ──

// ── Graph Grafting Regression ──

// ── Runtime policy enforcement tests ──

#[test]
fn test_policy_denies_file_read() {
    // A program that reads a file should fail when policy denies file_read
    let uid = unique_id();
    let target = format!("/tmp/lycan_policy_target_{}.txt", uid);
    std::fs::write(&target, "secret data").unwrap();

    let code = format!(r#"(!p (!cap "file.readText" "{target}"))"#);
    let src = format!("/tmp/lycan_policy_deny_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_policy_deny_{}.lyc", uid);
    std::fs::write(&src, &code).unwrap();

    // Compile
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    // Create capsule — name arg becomes {name}.lycap dir
    let capsule_name = format!("/tmp/lycan_capsule_deny_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "create", &lyc, &capsule_name, "test deny"])
        .output()
        .unwrap();

    // Overwrite policy.json to deny file_read
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(
        &policy_path,
        r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": false,
  "max_execution_ms": 30000,
  "max_memory_bytes": 268435456
}"#,
    )
    .unwrap();

    // capsule verify should reject (graph uses file_read but policy denies)
    let verify_out = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "verify", &capsule_dir])
        .output()
        .unwrap();
    let verify_stderr = String::from_utf8_lossy(&verify_out.stderr);
    assert!(
        verify_stderr.contains("file_read") || !verify_out.status.success(),
        "verify should reject capsule that denies required file_read: {verify_stderr}"
    );

    // Clean up
    std::fs::remove_file(&target).ok();
    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_dir_all(&capsule_dir).ok();
}

#[test]
fn test_policy_runtime_denial_capability() {
    // Test that capabilities::execute() respects policy at runtime
    // Use a simple program that calls file.exists (requires file_read)
    let uid = unique_id();
    let code = r#"(!p (!cap "file.exists" "/tmp"))"#;
    let src = format!("/tmp/lycan_rt_deny_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_rt_deny_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    // Compile
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    // Create capsule
    let capsule_name = format!("/tmp/lycan_rt_deny_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args([
            "capsule",
            "create",
            &lyc,
            &capsule_name,
            "test runtime deny",
        ])
        .output()
        .unwrap();

    // Overwrite policy to deny file_read but allow stdout
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(
        &policy_path,
        r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": true,
  "max_execution_ms": 30000,
  "max_memory_bytes": 268435456
}"#,
    )
    .unwrap();

    // Capsule run — should fail at runtime with structured denial message
    let run_out = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "run", &capsule_dir])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    // Either verify catches it or runtime catches it — both are correct
    assert!(
        stderr.contains("denied by policy") || stderr.contains("file_read"),
        "runtime should deny file.exists when file_read not allowed: {stderr}"
    );

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_dir_all(&capsule_dir).ok();
}

#[test]
fn test_policy_allows_permitted_capability() {
    // A capsule with file_read should allow file.exists for relative paths
    let uid = unique_id();
    // Use file.exists on "program.lyc" which exists inside the capsule dir
    let code = r#"(!p (!cap "file.exists" "program.lyc"))"#;
    let src = format!("/tmp/lycan_allow_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_allow_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    let capsule_name = format!("/tmp/lycan_allow_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "create", &lyc, &capsule_name, "test allow"])
        .output()
        .unwrap();

    // Relative file_root values should be anchored to the capsule directory,
    // so "." means the capsule root, not the server/process cwd.
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(
        &policy_path,
        r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": true,
  "allow_file_write": false,
  "allow_network": false,
  "file_root": ".",
  "allowed_hosts": [],
  "deny_private_networks": true
}"#,
    )
    .unwrap();

    // Capsule run should allow file.exists on relative path inside capsule
    let run_out = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "run", &capsule_dir])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(
        stdout.contains("true"),
        "file.exists program.lyc inside capsule should return true: {stdout}"
    );

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_dir_all(&capsule_dir).ok();
}

#[test]
fn test_no_context_unrestricted() {
    // Direct lycan run (no capsule) should allow all capabilities
    let uid = unique_id();
    let target = format!("/tmp/lycan_unres_{}.txt", uid);
    std::fs::write(&target, "hello unrestricted").unwrap();

    let code = format!(r#"(!p (!cap "file.readText" "{target}"))"#);
    let result = eval(&code);
    assert_eq!(result, "hello unrestricted");

    std::fs::remove_file(&target).ok();
}

#[test]
fn test_policy_denies_stdout() {
    // capsule with allow_stdout=false should reject print
    let uid = unique_id();
    let code = r#"(!p "hello")"#;
    let src = format!("/tmp/lycan_stdout_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_stdout_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    let capsule_name = format!("/tmp/lycan_stdout_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "create", &lyc, &capsule_name, "test stdout deny"])
        .output()
        .unwrap();

    // Overwrite policy to deny stdout
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(
        &policy_path,
        r#"{
  "allow_stdout": false,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": true,
  "max_execution_ms": 30000,
  "max_memory_bytes": 268435456
}"#,
    )
    .unwrap();

    let run_out = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["capsule", "run", &capsule_dir])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        stderr.contains("denied by policy") || stderr.contains("stdout"),
        "stdout should be denied by policy: {stderr}"
    );

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_dir_all(&capsule_dir).ok();
}

// ── Input adapter tests ──

#[test]
fn test_runtime_input_returns_injected_value() {
    // runtime.input should return the value injected via --input
    let result = eval(r#"(!p (!cap "runtime.input"))"#);
    assert_eq!(
        result, "null",
        "without --input, runtime.input returns null"
    );
}

#[test]
fn test_runtime_input_get_dot_path() {
    // runtime.inputGet with dot path on injected JSON
    let uid = unique_id();
    let json_path = format!("/tmp/lycan_input_{}.json", uid);
    std::fs::write(&json_path, r#"{"server": {"port": 8080}, "mode": "fast"}"#).unwrap();

    let code = r#"
(!p (!cap "runtime.inputGet" "mode"))
(!p (!cap "runtime.inputGet" "server.port"))
"#;
    let src = format!("/tmp/lycan_inputget_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_inputget_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args([lyc.as_str(), "--input", json_path.as_str()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("fast"),
        "should get mode=fast: {combined}"
    );
    assert!(
        combined.contains("8080"),
        "should get server.port=8080: {combined}"
    );

    std::fs::remove_file(&json_path).ok();
    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_runtime_input_get_numeric_index() {
    let uid = unique_id();
    let json_path = format!("/tmp/lycan_idx_{}.json", uid);
    std::fs::write(&json_path, r#"{"items": ["alpha", "beta", "gamma"]}"#).unwrap();

    let code = r#"(!p (!cap "runtime.inputGet" "items.1"))"#;
    let src = format!("/tmp/lycan_idx_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_idx_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args([lyc.as_str(), "--input", json_path.as_str()])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("beta"),
        "items.1 should be beta: {combined}"
    );

    std::fs::remove_file(&json_path).ok();
    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_runtime_input_get_missing_path_returns_null() {
    let uid = unique_id();
    let json_path = format!("/tmp/lycan_miss_{}.json", uid);
    std::fs::write(&json_path, r#"{"a": 1}"#).unwrap();

    let code = r#"(!p (!cap "runtime.inputGet" "b.c.d"))"#;
    let src = format!("/tmp/lycan_miss_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_miss_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args(["compile", &src])
        .output()
        .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .args([lyc.as_str(), "--input", json_path.as_str()])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("null"),
        "missing path should return null: {combined}"
    );
    assert!(output.status.success(), "should not crash on missing path");

    std::fs::remove_file(&json_path).ok();
    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

// ── Evolution loop tests ──
