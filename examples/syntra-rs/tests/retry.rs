// Copyright 2026 Ash Hart. Apache-2.0.

//! Integration tests for the Syntra retry client.
//!
//! Uses a hand-rolled `std::net::TcpListener` HTTP/1.1 stub server (see
//! `MockServer` below) so we don't pull in `httpmock` or `hyper` as a dev
//! dependency.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use reqwest::Method;
use serde_json::{json, Value};

use syntra_client::retry::{
    endpoint_host, EndpointTracker, RetryClient, RetryPolicy, Sleeper, DEFAULT_POLICIES,
};
use syntra_client::SyntraClient;

// ---------------------------------------------------------------------------
// MockServer: minimal HTTP/1.1 stub
// ---------------------------------------------------------------------------

/// A recorded request seen by the stub server.
#[derive(Debug, Clone)]
struct RecordedRequest {
    #[allow(dead_code)]
    method: String,
    path: String,
    body: String,
}

/// Pre-canned response keyed by path.
#[derive(Debug, Clone)]
struct CannedResponse {
    status: u16,
    body: String,
}

#[derive(Debug)]
struct MockState {
    responses: std::collections::HashMap<String, CannedResponse>,
    requests: Vec<RecordedRequest>,
    /// If true, every `/feedback` request returns 500.
    feedback_fails: bool,
    /// Count of requests received per path (used by the policy-attempts test).
    path_counts: std::collections::HashMap<String, usize>,
}

#[derive(Debug, Clone)]
struct MockServer {
    state: Arc<Mutex<MockState>>,
    base_url: String,
}

impl MockServer {
    fn start() -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("MockServer: bind 127.0.0.1:0 failed");
        let local = listener.local_addr().expect("local_addr");
        let base_url = format!("http://{local}");
        let state = Arc::new(Mutex::new(MockState {
            responses: std::collections::HashMap::new(),
            requests: Vec::new(),
            feedback_fails: false,
            path_counts: std::collections::HashMap::new(),
        }));

        let st = Arc::clone(&state);
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let st = Arc::clone(&st);
                        thread::spawn(move || handle_conn(s, st));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { state, base_url }
    }

    fn set_response(&self, path: &str, status: u16, body: impl Into<String>) {
        let mut g = self.state.lock().unwrap();
        g.responses.insert(
            path.to_owned(),
            CannedResponse {
                status,
                body: body.into(),
            },
        );
    }

    fn fail_feedback(&self) {
        let mut g = self.state.lock().unwrap();
        g.feedback_fails = true;
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    fn count_for(&self, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .path_counts
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

fn handle_conn(mut stream: TcpStream, state: Arc<Mutex<MockState>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .ok();
    let mut buf = [0u8; 8192];
    let mut data = Vec::new();
    // Read headers (until "\r\n\r\n"), then read up to Content-Length bytes.
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return,
        };
        data.extend_from_slice(&buf[..n]);
        if let Some(idx) = find_double_crlf(&data) {
            let header_end = idx + 4;
            let head = std::str::from_utf8(&data[..idx]).unwrap_or("").to_string();
            let mut method = String::new();
            let mut path = String::new();
            let mut content_length = 0usize;
            for (i, line) in head.split("\r\n").enumerate() {
                if i == 0 {
                    let mut parts = line.split_whitespace();
                    method = parts.next().unwrap_or("").to_owned();
                    path = parts.next().unwrap_or("").to_owned();
                } else if let Some(v) = line
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                {
                    content_length = v.trim().parse::<usize>().unwrap_or(0);
                }
            }

            // Drain remaining body bytes
            let mut body = data[header_end..].to_vec();
            while body.len() < content_length {
                let n = match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                body.extend_from_slice(&buf[..n]);
            }
            body.truncate(content_length);

            let body_str = String::from_utf8_lossy(&body).to_string();
            let (status, resp_body) = {
                let mut g = state.lock().unwrap();
                g.requests.push(RecordedRequest {
                    method: method.clone(),
                    path: path.clone(),
                    body: body_str.clone(),
                });
                *g.path_counts.entry(path.clone()).or_insert(0) += 1;

                if path.ends_with("/feedback") && g.feedback_fails {
                    (500u16, "feedback failed".to_string())
                } else if let Some(cr) = g.responses.get(&path) {
                    (cr.status, cr.body.clone())
                } else {
                    (404u16, format!("no canned response for {path}"))
                }
            };

            let status_text = status_text_for(status);
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{resp_body}",
                status = status,
                status_text = status_text,
                len = resp_body.as_bytes().len(),
                resp_body = resp_body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            return;
        }
    }
}

fn find_double_crlf(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn status_text_for(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

// ---------------------------------------------------------------------------
// Recording sleeper
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct RecordingSleeper {
    calls: Mutex<Vec<Duration>>,
}

impl Sleeper for RecordingSleeper {
    fn sleep(&self, dur: Duration) {
        self.calls.lock().unwrap().push(dur);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn syntra_for(server: &MockServer) -> SyntraClient {
    SyntraClient::with_timeout(
        server.base_url(),
        "test-key",
        "/tenants/t/jobs/j/capsules/c",
        Duration::from_secs(2),
    )
}

fn install_decide_response(server: &MockServer, chosen_option: usize) {
    let body = json!({
        "decisionId": "dec_test_1",
        "decisions": [{"chosen_option": chosen_option, "label": "test"}],
        "refused": false,
        "confidence": {},
        "oodScore": 0.1,
    });
    server.set_response(
        "/tenants/t/jobs/j/capsules/c/decide",
        200,
        body.to_string(),
    );
}

fn install_feedback_response(server: &MockServer) {
    server.set_response("/tenants/t/jobs/j/capsules/c/feedback", 200, "{}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_successful_decide_feedback_roundtrip() {
    let syntra_srv = MockServer::start();
    let target_srv = MockServer::start();

    // chosen_option=0 = "none" — no retries.
    install_decide_response(&syntra_srv, 0);
    install_feedback_response(&syntra_srv);
    target_srv.set_response("/ok", 200, r#"{"hello":"world"}"#);

    let client = RetryClient::new(syntra_for(&syntra_srv));
    let resp = client
        .get(&format!("{}/ok", target_srv.base_url()))
        .expect("request should succeed");
    assert_eq!(resp.status().as_u16(), 200);

    // Syntra should have seen exactly one decide and one feedback.
    let reqs = syntra_srv.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert!(
        paths.contains(&"/tenants/t/jobs/j/capsules/c/decide"),
        "expected /decide, saw {paths:?}"
    );
    assert!(
        paths.contains(&"/tenants/t/jobs/j/capsules/c/feedback"),
        "expected /feedback, saw {paths:?}"
    );

    // Feedback body should carry the decision id.
    let fb = reqs
        .iter()
        .find(|r| r.path.ends_with("/feedback"))
        .expect("feedback request");
    let parsed: Value = serde_json::from_str(&fb.body).expect("feedback body json");
    assert_eq!(parsed["decisionId"], "dec_test_1");
}

#[test]
fn test_refusal_falls_back_to_default_policy() {
    let syntra_srv = MockServer::start();
    let target_srv = MockServer::start();

    let refused = json!({
        "decisionId": "dec_refused",
        "decisions": [],
        "refused": true,
        "confidence": {"oodScore": 0.95, "refused": true},
        "oodScore": 0.95,
    });
    syntra_srv.set_response(
        "/tenants/t/jobs/j/capsules/c/decide",
        200,
        refused.to_string(),
    );
    install_feedback_response(&syntra_srv);
    target_srv.set_response("/ok", 200, "{}");

    // Use a custom fallback so we know which one was applied: "triple".
    let triple: &'static RetryPolicy = &DEFAULT_POLICIES[2];
    let sleeper = Arc::new(RecordingSleeper::default());
    let client = RetryClient::builder(syntra_for(&syntra_srv))
        .fallback_policy(triple)
        .sleeper(sleeper.clone())
        .build();

    let resp = client
        .get(&format!("{}/ok", target_srv.base_url()))
        .expect("ok");
    assert_eq!(resp.status().as_u16(), 200);

    // Feedback still posted (decision_id was carried even on refusal).
    let paths: Vec<String> = syntra_srv
        .requests()
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("/feedback")));
}

#[test]
fn test_syntra_unreachable_falls_back_without_error() {
    // Don't start a Syntra server — point at a port that nothing listens on.
    let target_srv = MockServer::start();
    target_srv.set_response("/ok", 200, "{}");

    // Use a free port we'll never bind. 127.0.0.1:1 is reliably refused on
    // POSIX without root.
    let syntra = SyntraClient::with_timeout(
        "http://127.0.0.1:1",
        "k",
        "/tenants/t/jobs/j/capsules/c",
        Duration::from_millis(200),
    );
    let client = RetryClient::new(syntra);
    let resp = client.get(&format!("{}/ok", target_srv.base_url()));
    assert!(resp.is_ok(), "should fall back, got {resp:?}");
}

#[test]
fn test_feedback_failure_does_not_propagate() {
    let syntra_srv = MockServer::start();
    let target_srv = MockServer::start();

    install_decide_response(&syntra_srv, 0);
    syntra_srv.fail_feedback();
    target_srv.set_response("/ok", 200, "{}");

    let hook_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook_calls_clone = Arc::clone(&hook_calls);

    let client = RetryClient::builder(syntra_for(&syntra_srv))
        .on_feedback_error(move |e| {
            hook_calls_clone.lock().unwrap().push(e.to_string());
        })
        .build();

    let resp = client
        .get(&format!("{}/ok", target_srv.base_url()))
        .expect("request should still succeed");
    assert_eq!(resp.status().as_u16(), 200);

    // Hook fired at least once.
    assert!(
        !hook_calls.lock().unwrap().is_empty(),
        "on_feedback_error should have been called"
    );
}

#[test]
fn test_endpoint_tracker_failure_rate() {
    let tracker = EndpointTracker::new(10);
    // Empty window: default 0.5.
    assert!((tracker.failure_rate("api.example.com") - 0.5).abs() < 1e-9);

    for _ in 0..3 {
        tracker.record("api.example.com", true, 100.0);
    }
    for _ in 0..7 {
        tracker.record("api.example.com", false, 500.0);
    }
    // 3 successes / 10 → failure rate 0.7.
    let fr = tracker.failure_rate("api.example.com");
    assert!(
        (fr - 0.7).abs() < 1e-9,
        "expected 0.7, got {fr}"
    );

    // Per-host isolation: a second host stays at default.
    assert!((tracker.failure_rate("other.example.com") - 0.5).abs() < 1e-9);

    // Window cap: pushing 20 more successes evicts the failures.
    for _ in 0..20 {
        tracker.record("api.example.com", true, 50.0);
    }
    assert!(
        tracker.failure_rate("api.example.com").abs() < 1e-9,
        "window should have evicted all failures"
    );

    // endpoint_host extracts host[:port] correctly.
    assert_eq!(endpoint_host("https://api.example.com/users"), "api.example.com");
    assert_eq!(endpoint_host("http://127.0.0.1:8080/x"), "127.0.0.1:8080");
}

#[test]
fn test_retry_policy_from_option_clamps_oob() {
    assert_eq!(RetryPolicy::from_option(0).name, "none");
    assert_eq!(RetryPolicy::from_option(1).name, "single");
    assert_eq!(RetryPolicy::from_option(4).name, "exponential_slow");
    // OOB → first policy.
    assert_eq!(RetryPolicy::from_option(99).name, "none");
    assert_eq!(RetryPolicy::from_option(usize::MAX).name, "none");

    assert_eq!(RetryPolicy::by_name("triple").unwrap().max_retries, 3);
    assert!(RetryPolicy::by_name("nonsense").is_none());
}

#[test]
fn test_backoff_multiplier_respected() {
    let syntra_srv = MockServer::start();
    let target_srv = MockServer::start();

    // chosen_option=3 = "exponential_fast": max_retries=3, initial=100ms, mult=2.0.
    install_decide_response(&syntra_srv, 3);
    install_feedback_response(&syntra_srv);
    // The target always returns 500 — forces every retry.
    target_srv.set_response("/flaky", 500, "boom");

    let sleeper = Arc::new(RecordingSleeper::default());
    let client = RetryClient::builder(syntra_for(&syntra_srv))
        .sleeper(sleeper.clone())
        .build();

    let resp = client
        .request(
            Method::GET,
            &format!("{}/flaky", target_srv.base_url()),
            None,
        )
        .expect("response, even if 500, should be Ok");
    assert_eq!(resp.status().as_u16(), 500);

    // Initial attempt + 3 retries = 4 total target calls.
    assert_eq!(target_srv.count_for("/flaky"), 4);

    // Three sleeps recorded: 100ms, 200ms, 400ms.
    let calls = sleeper.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "expected 3 backoff sleeps, got {calls:?}");
    let to_ms = |d: Duration| d.as_secs_f64() * 1000.0;
    assert!((to_ms(calls[0]) - 100.0).abs() < 1.0, "{calls:?}");
    assert!((to_ms(calls[1]) - 200.0).abs() < 1.0, "{calls:?}");
    assert!((to_ms(calls[2]) - 400.0).abs() < 1.0, "{calls:?}");
}
