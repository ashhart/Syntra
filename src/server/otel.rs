//! OpenTelemetry tracing: one server span per request, exported as OTLP
//! over HTTP with JSON encoding.
//!
//! Off unless an OTLP endpoint is configured. Configuration is the standard
//! environment variables:
//!
//! - `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (the full URL), or
//!   `OTEL_EXPORTER_OTLP_ENDPOINT` (a base URL; `/v1/traces` is appended).
//! - `OTEL_EXPORTER_OTLP_[TRACES_]HEADERS` (`key=value,...`, values
//!   percent-decoded), `..._TIMEOUT` (milliseconds, default 10000),
//!   `..._COMPRESSION` (`gzip` or `none`), `..._PROTOCOL` (`http/json`;
//!   `http/protobuf` is accepted because a collector's HTTP receiver takes
//!   both, `grpc` is not supported).
//! - `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`.
//! - `OTEL_TRACES_SAMPLER` (default `parentbased_always_on`) and
//!   `OTEL_TRACES_SAMPLER_ARG`.
//! - `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_MAX_QUEUE_SIZE`,
//!   `OTEL_BSP_MAX_EXPORT_BATCH_SIZE`.
//! - `OTEL_SDK_DISABLED=true` or `OTEL_TRACES_EXPORTER=none` turn it off.
//!
//! A W3C `traceparent` header makes the server span a child of the
//! caller's. `/health`, `/ready` and `/metrics` are not traced. A request
//! only samples, collects attributes and appends the finished span to a
//! bounded buffer; a background thread batches and posts them. A full
//! buffer drops the span (counted in `/metrics`) rather than slowing a
//! decision.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracing::warn;

use crate::decision::SplitMix64;

use super::http::{Request, Response};

/// Probes and scrapes would drown the spans worth reading.
const UNTRACED: [&str; 3] = ["/health", "/ready", "/metrics"];

/// Router labels of requests that matched no route; their paths are
/// arbitrary, so they get no `http.route`.
const UNMATCHED: [&str; 6] = [
    "not_found",
    "method_not_allowed",
    "unauthorized",
    "rate_limited",
    "personalizer.unbound",
    "invalid_name",
];

/// Where and how spans are exported.
#[derive(Clone)]
pub struct OtelConfig {
    /// The full URL spans are posted to.
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
    pub gzip: bool,
    pub sampler: Sampler,
    /// Resource attributes, `service.name` included.
    pub resource: Vec<(String, String)>,
    pub max_queue: usize,
    pub max_batch: usize,
    pub schedule_delay: Duration,
}

// Header values are usually credentials.
impl std::fmt::Debug for OtelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let header_names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("OtelConfig")
            .field("endpoint", &self.endpoint_for_logs())
            .field("headers", &header_names)
            .field("timeout", &self.timeout)
            .field("gzip", &self.gzip)
            .field("sampler", &self.sampler)
            .field("resource", &self.resource)
            .field("max_queue", &self.max_queue)
            .field("max_batch", &self.max_batch)
            .field("schedule_delay", &self.schedule_delay)
            .finish()
    }
}

impl OtelConfig {
    /// The endpoint without any `user:password@`, for logs.
    pub fn endpoint_for_logs(&self) -> String {
        let Some((scheme, rest)) = self.endpoint.split_once("://") else {
            return self.endpoint.clone();
        };
        let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
        match authority.rfind('@') {
            Some(at) => format!("{scheme}://***@{}", &rest[at + 1..]),
            None => self.endpoint.clone(),
        }
    }

    /// From the process environment: the config, or `None` when tracing is
    /// off, plus warnings about values that were ignored.
    pub fn from_env() -> (Option<OtelConfig>, Vec<String>) {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// As [`from_env`](Self::from_env), reading variables through `var`.
    /// Invalid values are warned about and replaced by their defaults, as
    /// the OpenTelemetry specification asks; only a setting that makes
    /// export impossible turns tracing off.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> (Option<OtelConfig>, Vec<String>) {
        let mut warnings = Vec::new();
        let get = |k: &str| {
            var(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        // The traces-specific variable wins over the generic one.
        let pick = |signal: &str| {
            get(&format!("OTEL_EXPORTER_OTLP_TRACES_{signal}"))
                .map(|v| (format!("OTEL_EXPORTER_OTLP_TRACES_{signal}"), v))
                .or_else(|| {
                    get(&format!("OTEL_EXPORTER_OTLP_{signal}"))
                        .map(|v| (format!("OTEL_EXPORTER_OTLP_{signal}"), v))
                })
        };

        if get("OTEL_SDK_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return (None, warnings);
        }
        if let Some(list) = get("OTEL_TRACES_EXPORTER") {
            let names: Vec<&str> = list.split(',').map(str::trim).collect();
            let others: Vec<&&str> = names
                .iter()
                .filter(|n| !matches!(**n, "otlp" | "none"))
                .collect();
            if !others.is_empty() {
                warnings.push(format!(
                    "OTEL_TRACES_EXPORTER: {others:?} not supported (only otlp and none)"
                ));
            }
            if !names.contains(&"otlp") {
                return (None, warnings);
            }
        }
        let endpoint = match (
            get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            get("OTEL_EXPORTER_OTLP_ENDPOINT"),
        ) {
            (Some(url), _) => url,
            (None, Some(base)) => format!("{}/v1/traces", base.trim_end_matches('/')),
            (None, None) => return (None, warnings),
        };
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            warnings.push(format!(
                "OTLP endpoint {endpoint:?} is not an http:// or https:// URL; tracing is off"
            ));
            return (None, warnings);
        }
        match pick("PROTOCOL") {
            None => {}
            Some((_, p)) if p == "http/json" => {}
            Some((name, p)) if p == "http/protobuf" => warnings.push(format!(
                "{name}=http/protobuf: Syntra sends http/json, which an OpenTelemetry \
                 Collector's HTTP receiver accepts on the same endpoint"
            )),
            Some((name, p)) => {
                warnings.push(format!(
                    "{name}={p:?} is not supported (Syntra exports http/json); tracing is off"
                ));
                return (None, warnings);
            }
        }

        let mut headers = Vec::new();
        if let Some((name, list)) = pick("HEADERS") {
            for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                match entry.split_once('=') {
                    Some((k, v)) if !k.trim().is_empty() => headers.push((
                        k.trim().to_string(),
                        super::http::decode_path_segment(v.trim()).into_owned(),
                    )),
                    _ => warnings.push(format!("{name}: ignoring an entry that is not key=value")),
                }
            }
        }
        let millis =
            |name: &str, value: Option<String>, default: u64, warnings: &mut Vec<String>| {
                match value.map(|v| v.parse::<u64>()) {
                    None => default,
                    Some(Ok(ms)) if ms > 0 => ms,
                    Some(_) => {
                        warnings.push(format!("{name} is not a positive number; using {default}"));
                        default
                    }
                }
            };
        let (timeout_name, timeout_value) = match pick("TIMEOUT") {
            Some((n, v)) => (n, Some(v)),
            None => ("OTEL_EXPORTER_OTLP_TIMEOUT".to_string(), None),
        };
        let timeout = millis(&timeout_name, timeout_value, 10_000, &mut warnings);
        let gzip = match pick("COMPRESSION") {
            None => false,
            Some((_, c)) if c == "gzip" => true,
            Some((_, c)) if c == "none" => false,
            Some((name, c)) => {
                warnings.push(format!(
                    "{name}={c:?} is not supported; sending uncompressed"
                ));
                false
            }
        };

        let sampler = Sampler::parse(
            get("OTEL_TRACES_SAMPLER").as_deref(),
            get("OTEL_TRACES_SAMPLER_ARG").as_deref(),
            &mut warnings,
        );

        let mut resource: Vec<(String, String)> = Vec::new();
        if let Some(list) = get("OTEL_RESOURCE_ATTRIBUTES") {
            for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                match entry.split_once('=') {
                    Some((k, v)) if !k.trim().is_empty() => resource.push((
                        k.trim().to_string(),
                        super::http::decode_path_segment(v.trim()).into_owned(),
                    )),
                    _ => warnings.push(
                        "OTEL_RESOURCE_ATTRIBUTES: ignoring an entry that is not key=value".into(),
                    ),
                }
            }
        }
        let mut set = |key: &str, value: String, overwrite: bool| match resource
            .iter_mut()
            .find(|(k, _)| k == key)
        {
            Some(slot) if overwrite => slot.1 = value,
            Some(_) => {}
            None => resource.push((key.to_string(), value)),
        };
        if let Some(name) = get("OTEL_SERVICE_NAME") {
            set("service.name", name, true);
        }
        set("service.name", "syntra".into(), false);
        set("service.version", env!("CARGO_PKG_VERSION").into(), false);
        set("telemetry.sdk.name", "syntra".into(), true);
        set("telemetry.sdk.language", "rust".into(), true);
        set(
            "telemetry.sdk.version",
            env!("CARGO_PKG_VERSION").into(),
            true,
        );

        let schedule_delay = millis(
            "OTEL_BSP_SCHEDULE_DELAY",
            get("OTEL_BSP_SCHEDULE_DELAY"),
            5_000,
            &mut warnings,
        );
        let max_queue = millis(
            "OTEL_BSP_MAX_QUEUE_SIZE",
            get("OTEL_BSP_MAX_QUEUE_SIZE"),
            2_048,
            &mut warnings,
        ) as usize;
        let mut max_batch = millis(
            "OTEL_BSP_MAX_EXPORT_BATCH_SIZE",
            get("OTEL_BSP_MAX_EXPORT_BATCH_SIZE"),
            512,
            &mut warnings,
        ) as usize;
        if max_batch > max_queue {
            warnings.push(format!(
                "OTEL_BSP_MAX_EXPORT_BATCH_SIZE is above the queue size; using {max_queue}"
            ));
            max_batch = max_queue;
        }

        (
            Some(OtelConfig {
                endpoint,
                headers,
                timeout: Duration::from_millis(timeout),
                gzip,
                sampler,
                resource,
                max_queue,
                max_batch,
                schedule_delay: Duration::from_millis(schedule_delay),
            }),
            warnings,
        )
    }
}

/// Which requests are traced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sampler {
    /// With a valid `traceparent`, follow the caller's sampled flag.
    pub parent_based: bool,
    /// Share of the other traces sampled, from 0 to 1.
    pub ratio: f64,
}

impl Sampler {
    fn parse(name: Option<&str>, arg: Option<&str>, warnings: &mut Vec<String>) -> Sampler {
        let mut ratio = || match arg.map(|a| a.parse::<f64>()) {
            None => 1.0,
            Some(Ok(r)) if (0.0..=1.0).contains(&r) => r,
            Some(_) => {
                warnings
                    .push("OTEL_TRACES_SAMPLER_ARG must be a number from 0 to 1; using 1".into());
                1.0
            }
        };
        let (parent_based, ratio) = match name.unwrap_or("parentbased_always_on") {
            "always_on" => (false, 1.0),
            "always_off" => (false, 0.0),
            "traceidratio" => (false, ratio()),
            "parentbased_always_on" => (true, 1.0),
            "parentbased_always_off" => (true, 0.0),
            "parentbased_traceidratio" => (true, ratio()),
            other => {
                warnings.push(format!(
                    "OTEL_TRACES_SAMPLER={other:?} is not supported; using parentbased_always_on"
                ));
                (true, 1.0)
            }
        };
        Sampler {
            parent_based,
            ratio,
        }
    }

    fn sample(&self, parent: Option<&TraceParent>, trace_id: &[u8; 16]) -> bool {
        if self.parent_based
            && let Some(p) = parent
        {
            return p.sampled;
        }
        if self.ratio >= 1.0 {
            return true;
        }
        if self.ratio <= 0.0 {
            return false;
        }
        // As the reference SDKs do: the low 63 bits of the trace id's last
        // eight bytes against ratio x 2^63, so every service that sees the
        // trace makes the same choice.
        let x = u64::from_be_bytes(trace_id[8..16].try_into().expect("8 bytes")) >> 1;
        x < (self.ratio * (1u64 << 63) as f64) as u64
    }
}

/// A parsed W3C `traceparent` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceParent {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub sampled: bool,
}

/// Parse `version-traceid-parentid-flags` as W3C Trace Context specifies:
/// lower-case hex, no all-zero ids, version `ff` invalid, version `00`
/// exactly 55 characters, later versions possibly longer.
pub fn parse_traceparent(value: &str) -> Option<TraceParent> {
    let v = value.trim().as_bytes();
    if v.len() < 55 || v[2] != b'-' || v[35] != b'-' || v[52] != b'-' {
        return None;
    }
    let version = hex_byte(&v[0..2])?;
    if version == 0xff
        || (version == 0 && v.len() != 55)
        || (version > 0 && v.len() > 55 && v[55] != b'-')
    {
        return None;
    }
    let mut trace_id = [0u8; 16];
    for (i, b) in trace_id.iter_mut().enumerate() {
        *b = hex_byte(&v[3 + 2 * i..5 + 2 * i])?;
    }
    let mut span_id = [0u8; 8];
    for (i, b) in span_id.iter_mut().enumerate() {
        *b = hex_byte(&v[36 + 2 * i..38 + 2 * i])?;
    }
    let flags = hex_byte(&v[53..55])?;
    if trace_id == [0; 16] || span_id == [0; 8] {
        return None;
    }
    Some(TraceParent {
        trace_id,
        span_id,
        sampled: flags & 1 == 1,
    })
}

/// Two lower-case hex digits.
fn hex_byte(pair: &[u8]) -> Option<u8> {
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    Some((digit(pair[0])? << 4) | digit(pair[1])?)
}

thread_local! {
    /// Span ids need to be unique, not secret: a per-thread SplitMix64
    /// seeded from the OS.
    static IDS: RefCell<SplitMix64> = RefCell::new(SplitMix64::new(crate::decision::random_seed()));
    /// Attributes handlers add to the span of the request this thread is
    /// routing; `None` when that request is not traced.
    static ATTRS: RefCell<Option<Vec<(&'static str, AttrValue)>>> = const { RefCell::new(None) };
}

fn random_nonzero() -> u64 {
    IDS.with(|rng| {
        let mut rng = rng.borrow_mut();
        loop {
            let x = rng.next_u64();
            if x != 0 {
                return x;
            }
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    F64(f64),
    Bool(bool),
}

/// Adds attributes to the current span.
pub struct Attrs<'a>(&'a mut Vec<(&'static str, AttrValue)>);

impl Attrs<'_> {
    pub fn str(&mut self, key: &'static str, value: &str) -> &mut Self {
        self.0.push((key, AttrValue::Str(value.to_string())));
        self
    }
    pub fn int(&mut self, key: &'static str, value: i64) -> &mut Self {
        self.0.push((key, AttrValue::Int(value)));
        self
    }
    pub fn f64(&mut self, key: &'static str, value: f64) -> &mut Self {
        self.0.push((key, AttrValue::F64(value)));
        self
    }
    pub fn bool(&mut self, key: &'static str, value: bool) -> &mut Self {
        self.0.push((key, AttrValue::Bool(value)));
        self
    }
}

/// Add attributes to the span of the request being routed on this thread.
/// Costs a thread-local lookup when the request is not traced.
pub fn annotate(f: impl FnOnce(&mut Attrs<'_>)) {
    ATTRS.with(|cell| {
        if let Some(attrs) = cell.borrow_mut().as_mut() {
            f(&mut Attrs(attrs));
        }
    });
}

/// A finished server span.
struct SpanData {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    trace_state: Option<String>,
    name: String,
    start_ns: u64,
    end_ns: u64,
    attributes: Vec<(&'static str, AttrValue)>,
    error: bool,
}

/// Export counters, rendered in `/metrics`.
#[derive(Default)]
pub struct TraceStats {
    pub exported: AtomicU64,
    pub dropped: AtomicU64,
}

/// Finished spans waiting for the exporter. Requests append under the
/// lock and wake the exporter only when a batch fills, so a traced request
/// never pays for a thread wake-up.
struct Buffer {
    spans: Vec<SpanData>,
    /// Flushes asked for, and the last one completed.
    flush_requested: u64,
    flush_done: u64,
    closed: bool,
}

struct Shared {
    buffer: Mutex<Buffer>,
    /// Wakes the exporter: a full batch, a flush, or close.
    wake: Condvar,
    /// Wakes `flush` callers.
    flushed: Condvar,
}

/// Samples requests and hands finished spans to the exporter thread.
pub struct Tracer {
    sampler: Sampler,
    max_queue: usize,
    max_batch: usize,
    shared: Arc<Shared>,
    pub stats: Arc<TraceStats>,
}

impl Tracer {
    /// Start the exporter thread.
    pub fn start(config: OtelConfig) -> Tracer {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(Buffer {
                spans: Vec::with_capacity(config.max_batch),
                flush_requested: 0,
                flush_done: 0,
                closed: false,
            }),
            wake: Condvar::new(),
            flushed: Condvar::new(),
        });
        let stats = Arc::new(TraceStats::default());
        let tracer = Tracer {
            sampler: config.sampler,
            max_queue: config.max_queue,
            max_batch: config.max_batch,
            shared: shared.clone(),
            stats: stats.clone(),
        };
        std::thread::Builder::new()
            .name("syntra-otlp".into())
            .spawn(move || export_loop(shared, config, stats))
            .expect("spawn the OTLP exporter");
        tracer
    }

    /// Begin the span for a request, or `None` when it is not traced.
    pub fn begin(&self, req: &Request) -> Option<ActiveSpan> {
        if UNTRACED.contains(&req.path.as_str()) {
            return None;
        }
        // More than one traceparent is invalid; the trace restarts here.
        let mut values = req
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("traceparent"));
        let parent = match (values.next(), values.next()) {
            (Some((_, v)), None) => parse_traceparent(v),
            _ => None,
        };
        let trace_id = match &parent {
            Some(p) => p.trace_id,
            None => {
                let mut id = [0u8; 16];
                id[..8].copy_from_slice(&random_nonzero().to_be_bytes());
                id[8..].copy_from_slice(&random_nonzero().to_be_bytes());
                id
            }
        };
        if !self.sampler.sample(parent.as_ref(), &trace_id) {
            return None;
        }
        let trace_state = parent
            .as_ref()
            .and_then(|_| req.header("tracestate"))
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 512)
            .map(String::from);
        ATTRS.with(|cell| *cell.borrow_mut() = Some(Vec::with_capacity(24)));
        Some(ActiveSpan {
            trace_id,
            span_id: random_nonzero().to_be_bytes(),
            parent_span_id: parent.map(|p| p.span_id),
            trace_state,
            start_ns: unix_nanos(),
            started: Instant::now(),
        })
    }

    fn enqueue(&self, span: SpanData) {
        let mut buffer = self.shared.buffer.lock().unwrap();
        if buffer.spans.len() >= self.max_queue {
            drop(buffer);
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        buffer.spans.push(span);
        let full = buffer.spans.len() == self.max_batch;
        drop(buffer);
        if full {
            self.shared.wake.notify_one();
        }
    }

    /// Export everything finished so far; false if that did not complete
    /// within `timeout`.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut buffer = self.shared.buffer.lock().unwrap();
        buffer.flush_requested += 1;
        let ticket = buffer.flush_requested;
        self.shared.wake.notify_one();
        while buffer.flush_done < ticket {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            buffer = self
                .shared
                .flushed
                .wait_timeout(buffer, deadline - now)
                .unwrap()
                .0;
        }
        true
    }
}

impl Drop for Tracer {
    // The exporter sends what is left, then exits.
    fn drop(&mut self) {
        self.shared.buffer.lock().unwrap().closed = true;
        self.shared.wake.notify_one();
    }
}

/// The span of a request being routed.
pub struct ActiveSpan {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    trace_state: Option<String>,
    start_ns: u64,
    started: Instant,
}

impl ActiveSpan {
    /// End the span with the router's label and the response, and queue it
    /// for export.
    pub fn finish(mut self, tracer: &Tracer, req: &Request, label: &str, resp: &Response) {
        let mut attributes = ATTRS
            .with(|cell| cell.borrow_mut().take())
            .unwrap_or_default();
        let end_ns = self
            .start_ns
            .saturating_add(self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        let method = match req.method.as_str() {
            m @ ("GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "PATCH" | "OPTIONS" | "TRACE"
            | "CONNECT") => m,
            _ => "_OTHER",
        };
        let route = http_route(label, &req.path);
        let name = match &route {
            Some(r) => format!("{method} {r}"),
            None => method.to_string(),
        };
        let mut a = Attrs(&mut attributes);
        a.str("http.request.method", method)
            .str("url.path", &req.path)
            .str("url.scheme", "http")
            .int("http.response.status_code", resp.status as i64)
            .str("network.protocol.version", "1.1")
            .str("syntra.route", label)
            .str("syntra.request_id", &req.request_id);
        if let Some(r) = route {
            a.0.push(("http.route", AttrValue::Str(r)));
        }
        if !req.query.is_empty() {
            a.0.push(("url.query", AttrValue::Str(scrub_query(&req.query))));
        }
        if let Some(remote) = req.remote {
            a.str("network.peer.address", &remote.ip().to_string())
                .int("network.peer.port", remote.port() as i64);
        }
        if let Some(ua) = req.header("user-agent") {
            a.str("user_agent.original", ua);
        }
        let error = resp.status >= 500;
        if error {
            a.str("error.type", &resp.status.to_string());
        }
        tracer.enqueue(SpanData {
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_span_id: self.parent_span_id,
            trace_state: self.trace_state.take(),
            name,
            start_ns: self.start_ns,
            end_ns,
            attributes,
            error,
        });
    }
}

impl Drop for ActiveSpan {
    // Also runs when a handler panics, so the next request on this thread
    // never inherits stale attributes.
    fn drop(&mut self) {
        ATTRS.with(|cell| *cell.borrow_mut() = None);
    }
}

/// The route template of a matched request: its path with every name
/// replaced by a placeholder, e.g.
/// `/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide`.
fn http_route(label: &str, path: &str) -> Option<String> {
    if UNMATCHED.contains(&label) {
        return None;
    }
    let mut out = String::with_capacity(path.len() + 16);
    let mut name_next: Option<&str> = None;
    for segment in path.trim_matches('/').split('/') {
        out.push('/');
        if let Some(placeholder) = name_next.take() {
            out.push_str(placeholder);
            continue;
        }
        let segment = super::http::decode_path_segment(segment);
        name_next = match segment.as_ref() {
            "tenants" => Some("{tenant}"),
            "jobs" => Some("{job}"),
            "capsules" => Some("{capsule}"),
            "decisions" => Some("{decisionId}"),
            "events" => Some("{eventId}"),
            "tokens" => Some("{tokenHash}"),
            _ => None,
        };
        out.push_str(&segment);
    }
    Some(out)
}

/// Query parameters the API reads; none carries a secret.
const KNOWN_QUERY: [&str; 6] = ["after", "limit", "since", "until", "replace", "snapshot"];

/// The query with every other parameter's value replaced, so a client that
/// puts a key in the URL (as some API gateways allow) never sends it to the
/// trace backend.
fn scrub_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for (i, pair) in query.split('&').enumerate() {
        if i > 0 {
            out.push('&');
        }
        let key = pair.split_once('=').map_or(pair, |(k, _)| k);
        if KNOWN_QUERY.contains(&super::http::percent_decode(key).as_str()) {
            out.push_str(pair);
        } else if pair.contains('=') {
            out.push_str(key);
            out.push_str("=REDACTED");
        } else {
            out.push_str("REDACTED");
        }
    }
    out
}

fn unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn export_loop(shared: Arc<Shared>, config: OtelConfig, stats: Arc<TraceStats>) {
    let resource = json!({
        "attributes": config
            .resource
            .iter()
            .map(|(k, v)| json!({"key": k, "value": {"stringValue": v}}))
            .collect::<Vec<_>>()
    })
    .to_string();
    let mut exporter = Exporter {
        agent: ureq::AgentBuilder::new().timeout(config.timeout).build(),
        config,
        resource,
        stats,
        last_warning: None,
    };
    loop {
        let (spans, flush_ticket, closed) = {
            let mut buffer = shared.buffer.lock().unwrap();
            let deadline = Instant::now() + exporter.config.schedule_delay;
            loop {
                if buffer.closed
                    || buffer.flush_requested > buffer.flush_done
                    || buffer.spans.len() >= exporter.config.max_batch
                {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                buffer = shared.wake.wait_timeout(buffer, deadline - now).unwrap().0;
            }
            let spans = std::mem::replace(
                &mut buffer.spans,
                Vec::with_capacity(exporter.config.max_batch),
            );
            (spans, buffer.flush_requested, buffer.closed)
        };
        for chunk in spans.chunks(exporter.config.max_batch) {
            exporter.export(chunk);
        }
        let mut buffer = shared.buffer.lock().unwrap();
        if flush_ticket > buffer.flush_done {
            buffer.flush_done = flush_ticket;
            shared.flushed.notify_all();
        }
        if closed {
            return;
        }
    }
}

struct Exporter {
    agent: ureq::Agent,
    config: OtelConfig,
    /// The resource, as JSON.
    resource: String,
    stats: Arc<TraceStats>,
    last_warning: Option<Instant>,
}

impl Exporter {
    /// Post a batch, retrying what OTLP calls retryable twice; a batch that
    /// still fails is dropped and counted.
    fn export(&mut self, spans: &[SpanData]) {
        if spans.is_empty() {
            return;
        }
        let count = spans.len() as u64;
        let body = encode(&self.resource, spans);
        let payload = if self.config.gzip {
            gzip(body.as_bytes())
        } else {
            body.into_bytes()
        };
        let mut backoff = Duration::from_millis(100);
        for attempt in 0..3 {
            let mut request = self
                .agent
                .post(&self.config.endpoint)
                .set("content-type", "application/json");
            if self.config.gzip {
                request = request.set("content-encoding", "gzip");
            }
            for (k, v) in &self.config.headers {
                request = request.set(k, v);
            }
            let failure = match request.send_bytes(&payload) {
                Ok(response) => {
                    let rejected = response
                        .into_string()
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .map(|v| rejected_spans(&v))
                        .unwrap_or(0)
                        .min(count);
                    self.stats
                        .exported
                        .fetch_add(count - rejected, Ordering::Relaxed);
                    self.stats.dropped.fetch_add(rejected, Ordering::Relaxed);
                    return;
                }
                Err(ureq::Error::Status(code, response)) => {
                    if matches!(code, 429 | 502 | 503 | 504) && attempt < 2 {
                        let wait = response
                            .header("retry-after")
                            .and_then(|v| v.trim().parse::<u64>().ok())
                            .map(Duration::from_secs)
                            .unwrap_or(backoff)
                            .min(Duration::from_secs(5));
                        std::thread::sleep(wait);
                        backoff *= 4;
                        continue;
                    }
                    format!("the collector answered {code}")
                }
                Err(ureq::Error::Transport(e)) => {
                    if attempt < 2 {
                        std::thread::sleep(backoff);
                        backoff *= 4;
                        continue;
                    }
                    // Not `e.to_string()`: that starts with the URL, which
                    // may hold credentials.
                    let detail = e
                        .message()
                        .map(str::to_string)
                        .or_else(|| std::error::Error::source(&e).map(|s| s.to_string()));
                    match detail {
                        Some(d) => format!("{}: {d}", e.kind()),
                        None => e.kind().to_string(),
                    }
                }
            };
            self.stats.dropped.fetch_add(count, Ordering::Relaxed);
            // One warning a minute: a collector outage must not flood the log.
            if self
                .last_warning
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
            {
                warn!(endpoint = %self.config.endpoint_for_logs(), error = %failure, spans = count, "OTLP export failed; spans dropped");
                self.last_warning = Some(Instant::now());
            }
            return;
        }
    }
}

/// `partialSuccess.rejectedSpans` of an OTLP response (an int64, which
/// JSON may carry as a string).
fn rejected_spans(response: &Value) -> u64 {
    let v = &response["partialSuccess"]["rejectedSpans"];
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// An OTLP `ExportTraceServiceRequest` in its JSON encoding: ids as hex,
/// enums as integers, 64-bit integers as strings. Written directly rather
/// than through `serde_json::Value`, which costs several times more.
fn encode(resource: &str, spans: &[SpanData]) -> String {
    let mut out = String::with_capacity(160 + spans.len() * 1024);
    out.push_str(r#"{"resourceSpans":[{"resource":"#);
    out.push_str(resource);
    out.push_str(r#","scopeSpans":[{"scope":{"name":"syntra","version":""#);
    out.push_str(env!("CARGO_PKG_VERSION"));
    out.push_str(r#""},"spans":["#);
    for (i, span) in spans.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_span(&mut out, span);
    }
    out.push_str("]}]}]}");
    out
}

fn write_span(out: &mut String, s: &SpanData) {
    use std::fmt::Write;
    out.push_str(r#"{"traceId":""#);
    push_hex(out, &s.trace_id);
    out.push_str(r#"","spanId":""#);
    push_hex(out, &s.span_id);
    out.push('"');
    if let Some(p) = &s.parent_span_id {
        out.push_str(r#","parentSpanId":""#);
        push_hex(out, p);
        out.push('"');
    }
    if let Some(ts) = &s.trace_state {
        out.push_str(r#","traceState":"#);
        push_json_str(out, ts);
    }
    out.push_str(r#","name":"#);
    push_json_str(out, &s.name);
    let _ = write!(
        out,
        r#","kind":2,"startTimeUnixNano":"{}","endTimeUnixNano":"{}","attributes":["#,
        s.start_ns, s.end_ns
    );
    for (i, (key, value)) in s.attributes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"key":"#);
        push_json_str(out, key);
        out.push_str(r#","value":{"#);
        match value {
            AttrValue::Str(v) => {
                out.push_str(r#""stringValue":"#);
                push_json_str(out, v);
            }
            AttrValue::Int(v) => {
                let _ = write!(out, r#""intValue":"{v}""#);
            }
            // Rust prints finite floats without exponents: valid JSON.
            AttrValue::F64(v) if v.is_finite() => {
                let _ = write!(out, r#""doubleValue":{v}"#);
            }
            AttrValue::F64(v) => {
                out.push_str(r#""stringValue":"#);
                push_json_str(out, &v.to_string());
            }
            AttrValue::Bool(v) => {
                let _ = write!(out, r#""boolValue":{v}"#);
            }
        }
        out.push_str("}}");
    }
    out.push(']');
    if s.error {
        out.push_str(r#","status":{"code":2}"#);
    }
    out.push('}');
}

fn push_hex(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
}

fn push_json_str(out: &mut String, s: &str) {
    use std::fmt::Write;
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    // Writing to a Vec cannot fail.
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn traceparent_follows_the_w3c_rules() {
        let ok = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
            .expect("valid");
        assert_eq!(hex(&ok.trace_id), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(hex(&ok.span_id), "00f067aa0ba902b7");
        assert!(ok.sampled);
        let unsampled =
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00").unwrap();
        assert!(!unsampled.sampled);
        // A later version may append fields after a dash.
        assert!(
            parse_traceparent("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra")
                .is_some()
        );
        for bad in [
            "",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01x",
            "00-4bf92f3577b34da6a3ce929d0e0e473-600f067aa0ba902b7-01",
            "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "0g-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ] {
            assert_eq!(parse_traceparent(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sampling_is_deterministic_by_trace_id() {
        let ratio = Sampler {
            parent_based: false,
            ratio: 0.25,
        };
        let mut rng = SplitMix64::new(7);
        let mut sampled = 0;
        for _ in 0..40_000 {
            let mut id = [0u8; 16];
            id[..8].copy_from_slice(&rng.next_u64().to_be_bytes());
            id[8..].copy_from_slice(&rng.next_u64().to_be_bytes());
            let first = ratio.sample(None, &id);
            assert_eq!(first, ratio.sample(None, &id));
            sampled += first as u32;
        }
        let share = sampled as f64 / 40_000.0;
        assert!((share - 0.25).abs() < 0.01, "{share}");

        let parent = TraceParent {
            trace_id: [1; 16],
            span_id: [1; 8],
            sampled: false,
        };
        let parent_based = Sampler {
            parent_based: true,
            ratio: 1.0,
        };
        assert!(!parent_based.sample(Some(&parent), &parent.trace_id));
        assert!(parent_based.sample(None, &parent.trace_id));
        let always = Sampler {
            parent_based: false,
            ratio: 1.0,
        };
        assert!(always.sample(Some(&parent), &parent.trace_id));
    }

    #[test]
    fn routes_become_templates_without_names() {
        let route = |label, path| http_route(label, path);
        assert_eq!(
            route(
                "capsule.decide",
                "/v1/tenants/acme/jobs/llm/capsules/router/decide"
            )
            .as_deref(),
            Some("/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide")
        );
        // A name that equals a keyword is still a name.
        assert_eq!(
            route(
                "capsule.decisions.get",
                "/v1/tenants/jobs/jobs/capsules/capsules/decisions/decisions/decisions"
            )
            .as_deref(),
            Some("/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decisions/{decisionId}")
        );
        // Escaped keywords are decoded before matching.
        assert_eq!(
            route("jobs.get", "/v1/tenant%73/acme/jobs/secret-job").as_deref(),
            Some("/v1/tenants/{tenant}/jobs/{job}")
        );
        assert_eq!(
            route(
                "personalizer.reward",
                "/personalizer/v1.0/events/abc-123/reward"
            )
            .as_deref(),
            Some("/personalizer/v1.0/events/{eventId}/reward")
        );
        assert_eq!(
            route("admin.tokens.revoke", "/v1/admin/tokens/9f3e").as_deref(),
            Some("/v1/admin/tokens/{tokenHash}")
        );
        assert_eq!(
            route("tenants.list", "/v1/tenants/").as_deref(),
            Some("/v1/tenants")
        );
        assert_eq!(route("not_found", "/anything/at/all"), None);
        assert_eq!(route("unauthorized", "/v1/tenants/acme"), None);
    }

    #[test]
    fn configuration_follows_the_otel_environment() {
        let (none, _) = OtelConfig::from_vars(vars(&[]));
        assert!(none.is_none(), "no endpoint means no tracing");

        let (config, warnings) = OtelConfig::from_vars(vars(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318/"),
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                "x-api-key=a%20b, x-team = ml ,bad",
            ),
            ("OTEL_SERVICE_NAME", "router-prod"),
            (
                "OTEL_RESOURCE_ATTRIBUTES",
                "service.name=ignored,deployment.environment=prod",
            ),
            ("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
            ("OTEL_TRACES_SAMPLER_ARG", "0.1"),
            ("OTEL_EXPORTER_OTLP_COMPRESSION", "gzip"),
            ("OTEL_BSP_SCHEDULE_DELAY", "250"),
        ]));
        let config = config.expect("on");
        assert_eq!(config.endpoint, "http://collector:4318/v1/traces");
        assert_eq!(
            config.headers,
            vec![
                ("x-api-key".to_string(), "a b".to_string()),
                ("x-team".to_string(), "ml".to_string())
            ]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let resource: std::collections::HashMap<_, _> = config.resource.iter().cloned().collect();
        assert_eq!(resource["service.name"], "router-prod");
        assert_eq!(resource["deployment.environment"], "prod");
        assert_eq!(resource["telemetry.sdk.language"], "rust");
        assert_eq!(
            config.sampler,
            Sampler {
                parent_based: true,
                ratio: 0.1
            }
        );
        assert!(config.gzip);
        assert_eq!(config.schedule_delay, Duration::from_millis(250));
        assert!(
            !format!("{config:?}").contains("a b"),
            "header values stay out of logs"
        );

        // The traces-specific endpoint is used as it is.
        let (config, _) = OtelConfig::from_vars(vars(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://ignored:4318"),
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "https://otlp.example/api/traces",
            ),
        ]));
        assert_eq!(config.unwrap().endpoint, "https://otlp.example/api/traces");

        // Invalid values fall back to defaults with a warning.
        let (config, warnings) = OtelConfig::from_vars(vars(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
            ("OTEL_TRACES_SAMPLER", "jaeger_remote"),
            ("OTEL_EXPORTER_OTLP_TIMEOUT", "soon"),
        ]));
        let config = config.unwrap();
        assert_eq!(config.timeout, Duration::from_secs(10));
        assert_eq!(
            config.sampler,
            Sampler {
                parent_based: true,
                ratio: 1.0
            }
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");

        // Settings that make export impossible, or turn it off.
        for off in [
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4317"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ],
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_SDK_DISABLED", "true"),
            ],
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_TRACES_EXPORTER", "none"),
            ],
            vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "collector:4318")],
        ] {
            let (config, _) = OtelConfig::from_vars(vars(&off));
            assert!(config.is_none(), "{off:?}");
        }
    }

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::new();
        push_hex(&mut s, bytes);
        s
    }

    #[test]
    fn spans_encode_as_otlp_json() {
        let span = SpanData {
            trace_id: [0xab; 16],
            span_id: [0x01; 8],
            parent_span_id: Some([0x02; 8]),
            trace_state: Some("vendor=1".into()),
            name: "POST /v1/\"quoted\"\n\u{1}é".into(),
            start_ns: 1_700_000_000_000_000_000,
            end_ns: 1_700_000_000_000_050_000,
            attributes: vec![
                ("a", AttrValue::Str("s\\t".into())),
                ("b", AttrValue::Int(-200)),
                ("c", AttrValue::F64(0.5)),
                ("d", AttrValue::Bool(true)),
                ("e", AttrValue::F64(f64::NAN)),
                ("f", AttrValue::F64(1e-7)),
                ("g", AttrValue::F64(3.0)),
            ],
            error: true,
        };
        let root = SpanData {
            trace_id: [0x0c; 16],
            span_id: [0x0d; 8],
            parent_span_id: None,
            trace_state: None,
            name: "GET".into(),
            start_ns: 1,
            end_ns: 2,
            attributes: vec![],
            error: false,
        };
        let resource =
            json!({"attributes": [{"key": "service.name", "value": {"stringValue": "x"}}]});
        let text = encode(&resource.to_string(), &[span, root]);
        let body: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(body["resourceSpans"][0]["resource"], resource);
        let scope = &body["resourceSpans"][0]["scopeSpans"][0];
        assert_eq!(scope["scope"]["name"], "syntra");
        let v = &scope["spans"][0];
        assert_eq!(v["traceId"], "abababababababababababababababab");
        assert_eq!(v["spanId"], "0101010101010101");
        assert_eq!(v["parentSpanId"], "0202020202020202");
        assert_eq!(v["traceState"], "vendor=1");
        assert_eq!(v["name"], "POST /v1/\"quoted\"\n\u{1}é");
        assert_eq!(v["kind"], 2);
        assert_eq!(v["startTimeUnixNano"], "1700000000000000000");
        assert_eq!(v["endTimeUnixNano"], "1700000000000050000");
        assert_eq!(v["status"]["code"], 2);
        let attrs = v["attributes"].as_array().unwrap();
        assert_eq!(attrs[0]["value"]["stringValue"], "s\\t");
        assert_eq!(attrs[1]["value"]["intValue"], "-200");
        assert_eq!(attrs[2]["value"]["doubleValue"], 0.5);
        assert_eq!(attrs[3]["value"]["boolValue"], true);
        assert_eq!(attrs[4]["value"]["stringValue"], "NaN");
        assert_eq!(attrs[5]["value"]["doubleValue"], 1e-7);
        assert_eq!(attrs[6]["value"]["doubleValue"], 3.0);
        let r = &scope["spans"][1];
        assert!(r.get("parentSpanId").is_none() && r.get("status").is_none());
        assert_eq!(r["attributes"], json!([]));
        assert_eq!(
            rejected_spans(&json!({"partialSuccess": {"rejectedSpans": "3"}})),
            3
        );
        assert_eq!(rejected_spans(&json!({})), 0);
    }

    #[test]
    fn secrets_stay_out_of_spans_and_logs() {
        assert_eq!(
            scrub_query("after=d-1&limit=10&subscription-key=abc&x%3Dy=z&bare&since=5"),
            "after=d-1&limit=10&subscription-key=REDACTED&x%3Dy=REDACTED&REDACTED&since=5"
        );
        assert_eq!(scrub_query("lim%69t=3"), "lim%69t=3");
        let (config, _) = OtelConfig::from_vars(vars(&[(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "https://user:p%40ss@otlp.example:4318/v1/traces",
        )]));
        let config = config.unwrap();
        assert_eq!(
            config.endpoint_for_logs(),
            "https://***@otlp.example:4318/v1/traces"
        );
        assert!(!format!("{config:?}").contains("p%40ss"));
        let mut plain = config.clone();
        plain.endpoint = "http://collector:4318/v1/traces?a=b@c".into();
        assert_eq!(plain.endpoint_for_logs(), plain.endpoint);
    }

    /// Arbitrary text (quotes, backslashes, control characters, any
    /// Unicode) always encodes to JSON that reads back unchanged, and no
    /// header, path or query makes parsing panic.
    #[test]
    fn arbitrary_text_round_trips_and_nothing_panics() {
        let mut rng = SplitMix64::new(0x5EED);
        let text = |rng: &mut SplitMix64| -> String {
            let len = (rng.next_u64() % 40) as usize;
            (0..len)
                .map(|_| match rng.next_u64() % 6 {
                    0 => ['"', '\\', '/', '\u{7f}'][(rng.next_u64() % 4) as usize],
                    1 => char::from_u32((rng.next_u64() % 0x20) as u32).unwrap(),
                    2 => char::from_u32(0x80 + (rng.next_u64() % 0xD000) as u32).unwrap_or('x'),
                    3 => char::from_u32(0x1_0000 + (rng.next_u64() % 0x1_0000) as u32).unwrap(),
                    _ => (b' ' + (rng.next_u64() % 95) as u8) as char,
                })
                .collect()
        };
        for _ in 0..2_000 {
            let name = text(&mut rng);
            let value = text(&mut rng);
            let state = text(&mut rng);
            let span = SpanData {
                trace_id: [7; 16],
                span_id: [9; 8],
                parent_span_id: None,
                trace_state: Some(state.clone()),
                name: name.clone(),
                start_ns: rng.next_u64(),
                end_ns: rng.next_u64(),
                attributes: vec![
                    ("s", AttrValue::Str(value.clone())),
                    ("f", AttrValue::F64(f64::from_bits(rng.next_u64()))),
                    ("i", AttrValue::Int(rng.next_u64() as i64)),
                ],
                error: false,
            };
            let body: Value = serde_json::from_str(&encode("{}", &[span])).expect("valid JSON");
            let v = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
            assert_eq!(v["name"], name.as_str());
            assert_eq!(v["traceState"], state.as_str());
            assert_eq!(v["attributes"][0]["value"]["stringValue"], value.as_str());

            let path = format!("/{}", text(&mut rng));
            let _ = http_route("capsule.get", &path);
            let _ = scrub_query(&text(&mut rng));
            let _ = parse_traceparent(&text(&mut rng));
        }
        // Near-valid traceparent headers: every one-byte change parses or
        // is refused, never panics.
        let valid = b"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_vec();
        for i in 0..valid.len() {
            for b in [b'-', b'0', b'f', b'F', b'g', b' ', 0xC3] {
                let mut v = valid.clone();
                v[i] = b;
                if let Ok(s) = std::str::from_utf8(&v) {
                    let _ = parse_traceparent(s);
                }
            }
            let _ = parse_traceparent(std::str::from_utf8(&valid[..i]).unwrap());
        }
    }

    #[test]
    fn annotations_only_reach_a_traced_request() {
        annotate(|a| {
            a.str("lost", "x");
        });
        ATTRS.with(|cell| assert!(cell.borrow().is_none()));
        ATTRS.with(|cell| *cell.borrow_mut() = Some(Vec::new()));
        annotate(|a| {
            a.int("kept", 1);
        });
        let got = ATTRS.with(|cell| cell.borrow_mut().take()).unwrap();
        assert_eq!(got, vec![("kept", AttrValue::Int(1))]);
    }
}
