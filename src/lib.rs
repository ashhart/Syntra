/// Syntra — self-hosted adaptive decision runtime.
///
/// One crate: the Lycan language core (parser, compiler, graph runtime,
/// server, learning, capabilities) plus the Syntra appliance CLI wrapper.
mod authoring;
mod capsule_compiler;
mod capsule_spec;
mod doctor;
mod proof_lab;
mod replay;
mod simulate;

// — Lycan language/runtime core (merged from the formerly vendored Lycan crate) —
extern crate self as lycan;

pub mod agent;
pub mod ast;
pub mod auth_tokens;
pub mod backup;
pub mod binary;
pub mod capabilities;
pub mod capsule;
pub mod change_detection;
pub mod combinatorics;
pub mod conformal;
pub mod context;
pub mod environment;
pub mod error;
pub mod evolution_loop;
pub mod evolve;
pub mod feature_schema;
pub mod graph;
pub mod graph_compiler;
pub mod graph_executor;
pub mod hierarchical;
pub mod hierarchical_state;
pub mod interpreter;
pub mod lambert;
pub mod learning;
pub mod lexer;
pub mod linucb;
pub mod meta_bandit;
pub mod ood;
pub mod optimizer;
pub mod parser;
pub mod rate_limit;
pub mod reward_characterization;
pub mod server;
pub mod shared_state_strategy;
pub mod store;
pub mod token;
pub mod value;
pub mod verifier;
pub mod warmup;

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

    if args.len() >= 2 && args[1] == "replay" {
        cli_replay(&args[2..]);
        return;
    }

    if args.len() >= 2 && args[1] == "proof-lab" {
        cli_proof_lab(&args[2..]);
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
            "doctor" => {
                doctor::cli_doctor(&args[2..]);
                return;
            }
            "backup" => {
                backup::cli_backup(&args[2..]);
                return;
            }
            "restore" => {
                backup::cli_restore(&args[2..]);
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
        println!(
            r#"{{"stopped":false,"port":{port},"reason":"no listener"}}"#,
            port = port
        );
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
    eprintln!("  syntra doctor --store <root> [--json]");
    eprintln!("    Read-only store validator: JSONL findings + exit 0/1/2. Cleans nothing.");
    eprintln!("  syntra backup --store <root> --out <file.json>");
    eprintln!("    Serialize the store to one fsynced versioned JSON bundle (quiesce first).");
    eprintln!("  syntra restore --bundle <file.json> --into <root> [--force]");
    eprintln!("    Atomically install a bundle; refuses a live root unless --force.");
    eprintln!("  syntra simulate <spec.yaml>");
    eprintln!("    [--rounds N] [--seed S | --seeds K] [--noise-std S] [--trace-every K]");
    eprintln!("    [--true-arm-rewards \"r1,r2,...\" | --traffic <traffic.yaml>]");
    eprintln!("    [--format json|table|plot] [--compare-vw]");
    eprintln!("  syntra replay --events decisions.jsonl [--policy-json policy.json]");
    eprintln!("    [--gates promotion.yaml] [--format json|markdown] [--out report.md]");
    eprintln!("    [--fail-on-gate]");
    eprintln!("    Replay shadow/historical decisions and evaluate promotion gates.");
    eprintln!("  syntra proof-lab erdos190 [--k 3] [--max-n 9] [--node-limit 20000]");
    eprintln!("    [--format json|markdown] [--out report.md] [--lean-out proof.lean]");
    eprintln!("    Run the finite-search/proof-obligation lab for Erdos #190 (solved).");
    eprintln!("  syntra proof-lab erdos160 [--max-n 18] [--node-limit 200000]");
    eprintln!("    [--backend dfs|sat] [--max-colors N] [--format json|markdown]");
    eprintln!("    [--out report.md] [--lean-out proof.lean]");
    eprintln!("    Finite certificates for the OPEN Erdos #160 (no asymptotic claim).");
    eprintln!();
    eprintln!("For language commands (compile, run, decide, feedback, evolve),");
    eprintln!("use the Lycan language CLI.");
}

fn cli_proof_lab(args: &[String]) {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage:");
        eprintln!("  syntra proof-lab erdos190 [--k 3] [--max-n 9] [--node-limit 20000]");
        eprintln!("    [--format json|markdown] [--out report.md] [--lean-out proof.lean]");
        eprintln!("    SOLVED #190: least N forcing a monochromatic or rainbow k-term AP.");
        eprintln!("  syntra proof-lab erdos160 [--max-n 18] [--node-limit 200000]");
        eprintln!("    [--backend dfs|sat] [--max-colors N] [--format json|markdown]");
        eprintln!("    [--out report.md] [--lean-out proof.lean]");
        eprintln!("    OPEN #160: smallest k-colouring h(N) so every 4-term AP has >= 3");
        eprintln!("    distinct colours. Finite certificates only; the asymptotic estimate");
        eprintln!("    is open and is never claimed (k is fixed at 4, so there is no --k).");
        eprintln!();
        eprintln!("Runs a bounded finite search, conjecture miner, proof-obligation");
        eprintln!("generator, Lean skeleton export, combinatorics kernel report, and");
        eprintln!("proof arena record. The Lean output is a skeleton, not a checked proof.");
        return;
    }

    let problem = args[0].as_str();
    if problem != "erdos190" && problem != "erdos160" {
        eprintln!(
            "unknown proof-lab problem '{}'; supported: erdos190, erdos160",
            args[0]
        );
        std::process::exit(1);
    }

    let mut k = 3usize;
    let mut max_n = if problem == "erdos160" { 18 } else { 9 };
    let mut node_limit = if problem == "erdos160" {
        200_000
    } else {
        20_000
    };
    let mut backend = proof_lab::ProofBackend::Dfs;
    let mut max_colors: Option<usize> = None;
    let mut format = "json".to_string();
    let mut out_path: Option<String> = None;
    let mut lean_out_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--k" => {
                if problem == "erdos160" {
                    eprintln!("proof-lab erdos160 does not take --k (the AP length is fixed at 4)");
                    std::process::exit(1);
                }
                k = parse_usize_flag(args, &mut i, "--k");
            }
            "--max-n" => {
                max_n = parse_usize_flag(args, &mut i, "--max-n");
            }
            "--node-limit" => {
                node_limit = parse_usize_flag(args, &mut i, "--node-limit");
            }
            "--max-colors" => {
                max_colors = Some(parse_usize_flag(args, &mut i, "--max-colors"));
            }
            "--backend" => {
                let value = parse_string_flag(args, &mut i, "--backend");
                let Some(parsed) = proof_lab::ProofBackend::parse(&value) else {
                    eprintln!("unsupported proof-lab --backend '{value}'; expected dfs or sat");
                    std::process::exit(1);
                };
                backend = parsed;
            }
            "--format" => {
                format = parse_string_flag(args, &mut i, "--format");
            }
            "--out" => {
                out_path = Some(parse_string_flag(args, &mut i, "--out"));
            }
            "--lean-out" => {
                lean_out_path = Some(parse_string_flag(args, &mut i, "--lean-out"));
            }
            other => {
                eprintln!("unknown proof-lab argument: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if k < 2 || max_n < 1 || node_limit < 1 || max_colors == Some(0) {
        eprintln!(
            "proof-lab expects --k >= 2, --max-n >= 1, --max-colors >= 1, and --node-limit >= 1"
        );
        std::process::exit(1);
    }

    let report = if problem == "erdos160" {
        proof_lab::run_erdos160_with_backend(max_n, node_limit, backend, max_colors)
    } else {
        proof_lab::run_erdos190(k, max_n, node_limit)
    };
    let rendered = match format.as_str() {
        "json" => proof_lab::render_json(&report).expect("serialize proof-lab report"),
        "markdown" => proof_lab::render_markdown(&report),
        other => {
            eprintln!("unsupported proof-lab --format '{other}'; expected json or markdown");
            std::process::exit(1);
        }
    };

    if let Some(path) = lean_out_path {
        if let Err(err) = std::fs::write(&path, &report.lean_skeleton) {
            eprintln!("failed to write Lean skeleton {path}: {err}");
            std::process::exit(1);
        }
    }

    if let Some(path) = out_path {
        if let Err(err) = std::fs::write(&path, rendered) {
            eprintln!("failed to write proof-lab report {path}: {err}");
            std::process::exit(1);
        }
    } else {
        println!("{rendered}");
    }
}

fn parse_usize_flag(args: &[String], i: &mut usize, flag: &str) -> usize {
    *i += 1;
    let Some(value) = args.get(*i) else {
        eprintln!("{flag} expects a value");
        std::process::exit(1);
    };
    match value.parse::<usize>() {
        Ok(parsed) => parsed,
        Err(_) => {
            eprintln!("{flag} expects a positive integer, got '{value}'");
            std::process::exit(1);
        }
    }
}

fn parse_string_flag(args: &[String], i: &mut usize, flag: &str) -> String {
    *i += 1;
    let Some(value) = args.get(*i) else {
        eprintln!("{flag} expects a value");
        std::process::exit(1);
    };
    value.clone()
}

fn cli_replay(args: &[String]) {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage:");
        eprintln!("  syntra replay --events decisions.jsonl [--policy-json policy.json]");
        eprintln!("    [--gates promotion.yaml] [--format json|markdown] [--out report.md]");
        eprintln!("    [--fail-on-gate]");
        eprintln!();
        eprintln!("Replay historical or shadow-mode decision logs, compare a candidate");
        eprintln!("policy against the baseline action, and produce a promotion report.");
        eprintln!();
        eprintln!("JSONL event fields:");
        eprintln!("  contextKey, baselineAction, candidateAction, actionRewards");
        eprintln!("  optional: actionCostsUsd, actionLatencyMs, segment, oracleAction");
        return;
    }

    let mut events_path: Option<String> = None;
    let mut policy_path: Option<String> = None;
    let mut gates_path: Option<String> = None;
    let mut format = "json".to_string();
    let mut out_path: Option<String> = None;
    let mut fail_on_gate = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--events" => {
                i += 1;
                events_path = args.get(i).cloned();
            }
            "--policy-json" => {
                i += 1;
                policy_path = args.get(i).cloned();
            }
            "--gates" => {
                i += 1;
                gates_path = args.get(i).cloned();
            }
            "--format" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    format = v.clone();
                }
            }
            "--out" => {
                i += 1;
                out_path = args.get(i).cloned();
            }
            "--fail-on-gate" => {
                fail_on_gate = true;
            }
            value if value.starts_with("--") => {
                eprintln!("unknown replay option: {value}");
                std::process::exit(2);
            }
            value => {
                if events_path.is_some() {
                    eprintln!("unexpected extra argument: {value}");
                    std::process::exit(2);
                }
                events_path = Some(value.to_string());
            }
        }
        i += 1;
    }

    let Some(events_path) = events_path else {
        eprintln!("replay requires --events <decisions.jsonl>");
        std::process::exit(2);
    };
    if !matches!(format.as_str(), "json" | "markdown") {
        eprintln!("unknown --format value: {format} (expected json|markdown)");
        std::process::exit(2);
    }

    let policy = match policy_path.as_deref() {
        Some(path) => match replay::Policy::from_json_file(std::path::Path::new(path)) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        None => None,
    };
    let gates = match gates_path.as_deref() {
        Some(path) => match replay::PromotionGate::from_yaml_file(std::path::Path::new(path)) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        None => replay::PromotionGate::default(),
    };

    let report = match replay::run_file(std::path::Path::new(&events_path), policy.as_ref(), gates)
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let rendered = if format == "markdown" {
        replay::render_markdown(&report)
    } else {
        replay::render_json(&report)
    };

    if let Some(path) = out_path {
        if let Err(e) = std::fs::write(&path, rendered.as_bytes()) {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(1);
        }
    } else {
        println!("{rendered}");
    }

    if fail_on_gate && !report.promotion.pass {
        std::process::exit(1);
    }
}

fn cli_simulate(args: &[String]) {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage:");
        eprintln!("  syntra simulate <spec.yaml>");
        eprintln!("    [--rounds N] [--seed S | --seeds K] [--noise-std S] [--trace-every K]");
        eprintln!("    [--true-arm-rewards \"r1,r2,...\" | --traffic <traffic.yaml>]");
        eprintln!("    [--format json|table|plot] [--compare-vw]");
        eprintln!();
        eprintln!("Runs the resolved bandit algorithm against synthetic traffic locally.");
        eprintln!("Reports cumulative regret (mean/std across seeds), per-context");
        eprintln!("convergence, refusal rate, and meta-bandit candidate selection.");
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
    let mut baseline: Option<simulate::BaselineKind> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--rounds" => {
                i += 1;
                rounds = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(rounds);
            }
            "--seed" => {
                i += 1;
                base_seed = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(base_seed);
            }
            "--seeds" => {
                i += 1;
                seeds_count = args.get(i).and_then(|s| s.parse().ok());
            }
            "--noise-std" => {
                i += 1;
                noise_std = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(noise_std);
            }
            "--trace-every" => {
                i += 1;
                trace_every = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(trace_every);
            }
            "--true-arm-rewards" => {
                i += 1;
                let raw = match args.get(i) {
                    Some(v) => v.clone(),
                    None => {
                        eprintln!("--true-arm-rewards requires a comma-separated list of numbers");
                        std::process::exit(2);
                    }
                };
                // Fail closed on any non-numeric token: silently dropping it
                // would shrink the arm list and mask a spec/capsule mismatch.
                let mut parsed: Vec<f64> = Vec::new();
                for tok in raw.split(',') {
                    let t = tok.trim();
                    if t.is_empty() {
                        continue;
                    }
                    match t.parse::<f64>() {
                        Ok(v) if v.is_finite() => parsed.push(v),
                        _ => {
                            eprintln!(
                                "invalid --true-arm-rewards entry: \"{t}\" is not a finite number"
                            );
                            std::process::exit(2);
                        }
                    }
                }
                arms = Some(parsed);
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
            "--compare-baseline" => {
                i += 1;
                let raw = match args.get(i) {
                    Some(v) => v.clone(),
                    None => {
                        eprintln!(
                            "--compare-baseline requires random | first-arm | epsilon-greedy:N"
                        );
                        std::process::exit(2);
                    }
                };
                match simulate::BaselineKind::parse(&raw) {
                    Ok(k) => baseline = Some(k),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(2);
                    }
                }
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
    match simulate::run_traffic_with_baseline(&spec, &traffic, &opts, baseline) {
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
