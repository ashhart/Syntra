//! Authentication, scope checks and rate limiting.

use tracing::warn;

use crate::auth_tokens::{Action, Scope};
use crate::rate_limit::Decision as RateDecision;

use super::http::{Request, Response};
use super::state::SharedState;

/// Constant-time byte comparison, so key checks leak no timing signal.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Who a request authenticated as. Route handlers check the granted scope
/// against the action they are about to perform.
pub enum AuthOutcome {
    /// No admin key configured (`--dev-mode`, loopback only): grants Admin.
    DevMode,
    /// The operator admin key.
    OperatorKey,
    /// A scoped token; the hash keys the rate limiter.
    Token { scope: Scope, hash: String },
}

impl AuthOutcome {
    pub fn kind(&self) -> &'static str {
        match self {
            AuthOutcome::DevMode => "dev_mode",
            AuthOutcome::OperatorKey => "legacy_admin",
            AuthOutcome::Token { .. } => "scoped_token",
        }
    }

    pub fn scope(&self) -> Scope {
        match self {
            AuthOutcome::DevMode | AuthOutcome::OperatorKey => Scope::Admin,
            AuthOutcome::Token { scope, .. } => scope.clone(),
        }
    }

    /// Principal for rate limiting; dev mode is not limited.
    pub fn principal_id(&self) -> Option<String> {
        match self {
            AuthOutcome::DevMode => None,
            AuthOutcome::OperatorKey => Some("operator".to_string()),
            AuthOutcome::Token { hash, .. } => Some(hash.clone()),
        }
    }
}

/// The presented credential: `Authorization: Bearer <key>`, or the
/// Personalizer-style `Ocp-Apim-Subscription-Key: <key>` header.
fn presented_key(req: &Request) -> Option<&str> {
    if let Some(v) = req.header("authorization") {
        // The scheme is case-insensitive (RFC 9110 11.1).
        let (scheme, rest) = v.split_once(' ')?;
        return scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| rest.trim())
            .filter(|k| !k.is_empty());
    }
    req.header("ocp-apim-subscription-key").map(str::trim)
}

pub fn authenticate(req: &Request, state: &SharedState) -> Result<AuthOutcome, Response> {
    if state.admin_key.is_none() {
        return Ok(AuthOutcome::DevMode);
    }
    let unauthorized = |reason: &str| {
        let remote = req
            .remote
            .map(|a| a.to_string())
            .unwrap_or_else(|| "unknown".into());
        warn!(remote = %remote, method = %req.method, path = %req.path, reason, request_id = %req.request_id, "auth failure");
        Response::error(401, "unauthorized")
    };
    let Some(raw) = presented_key(req).filter(|k| !k.is_empty()) else {
        return Err(unauthorized("missing_credential"));
    };
    if let Some(key) = &state.admin_key
        && constant_time_eq(raw.as_bytes(), key.as_bytes())
    {
        return Ok(AuthOutcome::OperatorKey);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut tokens = state.tokens.lock().unwrap();
    if let Some((hash, rec)) = tokens.lookup_with_hash(raw, now) {
        if let Err(e) = tokens.record_use(&hash, now) {
            warn!(token_hash = %hash, error = %e, "token last-use update failed");
        }
        return Ok(AuthOutcome::Token {
            scope: rec.scope,
            hash,
        });
    }
    Err(unauthorized("unknown_token"))
}

/// Check that a granted scope authorizes an action.
pub fn authorize(granted: &Scope, action: &Action) -> Result<(), Response> {
    if granted.allows(action) {
        return Ok(());
    }
    warn!(?granted, ?action, "authorization denied");
    Err(Response::error(
        403,
        "forbidden: scope does not allow this action",
    ))
}

/// 429 with `Retry-After` when the principal is throttled.
pub fn rate_limit(state: &SharedState, principal: Option<&str>) -> Option<Response> {
    let principal = principal?;
    match state.rate_limiter.check(principal) {
        RateDecision::Allow => None,
        RateDecision::Deny {
            retry_after_seconds,
        } => {
            let retry_after = retry_after_seconds.ceil() as u64;
            Some(
                Response::json(
                    429,
                    &serde_json::json!({
                        "error": "rate limit exceeded",
                        "retryAfterSeconds": retry_after,
                    }),
                )
                .with_header("retry-after", &retry_after.to_string()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_equal_inputs() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn credential_comes_from_bearer_or_subscription_key() {
        let mut r = Request::new("GET", "/x");
        assert_eq!(presented_key(&r), None);
        r.headers
            .push(("authorization".into(), "Bearer  k1 ".into()));
        assert_eq!(presented_key(&r), Some("k1"));
        let mut r = Request::new("GET", "/x");
        r.headers
            .push(("ocp-apim-subscription-key".into(), "k2".into()));
        assert_eq!(presented_key(&r), Some("k2"));
        let mut r = Request::new("GET", "/x");
        r.headers.push(("authorization".into(), "Basic zzz".into()));
        assert_eq!(presented_key(&r), None);
        let mut r = Request::new("GET", "/x");
        r.headers.push(("authorization".into(), "bearer k3".into()));
        assert_eq!(presented_key(&r), Some("k3"));
        let mut r = Request::new("GET", "/x");
        r.headers.push(("authorization".into(), "Bearer   ".into()));
        assert_eq!(presented_key(&r), None);
    }
}
