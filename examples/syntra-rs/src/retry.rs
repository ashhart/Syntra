// Copyright 2026 Ash Hart. Apache-2.0.

//! Syntra-driven HTTP retry policy selection.
//!
//! [`RetryClient`] wraps a `reqwest::blocking::Client` and asks Syntra to pick
//! a retry policy per request. Fail-safe: any Syntra error (transport,
//! refusal, malformed response) silently falls back to a configurable default
//! policy. Feedback failures are routed to an optional callback and never
//! propagate to the caller.
//!
//! ```no_run
//! use syntra_client::{SyntraClient, retry::RetryClient};
//!
//! let syntra = SyntraClient::new(
//!     "http://localhost:8787",
//!     "admin-key",
//!     "/tenants/myteam/jobs/retry/capsules/router",
//! );
//! let client = RetryClient::new(syntra);
//! let resp = client.get("https://api.example.com/users").unwrap();
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::blocking::{Client as HttpClient, Response};
use reqwest::Method;
use serde_json::{json, Value};

use crate::{DecideContext, Error, SyntraClient};

/// Concrete retry policy: number of attempts and backoff schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Stable name, used for logs and matching capsule option labels.
    pub name: &'static str,
    /// Number of retries *after* the initial attempt. `0` means no retry.
    pub max_retries: u32,
    /// Sleep before the first retry. Subsequent backoffs multiply by
    /// `backoff_multiplier`.
    pub initial_backoff: Duration,
    /// Multiplier applied to `initial_backoff` after each retry.
    pub backoff_multiplier: f64,
}

impl RetryPolicy {
    /// Return the policy at the given capsule-option index, clamping
    /// out-of-bounds inputs to the first policy (`none`).
    ///
    /// The order matches the demo capsule's `options:` list:
    /// `none, single, triple, exponential_fast, exponential_slow`.
    pub fn from_option(option_index: usize) -> &'static RetryPolicy {
        DEFAULT_POLICIES
            .get(option_index)
            .unwrap_or(&DEFAULT_POLICIES[0])
    }

    /// Return the policy with the given name, or `None` if not recognised.
    pub fn by_name(name: &str) -> Option<&'static RetryPolicy> {
        DEFAULT_POLICIES.iter().find(|p| p.name == name)
    }
}

/// The five default policies, indexed to match the demo capsule's
/// `options:` list order.
pub static DEFAULT_POLICIES: [RetryPolicy; 5] = [
    RetryPolicy {
        name: "none",
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        backoff_multiplier: 1.0,
    },
    RetryPolicy {
        name: "single",
        max_retries: 1,
        initial_backoff: Duration::ZERO,
        backoff_multiplier: 1.0,
    },
    RetryPolicy {
        name: "triple",
        max_retries: 3,
        initial_backoff: Duration::ZERO,
        backoff_multiplier: 1.0,
    },
    RetryPolicy {
        name: "exponential_fast",
        max_retries: 3,
        initial_backoff: Duration::from_millis(100),
        backoff_multiplier: 2.0,
    },
    RetryPolicy {
        name: "exponential_slow",
        max_retries: 3,
        initial_backoff: Duration::from_millis(500),
        backoff_multiplier: 2.0,
    },
];

/// A pluggable sleeper used for backoff. Tests inject a no-op implementation
/// that records the requested durations instead of actually sleeping.
pub trait Sleeper: Send + Sync + std::fmt::Debug {
    /// Sleep for `dur`. Implementations must not panic.
    fn sleep(&self, dur: Duration);
}

/// Default sleeper backed by `std::thread::sleep`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, dur: Duration) {
        if !dur.is_zero() {
            std::thread::sleep(dur);
        }
    }
}

/// Outcome of one full `request` call, used for feedback and tracker bookkeeping.
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    /// `true` iff the final response was 2xx/3xx (status < 400).
    pub success: bool,
    /// Wall-clock duration from first attempt start to last attempt finish.
    pub total_latency_ms: f64,
    /// Number of retries used (does not count the initial attempt).
    pub retries_used: u32,
    /// Final HTTP status code, if any response was received.
    pub status_code: Option<u16>,
}

#[derive(Debug, Clone, Copy)]
struct Outcome {
    success: bool,
    latency_ms: f64,
}

/// Per-host rolling window of request outcomes. Drives the feature vector
/// sent to Syntra for feature-context capsules.
#[derive(Debug)]
pub struct EndpointTracker {
    window: usize,
    inner: Mutex<HashMap<String, VecDeque<Outcome>>>,
}

impl EndpointTracker {
    /// Create a tracker with the given rolling-window size. Sizes `<= 0` are
    /// promoted to the default of 100.
    pub fn new(window: usize) -> Self {
        let window = if window == 0 { 100 } else { window };
        Self {
            window,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record one outcome for `host`.
    pub fn record(&self, host: &str, success: bool, latency_ms: f64) {
        let mut guard = self.inner.lock().expect("EndpointTracker mutex poisoned");
        let entry = guard
            .entry(host.to_owned())
            .or_insert_with(|| VecDeque::with_capacity(self.window));
        if entry.len() == self.window {
            entry.pop_front();
        }
        entry.push_back(Outcome {
            success,
            latency_ms,
        });
    }

    /// Return the rolling failure rate for `host`, in `[0, 1]`. Returns `0.5`
    /// when the window is empty (matching the Python/Go defaults).
    pub fn failure_rate(&self, host: &str) -> f64 {
        let guard = self.inner.lock().expect("EndpointTracker mutex poisoned");
        match guard.get(host) {
            Some(v) if !v.is_empty() => {
                let successes = v.iter().filter(|o| o.success).count();
                1.0 - successes as f64 / v.len() as f64
            }
            _ => 0.5,
        }
    }

    /// Return the feature vector consumed by Syntra for feature-context
    /// capsules: `recent_failure_rate`, `p99_latency_ms`, `hour`.
    pub fn features(&self, host: &str) -> Value {
        let (failure_rate, p99) = {
            let guard = self.inner.lock().expect("EndpointTracker mutex poisoned");
            match guard.get(host) {
                Some(v) if !v.is_empty() => {
                    let successes = v.iter().filter(|o| o.success).count();
                    let failure_rate = 1.0 - successes as f64 / v.len() as f64;
                    let mut lats: Vec<f64> = v.iter().map(|o| o.latency_ms).collect();
                    lats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    // Match the Python `int(len * 0.99) - 1` quantile rule.
                    let idx = ((lats.len() as f64 * 0.99) as usize).saturating_sub(1);
                    let p99 = lats.get(idx).copied().unwrap_or(1000.0);
                    (failure_rate, p99)
                }
                _ => (0.5, 1000.0),
            }
        };
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let hour = (secs / 3600.0) % 24.0;
        json!({
            "recent_failure_rate": failure_rate,
            "p99_latency_ms": p99,
            "hour": hour,
        })
    }
}

/// Hook called when feedback fails. The default is a no-op (failures are
/// silently swallowed).
pub type FeedbackErrorHook = Box<dyn Fn(&Error) + Send + Sync>;

/// HTTP retry client backed by Syntra.
pub struct RetryClient {
    syntra: SyntraClient,
    fallback: &'static RetryPolicy,
    http: HttpClient,
    tracker: Arc<EndpointTracker>,
    sleeper: Arc<dyn Sleeper>,
    on_feedback_error: Option<FeedbackErrorHook>,
}

impl std::fmt::Debug for RetryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryClient")
            .field("fallback", &self.fallback)
            .field("sleeper", &self.sleeper)
            .field(
                "on_feedback_error",
                &self.on_feedback_error.as_ref().map(|_| "<hook>"),
            )
            .finish()
    }
}

/// Builder for [`RetryClient`].
pub struct RetryClientBuilder {
    syntra: SyntraClient,
    fallback: &'static RetryPolicy,
    http: Option<HttpClient>,
    tracker_window: usize,
    sleeper: Arc<dyn Sleeper>,
    on_feedback_error: Option<FeedbackErrorHook>,
}

impl std::fmt::Debug for RetryClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryClientBuilder")
            .field("fallback", &self.fallback)
            .field("tracker_window", &self.tracker_window)
            .finish()
    }
}

impl RetryClient {
    /// Construct a `RetryClient` with defaults: `single` fallback policy,
    /// window 100, [`ThreadSleeper`], no feedback hook.
    pub fn new(syntra: SyntraClient) -> Self {
        Self::builder(syntra).build()
    }

    /// Start a builder for finer-grained configuration.
    pub fn builder(syntra: SyntraClient) -> RetryClientBuilder {
        RetryClientBuilder {
            syntra,
            fallback: &DEFAULT_POLICIES[1], // "single"
            http: None,
            tracker_window: 100,
            sleeper: Arc::new(ThreadSleeper),
            on_feedback_error: None,
        }
    }

    /// The per-host tracker. Exposed for inspection and integration tests.
    pub fn tracker(&self) -> &EndpointTracker {
        &self.tracker
    }

    /// Convenience: `GET url` with default headers.
    pub fn get(&self, url: &str) -> Result<Response, Error> {
        self.request(Method::GET, url, None)
    }

    /// Convenience: `POST url` with a JSON body.
    pub fn post_json(&self, url: &str, body: &Value) -> Result<Response, Error> {
        self.request(Method::POST, url, Some(body.clone()))
    }

    /// Execute a request through the Syntra-selected retry policy.
    ///
    /// Returns the final response when one was received (success or 5xx after
    /// exhaustion); returns [`Error::Transport`] only when every attempt
    /// failed at the transport layer.
    pub fn request(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> Result<Response, Error> {
        let host = endpoint_host(url);
        let features = self.tracker.features(&host);

        let (policy, decision_id) = self.get_policy(&features);

        let (outcome, response, error) =
            self.execute_with_policy(method, url, body.as_ref(), policy);

        self.tracker
            .record(&host, outcome.success, outcome.total_latency_ms);

        if let Some(id) = decision_id.as_deref() {
            self.send_feedback(id, &outcome);
        }

        match response {
            Some(r) => Ok(r),
            None => Err(error.unwrap_or_else(|| {
                Error::InvalidRequest("all retries exhausted without response".into())
            })),
        }
    }

    fn get_policy(&self, features: &Value) -> (&'static RetryPolicy, Option<String>) {
        let ctx = DecideContext::Features(features.clone());
        match self.syntra.decide(&ctx) {
            Ok(decision) => {
                if decision.refused {
                    (self.fallback, Some(decision.decision_id))
                } else {
                    match decision.chosen_option {
                        Some(idx) => (RetryPolicy::from_option(idx), Some(decision.decision_id)),
                        None => (self.fallback, Some(decision.decision_id)),
                    }
                }
            }
            Err(_) => (self.fallback, None),
        }
    }

    fn execute_with_policy(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
        policy: &RetryPolicy,
    ) -> (RequestOutcome, Option<Response>, Option<Error>) {
        let start = Instant::now();
        let mut backoff = policy.initial_backoff;
        let mut retries_used = 0u32;
        let mut last_error: Option<Error> = None;
        let mut last_status: Option<u16> = None;

        for attempt in 0..=policy.max_retries {
            let mut req = self.http.request(method.clone(), url);
            if let Some(b) = body {
                req = req.json(b);
            }
            match req.send() {
                Ok(resp) => {
                    let status = resp.status();
                    last_status = Some(status.as_u16());
                    if status.as_u16() < 500 {
                        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                        return (
                            RequestOutcome {
                                success: status.as_u16() < 400,
                                total_latency_ms: elapsed_ms,
                                retries_used,
                                status_code: Some(status.as_u16()),
                            },
                            Some(resp),
                            None,
                        );
                    }
                    // 5xx — eligible for retry. Capture the response so we can
                    // return it after exhaustion.
                    if attempt == policy.max_retries {
                        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                        return (
                            RequestOutcome {
                                success: false,
                                total_latency_ms: elapsed_ms,
                                retries_used,
                                status_code: Some(status.as_u16()),
                            },
                            Some(resp),
                            None,
                        );
                    }
                    // Drop the 5xx response and retry.
                    drop(resp);
                }
                Err(e) => {
                    last_error = Some(Error::Transport(e));
                }
            }

            if attempt < policy.max_retries {
                retries_used += 1;
                if !backoff.is_zero() {
                    self.sleeper.sleep(backoff);
                    let next = backoff.as_secs_f64() * policy.backoff_multiplier;
                    backoff = Duration::from_secs_f64(next);
                }
            }
        }

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        (
            RequestOutcome {
                success: false,
                total_latency_ms: elapsed_ms,
                retries_used,
                status_code: last_status,
            },
            None,
            last_error,
        )
    }

    fn send_feedback(&self, decision_id: &str, outcome: &RequestOutcome) {
        // Reward = success_bit − 0.3 × min(latency_ms / 10 000, 1).
        // Matches the Python and Go ports; stays inside the demo capsule's
        // [-1, 1] continuous reward range.
        let latency_penalty = (outcome.total_latency_ms / 10_000.0).min(1.0);
        let reward = if outcome.success { 1.0 } else { 0.0 } - 0.3 * latency_penalty;

        if let Err(e) = self.syntra.feedback(decision_id, reward) {
            if let Some(hook) = &self.on_feedback_error {
                hook(&e);
            }
            // Always swallowed: feedback failure must never break the caller.
        }
    }
}

impl RetryClientBuilder {
    /// Set the fallback policy used when Syntra is unreachable, refuses, or
    /// returns a malformed decision.
    pub fn fallback_policy(mut self, policy: &'static RetryPolicy) -> Self {
        self.fallback = policy;
        self
    }

    /// Override the underlying `reqwest::blocking::Client`. Use this to inject
    /// custom TLS, proxies, or timeouts.
    pub fn http_client(mut self, client: HttpClient) -> Self {
        self.http = Some(client);
        self
    }

    /// Set the rolling-window size for the per-host tracker. Default 100.
    pub fn tracker_window(mut self, window: usize) -> Self {
        self.tracker_window = window;
        self
    }

    /// Override the [`Sleeper`] used for backoff. Tests inject a recording
    /// sleeper to assert the backoff sequence without real sleeps.
    pub fn sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Register a hook invoked whenever feedback delivery fails. Even with a
    /// hook configured, the failure does not surface to the request caller.
    pub fn on_feedback_error<F>(mut self, hook: F) -> Self
    where
        F: Fn(&Error) + Send + Sync + 'static,
    {
        self.on_feedback_error = Some(Box::new(hook));
        self
    }

    /// Finalize the builder.
    pub fn build(self) -> RetryClient {
        let http = self
            .http
            .unwrap_or_else(|| HttpClient::builder().build().unwrap_or_else(|_| HttpClient::new()));
        RetryClient {
            syntra: self.syntra,
            fallback: self.fallback,
            http,
            tracker: Arc::new(EndpointTracker::new(self.tracker_window)),
            sleeper: self.sleeper,
            on_feedback_error: self.on_feedback_error,
        }
    }
}

/// Extract the `host[:port]` from a URL string. Falls back to the original
/// string when parsing fails so unrelated callers can still key the tracker.
pub fn endpoint_host(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        match (parsed.host_str(), parsed.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => url.to_owned(),
        }
    } else {
        url.to_owned()
    }
}
