//! Shared harness for the v2 server suites (`server_v2`, `auth_routes`,
//! `doctor_cli`, `crash_recovery`, `cli`).
//!
//! Two ways to reach the server:
//!
//! - [`App`]: the router in process (`build_state` + `handle`), no socket.
//!   Fast and deterministic; used for fine-grained API behavior.
//! - [`Server`]: a real `syntra serve` child on a free loopback port with
//!   its own temporary store; used for restarts, crashes, signals, the CLI
//!   and the hyper adapter (body limits, headers). Every child is killed in
//!   `Drop`.
//!
//! Temporary directories live under `std::env::temp_dir()` and are removed
//! in `Drop`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use syntra::server::http::{Request, Response};
use syntra::server::state::State;
use syntra::server::{ServerConfig, build_state, handle};

pub const SYNTRA: &str = env!("CARGO_BIN_EXE_syntra");
pub const LYCAN: &str = env!("CARGO_BIN_EXE_lycan");

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique suffix for names within this test process.
pub fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "{}-{nanos:x}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

// ───────────────────────── temporary directories ─────────────────────────

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let p = std::env::temp_dir().join(format!("syntra-v2t-{label}-{}", unique()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Capsule route: `/v1/tenants/{t}/jobs/{j}/capsules/{c}{tail}`.
pub fn cap(t: &str, j: &str, c: &str, tail: &str) -> String {
    format!("/v1/tenants/{t}/jobs/{j}/capsules/{c}{tail}")
}

pub fn parse_body(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

// ───────────────────────────── in-process ────────────────────────────────

/// How a request presents its credential.
#[derive(Clone, Debug)]
pub enum Cred {
    None,
    Bearer(String),
    SubscriptionKey(String),
    /// A raw `(header, value)` pair, for malformed credentials.
    Header(String, String),
}

/// The router in process on a fresh temporary store.
pub struct App {
    pub state: State,
    pub admin_key: Option<String>,
    store: PathBuf,
    // Declared last so it is dropped after the state (see `Drop`). `None`
    // when the caller owns the store directory (`App::at`).
    dir: Option<TempDir>,
}

impl App {
    /// Dev mode: no admin key, every request is the operator, no rate limit.
    pub fn dev(label: &str) -> App {
        let dir = TempDir::new(label);
        Self::open(dir.join("store"), None, Some(dir))
    }

    /// Authenticated: `key` is the operator admin key.
    pub fn with_key(label: &str, key: &str) -> App {
        let dir = TempDir::new(label);
        Self::open(dir.join("store"), Some(key.to_string()), Some(dir))
    }

    /// On a store the caller owns; it outlives the `App`, which closes the
    /// event store cleanly (like a graceful stop) when dropped.
    pub fn at(store: &Path, key: Option<&str>) -> App {
        Self::open(store.to_path_buf(), key.map(String::from), None)
    }

    fn open(store: PathBuf, admin_key: Option<String>, dir: Option<TempDir>) -> App {
        let state = build_state(&ServerConfig {
            addr: "127.0.0.1:0".into(),
            store_path: store.to_string_lossy().into_owned(),
            admin_key: admin_key.clone(),
            service_name: None,
            ..Default::default()
        })
        .expect("build_state");
        App {
            state,
            admin_key,
            store,
            dir,
        }
    }

    pub fn store(&self) -> PathBuf {
        self.store.clone()
    }

    /// Rebuild the state on the same store, as a process restart would.
    /// `graceful` runs the shutdown path (flush and snapshot every loaded
    /// model); otherwise only the queued events are committed and no model
    /// is snapshotted, which is what a crash leaves behind for models.
    pub fn restart(&mut self, graceful: bool) {
        if graceful {
            self.state.shutdown();
        } else {
            assert!(
                self.state.writer.flush(Duration::from_secs(10)),
                "event log did not drain"
            );
        }
        let fresh = build_state(&ServerConfig {
            addr: "127.0.0.1:0".into(),
            store_path: self.store().to_string_lossy().into_owned(),
            admin_key: self.admin_key.clone(),
            service_name: None,
            ..Default::default()
        })
        .expect("rebuild state");
        let old = std::mem::replace(&mut self.state, fresh);
        drop(old);
    }

    /// Wait until every queued decision and reward is committed.
    pub fn flush(&self) {
        assert!(self.state.writer.flush(Duration::from_secs(10)));
    }

    pub fn raw(&self, method: &str, target: &str, cred: &Cred, body: Option<&[u8]>) -> Response {
        let mut req = Request::new(method, target);
        match cred {
            Cred::None => {}
            Cred::Bearer(k) => req
                .headers
                .push(("authorization".into(), format!("Bearer {k}"))),
            Cred::SubscriptionKey(k) => req
                .headers
                .push(("ocp-apim-subscription-key".into(), k.clone())),
            Cred::Header(h, v) => req.headers.push((h.to_ascii_lowercase(), v.clone())),
        }
        if let Some(b) = body {
            req.body = b.to_vec().into();
        }
        handle(&self.state, &req)
    }

    fn admin_cred(&self) -> Cred {
        match &self.admin_key {
            Some(k) => Cred::Bearer(k.clone()),
            None => Cred::None,
        }
    }

    /// As the operator, with a raw body. Returns (status, parsed JSON).
    pub fn call_bytes(&self, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
        let r = self.raw(method, target, &self.admin_cred(), Some(body));
        (r.status, parse_body(&r.body))
    }

    /// As the operator, with an optional JSON body.
    pub fn call(&self, method: &str, target: &str, body: Option<Value>) -> (u16, Value) {
        let bytes = body.map(|b| b.to_string().into_bytes());
        let r = self.raw(method, target, &self.admin_cred(), bytes.as_deref());
        (r.status, parse_body(&r.body))
    }

    /// As the operator; panics unless the status matches.
    pub fn ok(&self, method: &str, target: &str, body: Option<Value>, want: u16) -> Value {
        let (st, v) = self.call(method, target, body);
        assert_eq!(st, want, "{method} {target}: {v}");
        v
    }

    /// Create or patch a capsule spec.
    pub fn put_spec(&self, t: &str, j: &str, c: &str, spec: Value) -> Value {
        let (st, v) = self.call("PUT", &cap(t, j, c, "/spec"), Some(spec));
        assert!(st == 200 || st == 201, "PUT spec: {st} {v}");
        v
    }

    pub fn decide(&self, t: &str, j: &str, c: &str, body: Value) -> Value {
        self.ok("POST", &cap(t, j, c, "/decide"), Some(body), 200)
    }

    pub fn reward(&self, t: &str, j: &str, c: &str, body: Value) -> Value {
        self.ok("POST", &cap(t, j, c, "/reward"), Some(body), 200)
    }

    pub fn model_version(&self, t: &str, j: &str, c: &str) -> u64 {
        self.ok("GET", &cap(t, j, c, "/model"), None, 200)["modelVersion"]
            .as_u64()
            .expect("modelVersion")
    }

    /// Issue a scoped token as the operator; returns (raw token, hash).
    pub fn issue_token(&self, scope: Value, ttl: Option<u64>) -> (String, String) {
        let mut body = json!({ "scope": scope, "label": "test" });
        if let Some(t) = ttl {
            body["ttlSeconds"] = json!(t);
        }
        let v = self.ok("POST", "/v1/admin/tokens", Some(body), 200);
        (
            v["token"].as_str().expect("token").to_string(),
            v["hash"].as_str().expect("hash").to_string(),
        )
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Stop the writer thread so the event store closes before the
        // directory is removed.
        self.state.shutdown();
    }
}

// ───────────────────────────── Lycan ─────────────────────────────────────

/// Compile Lycan source to `.lyc` bytes in process.
pub fn compile_lycan(src: &str) -> Vec<u8> {
    let tokens = syntra::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let program = syntra::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    syntra::graph_compiler::GraphCompiler::new()
        .compile(&program)
        .expect("compile")
        .to_bytes()
}

// ───────────────────────────── processes ─────────────────────────────────

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30))
        .build()
}

/// One HTTP response: status, headers we care about, body.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Send a request; an HTTP error status is a normal result, a transport
/// failure (refused, reset) is `Err`.
pub fn try_http(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse, String> {
    let mut req = agent.request(method, url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let res = match body {
        Some(b) => req.send_bytes(b),
        None => req.call(),
    };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("{method} {url}: {e}")),
    };
    let status = resp.status();
    let headers = resp
        .headers_names()
        .into_iter()
        .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
        .collect();
    let mut text = String::new();
    resp.into_reader()
        .read_to_string(&mut text)
        .map_err(|e| format!("{method} {url}: reading body: {e}"))?;
    Ok(HttpResponse {
        status,
        headers,
        body: text,
    })
}

/// A `syntra serve` child process with its own store.
pub struct Server {
    pub child: Child,
    pub addr: String,
    pub key: Option<String>,
    pub store: PathBuf,
    pub log: PathBuf,
    pub agent: ureq::Agent,
    extra: Vec<String>,
}

impl Server {
    /// Start a server on `store` with an admin key (`Some`) or in dev
    /// mode (`None`). Server logs go to `<store>.log`, outside the store,
    /// so they never show up in store snapshots or backups.
    pub fn start(store: &Path, key: Option<&str>) -> Server {
        Self::start_with(store, key, &[])
    }

    /// [`Server::start`] with extra `serve` arguments.
    pub fn start_with(store: &Path, key: Option<&str>, extra: &[&str]) -> Server {
        let log = store.with_extension("log");
        let agent = agent();
        let key = key.map(String::from);
        let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        let (child, addr) = spawn_server(store, &log, key.as_deref(), &extra, &agent);
        Server {
            child,
            addr,
            key,
            store: store.to_path_buf(),
            log,
            agent,
            extra,
        }
    }

    pub fn url(&self, target: &str) -> String {
        format!("http://{}{target}", self.addr)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn auth_header(&self) -> Option<String> {
        self.key.as_ref().map(|k| format!("Bearer {k}"))
    }

    /// As the operator. Transport errors panic.
    pub fn http(&self, method: &str, target: &str, body: Option<&[u8]>) -> HttpResponse {
        self.try_http(method, target, body).unwrap()
    }

    pub fn try_http(
        &self,
        method: &str,
        target: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, String> {
        let auth = self.auth_header();
        let headers: Vec<(&str, &str)> = auth
            .as_deref()
            .map(|a| vec![("Authorization", a)])
            .unwrap_or_default();
        try_http(&self.agent, method, &self.url(target), &headers, body)
    }

    /// As the operator with an optional JSON body: (status, JSON).
    pub fn call(&self, method: &str, target: &str, body: Option<Value>) -> (u16, Value) {
        let bytes = body.map(|b| b.to_string().into_bytes());
        let r = self.http(method, target, bytes.as_deref());
        (r.status, r.json())
    }

    pub fn ok(&self, method: &str, target: &str, body: Option<Value>, want: u16) -> Value {
        let (st, v) = self.call(method, target, body);
        assert_eq!(st, want, "{method} {target}: {v}");
        v
    }

    /// SIGKILL and reap.
    pub fn kill9(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// SIGTERM and wait for the graceful exit; returns the exit code.
    pub fn term(&mut self) -> Option<i32> {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            if Instant::now() > deadline {
                self.kill9();
                panic!("server did not stop on SIGTERM");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Start again on the same store (after `kill9` or `term`), on a new
    /// port.
    pub fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (child, addr) = spawn_server(
            &self.store,
            &self.log,
            self.key.as_deref(),
            &self.extra,
            &self.agent,
        );
        self.child = child;
        self.addr = addr;
    }
}

/// Spawn `syntra serve` on a free port and wait until it answers. With a
/// key, a 200 from `whoami` proves the answering server is ours (another
/// test may have taken the port in the meantime).
fn spawn_server(
    store: &Path,
    log: &Path,
    key: Option<&str>,
    extra: &[String],
    agent: &ureq::Agent,
) -> (Child, String) {
    for _ in 0..20 {
        let addr = format!("127.0.0.1:{}", free_port());
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        let mut cmd = Command::new(SYNTRA);
        cmd.args(["serve", "--addr", &addr, "--store"])
            .arg(store)
            .env_remove("LYCAN_ADMIN_KEY")
            .env("SYNTRA_RATE_LIMIT_RPS", "10000000")
            .env("SYNTRA_RATE_LIMIT_BURST", "10000000")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file));
        match key {
            Some(k) => cmd.args(["--admin-key", k]),
            None => cmd.arg("--dev-mode"),
        };
        cmd.args(extra);
        let mut child = cmd.spawn().expect("spawn syntra serve");
        let auth = key.map(|k| format!("Bearer {k}"));
        let headers: Vec<(&str, &str)> = auth
            .as_deref()
            .map(|a| vec![("Authorization", a)])
            .unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break; // most likely lost the port race; try another port
            }
            let url = format!("http://{addr}/v1/auth/whoami");
            if let Ok(r) = try_http(agent, "GET", &url, &headers, None)
                && r.status == 200
            {
                return (child, addr);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!(
        "could not boot syntra serve; log:\n{}",
        std::fs::read_to_string(log).unwrap_or_default()
    );
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run a binary to completion with a timeout; the child is killed if it
/// overruns.
pub fn run(bin: &str, args: &[&str]) -> Output {
    run_in(bin, args, None)
}

pub fn run_in(bin: &str, args: &[&str], cwd: Option<&Path>) -> Output {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env_remove("LYCAN_ADMIN_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let child = cmd.spawn().expect("spawn");
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(out) => out.expect("wait"),
        Err(_) => {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
            panic!("{bin} {args:?} did not finish in 60 s");
        }
    }
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// `syntra doctor --store <root> --json`: exit code and the findings.
pub fn doctor(store: &Path) -> (i32, Vec<Value>) {
    let out = run(
        SYNTRA,
        &["doctor", "--store", store.to_str().unwrap(), "--json"],
    );
    let findings = stdout(&out)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("doctor prints JSON lines"))
        .collect();
    (out.status.code().unwrap_or(-1), findings)
}

/// Finding codes, for assertions.
pub fn codes(findings: &[Value]) -> Vec<String> {
    findings
        .iter()
        .filter_map(|f| f["code"].as_str().map(String::from))
        .collect()
}

// ───────────────────────────── filesystem ────────────────────────────────

/// Every entry under `root` (relative path) with its kind, size and mtime.
pub type FsSnapshot = BTreeMap<PathBuf, (bool, u64, SystemTime)>;

pub fn fs_snapshot(root: &Path) -> FsSnapshot {
    fn walk(root: &Path, dir: &Path, out: &mut FsSnapshot) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            out.insert(rel, (m.is_dir(), m.len(), m.modified().unwrap()));
            if m.is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut out = FsSnapshot::new();
    // The root itself: its mtime changes when an entry is created or removed.
    let m = std::fs::metadata(root).unwrap();
    out.insert(PathBuf::new(), (true, 0, m.modified().unwrap()));
    walk(root, root, &mut out);
    out
}

/// Recursive copy of a directory tree (regular files and directories).
pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        let dest = to.join(e.file_name());
        if p.is_dir() {
            copy_tree(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// Poll `f` until it returns true or `timeout` passes.
pub fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Standard base64 (with padding) decoder for model snapshots.
pub fn base64_decode(s: &str) -> Vec<u8> {
    fn val(c: u8) -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("invalid base64 byte {c}"),
        }
    }
    let bytes = s.as_bytes();
    assert_eq!(bytes.len() % 4, 0, "base64 length");
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= if c == b'=' { 0 } else { val(c) } << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    out
}
