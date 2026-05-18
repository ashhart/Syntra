use std::io::{Read as IoRead, Cursor};

pub(super) type Resp = tiny_http::Response<Cursor<Vec<u8>>>;

pub(super) const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn json_resp(status: u16, body: &str) -> Resp {
    tiny_http::Response::from_data(body.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

pub(super) fn text_resp(status: u16, body: &str) -> Resp {
    tiny_http::Response::from_data(body.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..]).unwrap())
}

pub(super) fn html_resp(status: u16, body: &str) -> Resp {
    tiny_http::Response::from_data(body.as_bytes().to_vec())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap())
}

pub(super) fn err_json(msg: &str) -> String {
    serde_json::json!({"error": msg}).to_string()
}

pub(super) fn ok_json(fields: serde_json::Value) -> String {
    let mut m = match fields {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    m.insert("ok".to_string(), serde_json::Value::Bool(true));
    serde_json::Value::Object(m).to_string()
}

pub(super) fn read_body_limited(request: &mut tiny_http::Request) -> Result<String, Resp> {
    let len = request.body_length().unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    let mut body = Vec::with_capacity(len.min(MAX_BODY_BYTES));
    request.as_reader().take(MAX_BODY_BYTES as u64 + 1).read_to_end(&mut body).ok();
    if body.len() > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    Ok(String::from_utf8_lossy(&body).to_string())
}

pub(super) fn read_body_bytes_limited(request: &mut tiny_http::Request) -> Result<Vec<u8>, Resp> {
    let len = request.body_length().unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    let mut body = Vec::with_capacity(len.min(MAX_BODY_BYTES));
    request.as_reader().take(MAX_BODY_BYTES as u64 + 1).read_to_end(&mut body).ok();
    if body.len() > MAX_BODY_BYTES {
        return Err(json_resp(413, r#"{"error":"payload too large"}"#));
    }
    Ok(body)
}
