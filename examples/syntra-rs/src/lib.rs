// Copyright 2026 Ash Hart. Apache-2.0.

//! Rust client for [Syntra](https://github.com/ashhart/Syntra), a
//! self-hosted contextual-bandit appliance.
//!
//! The crate exposes two layers:
//!
//! * [`SyntraClient`] — a minimal HTTP client for `/decide` and `/feedback`.
//!   Mirrors the Python `syntra_retry` and Go `syntra-go` ports.
//! * [`retry::RetryClient`] — a Syntra-driven HTTP retry policy wrapper around
//!   `reqwest::blocking::Client`, with a per-host rolling window of outcomes
//!   driving feature vectors.
//!
//! ```no_run
//! use syntra_client::{DecideContext, SyntraClient};
//! use serde_json::json;
//!
//! let client = SyntraClient::new(
//!     "http://localhost:8787",
//!     std::env::var("SYNTRA_ADMIN_KEY").unwrap(),
//!     "/tenants/myteam/jobs/retry/capsules/router",
//! );
//!
//! let decision = client.decide(&DecideContext::Features(
//!     json!({"recent_failure_rate": 0.1, "p99_latency_ms": 200.0, "hour": 9.0}),
//! )).unwrap();
//!
//! if !decision.refused {
//!     // ... act on decision.chosen_option ...
//!     client.feedback(&decision.decision_id, 0.8).unwrap();
//! }
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::time::Duration;

use reqwest::blocking::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod error;
pub mod retry;

pub use error::Error;

/// Default HTTP timeout used when the caller does not override it. Matches the
/// 2 s timeout in the canonical Python and Go ports.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Minimal Syntra HTTP client.
///
/// Construct one with [`SyntraClient::new`] and call [`SyntraClient::decide`]
/// and [`SyntraClient::feedback`]. `Clone`-cheap once built (the inner
/// `reqwest::blocking::Client` uses an `Arc<Inner>` connection pool).
#[derive(Debug, Clone)]
pub struct SyntraClient {
    base_url: String,
    admin_key: String,
    capsule_path: String,
    http: HttpClient,
}

/// Body of `/decide` requests.
///
/// Exactly one of the two variants is sent on the wire. A discrete-context
/// capsule expects [`DecideContext::Discrete`]; a feature-context capsule
/// expects [`DecideContext::Features`] (a JSON object of named features).
#[derive(Debug, Clone)]
pub enum DecideContext {
    /// `{"contextKey": "..."}` — used by discrete-context capsules.
    Discrete(String),
    /// `{"features": {...}}` — used by feature-context capsules. The value
    /// must be a JSON object; otherwise [`Error::InvalidRequest`] is returned.
    Features(Value),
}

/// One entry of the `decisions` array in the `/decide` response.
#[derive(Debug, Clone, Deserialize)]
struct DecisionItem {
    chosen_option: usize,
    #[serde(default)]
    #[allow(dead_code)]
    label: Option<String>,
}

/// Raw `/decide` response shape as Syntra returns it.
#[derive(Debug, Clone, Deserialize)]
struct DecideResponse {
    #[serde(rename = "decisionId")]
    decision_id: String,
    #[serde(default)]
    decisions: Vec<DecisionItem>,
    #[serde(default)]
    refused: bool,
    #[serde(default = "Value::default", rename = "confidence")]
    confidence: Value,
    #[serde(default, rename = "oodScore")]
    ood_score: f64,
}

/// Parsed `/decide` response.
///
/// The `chosen_option` field is hoisted from `decisions[0].chosen_option` for
/// ergonomic access — most retry-style integrations only consume the first
/// decision. The raw `confidence` object is preserved as a [`serde_json::Value`]
/// because its shape depends on whether refusal is enabled and what calibrator
/// is in use.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Server-assigned decision id; pass back to `/feedback`.
    pub decision_id: String,
    /// `true` when Syntra refused to decide and the caller must fall back.
    pub refused: bool,
    /// Out-of-distribution score for this input. Higher means more anomalous.
    pub ood_score: f64,
    /// Raw `confidence` object from the response. May be `Value::Null`.
    pub confidence: Value,
    /// Index into the capsule's `options[]` list. `None` when `decisions[]`
    /// was empty (typically when `refused == true`).
    pub chosen_option: Option<usize>,
}

/// Request body for `/feedback`.
#[derive(Debug, Serialize)]
struct FeedbackBody<'a> {
    #[serde(rename = "decisionId")]
    decision_id: &'a str,
    reward: f64,
}

impl SyntraClient {
    /// Construct a new client.
    ///
    /// `base_url` should not have a trailing slash; if it does, the slash is
    /// stripped. `capsule_path` should start with `/`, e.g.
    /// `"/tenants/acme/jobs/routing/capsules/router"`.
    ///
    /// Uses [`DEFAULT_TIMEOUT`] for the HTTP client. For a custom timeout, use
    /// [`SyntraClient::with_timeout`].
    pub fn new(
        base_url: impl Into<String>,
        admin_key: impl Into<String>,
        capsule_path: impl Into<String>,
    ) -> Self {
        Self::with_timeout(base_url, admin_key, capsule_path, DEFAULT_TIMEOUT)
    }

    /// Construct a new client with a custom HTTP timeout.
    pub fn with_timeout(
        base_url: impl Into<String>,
        admin_key: impl Into<String>,
        capsule_path: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        let mut path = capsule_path.into();
        while path.ends_with('/') {
            path.pop();
        }
        // The blocking client builder cannot fail with defaults + timeout; if
        // it ever does (e.g. due to TLS backend init), fall back to the bare
        // client and lose the timeout rather than panicking.
        let http = HttpClient::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| HttpClient::new());
        Self {
            base_url: base,
            admin_key: admin_key.into(),
            capsule_path: path,
            http,
        }
    }

    /// The configured base URL (with trailing slashes stripped).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The configured capsule path.
    pub fn capsule_path(&self) -> &str {
        &self.capsule_path
    }

    /// Call `POST {base_url}{capsule_path}/decide` and parse the response.
    pub fn decide(&self, ctx: &DecideContext) -> Result<Decision, Error> {
        let body = match ctx {
            DecideContext::Discrete(key) => {
                serde_json::json!({ "contextKey": key })
            }
            DecideContext::Features(value) => {
                if !value.is_object() {
                    return Err(Error::InvalidRequest(
                        "DecideContext::Features must be a JSON object".into(),
                    ));
                }
                serde_json::json!({ "features": value })
            }
        };

        let url = format!("{}{}/decide", self.base_url, self.capsule_path);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.admin_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()?;

        let status = resp.status();
        let text = resp.text()?;
        if !status.is_success() {
            return Err(Error::Status {
                status: status.as_u16(),
                body: text,
            });
        }

        let raw: DecideResponse = serde_json::from_str(&text)?;
        let chosen_option = raw.decisions.first().map(|d| d.chosen_option);
        Ok(Decision {
            decision_id: raw.decision_id,
            refused: raw.refused,
            ood_score: raw.ood_score,
            confidence: raw.confidence,
            chosen_option,
        })
    }

    /// Call `POST {base_url}{capsule_path}/feedback`.
    ///
    /// `reward` should fall inside whatever range the capsule's reward spec
    /// declares — typically `[0, 1]` for binary capsules or `[-1, 1]` for
    /// continuous capsules. Syntra clips out-of-range values server-side.
    pub fn feedback(&self, decision_id: &str, reward: f64) -> Result<(), Error> {
        let url = format!("{}{}/feedback", self.base_url, self.capsule_path);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.admin_key)
            .header("Content-Type", "application/json")
            .json(&FeedbackBody {
                decision_id,
                reward,
            })
            .send()?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(Error::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }
}
