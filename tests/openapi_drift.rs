//! OpenAPI drift guard.
//!
//! Guards the contract between `docs/openapi.yaml` and the actual dispatch
//! in `src/server/routes.rs`:
//!
//! 1. Every path+method documented in the spec must exist in the dispatch.
//!    A probe against a dev-mode server must not return the unknown-route
//!    sentinel `{"error":"not_found"}` — resource-level 404s carry an
//!    error message and are distinct.
//! 2. Every route that exists in the dispatch but is intentionally NOT
//!    documented must be listed in [`UNDOCUMENTED_ROUTES`] and must also
//!    exist (catches silent removal of legacy/admin routes).
//!
//! Known limitation: a brand-new undocumented route is not discovered
//! automatically. When adding a route, either document it in the spec or
//! add it to [`UNDOCUMENTED_ROUTES`] — both force a conscious docs decision.

use std::io::Read as _;
use std::sync::OnceLock;
use std::time::Duration;

/// Routes present in the dispatch but intentionally absent from
/// `docs/openapi.yaml`. Keep in sync with `src/server/routes.rs`.
const UNDOCUMENTED_ROUTES: &[(&str, &str)] = &[
    // Admin console HTML shell (browser entry point, not a data API).
    ("GET", "/admin"),
    // Deterministic-RNG admin endpoint (operational tooling).
    ("POST", "/admin/rng/seed"),
    // Admin console data helper.
    ("GET", "/admin/capsules"),
    // Tenant-level capsule listing (no job scoping).
    ("GET", "/tenants/{tenant}/capsules"),
    // Hierarchical capsule spec sidecar.
    (
        "GET",
        "/tenants/{tenant}/jobs/{job}/capsules/{capsule}/hierarchical_spec",
    ),
    (
        "PUT",
        "/tenants/{tenant}/jobs/{job}/capsules/{capsule}/hierarchical_spec",
    ),
    // Chaos engineering probe.
    ("GET", "/tenants/{tenant}/jobs/{job}/capsules/{capsule}/chaos"),
    // Batched feedback.
    (
        "POST",
        "/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback/batch",
    ),
    // Legacy capsule prefix — the spec documents the server-side rewrite
    // to `job = "default"` in prose; each legacy route mirrors a
    // documented jobs-prefix route.
    ("POST", "/tenants/{tenant}/capsules/{capsule}/install"),
    ("POST", "/tenants/{tenant}/capsules/{capsule}/decide"),
    ("POST", "/tenants/{tenant}/capsules/{capsule}/feedback"),
    ("POST", "/tenants/{tenant}/capsules/{capsule}/evolve"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/report"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/decisions"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/audits"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/evolution"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/snapshots"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/policy"),
    ("PUT", "/tenants/{tenant}/capsules/{capsule}/policy"),
    ("GET", "/tenants/{tenant}/capsules/{capsule}/inspect"),
    ("DELETE", "/tenants/{tenant}/capsules/{capsule}"),
    ("DELETE", "/tenants/{tenant}/capsules/{capsule}/logs"),
];

/// Unknown-route sentinel returned by the dispatch fallback arm in
/// `src/server/routes.rs`. Resource-level 404s use `err_json` with a
/// message and never match this exact body.
const NOT_FOUND_SENTINEL: &str = r#"{"error":"not_found"}"#;

fn not_found_sentinel(status: u16, body: &str) -> bool {
    status == 404 && body.trim() == NOT_FOUND_SENTINEL
}

/// Replace `{param}` segments with a dummy value.
fn substitute(template: &str) -> String {
    template
        .split('/')
        .map(|seg| if seg.starts_with('{') && seg.ends_with('}') { "probe1" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

fn probe(base: &str, method: &str, template: &str) -> Result<(u16, String), String> {
    let url = format!("{}{}", base, substitute(template));
    let req = ureq::request(method, &url).timeout(Duration::from_secs(15));
    let resp = match method {
        "GET" | "HEAD" => req.call(),
        _ => req.send_string(""),
    };
    let (status, reader) = match resp {
        Ok(r) => (r.status(), r.into_reader()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_reader()),
        Err(e) => return Err(format!("{method} {template}: transport error: {e}")),
    };
    let mut body = String::new();
    let _ = reader.take(1 << 20).read_to_string(&mut body);
    Ok((status, body))
}

fn parse_spec_routes() -> Vec<(String, String)> {
    let text = std::fs::read_to_string("docs/openapi.yaml").expect("read docs/openapi.yaml");
    let yaml: serde_yml::Value = serde_yml::from_str(&text).expect("parse docs/openapi.yaml");
    let paths = yaml
        .get("paths")
        .and_then(|p| p.as_mapping())
        .expect("openapi.yaml has a `paths` mapping");
    let methods = ["get", "post", "put", "delete", "head", "options", "patch"];
    let mut out = Vec::new();
    for (key, item) in paths {
        let template = key.as_str().expect("path key is a string").to_string();
        for m in methods {
            if item.get(m).is_some() {
                out.push((m.to_uppercase(), template.clone()));
            }
        }
    }
    assert!(!out.is_empty(), "no routes parsed from openapi.yaml");
    out
}

/// Start one shared dev-mode server (no admin key => every request gets
/// Admin scope) against a throwaway store.
fn base_url() -> String {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        let port = 19000 + (std::process::id() % 900);
        let addr = format!("127.0.0.1:{port}");
        let store =
            std::env::temp_dir().join(format!("syntra-openapi-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        std::fs::create_dir_all(&store).unwrap();
        let cfg = syntra::server::ServerConfig {
            addr: addr.clone(),
            store_path: store.to_string_lossy().into_owned(),
            admin_key: None,
            service_name: None,
        };
        std::thread::spawn(move || syntra::server::run_server(cfg));
        let base = format!("http://{addr}");
        for _ in 0..100 {
            if let Ok((200, _)) = probe(&base, "GET", "/health") {
                return base;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("dev server did not become healthy on {addr}");
    }).clone()
}

#[test]
fn every_documented_route_exists_in_dispatch() {
    let base = base_url();
    for (method, template) in parse_spec_routes() {
        let (status, body) =
            probe(&base, &method, &template).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !not_found_sentinel(status, &body),
            "route documented in openapi.yaml but missing from dispatch: \
             {method} {template} (got {status} {body})"
        );
    }
}

#[test]
fn every_undocumented_route_is_listed_and_exists() {
    let base = base_url();
    for (method, template) in UNDOCUMENTED_ROUTES {
        let (status, body) =
            probe(&base, method, template).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !not_found_sentinel(status, &body),
            "allowlisted route missing from dispatch: \
             {method} {template} (got {status} {body})"
        );
    }
}
