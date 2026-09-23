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
mod doctor;
pub mod eventstore;
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
    eprintln!("Syntra: learned decisions in microseconds, with the evidence to trust them");
    eprintln!();
    eprintln!("Usage:");
    eprintln!(
        "  syntra serve [--addr 127.0.0.1:8787] [--store ./syntra-store] [--admin-key <key>]"
    );
    eprintln!("  syntra serve --dev-mode           Unauthenticated, loopback only");
    eprintln!("  syntra status [--addr host:port | --port N]");
    eprintln!("  syntra stop [--addr host:port | --port N]");
    eprintln!("  syntra doctor --store <root> [--json]");
    eprintln!("    Read-only store check: files, specs, policies, programs, event store.");
    eprintln!("  syntra backup --store <root> --out <dir>");
    eprintln!("    Consistent copy of the store, including an online SQLite backup.");
    eprintln!("  syntra restore --from <dir> --into <root> [--force]");
    eprintln!("    Install a backup; refuses a root that looks live unless --force.");
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
    let mut addr = "127.0.0.1:8787".to_string();
    let mut store_path = "./lycan-store".to_string();
    let mut admin_key: Option<String> = std::env::var("LYCAN_ADMIN_KEY").ok();
    let mut dev_mode = false;
    let mut dev_mode_allow_remote = false;

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
            "--dev-mode-allow-remote" => {
                dev_mode_allow_remote = true;
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
    });
}
