//! Framework-independent request and response types.
//!
//! The router and every handler work on these types. The hyper adapter in
//! `serve.rs` converts to and from them, so swapping the HTTP stack never
//! touches a handler.

use bytes::Bytes;

/// Largest request body accepted, in bytes.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// An HTTP request with its body fully read.
#[derive(Debug, Clone)]
pub struct Request {
    /// Upper-case method, e.g. `GET`.
    pub method: String,
    /// Path without the query string, e.g. `/v1/tenants/acme`.
    pub path: String,
    /// Raw query string without the leading `?`; empty when absent.
    pub query: String,
    /// Header names are lower-case.
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    pub remote: Option<std::net::SocketAddr>,
    /// Echoed as `x-request-id` and attached to log lines.
    pub request_id: String,
}

impl Request {
    /// A request with no headers or body; used by tests.
    pub fn new(method: &str, target: &str) -> Self {
        let (path, query) = split_target(target);
        Request {
            method: method.to_ascii_uppercase(),
            path,
            query,
            headers: Vec::new(),
            body: Bytes::new(),
            remote: None,
            request_id: new_request_id(),
        }
    }

    /// First value of a header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// First value of a query parameter, percent-decoded.
    pub fn query_param(&self, key: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k) == key).then(|| percent_decode(v))
        })
    }

    /// The original request target (path plus query), for logs and links.
    pub fn target(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }

    /// The body as UTF-8 text.
    pub fn text(&self) -> Result<&str, Response> {
        std::str::from_utf8(&self.body)
            .map_err(|_| Response::error(400, "request body must be UTF-8"))
    }

    /// The body parsed as JSON. An empty body is an error: every JSON route
    /// expects a document, and silently treating `""` as `{}` hides client
    /// bugs.
    pub fn json(&self) -> Result<serde_json::Value, Response> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Err(Response::error(400, "request body must be a JSON document"));
        }
        serde_json::from_slice(&self.body)
            .map_err(|e| Response::error(400, &format!("invalid JSON: {e}")))
    }
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Bytes>) -> Self {
        Response {
            status,
            headers: vec![("content-type".to_string(), content_type.to_string())],
            body: body.into(),
        }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Response::new(status, "application/json", value.to_string())
    }

    /// `{"error": message}` with the given status.
    pub fn error(status: u16, message: &str) -> Self {
        Response::json(status, &serde_json::json!({ "error": message }))
    }

    pub fn text(status: u16, body: impl Into<Bytes>) -> Self {
        Response::new(status, "text/plain; charset=utf-8", body)
    }

    pub fn html(status: u16, body: impl Into<Bytes>) -> Self {
        Response::new(status, "text/html; charset=utf-8", body)
    }

    /// Add a header, replacing any existing value for the same name.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers
            .push((name.to_ascii_lowercase(), value.to_string()));
        self
    }
}

/// Handlers return `Result<Response, Response>` so `?` works on the error
/// response helpers; both arms are sent as-is.
pub type HandlerResult = Result<Response, Response>;

/// Split `/a/b?x=1` into (`/a/b`, `x=1`).
pub fn split_target(target: &str) -> (String, String) {
    match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    }
}

/// Decode `%XX` escapes and `+` in a query component. Invalid escapes are
/// kept literally rather than rejected.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A random 16-hex-digit request id.
pub fn new_request_id() -> String {
    format!("{:016x}", crate::decision::random_seed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_params_decode() {
        let mut r = Request::new("get", "/x?a=1&b=hello%20world&c=x+y&d");
        assert_eq!(r.method, "GET");
        assert_eq!(r.query_param("a").as_deref(), Some("1"));
        assert_eq!(r.query_param("b").as_deref(), Some("hello world"));
        assert_eq!(r.query_param("c").as_deref(), Some("x y"));
        assert_eq!(r.query_param("d").as_deref(), Some(""));
        assert_eq!(r.query_param("zz"), None);
        r.query = "bad=%zz&tail=%4".into();
        assert_eq!(r.query_param("bad").as_deref(), Some("%zz"));
        assert_eq!(r.query_param("tail").as_deref(), Some("%4"));
    }

    #[test]
    fn empty_json_body_is_an_error() {
        let r = Request::new("POST", "/x");
        assert_eq!(r.json().unwrap_err().status, 400);
    }

    #[test]
    fn headers_are_case_insensitive_and_replaced() {
        let resp = Response::text(200, "ok")
            .with_header("X-Thing", "a")
            .with_header("x-thing", "b");
        let values: Vec<_> = resp
            .headers
            .iter()
            .filter(|(k, _)| k == "x-thing")
            .collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].1, "b");
    }
}
