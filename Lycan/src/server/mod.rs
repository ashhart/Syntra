/// Lycan HTTP server — `lycan serve`
///
/// Concurrent request handling with per tenant/job/capsule locking.
/// Read-only routes never block. Mutation routes serialize per runtime scope.

mod admin;
mod auth;
mod decide;
mod errors;
mod feedback;
mod helpers;
mod inspect;
mod metrics;
mod routes;
mod state;

#[allow(unused_imports)]
pub(crate) use self::helpers::{primary_choice_node, all_choice_nodes};

use std::sync::{Arc, Mutex};

use crate::store::LycanStore;
use crate::auth_tokens::TokenStore;
use crate::rate_limit::{RateLimiter, RateLimitConfig};
use tracing::{error, info, warn};

use self::metrics::Metrics;
use self::routes::route;
use self::state::{CapsuleLockManager, SharedState, State};

pub struct ServerConfig {
    pub addr: String,
    pub store_path: String,
    pub admin_key: Option<String>,
    pub service_name: Option<String>,
}

const WORKER_THREADS: usize = 8;

pub fn run_server(config: ServerConfig) {
    // 1C: structured logging via the tracing crate. JSON output to stderr by
    // default, level controlled by RUST_LOG (sensible default `info` so
    // operators get startup + auth-failure + drift events without DEBUG
    // chatter on every request). `try_init` is used so re-entrant test runs
    // that pre-initialised a global subscriber don't double-set.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .json()
        .try_init();

    let store = LycanStore::open_or_init(&config.store_path)
        .unwrap_or_else(|e| {
            error!(error = %e, store_path = %config.store_path, "cannot open store");
            std::process::exit(1);
        });

    let tokens = TokenStore::load_or_init(store.root_path());
    let state: State = Arc::new(SharedState {
        admin_key: config.admin_key,
        service_name: config.service_name.unwrap_or_else(|| "Lycan".to_string()),
        store,
        locks: CapsuleLockManager::new(),
        metrics: Metrics::new(),
        tokens: Mutex::new(tokens),
        rate_limiter: RateLimiter::new(RateLimitConfig::default()),
    });

    if state.admin_key.is_none() {
        warn!("no admin key set — all routes are unauthenticated (set LYCAN_ADMIN_KEY or use --admin-key)");
    }

    // Pre-bind probe: catch "port already in use" with an actionable message
    // before tiny_http swallows the io::ErrorKind. A stale syntra from a
    // previous run silently held :8787 during a benchmark regression and the
    // fresh binary appeared to start but never bound. Briefly racy — the
    // probe drops its listener before tiny_http binds, so a different process
    // could grab the port in that window — but the common case (a stale
    // syntra still holding the port) is caught reliably. No integration test:
    // process-management timing makes the two-server scenario fragile in CI.
    match std::net::TcpListener::bind(&config.addr) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let port = config.addr.rsplit(':').next().unwrap_or("");
            eprintln!("error: cannot bind to {} — port already in use", config.addr);
            eprintln!("  another process is holding this port. on macOS/Linux you can find it with:");
            eprintln!("    lsof -i :{port}");
            eprintln!("  to kill it:");
            eprintln!("    kill $(lsof -ti :{port})");
            std::process::exit(1);
        }
        Err(e) => {
            error!(error = %e, addr = %config.addr, "cannot bind listener");
            std::process::exit(1);
        }
    }

    let server = Arc::new(tiny_http::Server::http(&config.addr)
        .unwrap_or_else(|e| {
            error!(error = %e, addr = %config.addr, "cannot bind listener");
            std::process::exit(1);
        }));

    info!(
        addr = %config.addr,
        store = %config.store_path,
        workers = WORKER_THREADS,
        service = %state.service_name,
        "syntra server listening",
    );

    // Spawn worker threads
    let mut handles = Vec::new();
    for _ in 0..WORKER_THREADS {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        handles.push(std::thread::spawn(move || {
            loop {
                let mut request = match server.recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let resp = route(&mut request, &state);
                request.respond(resp).ok();
            }
        }));
    }

    for h in handles {
        h.join().ok();
    }
}
