//! Syntra: a self-hosted decision service.
//!
//! One crate: the Lycan language core (lexer, parser, graph compiler,
//! verifier, graph executor, sandboxed capabilities) and the Syntra service
//! (decision core, event store, HTTP server, operator CLI).

// The language core refers to itself as `lycan::` in a few places.
extern crate self as lycan;

// ── Lycan language core ──
pub mod ast;
pub mod binary;
pub mod capabilities;
pub mod capsule;
pub mod context;
pub mod error;
pub mod graph;
pub mod graph_compiler;
pub mod graph_executor;
pub mod lexer;
pub mod parser;
pub mod token;
pub mod value;
pub mod verifier;

// ── Syntra service ──
pub mod auth_tokens;
pub mod backup;
pub mod client;
pub mod decision;
mod demo;
mod doctor;
pub mod eventstore;
pub mod import;
pub mod ope;
pub mod rate_limit;
pub mod server;
pub mod store;

pub fn run() {
    let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
    let handler = builder.spawn(main_inner).unwrap();
    handler.join().unwrap();
}

fn main_inner() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1] == "serve" {
        serve_from_args(&args[2..], "Syntra");
        return;
    }

    if args.len() >= 2 {
        match args[1].as_str() {
            "--help" | "-h" => {
                print_usage();
                return;
            }
            "--version" | "-V" | "version" => {
                println!("syntra {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "health" => {
                cli_health(&args[2..]);
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
            "evaluate" => {
                let code = ope::cli::run(&args[2..]);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "demo" => {
                let code = demo::cli(&args[2..]);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            "import" => {
                let code = import::cli(&args[2..]);
                if code != 0 {
                    std::process::exit(code);
                }
                return;
            }
            // A typo (or a v1 command such as `migrate` or `author`) must
            // not look like success to a script.
            other => {
                eprintln!("syntra: unknown command {other:?}");
                eprintln!();
                print_usage();
                std::process::exit(2);
            }
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
                if let Some(a) = args.get(i + 1)
                    && let Some(p) = a.rsplit(':').next()
                    && p.parse::<u16>().is_ok()
                {
                    return p.to_string();
                }
            }
            "--port" => {
                if let Some(a) = args.get(i + 1)
                    && a.parse::<u16>().is_ok()
                {
                    return a.clone();
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

/// `syntra health [--addr host:port]`: ask a running server.
fn cli_health(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: syntra health [--addr host:port | --port N]");
        eprintln!("Queries GET /health on a running server (default 127.0.0.1:8787);");
        eprintln!("exits 1 when it does not answer.");
        return;
    }
    let port = parse_port(args);
    let host = args
        .iter()
        .position(|a| a == "--addr")
        .and_then(|i| args.get(i + 1))
        .and_then(|a| a.rsplit_once(':').map(|(h, _)| h.to_string()))
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let url = format!("http://{host}:{port}/health");
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(3))
        .build();
    match agent.get(&url).call().map(|r| r.into_string()) {
        Ok(Ok(body)) => println!("{}", body.trim()),
        Ok(Err(e)) => {
            eprintln!(r#"{{"ok":false,"url":"{url}","error":"{e}"}}"#);
            std::process::exit(1);
        }
        Err(e) => {
            let e = e.to_string().replace('"', "'");
            eprintln!(r#"{{"ok":false,"url":"{url}","error":"{e}"}}"#);
            std::process::exit(1);
        }
    }
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
        eprintln!("Sends SIGTERM to the syntra process listening on the configured port");
        eprintln!("(refuses if the listener is another program). Defaults to port 8787.");
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
    // Only ever signal a syntra (or lycan) server.
    let command = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let name = std::path::Path::new(&command)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name != "syntra" && name != "lycan" {
        eprintln!(
            r#"{{"stopped":false,"port":{port},"pid":{pid},"reason":"the listener is {name:?}, not syntra"}}"#
        );
        std::process::exit(1);
    }
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

fn print_serve_usage() {
    eprintln!(
        "  syntra serve [--addr 127.0.0.1:8787] [--store ./syntra-store] [--admin-key <key>]"
    );
    eprintln!("    The admin key can also come from SYNTRA_ADMIN_KEY (or LYCAN_ADMIN_KEY),");
    eprintln!("    which keeps it out of the process list.");
    eprintln!("    --metrics-public (or SYNTRA_METRICS_PUBLIC=1) serves /metrics without");
    eprintln!("    a credential; otherwise it needs an admin credential.");
    eprintln!("    --specs <dir> (or SYNTRA_SPECS_DIR) applies capsule specs from files");
    eprintln!("    at startup (YAML/JSON documents with tenant, job, capsule, spec).");
    eprintln!("    OTEL_EXPORTER_OTLP_ENDPOINT turns on OpenTelemetry tracing.");
    eprintln!("  syntra serve --dev-mode           Unauthenticated, loopback only");
}

fn print_usage() {
    eprintln!("Syntra: learned decisions in microseconds, with the evidence to trust them");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  syntra --version");
    print_serve_usage();
    eprintln!(
        "  syntra demo [--addr host:port]    A server with simulated traffic to watch it learn"
    );
    eprintln!("  syntra health [--addr host:port]  Ask a running server whether it is up");
    eprintln!("  syntra status [--addr host:port | --port N]");
    eprintln!("  syntra stop [--addr host:port | --port N]");
    eprintln!("  syntra doctor --store <root> [--json]");
    eprintln!("    Read-only store check: files, specs, policies, programs, event store.");
    eprintln!("  syntra backup --store <root> --out <dir>");
    eprintln!("    Consistent copy of the store, including an online SQLite backup.");
    eprintln!("  syntra restore --from <dir> --into <root> [--force]");
    eprintln!("    Install a backup; refuses a root that looks live unless --force.");
    eprintln!("  syntra import dsjson --store <root> --capsule t/j/c [--learn] <file>");
    eprintln!("    Import Azure Personalizer / Vowpal Wabbit DSJSON logs for evaluation.");
    eprintln!("  syntra evaluate --input rows.jsonl --policy <policy> [options]");
    eprintln!("    Off-policy evaluation (IPS, SNIPS, DR) with confidence intervals and");
    eprintln!("    promotion gates; see `syntra evaluate --help`.");
    eprintln!();
    eprintln!("Language tools (compile, run, inspect): the `lycan` binary.");
}

/// True when `addr` ("host:port") binds only the local machine.
fn is_loopback_addr(addr: &str) -> bool {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// `serve` subcommand shared by the `syntra` and `lycan` binaries.
pub fn serve_from_args(args: &[String], service_name: &str) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_serve_usage();
        return;
    }
    let mut addr = "127.0.0.1:8787".to_string();
    let mut store_path = "./syntra-store".to_string();
    // SYNTRA_ADMIN_KEY, or the older LYCAN_ADMIN_KEY (deployments use both).
    let mut admin_key: Option<String> = std::env::var("SYNTRA_ADMIN_KEY")
        .or_else(|_| std::env::var("LYCAN_ADMIN_KEY"))
        .ok()
        .filter(|k| !k.is_empty());
    let mut dev_mode = false;
    let mut dev_mode_allow_remote = false;
    let mut metrics_public = matches!(
        std::env::var("SYNTRA_METRICS_PUBLIC").as_deref(),
        Ok("1" | "true" | "yes")
    );
    let mut specs_dir = std::env::var("SYNTRA_SPECS_DIR")
        .ok()
        .filter(|d| !d.is_empty());

    let usage_error = |msg: String| -> ! {
        eprintln!("error: {msg}");
        eprintln!("Run `syntra --help` for usage.");
        std::process::exit(2);
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = || -> String {
            i += 1;
            args.get(i)
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .unwrap_or_else(|| usage_error(format!("{flag} needs a value")))
        };
        match flag {
            "--addr" => addr = value(),
            "--store" => store_path = value(),
            "--admin-key" => admin_key = Some(value()),
            "--dev-mode" => dev_mode = true,
            "--dev-mode-allow-remote" => dev_mode_allow_remote = true,
            "--metrics-public" => metrics_public = true,
            "--specs" => specs_dir = Some(value()),
            other => usage_error(format!("unknown serve option {other:?}")),
        }
        i += 1;
    }

    if admin_key.is_none() && !dev_mode {
        eprintln!("ERROR: no admin key set. Set SYNTRA_ADMIN_KEY or use --admin-key.");
        eprintln!("  For unauthenticated development, use --dev-mode (binds localhost only).");
        std::process::exit(1);
    }

    if dev_mode && admin_key.is_none() {
        eprintln!("WARNING: running in dev mode — all routes unauthenticated");
        if !is_loopback_addr(&addr) {
            if !dev_mode_allow_remote {
                eprintln!(
                    "ERROR: --dev-mode serves every route without authentication, so it only binds a loopback address (got {addr})."
                );
                eprintln!(
                    "  Use --addr 127.0.0.1:<port>, set an admin key, or pass --dev-mode-allow-remote inside an isolated container."
                );
                std::process::exit(1);
            }
            eprintln!(
                "WARNING: dev mode on non-loopback address {addr} (--dev-mode-allow-remote) — anyone who can reach it has full access"
            );
        }
    }

    lycan::server::run_server(lycan::server::ServerConfig {
        addr,
        store_path,
        admin_key,
        service_name: Some(service_name.to_string()),
        metrics_public,
        specs_dir,
        otel: None,
    });
}
