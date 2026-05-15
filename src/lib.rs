/// Run the Syntra appliance CLI.
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
    eprintln!("  syntra serve [--addr 127.0.0.1:8787] [--store ./syntra-store] [--admin-key <key>]");
    eprintln!("  syntra serve --dev-mode");
    eprintln!();
    eprintln!("For language commands (compile, run, decide, feedback, evolve),");
    eprintln!("use the Lycan language CLI.");
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
