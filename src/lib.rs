/// Run the Syntra appliance CLI.
mod authoring;
mod capsule_compiler;
mod capsule_spec;
mod simulate;

pub fn run() {
    let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
    let handler = builder.spawn(main_inner).unwrap();
    handler.join().unwrap();
}

fn main_inner() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1] == "serve" {
        cli_serve(&args[2..]);
        return;
    }

    if args.len() >= 2 && args[1] == "author" {
        cli_author(&args[2..]);
        return;
    }

    if args.len() >= 2 && args[1] == "simulate" {
        cli_simulate(&args[2..]);
        return;
    }

    if args.len() >= 2 {
        match args[1].as_str() {
            "--help" | "-h" => {
                print_usage();
                return;
            }
            "health" => {
                println!(r#"{{"ok":true,"service":"Syntra"}}"#);
                return;
            }
            "status" => {
                cli_status(&args[2..]);
                return;
            }
            "stop" => {
                cli_stop(&args[2..]);
                return;
            }
            _ => {}
        }
    }

    print_usage();
}

/// Parse `--addr host:port` (or `--port N`) out of args. Returns the port
/// as a string suitable for `lsof -ti :<port>`. Default 8787.
fn parse_port(args: &[String]) -> String {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                if let Some(a) = args.get(i + 1) {
                    if let Some(p) = a.rsplit(':').next() {
                        if p.parse::<u16>().is_ok() {
                            return p.to_string();
                        }
                    }
                }
            }
            "--port" => {
                if let Some(a) = args.get(i + 1) {
                    if a.parse::<u16>().is_ok() {
                        return a.clone();
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    "8787".to_string()
}

/// Run `lsof -ti :<port>` and return the first PID listening on the port.
/// `None` on no listener, lsof missing, or unparsable output.
fn find_pid_on_port(port: &str) -> Option<u32> {
    let out = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}"), "-sTCP:LISTEN"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next()?.trim().parse().ok()
}

fn cli_status(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: syntra status [--addr host:port | --port N]");
        eprintln!("Reports whether a process is listening on the configured port.");
        eprintln!("Defaults to port 8787 if --addr/--port are not given.");
        return;
    }
    let port = parse_port(args);
    match find_pid_on_port(&port) {
        Some(pid) => println!(
            r#"{{"running":true,"port":{port},"pid":{pid}}}"#,
            port = port,
            pid = pid,
        ),
        None => println!(r#"{{"running":false,"port":{port}}}"#, port = port),
    }
}

fn cli_stop(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: syntra stop [--addr host:port | --port N]");
        eprintln!("Sends SIGTERM to the process listening on the configured port.");
        eprintln!("Defaults to port 8787 if --addr/--port are not given.");
        eprintln!("Does not verify the process is actually syntra — use with care if");
        eprintln!("the port could be held by something else.");
        return;
    }
    let port = parse_port(args);
    let Some(pid) = find_pid_on_port(&port) else {
        println!(r#"{{"stopped":false,"port":{port},"reason":"no listener"}}"#, port = port);
        return;
    };
    let out = std::process::Command::new("kill")
        .arg(pid.to_string())
        .output();
    match out {
        Ok(o) if o.status.success() => println!(
            r#"{{"stopped":true,"port":{port},"pid":{pid},"signal":"TERM"}}"#,
            port = port,
            pid = pid,
        ),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            eprintln!(
                r#"{{"stopped":false,"port":{port},"pid":{pid},"reason":"kill failed: {err}"}}"#,
                port = port,
                pid = pid,
                err = err.trim().replace('"', "'"),
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!(
                r#"{{"stopped":false,"port":{port},"pid":{pid},"reason":"could not spawn kill: {e}"}}"#,
                port = port,
                pid = pid,
                e = e.to_string().replace('"', "'"),
            );
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Syntra — adaptive capsule appliance");
    eprintln!();
    eprintln!("Usage:");
    eprintln!(
        "  syntra serve [--addr 127.0.0.1:8787] [--store ./syntra-store] [--admin-key <key>]"
    );
    eprintln!("  syntra serve --dev-mode");
    eprintln!("  syntra author <spec.yaml> --out <capsule.lyc> [--source-out <capsule.lycs>]");
    eprintln!("  syntra author <spec.yaml> --out-dir <dir>");
    eprintln!("  syntra status [--addr host:port | --port N]");
    eprintln!("    Report whether a server is listening on the configured port.");
    eprintln!("  syntra stop [--addr host:port | --port N]");
    eprintln!("    Send SIGTERM to the process listening on the configured port.");
    eprintln!("  syntra simulate <spec.yaml>");
    eprintln!("    [--rounds N] [--seed S | --seeds K] [--noise-std S] [--trace-every K]");
    eprintln!("    [--true-arm-rewards \"r1,r2,...\" | --traffic <traffic.yaml>]");
    eprintln!("    [--format json|table|plot] [--compare-vw]");
    eprintln!();
    eprintln!("For language commands (compile, run, decide, feedback, evolve),");
    eprintln!("use the Lycan language CLI.");
}

fn cli_simulate(args: &[String]) {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage:");
        eprintln!(
            "  syntra simulate <spec.yaml>"
        );
        eprintln!(
            "    [--rounds N] [--seed S | --seeds K] [--noise-std S] [--trace-every K]"
        );
        eprintln!(
            "    [--true-arm-rewards \"r1,r2,...\" | --traffic <traffic.yaml>]"
        );
        eprintln!(
            "    [--format json|table|plot] [--compare-vw]"
        );
        eprintln!();
        eprintln!(
            "Runs the resolved bandit algorithm against synthetic traffic locally."
        );
        eprintln!(
            "Reports cumulative regret (mean/std across seeds), per-context"
        );
        eprintln!(
            "convergence, refusal rate, and meta-bandit candidate selection."
        );
        return;
    }

    let mut spec_path: Option<String> = None;
    let mut traffic_path: Option<String> = None;
    let mut rounds: usize = 2000;
    let mut base_seed: u64 = 42;
    let mut seeds_count: Option<usize> = None;
    let mut arms: Option<Vec<f64>> = None;
    let mut noise_std: f64 = 0.05;
    let mut trace_every: usize = 0;
    let mut format: String = "json".to_string();
    let mut compare_vw = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--rounds" => {
                i += 1;
                rounds = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(rounds);
            }
            "--seed" => {
                i += 1;
                base_seed = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(base_seed);
            }
            "--seeds" => {
                i += 1;
                seeds_count = args.get(i).and_then(|s| s.parse().ok());
            }
            "--noise-std" => {
                i += 1;
                noise_std = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(noise_std);
            }
            "--trace-every" => {
                i += 1;
                trace_every = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(trace_every);
            }
            "--true-arm-rewards" => {
                i += 1;
                arms = args.get(i).map(|s| {
                    s.split(',')
                        .filter_map(|t| t.trim().parse::<f64>().ok())
                        .collect()
                });
            }
            "--traffic" => {
                i += 1;
                traffic_path = args.get(i).cloned();
            }
            "--format" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    format = v.clone();
                }
            }
            "--compare-vw" => {
                compare_vw = true;
            }
            value if value.starts_with("--") => {
                eprintln!("unknown simulate option: {value}");
                std::process::exit(2);
            }
            value => {
                if spec_path.is_some() {
                    eprintln!("unexpected extra argument: {value}");
                    std::process::exit(2);
                }
                spec_path = Some(value.to_string());
            }
        }
        i += 1;
    }

    let Some(spec_path) = spec_path else {
        eprintln!("simulate requires <spec.yaml>");
        std::process::exit(2);
    };
    if arms.is_none() && traffic_path.is_none() {
        eprintln!(
            "simulate requires either --true-arm-rewards \"r1,r2,...\" or --traffic <traffic.yaml>"
        );
        std::process::exit(2);
    }
    if !matches!(format.as_str(), "json" | "table" | "plot") {
        eprintln!("unknown --format value: {format} (expected json|table|plot)");
        std::process::exit(2);
    }

    let yaml = match std::fs::read_to_string(&spec_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {spec_path}: {e}");
            std::process::exit(1);
        }
    };
    let spec = match capsule_spec::CapsuleSpec::from_yaml(&yaml) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Build TrafficSpec from either --traffic or --true-arm-rewards.
    let traffic: simulate::TrafficSpec = if let Some(p) = traffic_path {
        let t_yaml = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot read {p}: {e}");
                std::process::exit(1);
            }
        };
        match simulate::TrafficSpec::from_yaml(&t_yaml) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    } else {
        let arms = arms.expect("checked above");
        simulate::TrafficSpec {
            arms,
            noise_std,
            regime_shifts: vec![],
            context_distribution: None,
            feature_distribution: None,
        }
    };

    let seeds: Vec<u64> = match seeds_count {
        Some(k) if k > 0 => (0..k as u64).map(|i| base_seed.wrapping_add(i)).collect(),
        _ => vec![base_seed],
    };
    let opts = simulate::ExtSimOptions {
        rounds,
        seeds,
        trace_every,
        compare_vw,
    };
    match simulate::run_traffic(&spec, &traffic, &opts) {
        Ok(report) => match format.as_str() {
            "json" => println!("{}", simulate::render_json(&report)),
            "table" => print!("{}", simulate::render_table(&report)),
            "plot" => {
                // Plot mode: ASCII sparkline to stderr, JSON summary to stdout.
                eprintln!("{}", simulate::render_sparkline(&report, 60));
                println!("{}", simulate::render_json(&report));
            }
            _ => unreachable!(),
        },
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn cli_author(args: &[String]) {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage:");
        eprintln!("  syntra author <spec.yaml> --out <capsule.lyc> [--source-out <capsule.lycs>]");
        eprintln!("  syntra author <spec.yaml> --out-dir <capsule-dir/>");
        eprintln!();
        eprintln!(
            "The typed schema (with reward.type: bernoulli|continuous|sparse_continuous) emits"
        );
        eprintln!(
            "a directory containing program.lyc, learning.json, reward_spec.json, manifest.json."
        );
        return;
    }

    let mut spec_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut source_out: Option<String> = None;
    let mut out_dir: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out_path = args.get(i).cloned();
            }
            "--source-out" => {
                i += 1;
                source_out = args.get(i).cloned();
            }
            "--out-dir" => {
                i += 1;
                out_dir = args.get(i).cloned();
            }
            value if value.starts_with("--") => {
                eprintln!("unknown author option: {value}");
                std::process::exit(2);
            }
            value => {
                if spec_path.is_some() {
                    eprintln!("unexpected extra argument: {value}");
                    std::process::exit(2);
                }
                spec_path = Some(value.to_string());
            }
        }
        i += 1;
    }

    let Some(spec_path) = spec_path else {
        eprintln!("author requires <spec.yaml>");
        std::process::exit(2);
    };

    if let Some(dir) = out_dir {
        let yaml = match std::fs::read_to_string(&spec_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot read {spec_path}: {e}");
                std::process::exit(1);
            }
        };
        let spec = match capsule_spec::CapsuleSpec::from_yaml(&yaml) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        match capsule_compiler::compile_to_dir(&spec, std::path::Path::new(&dir)) {
            Ok(r) => {
                let body = serde_json::json!({
                    "ok": true,
                    "source": spec_path,
                    "outDir": r.out_dir.to_string_lossy().to_string(),
                    "bytes": r.lyc_bytes,
                    "nodes": r.nodes,
                    "edges": r.edges,
                    "options": r.options,
                });
                println!("{body}");
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let out_path = out_path.unwrap_or_else(|| default_lyc_path(&spec_path));

    let source_out_path = source_out.as_deref().map(std::path::Path::new);
    match authoring::author_yaml_file(
        std::path::Path::new(&spec_path),
        std::path::Path::new(&out_path),
        source_out_path,
    ) {
        Ok(result) => {
            let mut body = serde_json::json!({
                "ok": true,
                "source": spec_path,
                "lyc": result.lyc_path.to_string_lossy().to_string(),
                "bytes": result.bytes,
                "nodes": result.nodes,
                "edges": result.edges,
                "options": result.options,
            });
            if let Some(path) = result.source_path {
                body["lycs"] = serde_json::json!(path.to_string_lossy().to_string());
            }
            println!("{}", body);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn default_lyc_path(spec_path: &str) -> String {
    let path = std::path::Path::new(spec_path);
    path.with_extension("lyc").to_string_lossy().to_string()
}

fn cli_serve(args: &[String]) {
    let mut addr = "127.0.0.1:8787".to_string();
    let mut store_path = "./lycan-store".to_string();
    let mut admin_key: Option<String> = std::env::var("LYCAN_ADMIN_KEY").ok();
    let mut dev_mode = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    addr = v.clone();
                }
            }
            "--store" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    store_path = v.clone();
                }
            }
            "--admin-key" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    admin_key = Some(v.clone());
                }
            }
            "--dev-mode" => {
                dev_mode = true;
            }
            _ => {}
        }
        i += 1;
    }

    if admin_key.is_none() && !dev_mode {
        eprintln!("ERROR: no admin key set. Set LYCAN_ADMIN_KEY or use --admin-key.");
        eprintln!("  For unauthenticated development, use --dev-mode (binds localhost only).");
        std::process::exit(1);
    }

    if dev_mode && admin_key.is_none() {
        eprintln!("WARNING: running in dev mode — all routes unauthenticated");
        if !addr.starts_with("127.0.0.1") && !addr.starts_with("localhost") {
            eprintln!("WARNING: dev mode on non-loopback address {addr} — this is unsafe");
        }
    }

    lycan::server::run_server(lycan::server::ServerConfig {
        addr,
        store_path,
        admin_key,
        service_name: Some("Syntra".to_string()),
    });
}
