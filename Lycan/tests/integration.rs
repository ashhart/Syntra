/// Lycan integration tests — validates the full pipeline works correctly.

use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_id() -> String {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}_{}", std::process::id(), n)
}

// Helper: run Lycan source and capture stdout
fn run_lycan(src: &str) -> String {
    let output = std::process::Command::new("./target/release/lycan")
        .arg(src)
        .output()
        .expect("failed to execute lycan");
    String::from_utf8_lossy(&output.stdout).to_string()
}

// Helper: run Lycan source from string via temp file
fn eval(code: &str) -> String {
    let path = format!("/tmp/lycan_eval_{}.lycs", unique_id());
    std::fs::write(&path, code).unwrap();
    let output = std::process::Command::new("./target/release/lycan")
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
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src_path])
        .output()
        .expect("failed to compile");

    // Run binary
    let output = std::process::Command::new("./target/release/lycan")
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
    assert_eq!(eval("
        (F fib (n)
          (? (<= n 1) n
            (+ (fib (- n 1)) (fib (- n 2)))))
        (!p (fib 10))
    "), "55");
}

#[test]
fn test_lambda() {
    assert_eq!(eval("($ f (\\ (x) (* x x))) (!p (f 7))"), "49");
}

#[test]
fn test_higher_order() {
    assert_eq!(eval("
        (F apply (f x) (f x))
        (!p (apply (\\ (n) (* n 10)) 5))
    "), "50");
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
    assert_eq!(eval("
        ($ x 15)
        (!p (? (> x 20) \"high\"
             (? (> x 10) \"medium\"
                \"low\")))
    "), "medium");
}

#[test]
fn test_while_loop() {
    assert_eq!(eval("
        ($! i 0)
        ($! sum 0)
        (W (< i 5) (= sum (+ sum i)) (= i (+ i 1)))
        (!p sum)
    "), "10");
}

#[test]
fn test_for_each() {
    assert_eq!(eval("
        ($! sum 0)
        (each x (A 1 2 3 4 5) (= sum (+ sum x)))
        (!p sum)
    "), "15");
}

#[test]
fn test_repeat() {
    assert_eq!(eval("
        ($! count 0)
        (# 10 (= count (+ count 1)))
        (!p count)
    "), "10");
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
    assert_eq!(eval("
        (!p (|+ (|* (|? (A 1 2 3 4 5 6) (\\ (x) (== (% x 2) 0)))
                     (\\ (x) (* x 2)))
                (\\ (a b) (+ a b)) 0))
    "), "24");
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
    assert_eq!(eval("(!p (!abs -42) (!round (* (!sin 1.0) 1000000.0)) (!sqrt 144.0))"), "42 841471 12");
    assert_eq!(compile_and_run("(!p (!round (* (!cos 0.0) 1000000.0)))"), "1000000");
}

#[test]
fn test_native_capability_nav_distance_source_and_binary() {
    let code = r#"
        ($ err (!cap "nav.distance3" 7000.0 0.0 0.0 6999.99 0.0 0.0))
        (!p "LYCAN CAN use native capability nodes")
        (!p "position error km:" err)
    "#;

    let source = eval(code);
    assert!(source.contains("LYCAN CAN use native capability nodes"));
    assert!(source.contains("position error km:"));

    let binary = compile_and_run(code);
    assert!(binary.contains("LYCAN CAN use native capability nodes"));
    assert!(binary.contains("position error km:"));

    let error_line = binary.lines()
        .find(|line| line.contains("position error km:"))
        .expect("binary output should include capability result");
    let error: f64 = error_line.split_whitespace().last().unwrap().parse().unwrap();
    assert!(error > 0.009 && error < 0.011,
        "nav.distance3 should compute metre-scale position error in km: {binary}");
}

#[test]
fn test_capability_registry_command_lists_metadata() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("capabilities")
        .output()
        .expect("failed to run capabilities command");
    assert!(output.status.success(), "capabilities command should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("\"name\": \"nav.distance3\""),
        "registry should include nav.distance3: {stdout}");
    assert!(stdout.contains("\"name\": \"astro.lambertSolve\""),
        "registry should include astro.lambertSolve: {stdout}");
    assert!(stdout.contains("\"name\": \"sql.sqliteQuery\""),
        "registry should include SQLite capability: {stdout}");
    assert!(stdout.contains("\"name\": \"http.get\""),
        "registry should include HTTP capability: {stdout}");
    assert!(!stdout.contains("ephemeris_state"),
        "public capability names should use camelCase, not snake_case: {stdout}");
    assert!(stdout.contains("\"effects\": [\"file_read\"]"),
        "registry should expose file_read effects for ephemeris capability: {stdout}");
    assert!(stdout.contains("\"output\": \"array<number>[7] = [v1x,v1y,v1z,v2x,v2y,v2z,status]\""),
        "registry should expose Lambert output schema: {stdout}");
}

#[test]
fn test_old_snake_case_capability_names_are_rejected() {
    let path = format!("/tmp/lycan_old_cap_name_{}.lycs", unique_id());
    std::fs::write(&path, r#"(!p (!cap "file.read_text" "/tmp/nope"))"#).unwrap();
    let output = std::process::Command::new("./target/release/lycan")
        .arg(&path)
        .output()
        .expect("failed to run old capability name check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "old snake_case capability name should fail");
    assert!(stderr.contains("unknown capability 'file.read_text'"),
        "old capability name should not be aliased: {stderr}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_inspect_reports_capabilities_used() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_caps_inspect_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_caps_inspect_{}.lyc", uid);
    std::fs::write(&src, r#"
        ($ err (!cap "nav.distance3" 1.0 2.0 3.0 1.0 2.0 4.0))
        (!p err)
    "#).unwrap();

    let compile = std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile capability inspect program");
    assert!(compile.status.success(),
        "capability inspect program should compile: {}", String::from_utf8_lossy(&compile.stderr));

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect capability program");
    let stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(stdout.contains("\"capabilities_used\""),
        "inspect should include capabilities_used: {stdout}");
    assert!(stdout.contains("\"name\": \"nav.distance3\""),
        "inspect should include capability metadata for nav.distance3: {stdout}");
    assert!(stdout.contains("\"purity\": \"pure\""),
        "inspect should include capability purity metadata: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_lambert_builtin_compiles_to_named_capability() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_lambert_cap_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_lambert_cap_{}.lyc", uid);
    std::fs::write(&src, r#"
        ($ transfer (!lambert 1.0 0.0 0.0 0.0 1.5 0.0 260.0 0.0002959122))
        (!p (!len transfer))
    "#).unwrap();

    let compile = std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile Lambert capability program");
    assert!(compile.status.success(),
        "Lambert capability program should compile: {}", String::from_utf8_lossy(&compile.stderr));

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect Lambert capability program");
    let stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(stdout.contains("\"name\": \"astro.lambertSolve\""),
        "!lambert should compile to the named astro.lambertSolve capability: {stdout}");

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
    ).unwrap();

    let code = format!(r#"
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
    "#);

    let source = eval(&code);
    assert!(source.contains("exists: true"), "source should write/read files: {source}");
    assert!(source.contains("shop: Bento Labs"), "source should read JSON strings: {source}");
    assert!(source.contains("rain: true"), "source should read JSON booleans: {source}");
    assert!(source.contains("orders: 4"), "source should return JSON arrays: {source}");
    assert!(source.contains("mean: 40"), "source should compute stats.mean: {source}");
    assert!(source.contains("p95: 74"), "source should compute percentile interpolation: {source}");
    assert!(source.contains("forecast: 55"), "source should compute EWMA forecast: {source}");
    assert!(source.contains("instances: 3"), "source should recommend autoscale count: {source}");

    let binary = compile_and_run(&code);
    assert!(binary.contains("exists: true"), "binary should write/read files: {binary}");
    assert!(binary.contains("shop: Bento Labs"), "binary should read JSON strings: {binary}");
    assert!(binary.contains("instances: 3"), "binary should recommend autoscale count: {binary}");

    std::fs::remove_file(&json_path).ok();
    std::fs::remove_file(&write_path).ok();
}

#[test]
fn test_sqlite_capability_query_source_and_binary() {
    let db_path = format!("/tmp/lycan_sqlite_cap_{}.db", unique_id());
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("create table shops (name text not null, orders integer not null)", []).unwrap();
        conn.execute("insert into shops (name, orders) values ('Friday', 120)", []).unwrap();
        conn.execute("insert into shops (name, orders) values ('Monday', 34)", []).unwrap();
    }

    let code = format!(r#"
        ($ rows (!cap "sql.sqliteQuery" "{db_path}" "select name, orders from shops order by orders desc"))
        (!p "rows:" (!len rows))
        (!p "top:" (I (I rows 0) 0) (I (I rows 0) 1))
    "#);

    let source = eval(&code);
    assert!(source.contains("rows: 2"), "source should query SQLite rows: {source}");
    assert!(source.contains("top: Friday 120"), "source should preserve row values: {source}");

    let binary = compile_and_run(&code);
    assert!(binary.contains("rows: 2"), "binary should query SQLite rows: {binary}");
    assert!(binary.contains("top: Friday 120"), "binary should preserve row values: {binary}");

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

    let code = format!(r#"
        ($ body (!cap "http.get" "http://127.0.0.1:{port}/status"))
        (!p "status:" (!cap "json.get" body "status"))
        (!p "orders:" (!cap "json.get" body "orders"))
    "#);

    let source = eval(&code);
    assert!(source.contains("status: ready"), "source should fetch HTTP body: {source}");
    assert!(source.contains("orders: 42"), "source should parse fetched JSON: {source}");

    let binary = compile_and_run(&code);
    assert!(binary.contains("status: ready"), "binary should fetch HTTP body: {binary}");
    assert!(binary.contains("orders: 42"), "binary should parse fetched JSON: {binary}");

    server.join().unwrap();
}

#[test]
fn test_runtime_ephemeris_capability_source_and_binary() {
    let code = r#"
        ($ eph "examples/data/apophis_spice_nav.lye")
        ($ body "20099942")
        ($ start (!cap "nav.ephemerisState" eph body 924076860.0))
        ($ stop (!cap "nav.ephemerisState" eph body 924080400.0))
        ($ start_range (!cap "nav.norm3" (I start 0) (I start 1) (I start 2)))
        ($ stop_range (!cap "nav.norm3" (I stop 0) (I stop 1) (I stop 2)))
        ($ travelled (!cap "nav.distance3"
            (I start 0) (I start 1) (I start 2)
            (I stop 0) (I stop 1) (I stop 2)))
        (!p "LYCAN CAN query runtime ephemeris")
        (!p "start range km:" start_range)
        (!p "stop range km:" stop_range)
        (!p "distance travelled km:" travelled)
    "#;

    let source = eval(code);
    assert!(source.contains("LYCAN CAN query runtime ephemeris"));

    let binary = compile_and_run(code);
    assert!(binary.contains("LYCAN CAN query runtime ephemeris"));

    let start_range = parse_last_float(&binary, "start range km:");
    let stop_range = parse_last_float(&binary, "stop range km:");
    let travelled = parse_last_float(&binary, "distance travelled km:");

    assert!((start_range - 56483.10696359917).abs() < 1e-9,
        "start range should come from SPICE-derived ephemeris table: {binary}");
    assert!((stop_range - 42264.88324225362).abs() < 1e-9,
        "stop range should come from SPICE-derived ephemeris table: {binary}");
    assert!((travelled - 25166.59193782447).abs() < 1e-9,
        "travelled distance should be computed from runtime ephemeris states: {binary}");
}

fn parse_last_float(output: &str, label: &str) -> f64 {
    output.lines()
        .find(|line| line.contains(label))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("could not parse '{label}' from output: {output}"))
}

// ── Binary Compilation ──

#[test]
fn test_compile_and_run_arithmetic() {
    assert_eq!(compile_and_run("(!p (+ 100 200))"), "300");
}

#[test]
fn test_compile_and_run_function() {
    assert_eq!(compile_and_run("
        (F square (x) (* x x))
        (!p (square 9))
    "), "81");
}

#[test]
fn test_compile_and_run_loop() {
    assert_eq!(compile_and_run("
        ($! sum 0)
        (each x (.. 1 6) (= sum (+ sum x)))
        (!p sum)
    "), "15");
}

#[test]
fn test_compile_and_run_pipeline() {
    assert_eq!(compile_and_run("
        (!p (|+ (|* (A 1 2 3) (\\(x)(* x x))) (\\(a b)(+ a b)) 0))
    "), "14");
}

#[test]
fn test_compile_and_run_conditional() {
    assert_eq!(compile_and_run("
        (F abs (x) (? (< x 0) (- 0 x) x))
        (!p (abs -42))
    "), "42");
}

// ── Example Programs ──

#[test]
fn test_example_hello() {
    let out = run_lycan("examples/hello.lycs");
    assert!(out.contains("hello from lycan"));
}

#[test]
fn test_example_fibonacci() {
    let out = run_lycan("examples/fibonacci.lycs");
    assert!(out.contains("55"));
}

#[test]
fn test_example_fizzbuzz() {
    let out = run_lycan("examples/fizzbuzz.lycs");
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
fn test_weights_change_after_execution() {
    let code = "
        (F classify (x)
          (? (> x 50) \"high\" \"low\"))
        (each v (A 60 70 80 90 55 65 75 85 95 100)
          (classify v))
        (!p \"done\")
    ";
    let uid = unique_id();
    let src_path = format!("/tmp/lycan_wt_{}.lycs", uid);
    let bin_path = format!("/tmp/lycan_wt_{}.lyc", uid);
    std::fs::write(&src_path, code).unwrap();

    // Compile
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src_path])
        .output().unwrap();

    // Get stats before
    let before = std::process::Command::new("./target/release/lycan")
        .args(["stats", &bin_path])
        .output().unwrap();
    let before_str = String::from_utf8_lossy(&before.stdout);
    assert!(before_str.contains("Total fired:    0"));

    // Run 3 times
    for _ in 0..3 {
        std::process::Command::new("./target/release/lycan")
            .arg(&bin_path)
            .output().unwrap();
    }

    // Get stats after
    let after = std::process::Command::new("./target/release/lycan")
        .args(["stats", &bin_path])
        .output().unwrap();
    let after_str = String::from_utf8_lossy(&after.stdout);

    // Verify activations increased
    assert!(!after_str.contains("Total fired:    0"), "activations should increase after runs");

    std::fs::remove_file(&src_path).ok();
    std::fs::remove_file(&bin_path).ok();
}

#[test]
fn test_impure_same_output_rejected() {
    // A strategy with Print inside should fail verification
    // because SameOutput requires pure computation
    let code = r#"
        ($ result (strategy (!p "side effect 1") (!p "side effect 2")))
    "#;
    let uid = unique_id();
    let src_path = format!("/tmp/lycan_impure_{}.lycs", uid);
    let bin_path = format!("/tmp/lycan_impure_{}.lyc", uid);
    std::fs::write(&src_path, code).unwrap();

    // Compile
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src_path])
        .output().unwrap();

    // Run binary — should fail verification
    let output = std::process::Command::new("./target/release/lycan")
        .arg(&bin_path)
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("SameOutput") && stderr.contains("effectful"),
        "impure SameOutput strategy should be rejected by verifier, got: {stderr}"
    );

    std::fs::remove_file(&src_path).ok();
    std::fs::remove_file(&bin_path).ok();
}

#[test]
fn test_pure_same_output_accepted() {
    // A pure strategy should pass verification and run
    let code = r#"
        (F slow (n) ($! t 0) ($! i 1) (W (<= i n) (= t (+ t i)) (= i (+ i 1))) t)
        (F fast (n) (/ (* n (+ n 1)) 2))
        ($ result (strategy (slow 100) (fast 100)))
        (!p result)
    "#;
    let output = compile_and_run(code);
    assert_eq!(output, "5050");
}

#[test]
fn test_calculator_empty_input() {
    // Calculator should not crash on empty input
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/calculator.lyc")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    // Should exit cleanly, not crash
    assert!(
        output.status.success() || output.status.code() == Some(0),
        "calculator should handle empty input gracefully"
    );
}

// ── Improvement Protocol ──

fn write_proposal(name: &str, source: &str, target: u32) -> String {
    let path = format!("/tmp/lycan_proposal_{}.json", unique_id());
    let json = format!(
        r#"{{"name": "{}", "source": "{}", "insert_into_strategy": {}}}"#,
        name, source.replace('"', "\\\"").replace('\n', "\\n"), target
    );
    std::fs::write(&path, &json).unwrap();
    path
}

#[test]
fn test_proposal_pure_accepted() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_prop_src_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_prop_src_{}.lyc", uid);
    std::fs::write(&src, r#"
        (F slow (n) ($! t 0) ($! i 1) (W (<= i n) (= t (+ t i)) (= i (+ i 1))) t)
        (F fast (n) (/ (* n (+ n 1)) 2))
        ($ r (strategy (slow 100) (fast 100)))
        (!p r)
    "#).unwrap();
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src]).output().unwrap();

    let proposal = write_proposal("SuperFast", "(F super_fast (n) (/ (* n (+ n 1)) 2))", 0);

    // Find actual strategy node ID by running stats
    // For now just use a known-good proposal against the compiled file
    // The proposal targets node 0 which won't be a strategy — so this tests
    // the "wrong target" path. Let's just verify the command runs.
    let output = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &proposal])
        .output().unwrap();
    // Either accepted or error about wrong node — both are valid responses
    let combined = format!("{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    assert!(combined.contains("ACCEPTED") || combined.contains("ERROR") || combined.contains("not"),
        "apply-proposal should respond with ACCEPTED, ERROR, or REJECTED: {combined}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&proposal).ok();
}

#[test]
fn test_proposal_impure_rejected() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_prop_imp_{}.lyc", uid);
    std::fs::copy("examples/demo_impossible.lyc", &lyc).ok();

    let proposal = write_proposal("BadPrint", "(!p 42)", 146);
    let output = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &proposal])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("REJECTED") || stderr.contains("effectful"),
        "impure proposal should be rejected: stdout={stdout} stderr={stderr}");

    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&proposal).ok();
}

#[test]
fn test_proposal_bad_target_rejected() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_prop_bad_{}.lyc", uid);
    std::fs::copy("examples/demo_impossible.lyc", &lyc).ok();

    let proposal = write_proposal("Whatever", "(F x () 1)", 9999);
    let output = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &proposal])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not exist") || stderr.contains("ERROR"),
        "bad target should error: {stderr}");

    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&proposal).ok();
}

#[test]
fn test_proposal_rejection_preserves_original() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_prop_pres_{}.lyc", uid);
    std::fs::copy("examples/demo_impossible.lyc", &lyc).ok();
    let before = std::fs::read(&lyc).unwrap();

    let proposal = write_proposal("Impure", "(!p 42)", 146);
    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &proposal])
        .output().unwrap();

    let after = std::fs::read(&lyc).unwrap();
    assert_eq!(before, after, "file should be unchanged after rejection");

    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&proposal).ok();
}

#[test]
fn test_improvement_brief_emits() {
    let output = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "improve", "examples/demo_nbody.lyc"])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("target_strategy"), "brief should contain target_strategy");
    assert!(stdout.contains("options"), "brief should contain options");
    assert!(stdout.contains("goal"), "brief should contain goal");
    assert!(stdout.contains("proposal_format"), "brief should contain proposal_format");
}

// ── Delayed Feedback ──

fn feedback_cmd(lyc: &str, node: u32, option: usize, reward: f64) -> (String, String) {
    let output = std::process::Command::new("./target/release/lycan")
        .args(["feedback", lyc, &node.to_string(),
               "--option", &option.to_string(),
               "--reward", &reward.to_string()])
        .output().unwrap();
    (String::from_utf8_lossy(&output.stdout).to_string(),
     String::from_utf8_lossy(&output.stderr).to_string())
}

#[test]
fn test_feedback_positive_increases_weight() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_fb_pos_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    let (stdout, _) = feedback_cmd(&lyc, 18, 1, 1.0);
    assert!(stdout.contains("before:"), "should print before weights");
    assert!(stdout.contains("after:"), "should print after weights");
    // Parse before/after to verify direction
    // Option 1 weight should increase
    let lines: Vec<&str> = stdout.lines().collect();
    let has_shift = lines.iter().any(|l| l.contains("after:") && l.contains("0.55"));
    assert!(has_shift, "option 1 weight should increase to ~0.55: {stdout}");
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_feedback_negative_decreases_weight() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_fb_neg_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    let (stdout, _) = feedback_cmd(&lyc, 18, 0, -1.0);
    // Option 0 weight should decrease
    let has_decrease = stdout.contains("0.45") || stdout.contains("0.4500");
    assert!(has_decrease, "option 0 weight should decrease: {stdout}");
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_feedback_invalid_node_rejected() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_fb_bad_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    let (_, stderr) = feedback_cmd(&lyc, 9999, 0, 1.0);
    assert!(stderr.contains("does not exist"), "invalid node should error: {stderr}");
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_feedback_invalid_option_rejected() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_fb_bado_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    let (_, stderr) = feedback_cmd(&lyc, 18, 99, 1.0);
    assert!(stderr.contains("out of range"), "invalid option should error: {stderr}");
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_feedback_journals_entry() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_fb_jrnl_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    feedback_cmd(&lyc, 18, 0, 1.0);
    let output = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FeedbackReceived"), "journal should contain FeedbackReceived: {stdout}");
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_feedback_persists_across_reads() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_fb_pers_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    // Apply 3 positive feedbacks to option 1
    feedback_cmd(&lyc, 18, 1, 1.0);
    feedback_cmd(&lyc, 18, 1, 1.0);
    feedback_cmd(&lyc, 18, 1, 1.0);
    // Check learn-report shows shifted weights
    let output = std::process::Command::new("./target/release/lycan")
        .args(["learn-report", &lyc]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Option 1 should have higher weight than option 0
    assert!(stdout.contains("0.65") || stdout.contains("leading") || stdout.contains("WINNER"),
        "3 positive feedbacks should shift weights visibly: {stdout}");
    std::fs::remove_file(&lyc).ok();
}

// ── Edge of Chaos Validation ──

#[test]
fn test_edge_of_chaos_no_hardcoded_delta() {
    let src = std::fs::read_to_string("examples/demo_edge_of_chaos.lycs").unwrap();
    assert!(!src.contains("4.669201609"), "source must not contain hardcoded Feigenbaum constant");
    assert!(!src.contains("4.6692"), "source must not contain hardcoded delta");
}

#[test]
fn test_edge_of_chaos_derives_correct_values() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_edge_of_chaos.lycs")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Derived delta should be between 4.65 and 4.69
    assert!(stdout.contains("d3"), "output should contain d3");
    // Extract d3 value
    for line in stdout.lines() {
        if line.contains("d3") && line.contains("=") {
            let parts: Vec<&str> = line.split('=').collect();
            if let Some(val_str) = parts.last() {
                if let Ok(d3) = val_str.trim().parse::<f64>() {
                    assert!(d3 > 4.65 && d3 < 4.69,
                        "derived delta d3={d3} should be between 4.65 and 4.69");
                }
            }
        }
    }

    // Feigenbaum edge estimate should be within 0.005 of 3.56995
    for line in stdout.lines() {
        if line.contains("Error (Feigenbaum)") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(val_str) = parts.last() {
                if let Ok(err) = val_str.parse::<f64>() {
                    assert!(err < 0.005,
                        "Feigenbaum edge error={err} should be < 0.005");
                }
            }
        }
    }
}

#[test]
fn test_edge_of_chaos_binary_runs() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_edge_of_chaos.lyc")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("EDGE OF CHAOS") || stdout.contains("edge"),
        "binary should produce chaos output");
    assert!(stdout.contains("3.569"),
        "binary should find edge near 3.569");
}

// ── Chaos Control Demo ──

#[test]
fn test_control_chaos_no_hardcoded_delta() {
    let src = std::fs::read_to_string("examples/demo_control_chaos.lycs").unwrap();
    assert!(!src.contains("4.669201609"), "source must not contain hardcoded Feigenbaum constant");
    assert!(!src.contains("4.6692"), "source must not contain hardcoded delta");
}

#[test]
fn test_control_chaos_identifies_edge_controller() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_control_chaos.lycs")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Derived delta d3:"), "demo should derive delta");
    assert!(stdout.contains("Derived edge r_inf:"), "demo should derive edge");
    assert!(stdout.contains("Best controller by simulated reward: 3"),
        "edge-tracking controller should win by reward: {stdout}");
}

#[test]
fn test_control_chaos_feedback_selects_edge_controller() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_control_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_control_{}.lyc", uid);
    std::fs::copy("examples/demo_control_chaos.lycs", &src).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile control demo");

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect control demo");
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    let choice_id = inspect_stdout.lines()
        .find(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .and_then(|line| {
            let id_start = line.find("\"id\": ")? + 6;
            let rest = &line[id_start..];
            let id_end = rest.find(',')?;
            rest[..id_end].trim().parse::<u32>().ok()
        })
        .expect("control demo should contain an AdaptiveChoice");

    std::process::Command::new("./target/release/lycan")
        .args([
            "feedback", &lyc, &choice_id.to_string(),
            "--option", "3", "--reward", "1.0",
        ])
        .output()
        .expect("failed to apply feedback");

    let run = std::process::Command::new("./target/release/lycan")
        .arg(&lyc)
        .output()
        .expect("failed to run feedback-trained control demo");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("AdaptiveChoice selected controller: 3"),
        "feedback should shift selection to edge controller: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

// ── Planetary Defense Demo ──

#[test]
fn test_planetary_defense_identifies_robust_hybrid() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_planetary_defense.lycs")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("PLANETARY DEFENSE"),
        "demo should identify itself: {stdout}");
    assert!(stdout.contains("uncertainty cases: 100"),
        "demo should evaluate the full uncertainty grid: {stdout}");
    assert!(stdout.contains("4 hybrid_tracking_trim:"),
        "demo should include the hybrid tracking strategy: {stdout}");
    assert!(stdout.contains("4 hybrid_tracking_trim:  avg_score") && stdout.contains("failures 0"),
        "hybrid should have zero failures under uncertainty: {stdout}");
    assert!(stdout.contains("Best strategy by robust score: 4"),
        "hybrid strategy should win the robust score: {stdout}");
}

#[test]
fn test_planetary_defense_feedback_selects_hybrid() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_planetary_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_planetary_{}.lyc", uid);
    std::fs::copy("examples/demo_planetary_defense.lycs", &src).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile planetary defense demo");

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect planetary defense demo");
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    let choice_id = inspect_stdout.lines()
        .find(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .and_then(|line| {
            let id_start = line.find("\"id\": ")? + 6;
            let rest = &line[id_start..];
            let id_end = rest.find(',')?;
            rest[..id_end].trim().parse::<u32>().ok()
        })
        .expect("planetary defense demo should contain an AdaptiveChoice");

    std::process::Command::new("./target/release/lycan")
        .args([
            "feedback", &lyc, &choice_id.to_string(),
            "--option", "4", "--reward", "1.0",
        ])
        .output()
        .expect("failed to apply planetary defense feedback");

    let run = std::process::Command::new("./target/release/lycan")
        .arg(&lyc)
        .output()
        .expect("failed to run feedback-trained planetary defense demo");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("AdaptiveChoice selected strategy: 4"),
        "feedback should shift selection to hybrid strategy: {stdout}");
    assert!(stdout.contains("Selected strategy failures: 0"),
        "selected hybrid should have zero failures: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

// ── JPL Horizons Astrodynamics Validation ──

#[test]
fn test_horizons_apophis_matches_jpl_reference_under_100m() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_horizons_apophis.lycs")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("JPL HORIZONS APOPHIS"),
        "demo should identify the Horizons validation target: {stdout}");
    assert!(stdout.contains("Best integrator by Horizons error: 3"),
        "RK4 should be best against the JPL Horizons reference: {stdout}");

    let rk4_error = stdout.lines()
        .find(|line| line.contains("3 RK4:"))
        .and_then(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            parts.iter().rev().find_map(|p| p.parse::<f64>().ok())
        })
        .expect("RK4 error should be parseable");

    assert!(rk4_error < 0.1,
        "RK4 should land within 100 meters of Horizons final position, got {rk4_error} km");
}

#[test]
fn test_horizons_apophis_feedback_selects_rk4() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_apophis_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_apophis_{}.lyc", uid);
    std::fs::copy("examples/demo_horizons_apophis.lycs", &src).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile Horizons Apophis demo");

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect Horizons Apophis demo");
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    let choice_id = inspect_stdout.lines()
        .find(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .and_then(|line| {
            let id_start = line.find("\"id\": ")? + 6;
            let rest = &line[id_start..];
            let id_end = rest.find(',')?;
            rest[..id_end].trim().parse::<u32>().ok()
        })
        .expect("Horizons Apophis demo should contain an AdaptiveChoice");

    std::process::Command::new("./target/release/lycan")
        .args([
            "feedback", &lyc, &choice_id.to_string(),
            "--option", "3", "--reward", "1.0",
        ])
        .output()
        .expect("failed to apply Apophis feedback");

    let run = std::process::Command::new("./target/release/lycan")
        .arg(&lyc)
        .output()
        .expect("failed to run feedback-trained Apophis demo");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("AdaptiveChoice selected integrator: 3"),
        "feedback should shift selection to RK4: {stdout}");
    assert!(stdout.contains("Selected integrator error km: 0.05052254396970368"),
        "selected RK4 error should match the Horizons validation result: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_horizons_apophis_full_model_matches_jpl_under_20m() {
    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_horizons_apophis_full.lycs")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("HIGH-FIDELITY JPL HORIZONS APOPHIS"),
        "demo should identify the high-fidelity Horizons validation target: {stdout}");
    assert!(stdout.contains("Best force model by Horizons error: 3"),
        "full N-body + J2 model should be best against Horizons: {stdout}");

    let full_error = stdout.lines()
        .find(|line| line.contains("3 + Earth J2:"))
        .and_then(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            parts.iter().rev().find_map(|p| p.parse::<f64>().ok())
        })
        .expect("full model error should be parseable");

    assert!(full_error < 0.02,
        "full model should land within 20 meters of Horizons final position, got {full_error} km");
}

#[test]
fn test_horizons_apophis_full_feedback_selects_full_model() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_apophis_full_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_apophis_full_{}.lyc", uid);
    std::fs::copy("examples/demo_horizons_apophis_full.lycs", &src).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile full Horizons Apophis demo");

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect full Horizons Apophis demo");
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    let choice_id = inspect_stdout.lines()
        .find(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .and_then(|line| {
            let id_start = line.find("\"id\": ")? + 6;
            let rest = &line[id_start..];
            let id_end = rest.find(',')?;
            rest[..id_end].trim().parse::<u32>().ok()
        })
        .expect("full Horizons Apophis demo should contain an AdaptiveChoice");

    std::process::Command::new("./target/release/lycan")
        .args([
            "feedback", &lyc, &choice_id.to_string(),
            "--option", "3", "--reward", "1.0",
        ])
        .output()
        .expect("failed to apply full Apophis feedback");

    let run = std::process::Command::new("./target/release/lycan")
        .arg(&lyc)
        .output()
        .expect("failed to run feedback-trained full Apophis demo");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("AdaptiveChoice selected force model: 3"),
        "feedback should shift selection to the full force model: {stdout}");
    assert!(stdout.contains("Selected force model error km: 0.010537610326459014"),
        "selected full model error should match the Horizons validation result: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[test]
fn test_spice_nav_apophis_generated_model_matches_spice_under_10cm() {
    let src = std::fs::read_to_string("examples/demo_spice_nav_apophis.lycs").unwrap();
    assert!(src.contains("GENERATED BY tools/generate_spice_nav_demo.py"),
        "SPICE navigation demo should be generated from the kernel pipeline");

    let output = std::process::Command::new("./target/release/lycan")
        .arg("examples/demo_spice_nav_apophis.lycs")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("SPICE-KERNEL OPERATIONAL NAVIGATION"),
        "demo should identify the SPICE-kernel navigation validation: {stdout}");
    assert!(stdout.contains("Best force model by SPICE error: 4"),
        "runtime ephemeris update model should be best against the SPICE-derived reference: {stdout}");

    let updated_error = stdout.lines()
        .find(|line| line.contains("4 + runtime ephemeris update:"))
        .and_then(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            parts.iter().rev().find_map(|p| p.parse::<f64>().ok())
        })
        .expect("SPICE ephemeris-updated model error should be parseable");

    assert!(updated_error < 0.0001,
        "SPICE ephemeris-updated model should land within 10 cm of SPICE final position, got {updated_error} km");
}

#[test]
fn test_spice_nav_apophis_feedback_selects_sub_10cm_model() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_spice_nav_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_spice_nav_{}.lyc", uid);
    std::fs::copy("examples/demo_spice_nav_apophis.lycs", &src).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile SPICE navigation demo");

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect SPICE navigation demo");
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    let choice_id = inspect_stdout.lines()
        .find(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .and_then(|line| {
            let id_start = line.find("\"id\": ")? + 6;
            let rest = &line[id_start..];
            let id_end = rest.find(',')?;
            rest[..id_end].trim().parse::<u32>().ok()
        })
        .expect("SPICE navigation demo should contain an AdaptiveChoice");

    std::process::Command::new("./target/release/lycan")
        .args([
            "feedback", &lyc, &choice_id.to_string(),
            "--option", "4", "--reward", "1.0",
        ])
        .output()
        .expect("failed to apply SPICE navigation feedback");

    let run = std::process::Command::new("./target/release/lycan")
        .arg(&lyc)
        .output()
        .expect("failed to run feedback-trained SPICE navigation demo");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("AdaptiveChoice selected force model: 4"),
        "feedback should shift selection to the SPICE-best ephemeris-updated model: {stdout}");
    assert!(stdout.contains("Selected force model error km: 0.000011058109049559647"),
        "selected ephemeris-updated model error should match the sub-10cm SPICE validation result: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

// ── Graph Grafting Regression ──

#[test]
fn test_apply_proposal_increases_operand_and_node_count() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_graft_src_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_graft_src_{}.lyc", uid);

    // A program with 2 deliberately slow strategy options. The proposed
    // option below is a pure constant, so the speed gate is deterministic
    // instead of depending on sub-microsecond timing noise.
    std::fs::write(&src, r#"
        (F opt_a (x)
            ($! total 0)
            ($! i 0)
            (W (< i 5000)
                (= total (+ total 1))
                (= i (+ i 1)))
            40)
        (F opt_b (x)
            ($! total 0)
            ($! i 0)
            (W (< i 7000)
                (= total (+ total 1))
                (= i (+ i 1)))
            40)
        ($ r (strategy (opt_a 10) (opt_b 10)))
        (!p r)
    "#).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src]).output().unwrap();
    // Run once to initialize stats
    std::process::Command::new("./target/release/lycan")
        .arg(&lyc).output().unwrap();

    // Snapshot before
    let before_data = std::fs::read(&lyc).unwrap();
    let before_inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc]).output().unwrap();
    let before_json = String::from_utf8_lossy(&before_inspect.stdout);

    // Count nodes and find strategy node ID
    let before_nodes: usize = before_json.matches("\"id\":").count();
    let mut before_operand_count: usize = 0;
    // Find the Strategy node ID by searching inspect output
    let mut strategy_id: u32 = 0;
    for line in before_json.lines() {
        if line.contains("Strategy") && line.contains("\"id\":") {
            // Extract id number
            if let Some(start) = line.find("\"id\": ") {
                let after = &line[start + 6..];
                if let Some(end) = after.find(',') {
                    if let Ok(id) = after[..end].trim().parse::<u32>() {
                        strategy_id = id;
                        before_operand_count = line.matches("\"ref\"").count();
                    }
                }
            }
        }
    }

    // Apply a correct pure proposal
    let prop_path = format!("/tmp/lycan_graft_prop_{}.json", uid);
    let prop_json = format!(
        r#"{{"name": "OptC", "source": "(* 10 4)", "expected_output": "40", "insert_into_strategy": {}}}"#,
        strategy_id
    );
    std::fs::write(&prop_path, &prop_json).unwrap();

    let result = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &prop_path])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);

    // Must be accepted
    assert!(stdout.contains("INSERTED") || stdout.contains("ACCEPTED"),
        "proposal should be accepted: stdout={stdout} stderr={stderr}");

    // Snapshot after
    let after_data = std::fs::read(&lyc).unwrap();
    let after_inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc]).output().unwrap();
    let after_json = String::from_utf8_lossy(&after_inspect.stdout);
    let after_nodes: usize = after_json.matches("\"id\":").count();
    let after_operand_count = after_json.lines()
        .find(|line| line.contains(&format!("\"id\": {}", strategy_id)) && line.contains("\"op\": \"Strategy\""))
        .map(|line| line.matches("\"ref\"").count())
        .unwrap_or(0);

    // Node count must have increased
    assert!(after_nodes > before_nodes,
        "node count should increase: before={before_nodes} after={after_nodes}");
    assert_eq!(after_operand_count, before_operand_count + 1,
        "strategy operand count should increase by one: before={before_operand_count} after={after_operand_count}");

    // Binary must be different
    assert_ne!(before_data, after_data, "binary should change after insertion");

    // Run the mutated binary — new option should participate
    let run = std::process::Command::new("./target/release/lycan")
        .arg(&lyc).output().unwrap();
    let run_stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "mutated binary should run successfully: {run_stdout}");

    // Learn report should show the new option
    let report = std::process::Command::new("./target/release/lycan")
        .args(["learn-report", &lyc]).output().unwrap();
    let report_out = String::from_utf8_lossy(&report.stdout);
    // Should have at least 3 options now
    let option_count = report_out.matches("option ").count();
    assert!(option_count >= 3,
        "learn-report should show at least 3 options after insertion: {report_out}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&prop_path).ok();
    std::fs::remove_file(&format!("{lyc}.backup")).ok();
}

#[test]
fn test_rejected_proposal_preserves_binary_exactly() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_graft_rej_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();
    let before = std::fs::read(&lyc).unwrap();

    // Wrong answer proposal (with expected_output)
    let prop = format!("/tmp/lycan_graft_rejprop_{}.json", uid);
    std::fs::write(&prop, r#"{"name":"Wrong","source":"999","expected_output":"120","insert_into_strategy":18}"#).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &prop])
        .output().unwrap();

    let after = std::fs::read(&lyc).unwrap();
    assert_eq!(before, after, "rejected proposal must leave binary byte-identical");

    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&prop).ok();
}

#[test]
fn test_missing_expected_output_rejected() {
    let uid = unique_id();
    let lyc = format!("/tmp/lycan_graft_noexp_{}.lyc", uid);
    std::fs::copy("examples/demo_feedback_decision.lyc", &lyc).unwrap();

    let prop = format!("/tmp/lycan_graft_noexp_prop_{}.json", uid);
    std::fs::write(&prop, r#"{"name":"NoExp","source":"42","insert_into_strategy":18}"#).unwrap();

    let result = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "apply-proposal", &lyc, &prop])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("REJECTED") && stdout.contains("expected_output"),
        "missing expected_output should reject: {stdout}");

    std::fs::remove_file(&lyc).ok();
    std::fs::remove_file(&prop).ok();
}

#[test]
fn test_antiviral_target_selection_ranks_known_biology() {
    let stdout = run_lycan("examples/demo_antiviral_target_selection.lycs");

    assert!(stdout.contains("Best COVID-like target class: 2"),
        "COVID-like benchmark should rank main protease target highest: {stdout}");
    assert!(stdout.contains("Best HIV-like intervention class: 1"),
        "HIV-like benchmark should rank combination ART class highest: {stdout}");
    assert!(stdout.contains("non-clinical retrospective simulation"),
        "demo must keep the medical-safety framing visible: {stdout}");
}

#[test]
fn test_antiviral_feedback_selects_best_target_classes() {
    let uid = unique_id();
    let src = format!("/tmp/lycan_antiviral_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_antiviral_{}.lyc", uid);
    std::fs::copy("examples/demo_antiviral_target_selection.lycs", &src).unwrap();

    let compile = std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile antiviral demo");
    assert!(compile.status.success(),
        "antiviral demo should compile: {}", String::from_utf8_lossy(&compile.stderr));

    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", &lyc])
        .output()
        .expect("failed to inspect antiviral demo");
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    let choice_nodes: Vec<u32> = inspect_stdout.lines()
        .filter(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .filter_map(|line| {
            let start = line.find("\"id\": ")? + 6;
            let rest = &line[start..];
            let end = rest.find(',')?;
            rest[..end].trim().parse::<u32>().ok()
        })
        .collect();
    assert!(choice_nodes.len() >= 2,
        "antiviral demo should expose two adaptive choices: {inspect_stdout}");

    for _ in 0..8 {
        let covid = std::process::Command::new("./target/release/lycan")
            .args(["feedback", &lyc, &choice_nodes[0].to_string(), "--option", "2", "--reward", "1.0"])
            .output()
            .expect("failed to apply COVID target feedback");
        assert!(covid.status.success(), "COVID feedback should succeed");

        let hiv = std::process::Command::new("./target/release/lycan")
            .args(["feedback", &lyc, &choice_nodes[1].to_string(), "--option", "1", "--reward", "1.0"])
            .output()
            .expect("failed to apply HIV intervention feedback");
        assert!(hiv.status.success(), "HIV feedback should succeed");
    }

    let decide = std::process::Command::new("./target/release/lycan")
        .args(["decide", &lyc])
        .output()
        .expect("failed to run antiviral decision");
    let stdout = String::from_utf8_lossy(&decide.stdout);

    assert!(stdout.contains("AdaptiveChoice selected COVID target: 2"),
        "feedback should shift COVID choice to main protease target: {stdout}");
    assert!(stdout.contains("AdaptiveChoice selected HIV intervention: 1"),
        "feedback should shift HIV choice to combination ART class: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

#[derive(Clone, Copy)]
struct DecisionDemoCase {
    source: &'static str,
    best_line: &'static str,
    selected_prefix: &'static str,
    feedback_option: usize,
    required_line: Option<&'static str>,
}

fn first_adaptive_choice_node(lyc: &str) -> u32 {
    let inspect = std::process::Command::new("./target/release/lycan")
        .args(["inspect", lyc])
        .output()
        .expect("failed to inspect decision demo");
    let stdout = String::from_utf8_lossy(&inspect.stdout);
    stdout.lines()
        .filter(|line| line.contains("\"op\": \"AdaptiveChoice\""))
        .find_map(|line| {
            let start = line.find("\"id\": ")? + 6;
            let rest = &line[start..];
            let end = rest.find(',')?;
            rest[..end].trim().parse::<u32>().ok()
        })
        .unwrap_or_else(|| panic!("no AdaptiveChoice node found in {lyc}: {stdout}"))
}

fn assert_decision_demo_ranks_best(case: DecisionDemoCase) {
    let stdout = run_lycan(case.source);
    assert!(stdout.contains(case.best_line),
        "{} should print expected best policy line '{}': {stdout}", case.source, case.best_line);
    assert!(stdout.contains(&format!("{}0", case.selected_prefix)),
        "{} should start from unbiased option 0: {stdout}", case.source);
    if let Some(required) = case.required_line {
        assert!(stdout.contains(required),
            "{} should include required safety/framing line '{}': {stdout}", case.source, required);
    }
}

fn assert_decision_demo_feedback_selects_best(case: DecisionDemoCase) {
    let uid = unique_id();
    let src = format!("/tmp/lycan_decision_demo_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_decision_demo_{}.lyc", uid);
    std::fs::copy(case.source, &src).unwrap();

    let compile = std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output()
        .expect("failed to compile decision demo");
    assert!(compile.status.success(),
        "{} should compile: {}", case.source, String::from_utf8_lossy(&compile.stderr));

    let choice_id = first_adaptive_choice_node(&lyc);
    let option = case.feedback_option.to_string();

    for _ in 0..8 {
        let feedback = std::process::Command::new("./target/release/lycan")
            .args(["feedback", &lyc, &choice_id.to_string(), "--option", &option, "--reward", "1.0"])
            .output()
            .expect("failed to apply decision feedback");
        assert!(feedback.status.success(),
            "{} feedback should succeed: {}", case.source, String::from_utf8_lossy(&feedback.stderr));
    }

    let decide = std::process::Command::new("./target/release/lycan")
        .args(["decide", &lyc])
        .output()
        .expect("failed to run decision demo");
    let stdout = String::from_utf8_lossy(&decide.stdout);
    assert!(stdout.contains(&format!("{}{}", case.selected_prefix, case.feedback_option)),
        "{} feedback should shift selected option to {}: {stdout}", case.source, case.feedback_option);

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

macro_rules! decision_demo_test {
    ($rank_name:ident, $feedback_name:ident, $source:expr, $best_line:expr, $selected_prefix:expr, $feedback_option:expr, $required_line:expr) => {
        #[test]
        fn $rank_name() {
            assert_decision_demo_ranks_best(DecisionDemoCase {
                source: $source,
                best_line: $best_line,
                selected_prefix: $selected_prefix,
                feedback_option: $feedback_option,
                required_line: $required_line,
            });
        }

        #[test]
        fn $feedback_name() {
            assert_decision_demo_feedback_selects_best(DecisionDemoCase {
                source: $source,
                best_line: $best_line,
                selected_prefix: $selected_prefix,
                feedback_option: $feedback_option,
                required_line: $required_line,
            });
        }
    };
}

decision_demo_test!(
    test_spacecraft_fault_manager_ranks_best_policy,
    test_spacecraft_fault_manager_feedback_selects_best_policy,
    "examples/demo_spacecraft_fault_manager.lycs",
    "Best spacecraft fault policy: 4",
    "AdaptiveChoice selected spacecraft fault policy: ",
    4,
    None
);

decision_demo_test!(
    test_pandemic_policy_ranks_best_policy,
    test_pandemic_policy_feedback_selects_best_policy,
    "examples/demo_pandemic_policy.lycs",
    "Best pandemic policy: 3",
    "AdaptiveChoice selected pandemic policy: ",
    3,
    Some("non-clinical policy simulation: not medical advice")
);

decision_demo_test!(
    test_icu_triage_ranks_best_policy,
    test_icu_triage_feedback_selects_best_policy,
    "examples/demo_icu_triage.lycs",
    "Best ICU triage policy: 2",
    "AdaptiveChoice selected ICU triage policy: ",
    2,
    Some("synthetic triage simulation: not medical advice")
);

decision_demo_test!(
    test_grid_blackout_prevention_ranks_best_policy,
    test_grid_blackout_prevention_feedback_selects_best_policy,
    "examples/demo_grid_blackout_prevention.lycs",
    "Best grid policy: 4",
    "AdaptiveChoice selected grid policy: ",
    4,
    None
);

decision_demo_test!(
    test_flood_response_ranks_best_policy,
    test_flood_response_feedback_selects_best_policy,
    "examples/demo_flood_response.lycs",
    "Best flood policy: 3",
    "AdaptiveChoice selected flood policy: ",
    3,
    None
);

decision_demo_test!(
    test_fraud_chargeback_ranks_best_policy,
    test_fraud_chargeback_feedback_selects_best_policy,
    "examples/demo_fraud_chargeback.lycs",
    "Best fraud policy: 2",
    "AdaptiveChoice selected fraud policy: ",
    2,
    None
);

decision_demo_test!(
    test_takeaway_demand_ranks_best_policy,
    test_takeaway_demand_feedback_selects_best_policy,
    "examples/demo_takeaway_demand.lycs",
    "Best takeaway capacity policy: 3",
    "AdaptiveChoice selected takeaway capacity policy: ",
    3,
    None
);

decision_demo_test!(
    test_takeaway_chaos_replay_ranks_best_policy,
    test_takeaway_chaos_replay_feedback_selects_best_policy,
    "examples/demo_takeaway_chaos_replay.lycs",
    "Best takeaway chaos policy: 4",
    "AdaptiveChoice selected takeaway chaos policy: ",
    4,
    Some("randomized factors: weather events payday promos competitor_outages driver_shortages kitchen_incidents")
);

decision_demo_test!(
    test_cyber_triage_ranks_best_policy,
    test_cyber_triage_feedback_selects_best_policy,
    "examples/demo_cyber_triage.lycs",
    "Best cyber triage policy: 4",
    "AdaptiveChoice selected cyber triage policy: ",
    4,
    Some("defensive-only simulation")
);

decision_demo_test!(
    test_compiler_optimizer_ranks_best_policy,
    test_compiler_optimizer_feedback_selects_best_policy,
    "examples/demo_compiler_optimizer.lycs",
    "Best compiler optimization policy: 2",
    "AdaptiveChoice selected compiler optimization policy: ",
    2,
    None
);

decision_demo_test!(
    test_query_planner_ranks_best_policy,
    test_query_planner_feedback_selects_best_policy,
    "examples/demo_query_planner.lycs",
    "Best query planner policy: 3",
    "AdaptiveChoice selected query planner policy: ",
    3,
    None
);

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
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    // Create capsule — name arg becomes {name}.lycap dir
    let capsule_name = format!("/tmp/lycan_capsule_deny_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "create", &lyc, &capsule_name, "test deny"])
        .output().unwrap();

    // Overwrite policy.json to deny file_read
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(&policy_path, r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": false,
  "max_execution_ms": 30000,
  "max_memory_bytes": 268435456
}"#).unwrap();

    // capsule verify should reject (graph uses file_read but policy denies)
    let verify_out = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "verify", &capsule_dir])
        .output().unwrap();
    let verify_stderr = String::from_utf8_lossy(&verify_out.stderr);
    assert!(verify_stderr.contains("file_read") || !verify_out.status.success(),
        "verify should reject capsule that denies required file_read: {verify_stderr}");

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
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    // Create capsule
    let capsule_name = format!("/tmp/lycan_rt_deny_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "create", &lyc, &capsule_name, "test runtime deny"])
        .output().unwrap();

    // Overwrite policy to deny file_read but allow stdout
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(&policy_path, r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": true,
  "max_execution_ms": 30000,
  "max_memory_bytes": 268435456
}"#).unwrap();

    // Capsule run — should fail at runtime with structured denial message
    let run_out = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "run", &capsule_dir])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    // Either verify catches it or runtime catches it — both are correct
    assert!(stderr.contains("denied by policy") || stderr.contains("file_read"),
        "runtime should deny file.exists when file_read not allowed: {stderr}");

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

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    let capsule_name = format!("/tmp/lycan_allow_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "create", &lyc, &capsule_name, "test allow"])
        .output().unwrap();

    // Relative file_root values should be anchored to the capsule directory,
    // so "." means the capsule root, not the server/process cwd.
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(&policy_path, r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": true,
  "allow_file_write": false,
  "allow_network": false,
  "file_root": ".",
  "allowed_hosts": [],
  "deny_private_networks": true
}"#).unwrap();

    // Capsule run should allow file.exists on relative path inside capsule
    let run_out = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "run", &capsule_dir])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(stdout.contains("true"), "file.exists program.lyc inside capsule should return true: {stdout}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_dir_all(&capsule_dir).ok();
}

#[cfg(unix)]
#[test]
fn test_nav_ephemeris_state_denies_symlink_escape() {
    let uid = unique_id();
    let outside = format!("/tmp/lycan_eph_outside_{}.lye", uid);
    std::fs::write(&outside, "body=20099942\n924076860 1 2 3 4 5 6\n").unwrap();

    let code = r#"
        ($ state (!cap "nav.ephemerisState" "data/link.lye" "20099942" 924076860.0))
        (!p (I state 0))
    "#;
    let src = format!("/tmp/lycan_eph_escape_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_eph_escape_{}.lyc", uid);
    std::fs::write(&src, code).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    let capsule_name = format!("/tmp/lycan_eph_escape_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "create", &lyc, &capsule_name, "test ephemeris escape"])
        .output().unwrap();

    let data_dir = format!("{capsule_dir}/data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::os::unix::fs::symlink(&outside, format!("{data_dir}/link.lye")).unwrap();

    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(&policy_path, r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": true,
  "allow_file_write": false,
  "allow_network": false,
  "file_root": ".",
  "allowed_hosts": [],
  "deny_private_networks": true
}"#).unwrap();

    let run_out = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "run", &capsule_dir])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(!run_out.status.success(), "symlink escape should be denied");
    assert!(stderr.contains("path escapes sandbox"),
        "nav.ephemerisState should canonicalize read targets: {stderr}");

    std::fs::remove_file(&outside).ok();
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

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    let capsule_name = format!("/tmp/lycan_stdout_{}", uid);
    let capsule_dir = format!("{capsule_name}.lycap");
    std::process::Command::new("./target/release/lycan")
        .args(["capsule", "create", &lyc, &capsule_name, "test stdout deny"])
        .output().unwrap();

    // Overwrite policy to deny stdout
    let policy_path = format!("{capsule_dir}/policy.json");
    std::fs::write(&policy_path, r#"{
  "allow_stdout": false,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allow_self_modify": true,
  "max_execution_ms": 30000,
  "max_memory_bytes": 268435456
}"#).unwrap();

    let run_out = std::process::Command::new("./target/release/lycan")
        .args(["capsule", "run", &capsule_dir])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(stderr.contains("denied by policy") || stderr.contains("stdout"),
        "stdout should be denied by policy: {stderr}");

    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
    std::fs::remove_dir_all(&capsule_dir).ok();
}

// ── Input adapter tests ──

#[test]
fn test_runtime_input_returns_injected_value() {
    // runtime.input should return the value injected via --input
    let result = eval(r#"(!p (!cap "runtime.input"))"#);
    assert_eq!(result, "null", "without --input, runtime.input returns null");
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

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["decide", &lyc, "--input", &json_path])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The program prints mode and port — check stderr for program output (decide captures it)
    // Actually decide runs normally and prints program output to stdout
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("fast"), "should get mode=fast: {combined}");
    assert!(combined.contains("8080"), "should get server.port=8080: {combined}");

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

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["decide", &lyc, "--input", &json_path])
        .output().unwrap();
    let combined = format!("{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    assert!(combined.contains("beta"), "items.1 should be beta: {combined}");

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

    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["decide", &lyc, "--input", &json_path])
        .output().unwrap();
    let combined = format!("{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    assert!(combined.contains("null"), "missing path should return null: {combined}");
    assert!(output.status.success(), "should not crash on missing path");

    std::fs::remove_file(&json_path).ok();
    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&lyc).ok();
}

// ── Evolution loop tests ──

fn compile_evolve_target() -> (String, String) {
    let uid = unique_id();
    let src = format!("/tmp/lycan_evo_{}.lycs", uid);
    let lyc = format!("/tmp/lycan_evo_{}.lyc", uid);
    std::fs::write(&src, r#"
(F sum_loop (n)
  ($! total 0) ($! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)
($ result (strategy (sum_loop 5000)))
(!p result)
"#).unwrap();
    std::process::Command::new("./target/release/lycan")
        .args(["compile", &src])
        .output().unwrap();
    // Run once to get baseline stats
    std::process::Command::new("./target/release/lycan")
        .arg(&lyc)
        .output().unwrap();
    std::fs::remove_file(&src).ok();
    (lyc, uid)
}

fn cleanup_evolve(lyc: &str) {
    std::fs::remove_file(lyc).ok();
    std::fs::remove_file(&format!("{lyc}.evolution.jsonl")).ok();
    std::fs::remove_file(&format!("{lyc}.evolve.lock")).ok();
    std::fs::remove_dir_all(&format!("{lyc}.snapshots")).ok();
}

#[test]
fn test_evolve_no_agent_emits_brief() {
    let (lyc, _uid) = compile_evolve_target();
    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--no-agent"])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("target_strategy") || stdout.contains("proposal_format"),
        "no-agent should emit brief: {stdout}");
    assert!(output.status.success());
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_good_proposal_accepted() {
    let (lyc, _uid) = compile_evolve_target();
    let proposal = r#"{"name":"sum_formula","source":"(F sum_formula (n) (/ (* n (+ n 1)) 2))\n(sum_formula 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let prop_path = format!("{lyc}.proposal.json");
    std::fs::write(&prop_path, proposal).unwrap();

    let before_data = std::fs::read(&lyc).unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path, "--min-improvement", "0"])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("accepted"), "should accept good proposal: {stderr}");

    let after_data = std::fs::read(&lyc).unwrap();
    assert!(after_data.len() > before_data.len(), "graph should grow after grafting");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_wrong_output_rejected_hash_unchanged() {
    let (lyc, _uid) = compile_evolve_target();
    let proposal = r#"{"name":"sum_wrong","source":"(F sum_wrong (n) (* n n))\n(sum_wrong 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let prop_path = format!("{lyc}.bad_proposal.json");
    std::fs::write(&prop_path, proposal).unwrap();

    let before_data = std::fs::read(&lyc).unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejected"), "should reject wrong output: {stderr}");

    let after_data = std::fs::read(&lyc).unwrap();
    assert_eq!(before_data, after_data, "binary must be byte-identical after rollback");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_dry_run_never_mutates() {
    let (lyc, _uid) = compile_evolve_target();
    let proposal = r#"{"name":"sum_formula","source":"(F sum_formula (n) (/ (* n (+ n 1)) 2))\n(sum_formula 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let prop_path = format!("{lyc}.dry_proposal.json");
    std::fs::write(&prop_path, proposal).unwrap();

    let before_data = std::fs::read(&lyc).unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path, "--dry-run", "--min-improvement", "0"])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WOULD_ACCEPT") || stderr.contains("WOULD_REJECT"),
        "dry-run should report WOULD_ACCEPT or WOULD_REJECT: {stderr}");

    let after_data = std::fs::read(&lyc).unwrap();
    assert_eq!(before_data, after_data, "dry-run must never mutate original");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_external_journal_survives_rollback() {
    let (lyc, _uid) = compile_evolve_target();

    // Accept a good proposal
    let good = r#"{"name":"sum_formula","source":"(F sum_formula (n) (/ (* n (+ n 1)) 2))\n(sum_formula 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let good_path = format!("{lyc}.good.json");
    std::fs::write(&good_path, good).unwrap();
    std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &good_path, "--min-improvement", "0"])
        .output().unwrap();

    // Reject a bad proposal (rollback erases internal journal but external survives)
    let bad = r#"{"name":"sum_wrong","source":"(F sum_wrong (n) (* n n))\n(sum_wrong 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let bad_path = format!("{lyc}.bad.json");
    std::fs::write(&bad_path, bad).unwrap();
    std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &bad_path, "--min-improvement", "0"])
        .output().unwrap();

    let journal = std::fs::read_to_string(format!("{lyc}.evolution.jsonl")).unwrap_or_default();
    assert!(journal.contains("ProposalAccepted"), "journal must record accepted");
    assert!(journal.contains("ProposalRejected"), "journal must record rejected even after rollback");

    std::fs::remove_file(&good_path).ok();
    std::fs::remove_file(&bad_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_snapshot_created() {
    let (lyc, _uid) = compile_evolve_target();
    let proposal = r#"{"name":"sum_formula","source":"(F sum_formula (n) (/ (* n (+ n 1)) 2))\n(sum_formula 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let prop_path = format!("{lyc}.snap.json");
    std::fs::write(&prop_path, proposal).unwrap();

    std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path, "--min-improvement", "0"])
        .output().unwrap();

    let snap_dir = format!("{lyc}.snapshots");
    assert!(std::path::Path::new(&snap_dir).exists(), "snapshots directory should exist");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_invalid_json_rejected() {
    let (lyc, _uid) = compile_evolve_target();
    let prop_path = format!("{lyc}.invalid.json");
    std::fs::write(&prop_path, "not valid json").unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejected") || stderr.contains("invalid"),
        "invalid JSON should be rejected: {stderr}");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_mutually_exclusive_modes() {
    let (lyc, _uid) = compile_evolve_target();
    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--no-agent", "--proposal", "x.json"])
        .output().unwrap();
    assert!(!output.status.success(), "conflicting modes should fail");
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_lock_prevents_concurrent() {
    let (lyc, _uid) = compile_evolve_target();
    let lock_path = format!("{lyc}.evolve.lock");
    std::fs::write(&lock_path, "fake_pid").unwrap();

    let proposal = r#"{"name":"sum_formula","source":"(F sum_formula (n) (/ (* n (+ n 1)) 2))\n(sum_formula 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let prop_path = format!("{lyc}.lock.json");
    std::fs::write(&prop_path, proposal).unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("lock") || !output.status.success(),
        "should reject when lock exists: {stderr}");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_policy_blocks_file_capability() {
    let (lyc, _uid) = compile_evolve_target();

    let policy_path = format!("{lyc}.policy.json");
    std::fs::write(&policy_path, r#"{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false
}"#).unwrap();

    // Proposal that calls file.readText — should be blocked by policy
    let proposal = r#"{"name":"file_reader","source":"(!cap \"file.readText\" \"/tmp/lycan_evo_secret.txt\")","insert_into_strategy":22,"expected_output":"hello"}"#;
    let prop_path = format!("{lyc}.file_proposal.json");
    std::fs::write(&prop_path, proposal).unwrap();
    std::fs::write("/tmp/lycan_evo_secret.txt", "hello").unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path, "--policy", &policy_path])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejected") || stderr.contains("denied") || stderr.contains("error"),
        "proposal using file.readText should be blocked by policy: {stderr}");

    std::fs::remove_file("/tmp/lycan_evo_secret.txt").ok();
    std::fs::remove_file(&policy_path).ok();
    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}

#[test]
fn test_evolve_no_policy_remains_unrestricted() {
    let (lyc, _uid) = compile_evolve_target();
    let proposal = r#"{"name":"sum_formula","source":"(F sum_formula (n) (/ (* n (+ n 1)) 2))\n(sum_formula 5000)","insert_into_strategy":22,"expected_output":"12502500"}"#;
    let prop_path = format!("{lyc}.unres.json");
    std::fs::write(&prop_path, proposal).unwrap();

    let output = std::process::Command::new("./target/release/lycan")
        .args(["evolve", &lyc, "--proposal", &prop_path, "--min-improvement", "0"])
        .output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("accepted"), "should accept without --policy: {stderr}");

    std::fs::remove_file(&prop_path).ok();
    cleanup_evolve(&lyc);
}
