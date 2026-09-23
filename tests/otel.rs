//! OpenTelemetry export end to end: the server binary configured by the
//! standard `OTEL_*` variables posts OTLP/HTTP JSON to a stand-in
//! collector, continuing the caller's W3C trace.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const KEY: &str = "otel-test-key";
const CAPSULE: &str = "/v1/tenants/acme/jobs/llm/capsules/router";

/// One POST the collector received.
struct Export {
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}

/// A stand-in OTLP/HTTP collector that answers 503 to its first
/// `fail_first` requests, then 200.
struct Collector {
    port: u16,
    exports: Arc<Mutex<Vec<Export>>>,
    requests: Arc<AtomicUsize>,
}

impl Collector {
    fn start(fail_first: usize) -> Collector {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("collector");
        let port = server.server_addr().to_ip().unwrap().port();
        let exports = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        let (sink, count) = (exports.clone(), requests.clone());
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let n = count.fetch_add(1, Ordering::SeqCst);
                if n < fail_first {
                    let _ = request.respond(tiny_http::Response::empty(503));
                    continue;
                }
                let headers: Vec<(String, String)> = request
                    .headers()
                    .iter()
                    .map(|h| {
                        (
                            h.field.as_str().as_str().to_ascii_lowercase(),
                            h.value.as_str().to_string(),
                        )
                    })
                    .collect();
                let mut raw = Vec::new();
                request.as_reader().read_to_end(&mut raw).unwrap();
                let gzipped = headers
                    .iter()
                    .any(|(k, v)| k == "content-encoding" && v == "gzip");
                let text = if gzipped {
                    let mut s = String::new();
                    flate2::read::GzDecoder::new(&raw[..])
                        .read_to_string(&mut s)
                        .expect("gzip body");
                    s
                } else {
                    String::from_utf8(raw).unwrap()
                };
                sink.lock().unwrap().push(Export {
                    path: request.url().to_string(),
                    headers,
                    body: serde_json::from_str(&text).expect("OTLP JSON"),
                });
                let _ = request.respond(tiny_http::Response::from_string("{}"));
            }
        });
        Collector {
            port,
            exports,
            requests,
        }
    }

    /// Every span received so far.
    fn spans(&self) -> Vec<Value> {
        self.exports
            .lock()
            .unwrap()
            .iter()
            .flat_map(|e| {
                e.body["resourceSpans"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .flat_map(|rs| rs["scopeSpans"].as_array().cloned().unwrap_or_default())
            .flat_map(|ss| ss["spans"].as_array().cloned().unwrap_or_default())
            .collect()
    }

    fn wait_for_spans(&self, n: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let spans = self.spans();
            if spans.len() >= n || Instant::now() > deadline {
                return spans;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct Server {
    child: Child,
    addr: String,
    store: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.store);
    }
}

fn boot(env: &[(&str, String)]) -> Server {
    let store = std::env::temp_dir().join(format!(
        "syntra-otel-{}-{}",
        std::process::id(),
        syntra::decision::random_seed()
    ));
    for _ in 0..10 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_syntra"));
        command
            .args(["serve", "--addr", &addr, "--store"])
            .arg(&store)
            .args(["--admin-key", KEY])
            .env("SYNTRA_RATE_LIMIT_RPS", "10000000")
            .env("SYNTRA_RATE_LIMIT_BURST", "10000000")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Only this test's OTEL settings, whatever the developer's shell has.
        for (k, _) in std::env::vars() {
            if k.starts_with("OTEL_") {
                command.env_remove(k);
            }
        }
        for (k, v) in env {
            command.env(k, v);
        }
        let mut child = command.spawn().expect("spawn syntra");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(r) = ureq::get(&format!("http://{addr}/health")).call()
                && r.status() == 200
            {
                return Server { child, addr, store };
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("could not boot syntra");
}

impl Server {
    fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = ureq::request(method, &format!("http://{}{path}", self.addr))
            .set("Authorization", &format!("Bearer {KEY}"))
            .set("Content-Type", "application/json");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let res = match body {
            Some(b) => req.send_string(&b.to_string()),
            None => req.call(),
        };
        let resp = match res {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("{method} {path}: {e}"),
        };
        let status = resp.status();
        let text = resp.into_string().unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

fn attr_str<'a>(span: &'a Value, key: &str) -> Option<&'a str> {
    attr(span, key)?["stringValue"].as_str()
}

const SPEC: &str = r#"{"actions": [{"id": "small"}, {"id": "large"}]}"#;

#[test]
fn spans_continue_the_callers_trace_and_carry_the_decision() {
    let collector = Collector::start(0);
    let srv = boot(&[
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://127.0.0.1:{}", collector.port),
        ),
        (
            "OTEL_EXPORTER_OTLP_HEADERS",
            "x-api-key=s3cret%20key".to_string(),
        ),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", "gzip".to_string()),
        ("OTEL_SERVICE_NAME", "syntra-test".to_string()),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "deployment.environment=test".to_string(),
        ),
        ("OTEL_BSP_SCHEDULE_DELAY", "100".to_string()),
    ]);

    let (status, _) = srv.call(
        "PUT",
        &format!("{CAPSULE}/spec"),
        &[],
        Some(serde_json::from_str(SPEC).unwrap()),
    );
    assert_eq!(status, 201, "capsule created");

    // A sampled caller: the server span joins its trace.
    let (status, sampled) = srv.call(
        "POST",
        &format!("{CAPSULE}/decide"),
        &[
            (
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
            ("tracestate", "rojo=00f067aa0ba902b7"),
        ],
        Some(json!({"context": {"segment": "pro"}})),
    );
    assert_eq!(status, 200);
    let decision_id = sampled["decisionId"].as_str().unwrap().to_string();

    // A caller that did not sample: under parentbased_always_on, no span.
    let (status, _) = srv.call(
        "POST",
        &format!("{CAPSULE}/decide"),
        &[(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00",
        )],
        Some(json!({"context": {}})),
    );
    assert_eq!(status, 200);

    // No caller context: a new root span.
    let (status, _) = srv.call(
        "POST",
        &format!("{CAPSULE}/decide"),
        &[],
        Some(json!({"context": {}})),
    );
    assert_eq!(status, 200);

    let (status, _) = srv.call("GET", "/health", &[], None);
    assert_eq!(status, 200);
    let (status, _) = srv.call(
        "POST",
        &format!("{CAPSULE}/reward"),
        &[],
        Some(json!({"decisionId": decision_id, "reward": 0.75})),
    );
    assert_eq!(status, 200);
    let (status, _) = srv.call("GET", "/v1/no/such/route", &[], None);
    assert_eq!(status, 404);

    // spec, sampled decide, root decide, reward, 404.
    let spans = collector.wait_for_spans(5);
    assert_eq!(spans.len(), 5, "{spans:#?}");

    let exports = collector.exports.lock().unwrap();
    for e in exports.iter() {
        assert_eq!(e.path, "/v1/traces");
        let header = |name: &str| {
            e.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(header("content-type"), Some("application/json"));
        assert_eq!(header("content-encoding"), Some("gzip"));
        assert_eq!(header("x-api-key"), Some("s3cret key"));
        let resource: Vec<(String, String)> = e.body["resourceSpans"][0]["resource"]["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["key"].as_str().unwrap().to_string(),
                    a["value"]["stringValue"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert!(resource.contains(&("service.name".into(), "syntra-test".into())));
        assert!(resource.contains(&("deployment.environment".into(), "test".into())));
    }
    drop(exports);

    let decide_route = "/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide";
    let child = spans
        .iter()
        .find(|s| s["traceId"] == "4bf92f3577b34da6a3ce929d0e0e4736")
        .expect("the caller's trace");
    assert_eq!(child["parentSpanId"], "00f067aa0ba902b7");
    assert_eq!(child["traceState"], "rojo=00f067aa0ba902b7");
    assert_eq!(child["kind"], 2);
    assert_eq!(child["name"], format!("POST {decide_route}"));
    assert_eq!(attr_str(child, "http.route"), Some(decide_route));
    assert_eq!(attr_str(child, "http.request.method"), Some("POST"));
    assert_eq!(
        attr(child, "http.response.status_code").unwrap()["intValue"],
        "200"
    );
    assert_eq!(attr_str(child, "syntra.tenant"), Some("acme"));
    assert_eq!(attr_str(child, "syntra.capsule"), Some("router"));
    assert_eq!(
        attr_str(child, "syntra.decision.id"),
        Some(decision_id.as_str())
    );
    assert_eq!(
        attr_str(child, "syntra.decision.action"),
        sampled["action"].as_str()
    );
    assert_eq!(
        attr(child, "syntra.decision.probability").unwrap()["doubleValue"],
        sampled["probability"]
    );
    let start: u64 = child["startTimeUnixNano"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let end: u64 = child["endTimeUnixNano"].as_str().unwrap().parse().unwrap();
    assert!(end >= start && end - start < 5_000_000_000);

    assert!(
        spans
            .iter()
            .all(|s| s["traceId"] != "0af7651916cd43dd8448eb211c80319c"),
        "an unsampled caller's request is not traced"
    );
    let roots: Vec<&Value> = spans
        .iter()
        .filter(|s| s["name"] == format!("POST {decide_route}") && s.get("parentSpanId").is_none())
        .collect();
    assert_eq!(roots.len(), 1, "one decide without a caller context");
    assert_eq!(roots[0]["traceId"].as_str().unwrap().len(), 32);

    let reward = spans
        .iter()
        .find(|s| attr_str(s, "syntra.route") == Some("capsule.reward"))
        .expect("reward span");
    assert_eq!(
        attr_str(reward, "syntra.decision.id"),
        Some(decision_id.as_str())
    );
    assert_eq!(
        attr(reward, "syntra.reward.value").unwrap()["doubleValue"],
        0.75
    );

    // An unmatched path gets no route template, so no raw path in the name.
    let missing = spans
        .iter()
        .find(|s| attr_str(s, "syntra.route") == Some("not_found"))
        .expect("404 span");
    assert_eq!(missing["name"], "GET");
    assert!(attr(missing, "http.route").is_none());

    assert!(
        spans
            .iter()
            .all(|s| attr_str(s, "url.path") != Some("/health")),
        "probes are not traced"
    );

    let text = ureq::get(&format!("http://{}/metrics", srv.addr))
        .set("Authorization", &format!("Bearer {KEY}"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(
        text.contains("syntra_otel_spans_exported_total 5"),
        "{text}"
    );
    assert!(text.contains("syntra_otel_spans_dropped_total 0"), "{text}");
}

#[test]
fn queued_spans_are_exported_at_shutdown_after_a_retry() {
    // The collector refuses the first export with 503; the batch would
    // otherwise wait ten minutes, so only the shutdown flush sends it.
    let collector = Collector::start(1);
    let mut srv = boot(&[
        (
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            format!("http://127.0.0.1:{}/v1/traces", collector.port),
        ),
        ("OTEL_BSP_SCHEDULE_DELAY", "600000".to_string()),
    ]);
    let (status, _) = srv.call(
        "PUT",
        &format!("{CAPSULE}/spec"),
        &[],
        Some(serde_json::from_str(SPEC).unwrap()),
    );
    assert_eq!(status, 201, "capsule created");
    let (status, _) = srv.call(
        "POST",
        &format!("{CAPSULE}/decide"),
        &[],
        Some(json!({"context": {}})),
    );
    assert_eq!(status, 200);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        collector.spans().is_empty(),
        "nothing exported before the delay"
    );

    let status = Command::new("kill")
        .args(["-TERM", &srv.child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    while srv.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "server did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    let spans = collector.spans();
    assert_eq!(spans.len(), 2, "{spans:#?}");
    assert_eq!(
        collector.requests.load(Ordering::SeqCst),
        2,
        "one 503, one retry"
    );
    assert!(
        spans
            .iter()
            .any(|s| attr_str(s, "syntra.route") == Some("capsule.decide"))
    );
}

#[test]
fn tracing_is_off_without_an_endpoint() {
    let srv = boot(&[("OTEL_SERVICE_NAME", "no-endpoint".to_string())]);
    let (status, _) = srv.call(
        "PUT",
        &format!("{CAPSULE}/spec"),
        &[],
        Some(serde_json::from_str(SPEC).unwrap()),
    );
    assert_eq!(status, 201, "capsule created");
    let text = ureq::get(&format!("http://{}/metrics", srv.addr))
        .set("Authorization", &format!("Bearer {KEY}"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(!text.contains("syntra_otel_"), "{text}");
}
