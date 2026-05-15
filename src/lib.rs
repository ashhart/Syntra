/// Run the Syntra appliance CLI.
mod authoring;

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
            _ => {}
        }
    }

    print_usage();
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
    eprintln!();
    eprintln!("For language commands (compile, run, decide, feedback, evolve),");
    eprintln!("use the Lycan language CLI.");
}

fn cli_author(args: &[String]) {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage:");
        eprintln!("  syntra author <spec.yaml> --out <capsule.lyc> [--source-out <capsule.lycs>]");
        eprintln!();
        eprintln!(
            "Compiles a small YAML bandit spec into the .lyc capsule binary accepted by /install."
        );
        return;
    }

    let mut spec_path: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut source_out: Option<String> = None;

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
