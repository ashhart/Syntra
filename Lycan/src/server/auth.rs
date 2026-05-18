use tracing::warn;

use crate::auth_tokens::{Scope, Action};
use crate::rate_limit::Decision as RateDecision;

use super::errors::{Resp, json_resp};
use super::state::SharedState;

/// Constant-time byte comparison — prevents timing side-channel on key.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Outcome of authenticating a request. The Scope is what was granted; the
/// route handler then checks it against the action it's about to perform.
pub(super) enum AuthOutcome {
    /// Server was started in dev mode (no admin key) — grants Admin.
    DevMode,
    /// Legacy single admin key matched.
    LegacyAdmin,
    /// A real scoped token matched. Carries the token hash so the rate
    /// limiter can key per-principal.
    Token { scope: Scope, hash: String },
}

impl AuthOutcome {
    pub(super) fn scope(&self) -> Scope {
        match self {
            AuthOutcome::DevMode | AuthOutcome::LegacyAdmin => Scope::Admin,
            AuthOutcome::Token { scope, .. } => scope.clone(),
        }
    }

    /// Stable principal id for rate-limit keying. Dev mode returns None
    /// — rate-limiting is bypassed when there's no auth (local-dev only).
    pub(super) fn principal_id(&self) -> Option<String> {
        match self {
            AuthOutcome::DevMode => None,
            AuthOutcome::LegacyAdmin => Some("legacy-admin".to_string()),
            AuthOutcome::Token { hash, .. } => Some(hash.clone()),
        }
    }
}

pub(super) fn authenticate(request: &tiny_http::Request, state: &SharedState) -> Result<AuthOutcome, Resp> {
    let auth_header = request.headers().iter()
        .find(|h| h.field.as_str().to_ascii_lowercase() == "authorization")
        .map(|h| h.value.as_str().to_string());

    // Dev mode: server started with no admin key at all.
    if state.admin_key.is_none() {
        return Ok(AuthOutcome::DevMode);
    }

    let raw = match auth_header.as_deref().and_then(|v| v.strip_prefix("Bearer ")) {
        Some(s) => s.to_string(),
        None => {
            let method = request.method().to_string();
            let url = request.url().to_string();
            let remote = request.remote_addr().map(|a| a.to_string()).unwrap_or_else(|| "unknown".into());
            warn!(remote = %remote, method = %method, url = %url, reason = "missing_bearer", "auth failure");
            return Err(json_resp(401, r#"{"error":"unauthorized"}"#));
        }
    };

    // Legacy admin key match (constant-time).
    if let Some(ref key) = state.admin_key {
        if constant_time_eq(raw.as_bytes(), key.as_bytes()) {
            return Ok(AuthOutcome::LegacyAdmin);
        }
    }

    // Scoped token lookup.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let store = state.tokens.lock().unwrap();
    if let Some(rec) = store.lookup(&raw, now) {
        let hash = crate::store::sha256_hex(raw.as_bytes());
        return Ok(AuthOutcome::Token { scope: rec.scope.clone(), hash });
    }

    let method = request.method().to_string();
    let url = request.url().to_string();
    let remote = request.remote_addr().map(|a| a.to_string()).unwrap_or_else(|| "unknown".into());
    warn!(remote = %remote, method = %method, url = %url, reason = "unknown_token", "auth failure");
    Err(json_resp(401, r#"{"error":"unauthorized"}"#))
}

/// Check that a granted scope authorizes a specific action. Audits the
/// decision to stderr so refused requests show up in operator logs.
pub(super) fn authorize_action(granted: &Scope, action: &Action) -> Result<(), Resp> {
    if granted.allows(action) {
        return Ok(());
    }
    warn!(?granted, ?action, "authorization denied");
    Err(json_resp(403, r#"{"error":"forbidden: scope does not allow this action"}"#))
}

/// Legacy entry-point still used by a handful of routes that need only the
/// authentication step (no fine-grained scope check). Returns `Some(Resp)`
/// to abort with 401, `None` to proceed.
pub(super) fn check_auth(request: &tiny_http::Request, state: &SharedState) -> Option<Resp> {
    match authenticate(request, state) {
        Ok(_) => None,
        Err(r) => Some(r),
    }
}

/// If the principal has a bucket and is currently throttled, return a 429
/// with `Retry-After` (in whole seconds, rounded up).
pub(super) fn rate_limit_check(state: &SharedState, principal: Option<&str>) -> Option<Resp> {
    let principal = principal?;
    match state.rate_limiter.check(principal) {
        RateDecision::Allow => None,
        RateDecision::Deny { retry_after_seconds } => {
            let retry_after = retry_after_seconds.ceil() as u64;
            let body = serde_json::json!({
                "error": "rate limit exceeded",
                "retryAfterSeconds": retry_after,
            }).to_string();
            let resp = tiny_http::Response::from_data(body.into_bytes())
                .with_status_code(429)
                .with_header(tiny_http::Header::from_bytes(
                    &b"Content-Type"[..], &b"application/json"[..]
                ).unwrap())
                .with_header(tiny_http::Header::from_bytes(
                    &b"Retry-After"[..], retry_after.to_string().as_bytes()
                ).unwrap());
            Some(resp)
        }
    }
}
