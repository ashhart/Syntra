//! Syntra HTTP server.
//!
//! - `http`: framework-independent request/response types.
//! - `serve`: hyper/tokio adapter with timeouts and graceful shutdown.
//! - `routes`: dispatch, auth, scopes, metrics labels.
//! - `decide`, `reward`: the hot path.
//! - `runtime`: in-memory capsule runtimes (engine, feature program, policy).
//! - `writer`: write-behind decision log.
//! - `capsules`, `query`: management and read routes.
//! - `otel`: OpenTelemetry spans, exported over OTLP/HTTP.

mod admin;
mod auth;
pub mod capsules;
pub mod decide;
pub mod evaluate;
pub mod http;
mod metrics;
pub mod otel;
pub mod personalizer;
pub mod query;
pub mod reward;
mod routes;
pub mod runtime;
mod serve;
pub mod specs;
pub mod state;
pub mod sweeper;
pub mod upload;
pub mod writer;

use std::sync::{Arc, Mutex};

use tracing::{error, info, warn};

use crate::auth_tokens::TokenStore;
use crate::eventstore::{EventStore, SqliteStore};
use crate::rate_limit::RateLimiter;
use crate::store::Store;

use self::state::{CapsuleLocks, SharedState, State};

#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    pub addr: String,
    pub store_path: String,
    pub admin_key: Option<String>,
    pub service_name: Option<String>,
    /// Serve `/metrics` without a credential (it names every tenant, job
    /// and capsule); otherwise it needs an admin credential.
    pub metrics_public: bool,
    /// Apply the spec files in this directory at startup (see `specs.rs`).
    pub specs_dir: Option<String>,
    /// Export a span per request; `run_server` reads it from the `OTEL_*`
    /// environment when this is `None`.
    pub otel: Option<otel::OtelConfig>,
}

/// Rate limiter config, overridable via `SYNTRA_RATE_LIMIT_RPS` and
/// `SYNTRA_RATE_LIMIT_BURST`. A value that does not parse keeps the default;
/// a typo can never silently remove the limiter.
fn rate_limit_config_from_env() -> crate::rate_limit::RateLimitConfig {
    let mut cfg = crate::rate_limit::RateLimitConfig::default();
    for (var, slot) in [
        ("SYNTRA_RATE_LIMIT_RPS", &mut cfg.rate_per_second),
        ("SYNTRA_RATE_LIMIT_BURST", &mut cfg.burst),
    ] {
        if let Ok(v) = std::env::var(var) {
            match v.parse::<f64>() {
                Ok(n) if n > 0.0 => *slot = n,
                _ => warn!(var, value = %v, "not a positive number; keeping the default"),
            }
        }
    }
    cfg
}

/// Open the store and event log and build the shared state. Also used by
/// tests and embedders that drive [`handle`] directly.
pub fn build_state(config: &ServerConfig) -> Result<State, String> {
    let store = Store::open_or_init(&config.store_path)?;
    let events: Arc<dyn EventStore> = Arc::new(
        SqliteStore::open(store.events_path()).map_err(|e| format!("opening event store: {e}"))?,
    );
    let tokens = TokenStore::load_or_init(store.root_path());
    Ok(Arc::new(SharedState {
        writer: writer::DecisionWriter::start(events.clone()),
        events,
        store,
        runtimes: Default::default(),
        admin_key: config.admin_key.clone(),
        service_name: config
            .service_name
            .clone()
            .unwrap_or_else(|| "Syntra".to_string()),
        tokens: Mutex::new(tokens),
        rate_limiter: RateLimiter::new(rate_limit_config_from_env()),
        metrics: Default::default(),
        locks: CapsuleLocks::default(),
        started_at: std::time::Instant::now(),
        metrics_public: config.metrics_public,
        background_snapshots: std::sync::atomic::AtomicBool::new(false),
        otel: config.otel.clone().map(otel::Tracer::start),
    }))
}

/// Route one request against a state built by [`build_state`], without a
/// network listener.
pub fn handle(state: &State, request: &http::Request) -> http::Response {
    routes::route(request, state)
}

pub fn run_server(mut config: ServerConfig) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .json()
        .try_init();
    if config.otel.is_none() {
        let (otel, warnings) = otel::OtelConfig::from_env();
        for warning in warnings {
            warn!("OpenTelemetry: {warning}");
        }
        config.otel = otel;
    }
    if let Some(o) = &config.otel {
        info!(
            endpoint = %o.endpoint_for_logs(),
            sampler = ?o.sampler,
            gzip = o.gzip,
            "exporting OpenTelemetry spans"
        );
    }

    // One server per store: held until this function returns (the OS
    // releases it if the process dies first).
    let _store_lock = crate::store::lock_root(std::path::Path::new(&config.store_path))
        .unwrap_or_else(|e| {
            error!(error = %e, store = %config.store_path, "cannot start");
            eprintln!("error: {e}");
            std::process::exit(1);
        });
    let state = build_state(&config).unwrap_or_else(|e| {
        error!(error = %e, store = %config.store_path, "cannot start");
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    if let Some(dir) = &config.specs_dir {
        match specs::apply_dir(&state, std::path::Path::new(dir)) {
            Ok(applied) => info!(
                dir = %dir,
                files = applied.files,
                applied = ?applied.applied,
                unchanged = applied.unchanged.len(),
                "specs applied"
            ),
            Err(e) => {
                error!(error = %e, "cannot apply --specs");
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    if state.admin_key.is_none() {
        warn!("no admin key set; every route is unauthenticated (dev mode)");
    }
    // Marks the store as in use; `syntra restore` refuses a live root.
    let pid_file = state.store.root_path().join("server.pid");
    if let Err(e) = std::fs::write(&pid_file, std::process::id().to_string()) {
        warn!(error = %e, "could not write server.pid");
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .thread_name("syntra-http")
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error: cannot start the async runtime: {e}");
            std::process::exit(1);
        });

    let sweeper = sweeper::Sweeper::start(state.clone());
    let serve_state = state.clone();
    let addr = config.addr.clone();
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                let port = addr.rsplit(':').next().unwrap_or("");
                eprintln!("error: cannot bind to {addr}: port already in use");
                eprintln!("  find the process holding it with: lsof -i :{port}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("error: cannot bind to {addr}: {e}");
                std::process::exit(1);
            }
        };
        info!(
            addr = %addr,
            store = %config.store_path,
            workers,
            service = %serve_state.service_name,
            "syntra server listening"
        );
        serve::serve(listener, serve_state).await;
    });
    sweeper.stop();
    info!("flushing the decision log and saving models");
    state.shutdown();
    let _ = std::fs::remove_file(&pid_file);
    info!("stopped");
}
