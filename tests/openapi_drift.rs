//! OpenAPI drift guard: `docs/openapi.yaml` must describe exactly the server
//! in `src/server`.
//!
//! - The document is well formed: it parses, every `$ref` resolves,
//!   operation ids are unique, path parameters are declared, examples match
//!   their schemas, and every authenticated operation lists the 401, 403,
//!   413 and 429 answers it can give.
//! - Routes, both directions: the (method, path) pairs matched in
//!   `src/server/routes.rs` are exactly the documented operations.
//! - Dispatch, both directions, against an in-process server: every
//!   documented operation is routed (also through its deprecated unversioned
//!   alias, which must carry `Deprecation` and `Link`), and every other
//!   method on a documented path, or one segment deeper, gets the router's
//!   not-found answer.
//! - Request schemas against the real serde types: `DecideRequest` and
//!   `RewardRequest` document exactly the fields the server accepts (read
//!   from serde's unknown-field error), a request using every documented
//!   field succeeds, and `DecisionSpec` and `Policy` match
//!   `syntra::decision::DecisionSpec` and `syntra::context::POLICY_KEYS` in
//!   fields, defaults, enums and ranges. Upload items (`decisions:batch`,
//!   `rewards:batch`) are checked the same way, with a decision made
//!   in-process from the published model, as an SDK makes it; and
//!   `EvaluateRequest` against the evaluator's defaults and limits.
//! - Responses: a scripted session that reaches every documented success
//!   response validates each real response against its documented schema (an
//!   undocumented property fails unless `additionalProperties` allows it),
//!   and every status it sees must be documented for its operation.
//! - `x-scopes`: each operation answers 403 to exactly the token scopes it
//!   does not list.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};
use syntra::server::http::{Request, Response};

const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
const SCOPES: &[&str] = &["admin", "tenant_admin", "read"];

// ── The document ─────────────────────────────────────────────────────────

fn manifest_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn doc() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| {
        let text = std::fs::read_to_string(manifest_path("docs/openapi.yaml"))
            .expect("read docs/openapi.yaml");
        serde_norway::from_str(&text).expect("docs/openapi.yaml is valid YAML")
    })
}

fn lookup(reference: &str) -> Option<&'static Value> {
    doc().pointer(reference.strip_prefix('#')?)
}

/// Follow `$ref` chains.
fn resolve(mut v: &'static Value) -> &'static Value {
    for _ in 0..16 {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => v = lookup(r).unwrap_or_else(|| panic!("unresolved $ref {r}")),
            None => return v,
        }
    }
    panic!("$ref chain too deep");
}

fn schema(name: &str) -> &'static Value {
    lookup(&format!("#/components/schemas/{name}"))
        .unwrap_or_else(|| panic!("no schema {name} in docs/openapi.yaml"))
}

fn properties(s: &'static Value) -> &'static Map<String, Value> {
    resolve(s)["properties"]
        .as_object()
        .expect("schema has properties")
}

fn property_names(s: &'static Value) -> BTreeSet<String> {
    properties(s).keys().cloned().collect()
}

struct Operation {
    method: String,
    path: String,
    op: &'static Value,
    path_item: &'static Value,
}

impl Operation {
    fn infra(&self) -> bool {
        !self.path.starts_with("/v1/")
    }

    fn scopes(&self) -> BTreeSet<String> {
        self.op["x-scopes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| s.as_str().expect("scope name").to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The documented response for a status, `$ref` resolved.
    fn response(&self, status: u16) -> Option<&'static Value> {
        self.op["responses"].get(status.to_string()).map(resolve)
    }

    fn label(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

fn operations() -> Vec<Operation> {
    let mut out = Vec::new();
    for (path, item) in doc()["paths"].as_object().expect("paths mapping") {
        for (key, op) in item.as_object().expect("path item mapping") {
            let method = key.to_ascii_uppercase();
            if METHODS.contains(&method.as_str()) || key == "trace" {
                out.push(Operation {
                    method,
                    path: path.clone(),
                    op,
                    path_item: item,
                });
            }
        }
    }
    assert!(!out.is_empty(), "no operations in docs/openapi.yaml");
    out
}

/// A path template with every parameter written `{}`.
fn shape(template: &str) -> String {
    template
        .split('/')
        .map(|s| if s.starts_with('{') { "{}" } else { s })
        .collect::<Vec<_>>()
        .join("/")
}

/// A concrete path for a template: `{name}` becomes `probe-name`.
fn concrete(template: &str) -> String {
    template
        .split('/')
        .map(
            |s| match s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) => format!("probe-{name}"),
                None => s.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("/")
}

fn template_matches(template: &str, path: &str) -> bool {
    let (t, p): (Vec<_>, Vec<_>) = (template.split('/').collect(), path.split('/').collect());
    t.len() == p.len() && t.iter().zip(&p).all(|(t, p)| t.starts_with('{') || t == p)
}

fn operation_for(method: &str, path: &str) -> Option<Operation> {
    operations()
        .into_iter()
        .find(|o| o.method == method && template_matches(&o.path, path))
}

// ── The router, read from its source ─────────────────────────────────────

struct RouterRoutes {
    /// `(method, path shape)` of every API route, served under `/v1`.
    api: BTreeSet<(String, String)>,
    /// Unversioned paths matched before authentication, with their aliases.
    infra: Vec<(String, Vec<String>)>,
    /// Error messages of the router's unknown-route answers.
    not_found: BTreeSet<String>,
}

fn router() -> &'static RouterRoutes {
    static ROUTES: OnceLock<RouterRoutes> = OnceLock::new();
    ROUTES.get_or_init(parse_router)
}

fn function_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("\nfn {name}("))
        .unwrap_or_else(|| panic!("fn {name} not found in src/server/routes.rs"));
    let body = &src[start + 1..];
    &body[..body.find("\n}\n").expect("end of function")]
}

/// A `("METHOD", [segments])` or `(_, [segments])` tuple pattern: the
/// method (`*` for `_`), the segments (`None` for a binding) and whether it
/// ends in `rest @ ..`.
type Pattern = (String, Vec<Option<String>>, bool);

fn tuple_patterns(text: &str) -> Vec<Pattern> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices('(') {
        let after = &text[at + 1..];
        let (method, rest) = if let Some(r) = after.strip_prefix('"') {
            let Some(end) = r.find('"') else { continue };
            let m = &r[..end];
            if m.is_empty() || !m.chars().all(|c| c.is_ascii_uppercase()) {
                continue;
            }
            (m.to_string(), &r[end + 1..])
        } else if let Some(r) = after.strip_prefix('_') {
            ("*".to_string(), r)
        } else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix(',') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('[') else {
            continue;
        };
        let close = rest.find(']').expect("closing ] of a route pattern");
        if !rest[close + 1..].trim_start().starts_with(')') {
            continue;
        }
        let mut segments = Vec::new();
        let mut tail = false;
        for token in rest[..close]
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            if let Some(lit) = token.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                segments.push(Some(lit.to_string()));
            } else if token.ends_with("@ ..") {
                tail = true;
            } else if token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                segments.push(None);
            } else {
                panic!("unrecognized route segment {token:?} in src/server/routes.rs");
            }
        }
        out.push((method, segments, tail));
    }
    out
}

/// Fail closed: every match arm that starts with a tuple must be a route
/// pattern this parser understands.
fn check_arms(body: &str, function: &str) {
    for line in body.lines().map(str::trim) {
        let arm = (line.starts_with('(') || line.starts_with("| (")) && line.contains("=>");
        if arm && tuple_patterns(line).is_empty() {
            panic!(
                "unrecognized match arm in {function} (src/server/routes.rs): {line}\n\
                 teach tests/openapi_drift.rs to read it"
            );
        }
    }
}

fn join(segments: &[Option<String>]) -> String {
    segments
        .iter()
        .map(|s| s.as_deref().unwrap_or("{}"))
        .collect::<Vec<_>>()
        .join("/")
}

fn parse_router() -> RouterRoutes {
    let src = std::fs::read_to_string(manifest_path("src/server/routes.rs"))
        .expect("read src/server/routes.rs");

    // Infra paths: the string arms of `match req.path.as_str()`.
    let inner = function_body(&src, "route_inner");
    let region = &inner[inner
        .find("match req.path.as_str()")
        .expect("infra match in route_inner")..];
    let region = &region[..region.find("_ => {}").expect("infra fallback arm")];
    let mut infra = Vec::new();
    for line in region.lines().filter(|l| l.contains("=>")) {
        let pattern = &line[..line.find("=>").unwrap()];
        let literals: Vec<String> = pattern
            .split('"')
            .skip(1)
            .step_by(2)
            .map(String::from)
            .collect();
        assert!(
            !literals.is_empty() && literals.iter().all(|l| l.starts_with('/')),
            "unrecognized infra arm in route_inner: {line}"
        );
        infra.push((literals[0].clone(), literals[1..].to_vec()));
    }

    let dispatch = function_body(&src, "dispatch");
    let capsule = function_body(&src, "capsule_route");
    check_arms(dispatch, "dispatch");
    check_arms(capsule, "capsule_route");

    let mut api = BTreeSet::new();
    let mut prefix = None;
    for (method, segments, tail) in tuple_patterns(dispatch) {
        if tail {
            assert!(prefix.is_none(), "two `rest @ ..` routes in dispatch");
            prefix = Some(segments);
            continue;
        }
        assert_ne!(method, "*", "a wildcard-method route in dispatch");
        api.insert((method, format!("/v1/{}", join(&segments))));
    }
    let prefix = prefix.expect("the capsule route prefix in dispatch");
    for (method, segments, tail) in tuple_patterns(capsule) {
        assert!(!tail && method != "*", "unexpected capsule route pattern");
        let mut all = prefix.clone();
        all.extend(segments);
        api.insert((method, format!("/v1/{}", join(&all))));
    }

    let mut not_found = BTreeSet::new();
    for (at, _) in src.match_indices("\"not_found\"") {
        let rest = &src[at..];
        let start = rest
            .find("Response::error(404, \"")
            .expect("not_found arm answers 404")
            + "Response::error(404, \"".len();
        let end = rest[start..].find('"').expect("closing quote");
        not_found.insert(rest[start..start + end].to_string());
    }
    assert!(!api.is_empty() && !infra.is_empty() && !not_found.is_empty());
    RouterRoutes {
        api,
        infra,
        not_found,
    }
}

fn is_not_found(resp: &Response) -> bool {
    // Personalizer routes answer `{"error": {"code", "message"}}`.
    resp.status == 404
        && serde_json::from_slice::<Value>(&resp.body)
            .ok()
            .and_then(|v| {
                v["error"]
                    .as_str()
                    .or_else(|| v["error"]["message"].as_str())
                    .map(String::from)
            })
            .is_some_and(|e| router().not_found.contains(&e))
}

// ── An in-process server ─────────────────────────────────────────────────

struct Srv {
    state: syntra::server::state::State,
    root: PathBuf,
}

impl Srv {
    fn new(label: &str, admin_key: Option<&str>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "syntra-openapi-{label}-{}-{:x}",
            std::process::id(),
            syntra::decision::random_seed()
        ));
        let state = syntra::server::build_state(&syntra::server::ServerConfig {
            addr: String::new(),
            store_path: root.to_string_lossy().into_owned(),
            admin_key: admin_key.map(String::from),
            service_name: None,
            ..Default::default()
        })
        .expect("build server state");
        Srv { state, root }
    }

    fn call(&self, method: &str, target: &str, body: Option<&[u8]>, key: Option<&str>) -> Response {
        self.call_with(method, target, body, key, &[])
    }

    fn call_with(
        &self,
        method: &str,
        target: &str,
        body: Option<&[u8]>,
        key: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut req = Request::new(method, target);
        if let Some(b) = body {
            req.body = b.to_vec().into();
        }
        if let Some(k) = key {
            req.headers
                .push(("authorization".into(), format!("Bearer {k}")));
        }
        for (name, value) in headers {
            req.headers
                .push((name.to_ascii_lowercase(), value.to_string()));
        }
        syntra::server::handle(&self.state, &req)
    }

    fn post_json(&self, target: &str, body: &Value) -> (u16, Value) {
        let r = self.call("POST", target, Some(body.to_string().as_bytes()), None);
        (r.status, body_json(&r))
    }
}

/// An upload item for a decision made in-process, the way an SDK makes it:
/// the published model (`GET .../model?snapshot=true`) restored into an
/// [`syntra::decision::Engine`] and asked to decide.
fn local_decision(
    srv: &Srv,
    key: Option<&str>,
    capsule: &str,
    id: &str,
    input: &Value,
    seed: u64,
) -> Value {
    let r = srv.call("GET", &format!("{capsule}/model?snapshot=true"), None, key);
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let model = body_json(&r);
    let spec = syntra::decision::DecisionSpec::from_json(&model["spec"]).expect("published spec");
    let bytes = syntra::client::base64_decode(model["snapshot"].as_str().expect("snapshot"))
        .expect("base64 snapshot");
    let engine = syntra::decision::Engine::restore(spec, &bytes).expect("restore snapshot");
    let decide_input = syntra::decision::DecideInput {
        context: input["context"].clone(),
        actions: serde_json::from_value(input["actions"].clone()).expect("actions"),
        excluded: serde_json::from_value(input["excludedActions"].clone()).unwrap_or_default(),
        baseline: input["baselineAction"].as_str().map(String::from),
        ..Default::default()
    };
    let d = engine.decide(&decide_input, seed).expect("local decide");
    json!({
        "decisionId": id,
        "tsMs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        "modelTag": model["modelTag"],
        "modelVersion": model["modelVersion"],
        "seed": seed.to_string(),
        "input": input,
        "chosenIndex": d.chosen,
        "chosenId": d.chosen_action().id,
        "probability": d.probability,
        "pmf": d.pmf,
        "eligible": d.eligible,
    })
}

impl Drop for Srv {
    fn drop(&mut self) {
        self.state.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn header<'a>(resp: &'a Response, name: &str) -> Option<&'a str> {
    resp.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn body_json(resp: &Response) -> Value {
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

/// A probe body: `{}` for methods that take one.
fn probe_body(method: &str) -> Option<&'static [u8]> {
    matches!(method, "POST" | "PUT" | "PATCH").then_some(b"{}".as_slice())
}

// ── A small schema validator (the subset the document uses) ─────────────

/// Errors from validating `v` against `s`. Unlike JSON Schema, an object
/// schema with `properties` and no `additionalProperties` is closed: a
/// response must not carry properties the document does not mention.
fn validate(s: &'static Value, v: &Value, at: &str, errors: &mut Vec<String>) {
    let s = resolve(s);
    if v.is_null() {
        let typed = s.get("type").is_some() || s.get("properties").is_some();
        if typed && s.get("nullable") != Some(&Value::Bool(true)) {
            errors.push(format!("{at}: null is not allowed"));
        }
        return;
    }
    if let Some(t) = s.get("type").and_then(Value::as_str) {
        let ok = match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            other => panic!("{at}: unsupported schema type {other}"),
        };
        if !ok {
            errors.push(format!("{at}: expected {t}, got {v}"));
            return;
        }
    }
    if let Some(allowed) = s.get("enum").and_then(Value::as_array)
        && !allowed.contains(v)
    {
        errors.push(format!("{at}: {v} is not one of {allowed:?}"));
    }
    if let Some(x) = v.as_f64() {
        if let Some(min) = s.get("minimum").and_then(Value::as_f64) {
            let exclusive = s.get("exclusiveMinimum") == Some(&Value::Bool(true));
            if x < min || (exclusive && x == min) {
                errors.push(format!("{at}: {x} is below the minimum {min}"));
            }
        }
        if let Some(max) = s.get("maximum").and_then(Value::as_f64)
            && x > max
        {
            errors.push(format!("{at}: {x} is above the maximum {max}"));
        }
    }
    if let Some(text) = v.as_str() {
        let n = text.chars().count() as u64;
        if s.get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| n < m)
            || s.get("maxLength")
                .and_then(Value::as_u64)
                .is_some_and(|m| n > m)
        {
            errors.push(format!("{at}: length {n} out of range"));
        }
    }
    if let Some(items) = v.as_array() {
        let n = items.len() as u64;
        if s.get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|m| n < m)
            || s.get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|m| n > m)
        {
            errors.push(format!("{at}: {n} items out of range"));
        }
        if let Some(item) = s.get("items") {
            for (i, x) in items.iter().enumerate() {
                validate(item, x, &format!("{at}[{i}]"), errors);
            }
        }
    }
    if let Some(obj) = v.as_object() {
        for name in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = name.as_str().expect("required entries are strings");
            if !obj.contains_key(name) {
                errors.push(format!("{at}: missing required property {name}"));
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        let extra = s.get("additionalProperties");
        for (k, x) in obj {
            match (props.and_then(|p| p.get(k)), extra) {
                (Some(p), _) => validate(p, x, &format!("{at}.{k}"), errors),
                (None, Some(Value::Bool(true))) => {}
                (None, Some(e @ Value::Object(_))) => validate(e, x, &format!("{at}.{k}"), errors),
                (None, None) if props.is_none() => {}
                (None, _) => errors.push(format!("{at}: undocumented property {k}")),
            }
        }
    }
    if let Some(variants) = s.get("oneOf").and_then(Value::as_array) {
        let matching = variants
            .iter()
            .filter(|variant| {
                let mut e = Vec::new();
                validate(variant, v, at, &mut e);
                e.is_empty()
            })
            .count();
        if matching != 1 {
            errors.push(format!("{at}: matches {matching} oneOf variants, not 1"));
        }
    }
}

fn assert_valid(s: &'static Value, v: &Value, what: &str) {
    let mut errors = Vec::new();
    validate(s, v, "$", &mut errors);
    assert!(
        errors.is_empty(),
        "{what} does not match its schema:\n  {}\n{v:#}",
        errors.join("\n  ")
    );
}

// ── 1. The document is well formed ───────────────────────────────────────

fn walk_refs(v: &'static Value, out: &mut Vec<&'static str>) {
    match v {
        Value::Object(m) => {
            if let Some(r) = m.get("$ref").and_then(Value::as_str) {
                out.push(r);
            }
            m.values().for_each(|x| walk_refs(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| walk_refs(x, out)),
        _ => {}
    }
}

fn walk_examples(v: &'static Value, at: String, out: &mut Vec<(String, &'static Value)>) {
    if let Value::Object(m) = v {
        if m.contains_key("example") && (m.contains_key("type") || m.contains_key("properties")) {
            out.push((at.clone(), v));
        }
        for (k, x) in m {
            walk_examples(x, format!("{at}/{k}"), out);
        }
    }
}

#[test]
fn document_is_well_formed() {
    let d = doc();
    assert_eq!(d["openapi"], "3.0.3");
    assert_eq!(
        d["info"]["version"],
        env!("CARGO_PKG_VERSION"),
        "info.version must match the crate version"
    );
    let description = d["info"]["description"].as_str().unwrap_or("");
    for needle in ["Deprecation: true", "successor-version", "/v1"] {
        assert!(
            description.contains(needle),
            "info.description must document the unversioned aliases ({needle})"
        );
    }

    let mut refs = Vec::new();
    walk_refs(d, &mut refs);
    for r in refs {
        assert!(lookup(r).is_some(), "unresolved $ref {r}");
    }

    let schemes: BTreeSet<&str> = d["components"]["securitySchemes"]
        .as_object()
        .expect("security schemes")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(schemes, BTreeSet::from(["bearerAuth", "subscriptionKey"]));
    assert_eq!(
        d["components"]["securitySchemes"]["subscriptionKey"]["name"],
        "Ocp-Apim-Subscription-Key"
    );

    let mut ids = BTreeSet::new();
    for op in operations() {
        let label = op.label();
        let id = op.op["operationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: no operationId"));
        assert!(ids.insert(id.to_string()), "duplicate operationId {id}");
        let responses = op.op["responses"]
            .as_object()
            .unwrap_or_else(|| panic!("{label}: no responses"));
        for (status, r) in responses {
            status
                .parse::<u16>()
                .unwrap_or_else(|_| panic!("{label}: bad status {status}"));
            assert!(
                resolve(r)["description"].is_string(),
                "{label} {status}: no description"
            );
        }

        // Path parameters: declared exactly once per template parameter.
        let declared: BTreeSet<String> = op.path_item["parameters"]
            .as_array()
            .into_iter()
            .chain(op.op["parameters"].as_array())
            .flatten()
            .map(resolve)
            .filter(|p| p["in"] == "path")
            .map(|p| p["name"].as_str().expect("parameter name").to_string())
            .collect();
        let used: BTreeSet<String> = op
            .path
            .split('/')
            .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .map(String::from)
            .collect();
        assert_eq!(declared, used, "{label}: path parameters");

        // /metrics names every tenant, job and capsule, so it takes an
        // admin credential; the other infra routes take none.
        if op.infra() && op.path != "/metrics" {
            assert_eq!(
                op.op["security"],
                json!([]),
                "{label}: infra routes need no credential"
            );
            continue;
        }
        let scopes = op.scopes();
        assert!(
            !scopes.is_empty() && scopes.iter().all(|s| SCOPES.contains(&s.as_str())),
            "{label}: x-scopes must list some of {SCOPES:?}"
        );
        let mut must = vec![401, 429];
        if scopes.len() < SCOPES.len() {
            must.push(403);
        }
        if op.op.get("requestBody").is_some() {
            must.push(413);
        }
        for status in must {
            assert!(
                op.response(status).is_some(),
                "{label}: must document {status}"
            );
        }
        if op.path.contains("{tenant}") {
            assert!(
                op.response(400).is_some(),
                "{label}: invalid names answer 400"
            );
        }
    }

    let mut examples = Vec::new();
    walk_examples(d, String::new(), &mut examples);
    assert!(examples.len() > 10, "examples: {}", examples.len());
    for (at, s) in examples {
        assert_valid(s, &s["example"], &format!("example at {at}"));
    }
}

// ── 2. Routes, both directions ───────────────────────────────────────────

#[test]
fn documented_operations_are_exactly_the_routes_in_routes_rs() {
    let r = router();
    let mut routed: BTreeSet<(String, String)> = r.api.clone();
    for (path, _) in &r.infra {
        // Infra arms match on the path alone; the document lists GET.
        routed.insert(("GET".to_string(), path.clone()));
    }
    let documented: BTreeSet<(String, String)> = operations()
        .iter()
        .map(|o| (o.method.clone(), shape(&o.path)))
        .collect();
    let undocumented: Vec<_> = routed.difference(&documented).collect();
    let unrouted: Vec<_> = documented.difference(&routed).collect();
    assert!(
        undocumented.is_empty() && unrouted.is_empty(),
        "routes.rs and docs/openapi.yaml disagree\n  routed but not documented: {undocumented:?}\n  documented but not routed: {unrouted:?}"
    );

    // Infra aliases (for example /v1/admin) are named in the description of
    // the path they alias.
    for (path, aliases) in &r.infra {
        let op = operation_for("GET", path).expect("infra operation");
        let text = op.op["description"].as_str().unwrap_or("");
        for alias in aliases {
            assert!(
                text.contains(alias.as_str()),
                "GET {path}: its description must mention the alias {alias}"
            );
        }
    }
}

// ── 3. Dispatch, both directions ─────────────────────────────────────────

#[test]
fn every_documented_operation_is_dispatched_and_nothing_else() {
    let srv = Srv::new("dispatch", None);
    let ops = operations();

    // Positive controls: the detector recognizes the router's not-found.
    for path in ["/v1/zzz-probe", "/v1/tenants/t/jobs/j/capsules/c/zzz-probe"] {
        assert!(is_not_found(&srv.call("GET", path, None, None)), "{path}");
    }

    let templates: BTreeSet<&str> = ops.iter().map(|o| o.path.as_str()).collect();
    let mut probes = 0;
    for template in templates {
        let base = concrete(template);
        for path in [base.clone(), format!("{base}/zzz-probe")] {
            for &method in METHODS {
                let documented = ops
                    .iter()
                    .any(|o| o.method == method && template_matches(&o.path, &path));
                if !template.starts_with("/v1/") && path == base && method != "GET" {
                    // Infra paths answer every method; only GET is documented.
                    continue;
                }
                let resp = srv.call(method, &path, probe_body(method), None);
                probes += 1;
                assert_eq!(
                    !is_not_found(&resp),
                    documented,
                    "{method} {path}: routed = {}, documented = {documented} ({} {})",
                    !is_not_found(&resp),
                    resp.status,
                    String::from_utf8_lossy(&resp.body)
                );
                let Some(alias) = path.strip_prefix("/v1").filter(|_| documented) else {
                    continue;
                };
                let resp = srv.call(method, alias, probe_body(method), None);
                assert!(
                    !is_not_found(&resp),
                    "{method} {alias}: the unversioned alias is not routed"
                );
                assert_eq!(
                    header(&resp, "deprecation"),
                    Some("true"),
                    "{method} {alias}"
                );
                assert_eq!(
                    header(&resp, "link"),
                    Some(format!("</v1{alias}>; rel=\"successor-version\"").as_str()),
                    "{method} {alias}"
                );
            }
        }
    }
    assert!(probes > 200, "only {probes} probes");

    // Infra aliases answer like the path they alias, without deprecation.
    for (path, aliases) in &router().infra {
        let want = srv.call("GET", path, None, None);
        for alias in aliases {
            let got = srv.call("GET", alias, None, None);
            assert_eq!(
                (got.status, header(&got, "content-type")),
                (want.status, header(&want, "content-type")),
                "{alias}"
            );
            assert_eq!(header(&got, "deprecation"), None, "{alias}");
        }
    }
}

// ── 4. Request schemas against the real serde types ──────────────────────

/// The field names serde lists in an unknown-field error.
fn serde_fields(error: &str) -> BTreeSet<String> {
    let (_, list) = error
        .split_once("expected")
        .unwrap_or_else(|| panic!("no field list in {error:?}"));
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(String::from)
        .collect()
}

/// An object with every documented property set to its `example`.
fn from_property_examples(s: &'static Value, skip: &[&str]) -> Value {
    let mut m = Map::new();
    for (name, p) in properties(s) {
        if skip.contains(&name.as_str()) {
            continue;
        }
        let example = p
            .get("example")
            .or_else(|| resolve(p).get("example"))
            .unwrap_or_else(|| panic!("property {name} has no example"));
        m.insert(name.clone(), example.clone());
    }
    Value::Object(m)
}

const CAPSULE: &str = "/v1/tenants/acme/jobs/routing/capsules/router";

#[test]
fn decide_and_reward_schemas_match_the_server_types() {
    let srv = Srv::new("requests", None);
    let spec = &schema("DecisionSpec")["example"];
    let r = srv.call(
        "PUT",
        &format!("{CAPSULE}/spec"),
        Some(spec.to_string().as_bytes()),
        None,
    );
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let post = |tail: &str, body: &Value| {
        let r = srv.call(
            "POST",
            &format!("{CAPSULE}/{tail}"),
            Some(body.to_string().as_bytes()),
            None,
        );
        (r.status, body_json(&r))
    };

    // Documented fields are exactly the fields serde accepts (aliases too).
    for (tail, name) in [("decide", "DecideRequest"), ("reward", "RewardRequest")] {
        let (status, body) = post(tail, &json!({ "zzzUndocumented": 1 }));
        assert_eq!(status, 400, "{body}");
        let accepted = serde_fields(body["error"].as_str().expect("error message"));
        assert_eq!(
            property_names(schema(name)),
            accepted,
            "{name}: documented properties vs the fields the server's type accepts"
        );
    }

    // Every documented field at once, and the schema-level example.
    let full = from_property_examples(schema("DecideRequest"), &[]);
    let (status, decided) = post("decide", &full);
    assert_eq!(status, 200, "{full}: {decided}");
    assert_valid(schema("DecideResponse"), &decided, "decide response");
    assert_eq!(decided["decisionId"], full["eventId"]);
    assert_eq!(decided["action"], "small", "large is excluded");
    let (status, other) = post("decide", &schema("DecideRequest")["example"]);
    assert_eq!(
        status, 409,
        "the example reuses the eventId with another body: {other}"
    );

    let mut reward = from_property_examples(schema("RewardRequest"), &["value"]);
    reward["decisionId"] = decided["decisionId"].clone();
    let (status, rewarded) = post("reward", &reward);
    assert_eq!(status, 200, "{reward}: {rewarded}");
    assert_valid(schema("RewardResponse"), &rewarded, "reward response");
    assert_eq!(rewarded["applied"], true);
    // Under `rewards: first` a second reward is a duplicate whatever its key.
    let again =
        json!({ "decisionId": decided["decisionId"], "reward": 0, "idempotencyKey": "other" });
    let (status, body) = post("reward", &again);
    assert_eq!((status, &body["duplicate"]), (200, &json!(true)), "{body}");
    let (_, second) = post("decide", &json!({ "context": {} }));
    let alias = json!({ "decisionId": second["decisionId"], "value": 0.5 });
    let (status, body) = post("reward", &alias);
    assert_eq!((status, &body["applied"]), (200, &json!(true)), "{body}");
    let both = json!({ "decisionId": second["decisionId"], "value": 0.5, "reward": 1 });
    assert_eq!(post("reward", &both).0, 400, "reward and value together");
}

/// The first rejection's error for a one-item upload.
fn upload_error(srv: &Srv, capsule: &str, item: Value) -> String {
    let (status, body) = srv.post_json(
        &format!("{capsule}/decisions:batch"),
        &json!({ "decisions": [item] }),
    );
    assert_eq!(status, 200, "{body}");
    assert_valid(
        schema("DecisionUploadResponse"),
        &body,
        "decisions:batch response",
    );
    assert_eq!(body["accepted"], 0, "{body}");
    body["rejected"][0]["error"]
        .as_str()
        .expect("rejection error")
        .to_string()
}

#[test]
fn upload_schemas_match_the_server_types() {
    let srv = Srv::new("uploads", None);
    let spec = &schema("DecisionSpec")["example"];
    let r = srv.call(
        "PUT",
        &format!("{CAPSULE}/spec"),
        Some(spec.to_string().as_bytes()),
        None,
    );
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));

    // A decision made in-process with every documented field, input
    // included, is accepted; its retry is a duplicate.
    let input = from_property_examples(schema("UploadInput"), &[]);
    let item = local_decision(
        &srv,
        None,
        CAPSULE,
        "loc_every_field",
        &input,
        1234567890123,
    );
    assert_eq!(
        property_names(schema("UploadedDecision")),
        item.as_object().unwrap().keys().cloned().collect(),
        "the item sets every documented UploadedDecision field"
    );
    assert_valid(schema("UploadedDecision"), &item, "upload item");
    let batch = format!("{CAPSULE}/decisions:batch");
    let (status, body) = srv.post_json(&batch, &json!({ "decisions": [item.clone()] }));
    assert_eq!(status, 200, "{body}");
    assert_valid(
        schema("DecisionUploadResponse"),
        &body,
        "decisions:batch response",
    );
    assert_eq!(
        (&body["accepted"], &body["duplicates"]),
        (&json!(1), &json!(0)),
        "{body}"
    );
    let (_, body) = srv.post_json(&batch, &json!({ "decisions": [item.clone()] }));
    assert_eq!(
        (&body["accepted"], &body["duplicates"]),
        (&json!(1), &json!(1)),
        "{body}"
    );
    let r = srv.call(
        "GET",
        &format!("{CAPSULE}/decisions/loc_every_field"),
        None,
        None,
    );
    assert_eq!(r.status, 200, "the accepted upload is in the log");
    assert_valid(schema("Decision"), &body_json(&r), "uploaded decision");

    // A probability the model did not produce does not replay.
    let mut forged = local_decision(&srv, None, CAPSULE, "loc_forged", &input, 99);
    forged["probability"] = json!(1.0);
    assert!(upload_error(&srv, CAPSULE, forged).contains("does not replay"));

    // Documented fields are exactly the fields serde accepts, at both levels.
    let mut extra = item.clone();
    extra["decisionId"] = json!("loc_extra");
    extra["zzzUndocumented"] = json!(1);
    let accepted = serde_fields(&upload_error(&srv, CAPSULE, extra));
    assert_eq!(
        property_names(schema("UploadedDecision")),
        accepted,
        "UploadedDecision"
    );
    let mut extra = item.clone();
    extra["decisionId"] = json!("loc_extra_input");
    extra["input"]["zzzUndocumented"] = json!(1);
    let accepted = serde_fields(&upload_error(&srv, CAPSULE, extra));
    assert_eq!(
        property_names(schema("UploadInput")),
        accepted,
        "UploadInput"
    );

    // Rewards: an applied item, an unparsable one (serde lists the fields)
    // and one for an unknown decision.
    let mut reward = from_property_examples(schema("RewardUploadItem"), &["value"]);
    reward["decisionId"] = json!("loc_every_field");
    let (status, body) = srv.post_json(
        &format!("{CAPSULE}/rewards:batch"),
        &json!({ "rewards": [reward, {"zzzUndocumented": 1}, {"decisionId": "nope", "value": 1}] }),
    );
    assert_eq!(status, 200, "{body}");
    assert_valid(
        schema("RewardUploadResponse"),
        &body,
        "rewards:batch response",
    );
    let results = body["results"].as_array().expect("results");
    assert_eq!(results[0]["applied"], true, "{body}");
    let accepted = serde_fields(results[1]["error"].as_str().expect("error"));
    assert_eq!(
        property_names(schema("RewardUploadItem")),
        accepted,
        "RewardUploadItem"
    );
    assert_eq!(
        (&results[2]["ok"], &results[2]["status"]),
        (&json!(false), &json!(404)),
        "{body}"
    );

    // Batch limits.
    let max = schema("DecisionUploadRequest")["properties"]["decisions"]["maxItems"]
        .as_u64()
        .unwrap() as usize;
    let too_many = json!({ "decisions": vec![json!({}); max + 1] });
    assert_eq!(srv.post_json(&batch, &too_many).0, 413);
    let too_many = json!({ "rewards": vec![json!({}); max + 1] });
    assert_eq!(
        srv.post_json(&format!("{CAPSULE}/rewards:batch"), &too_many)
            .0,
        413
    );
}

/// Every property path in a schema, `$ref`s resolved, with its schema.
fn schema_leaves(s: &'static Value, at: &str, out: &mut Vec<(String, &'static Value)>) {
    let s = resolve(s);
    if let Some(props) = s.get("properties").and_then(Value::as_object) {
        for (k, p) in props {
            let path = if at.is_empty() {
                k.clone()
            } else {
                format!("{at}.{k}")
            };
            let p = resolve(p);
            if p.get("properties").is_some() {
                schema_leaves(p, &path, out);
            } else {
                out.push((path, p));
            }
        }
    }
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| v.get(k))
}

fn patch_at(path: &str, value: Value) -> Value {
    path.rsplit('.').fold(value, |v, k| json!({ k: v }))
}

fn numbers_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| numbers_equal(x, y))
        }
        _ => a == b,
    }
}

/// Values just inside and just outside a numeric property's documented
/// range, with whether each must be accepted.
fn range_cases(p: &Value) -> Vec<(Value, bool)> {
    let integer = p["type"] == "integer";
    let exclusive = p.get("exclusiveMinimum") == Some(&Value::Bool(true));
    let mut cases = Vec::new();
    let nudge = |x: f64, up: bool| {
        let d = (x.abs() * 1e-9).max(1e-9);
        if up { x + d } else { x - d }
    };
    if let Some(min) = p.get("minimum") {
        cases.push((min.clone(), !exclusive));
        let below = if integer {
            min.as_i64().map(|m| json!(m - 1)).unwrap_or(json!(-1))
        } else {
            json!(nudge(min.as_f64().unwrap(), false))
        };
        cases.push((below, false));
    }
    if let Some(max) = p.get("maximum") {
        cases.push((max.clone(), true));
        let above = match (integer, max.as_i64()) {
            (true, Some(m)) if m < i64::MAX => json!(m + 1),
            // Beyond i64 (the u64 seed): a float, which an integer rejects.
            _ => json!(nudge(max.as_f64().unwrap(), true)),
        };
        cases.push((above, false));
    }
    cases
}

#[test]
fn decision_spec_schema_matches_the_rust_type() {
    use syntra::decision::{ActionSpec, DecisionSpec};
    let mut full = DecisionSpec {
        seed: Some(7),
        ..DecisionSpec::default()
    };
    // Every optional field set, so each one appears.
    full.reward.default = Some(0.0);
    let mut action = ActionSpec::new("a");
    action.features.insert("cost".into(), json!(1));
    full.actions = vec![action];
    let full = full.to_json();
    let defaults = DecisionSpec::default().to_json();

    // Fields, both directions, at every level.
    let mut leaves = Vec::new();
    schema_leaves(schema("DecisionSpec"), "", &mut leaves);
    let documented: BTreeSet<&str> = leaves.iter().map(|(p, _)| p.as_str()).collect();
    let mut actual = BTreeSet::new();
    fn object_leaves(v: &Value, at: &str, out: &mut BTreeSet<String>) {
        for (k, x) in v.as_object().into_iter().flatten() {
            let path = if at.is_empty() {
                k.clone()
            } else {
                format!("{at}.{k}")
            };
            if x.is_object() {
                object_leaves(x, &path, out);
            } else {
                out.insert(path);
            }
        }
    }
    object_leaves(&full, "", &mut actual);
    let actual: BTreeSet<&str> = actual.iter().map(String::as_str).collect();
    assert_eq!(
        documented, actual,
        "DecisionSpec properties vs the Rust type"
    );
    assert_eq!(
        property_names(schema("Action")),
        full["actions"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect(),
        "Action properties vs ActionSpec"
    );

    // Defaults: every documented default is the real one, and every field
    // with a default documents it.
    for (path, p) in &leaves {
        match (p.get("default"), get_path(&defaults, path)) {
            (Some(doc_default), Some(real)) => assert!(
                numbers_equal(doc_default, real),
                "{path}: documented default {doc_default}, real {real}"
            ),
            (None, None) => {}
            (d, r) => panic!("{path}: documented default {d:?}, real default {r:?}"),
        }
    }

    let base = DecisionSpec::default();
    let accepts = |patch: &Value| base.merge_patch(patch).is_ok();
    for (path, p) in &leaves {
        // Enums: every documented value is accepted, others are not.
        if let Some(values) = p.get("enum").and_then(Value::as_array) {
            for v in values {
                assert!(
                    accepts(&patch_at(path, v.clone())),
                    "{path} = {v} is documented"
                );
            }
            assert!(
                !accepts(&patch_at(path, json!("zzz-undocumented"))),
                "{path}"
            );
        }
        // Ranges: the documented bounds are the enforced ones.
        for (value, ok) in range_cases(p) {
            assert_eq!(
                accepts(&patch_at(path, value.clone())),
                ok,
                "{path} = {value}: documented as {}",
                if ok { "valid" } else { "invalid" }
            );
        }
    }

    // Action list and id limits.
    let actions = |n: usize| json!({ "actions": (0..n).map(|i| json!({"id": format!("a{i}")})).collect::<Vec<_>>() });
    let max = schema("DecisionSpec")["properties"]["actions"]["maxItems"]
        .as_u64()
        .unwrap() as usize;
    assert!(
        accepts(&actions(max)) && !accepts(&actions(max + 1)),
        "actions.maxItems"
    );
    let id = &properties(schema("Action"))["id"];
    let max_id = id["maxLength"].as_u64().unwrap() as usize;
    assert!(
        accepts(&json!({"actions": [{"id": "é".repeat(max_id)}]})),
        "Action.id maxLength"
    );
    assert!(
        !accepts(&json!({"actions": [{"id": "é".repeat(max_id + 1)}]})),
        "Action.id maxLength"
    );
    assert!(
        !accepts(&json!({"actions": [{"id": ""}]})),
        "Action.id minLength"
    );

    // The example is a valid spec.
    DecisionSpec::from_json(&schema("DecisionSpec")["example"]).expect("DecisionSpec example");
}

#[test]
fn evaluate_schema_matches_the_server_types() {
    let srv = Srv::new("evaluate", None);
    let spec = &schema("DecisionSpec")["example"];
    let r = srv.call(
        "PUT",
        &format!("{CAPSULE}/spec"),
        Some(spec.to_string().as_bytes()),
        None,
    );
    assert_eq!(r.status, 201);
    let evaluate = format!("{CAPSULE}/evaluate");

    // Documented fields are exactly the fields serde accepts.
    let (status, body) = srv.post_json(&evaluate, &json!({ "zzzUndocumented": 1 }));
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        property_names(schema("EvaluateRequest")),
        serde_fields(body["error"].as_str().expect("error")),
        "EvaluateRequest"
    );

    // Documented defaults are the evaluator's.
    let d = syntra::ope::estimators::EvalConfig::default();
    let props = properties(schema("EvaluateRequest"));
    assert_eq!(props["folds"]["default"], json!(d.folds));
    assert_eq!(props["bootstrap"]["default"], json!(d.bootstrap));
    assert_eq!(props["seed"]["default"], json!(d.seed));
    assert!(numbers_equal(&props["wMax"]["default"], &json!(d.w_max)));
    assert!(props["rewardRange"].get("default").is_none() && d.reward_range.is_none());

    // Ranges: settings are validated before the log is read, so a valid
    // request on an empty log answers 409 and an invalid one 400.
    let status_of = |extra: Value| {
        let mut body = json!({ "policy": "logged" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        srv.post_json(&evaluate, &body).0
    };
    assert_eq!(status_of(json!({})), 409, "an empty log");
    for (key, prop) in props {
        for (value, ok) in range_cases(prop) {
            assert_eq!(
                status_of(json!({ key.as_str(): value })),
                if ok { 409 } else { 400 },
                "EvaluateRequest.{key} = {value}"
            );
        }
    }
    assert_eq!(
        status_of(json!({"bootstrap": 50})),
        400,
        "1-99 resamples are refused"
    );
    assert_eq!(status_of(json!({"rewardRange": [1, 0]})), 400);
    assert_eq!(status_of(json!({"gates": ["lift.dr.lower >="]})), 400);
    assert_eq!(status_of(json!({"gates": ["lift.dr.lower >= 0"]})), 409);
    for body in [
        json!({}),
        json!({"policy": "logged", "spec": {}}),
        json!({"policy": "spec:/etc/passwd"}),
        json!({"spec": {"exploration": {"gama": 1}}}),
    ] {
        assert_eq!(srv.post_json(&evaluate, &body).0, 400, "{body}");
    }
    let promote = format!("{CAPSULE}/promote");
    for body in [
        json!({"gates": ["n >= 1"]}),
        json!({"spec": {}, "gates": []}),
        json!({"spec": {}, "policy": "logged", "gates": ["n >= 1"]}),
    ] {
        assert_eq!(srv.post_json(&promote, &body).0, 400, "{body}");
    }
}

#[test]
fn policy_schema_matches_the_policy_parser() {
    use syntra::context::{ExecutionPolicy, POLICY_KEYS};
    let documented = property_names(schema("Policy"));
    let keys: BTreeSet<String> = POLICY_KEYS.iter().map(|k| k.to_string()).collect();
    assert_eq!(documented, keys, "Policy properties vs POLICY_KEYS");

    // Documented defaults are what a document without the key gets.
    let p = ExecutionPolicy::from_policy_json("{}").unwrap();
    let real: BTreeMap<&str, Value> = BTreeMap::from([
        ("allow_stdout", json!(p.allow_stdout)),
        ("allow_stdin", json!(p.allow_stdin)),
        ("allow_file_read", json!(p.allow_file_read)),
        ("allow_file_write", json!(p.allow_file_write)),
        ("allow_network", json!(p.allow_network)),
        ("allow_insecure_http", json!(p.allow_insecure_http)),
        ("allowed_hosts", json!(p.allowed_hosts)),
        ("deny_private_networks", json!(p.deny_private_networks)),
        ("max_execution_ms", json!(p.max_execution_ms)),
    ]);
    for (key, prop) in properties(schema("Policy")) {
        if let Some(d) = prop.get("default") {
            assert_eq!(Some(d), real.get(key.as_str()), "Policy.{key} default");
        } else {
            assert!(
                !real.contains_key(key.as_str()),
                "Policy.{key}: document its default"
            );
        }
    }

    let accepts = |doc: Value| ExecutionPolicy::from_policy_value(&doc).is_ok();
    for (key, prop) in properties(schema("Policy")) {
        for (value, ok) in range_cases(prop) {
            assert_eq!(
                accepts(json!({ key.as_str(): value })),
                ok,
                "Policy.{key} = {value}"
            );
        }
        if let Some(max) = prop.get("maxLength").and_then(Value::as_u64) {
            assert!(
                accepts(json!({ key.as_str(): "a".repeat(max as usize) })),
                "{key}"
            );
            assert!(
                !accepts(json!({ key.as_str(): "a".repeat(max as usize + 1) })),
                "{key}"
            );
        }
    }
    assert!(
        accepts(schema("Policy")["example"].clone()),
        "Policy example"
    );
}

// ── 5. Responses ─────────────────────────────────────────────────────────

struct Session {
    srv: Srv,
    key: &'static str,
    /// `(method, template, status)` seen and validated.
    seen: BTreeSet<(String, String, u16)>,
}

impl Session {
    /// Send a request, then check its status is documented for the
    /// operation and its body matches the documented schema.
    fn call(
        &mut self,
        method: &str,
        target: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> (u16, Value) {
        let raw = body.map(|b| b.to_string().into_bytes());
        self.send(method, target, raw.as_deref(), key)
    }

    fn send(
        &mut self,
        method: &str,
        target: &str,
        body: Option<&[u8]>,
        key: Option<&str>,
    ) -> (u16, Value) {
        self.send_with(method, target, body, key, &[]).0
    }

    /// [`Self::send`] with request headers; also returns the response.
    fn send_with(
        &mut self,
        method: &str,
        target: &str,
        body: Option<&[u8]>,
        key: Option<&str>,
        headers: &[(&str, &str)],
    ) -> ((u16, Value), Response) {
        let key = key.or(Some(self.key)).filter(|k| !k.is_empty());
        let resp = self.srv.call_with(method, target, body, key, headers);
        let path = target.split('?').next().unwrap();
        let op = operation_for(method, path)
            .unwrap_or_else(|| panic!("{method} {path} is not documented"));
        let label = format!("{} -> {}", op.label(), resp.status);
        let documented = op.response(resp.status).unwrap_or_else(|| {
            panic!(
                "{label}: status not documented ({})",
                String::from_utf8_lossy(&resp.body)
            )
        });
        self.seen
            .insert((op.method.clone(), op.path.clone(), resp.status));
        if documented.get("content").is_none() {
            assert!(resp.body.is_empty(), "{label}: documented without a body");
            return ((resp.status, Value::Null), resp);
        }
        let content_type = header(&resp, "content-type").unwrap_or("");
        let media = content_type.split(';').next().unwrap().trim();
        let content = documented["content"]
            .get(media)
            .unwrap_or_else(|| panic!("{label}: content type {content_type:?} not documented"));
        let body = if media == "application/json" {
            let v: Value = serde_json::from_slice(&resp.body).expect("JSON body");
            assert_valid(&content["schema"], &v, &label);
            v
        } else {
            Value::String(String::from_utf8_lossy(&resp.body).into_owned())
        };
        ((resp.status, body), resp)
    }
}

/// Compile Lycan source with the `lycan` binary.
fn compile(source: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!(
        "syntra-openapi-lyc-{}-{:x}",
        std::process::id(),
        syntra::decision::random_seed()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("program.lycs");
    std::fs::write(&src, source).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
        .arg("compile")
        .arg(&src)
        .output()
        .expect("run lycan compile");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(dir.join("program.lyc")).expect("compiled program");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

#[test]
fn responses_match_documented_schemas() {
    const KEY: &str = "openapi-operator-key";
    let mut s = Session {
        srv: Srv::new("responses", Some(KEY)),
        key: KEY,
        seen: BTreeSet::new(),
    };
    let c = CAPSULE;

    for path in ["/health", "/ready", "/admin"] {
        assert_eq!(s.call("GET", path, None, Some("")).0, 200, "{path}");
    }
    assert_eq!(s.call("GET", "/metrics", None, Some("")).0, 401);
    assert_eq!(s.call("GET", "/metrics", None, None).0, 200);
    assert_eq!(s.call("GET", "/v1/tenants", None, Some("")).0, 401);
    assert_eq!(s.call("GET", "/v1/tenants", None, Some("wrong-key")).0, 401);
    s.call("GET", "/v1/auth/whoami", None, None);
    s.call("GET", "/v1/capabilities", None, None);

    // Capsule lifecycle.
    let spec = schema("DecisionSpec")["example"].clone();
    assert_eq!(s.call("PUT", &format!("{c}/spec"), Some(spec), None).0, 201);
    assert_eq!(
        s.call(
            "PUT",
            &format!("{c}/spec"),
            Some(json!({"exploration": {"floor": 0.1}})),
            None
        )
        .0,
        200
    );
    assert_eq!(
        s.call(
            "PUT",
            &format!("{c}/spec"),
            Some(json!({"exploration": {"gama": 1}})),
            None
        )
        .0,
        400
    );
    s.call("GET", &format!("{c}/spec"), None, None);
    s.call("GET", c, None, None);
    let program = compile(
        "($ probe (!cap \"runtime.inputGet\" \"probe\"))\n\
         (!cap \"runtime.publish\" \"features.one\" 1)\n\
         (!cap \"runtime.publish\" \"reason\" (? (== probe \"fs\") (+ \"\" (!cap \"file.exists\" \"x\")) \"ok\"))\n",
    );
    assert_eq!(
        s.send(
            "POST",
            &format!("{c}/install"),
            Some(program.as_slice()),
            None
        )
        .0,
        200
    );
    assert_eq!(
        s.send(
            "POST",
            &format!("{c}/install"),
            Some(b"not a program".as_slice()),
            None
        )
        .0,
        400
    );
    let (_, capsule) = s.call("GET", c, None, None);
    assert!(capsule["program"].is_object(), "{capsule}");
    s.call("GET", &format!("{c}/policy"), None, None);
    let policy = json!({"allow_stdout": false, "max_execution_ms": 1000});
    assert_eq!(
        s.call("PUT", &format!("{c}/policy"), Some(policy), None).0,
        200
    );
    assert_eq!(
        s.call(
            "PUT",
            &format!("{c}/policy"),
            Some(json!({"allow_netwrok": true})),
            None
        )
        .0,
        400
    );

    // Decide and reward.
    let example = schema("DecideRequest")["example"].clone();
    let (status, first) = s.call("POST", &format!("{c}/decide"), Some(example.clone()), None);
    assert_eq!(
        (status, first["reason"].as_str()),
        (200, Some("ok")),
        "{first}"
    );
    let (_, replay) = s.call("POST", &format!("{c}/decide"), Some(example.clone()), None);
    assert_eq!(replay["replayed"], true);
    let mut changed = example.clone();
    changed["context"]["task"] = json!("chat");
    assert_eq!(
        s.call("POST", &format!("{c}/decide"), Some(changed), None)
            .0,
        409
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/decide"),
            Some(json!({"contexts": {}})),
            None
        )
        .0,
        400
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/decide"),
            Some(json!({"context": {"probe": "fs"}})),
            None
        )
        .0,
        500
    );
    let missing = "/v1/tenants/acme/jobs/routing/capsules/nope";
    assert_eq!(
        s.call("POST", &format!("{missing}/decide"), Some(json!({})), None)
            .0,
        404
    );
    let (_, second) = s.call(
        "POST",
        &format!("{c}/decide"),
        Some(json!({"context": {}, "durable": true})),
        None,
    );

    let mut reward = schema("RewardRequest")["example"].clone();
    reward["decisionId"] = first["decisionId"].clone();
    let (_, applied) = s.call("POST", &format!("{c}/reward"), Some(reward.clone()), None);
    assert_eq!(applied["applied"], true);
    let (_, dup) = s.call("POST", &format!("{c}/reward"), Some(reward), None);
    assert_eq!(dup["duplicate"], true);
    let fb = json!({"decisionId": second["decisionId"], "value": 0.25, "durable": true});
    assert_eq!(
        s.call("POST", &format!("{c}/feedback"), Some(fb), None).0,
        200
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/reward"),
            Some(json!({"decisionId": "nope", "reward": 1})),
            None
        )
        .0,
        404
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/reward"),
            Some(json!({"decisionId": "x"})),
            None
        )
        .0,
        400
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/feedback"),
            Some(json!({"decisionId": "nope", "reward": 1})),
            None
        )
        .0,
        404
    );

    // Logs.
    let (_, page) = s.call("GET", &format!("{c}/decisions?limit=1"), None, None);
    assert!(page["next"].is_string(), "{page}");
    s.call("GET", &format!("{c}/decisions"), None, None);
    assert_eq!(
        s.call("GET", &format!("{c}/decisions?after=nope"), None, None)
            .0,
        400
    );
    let id = first["decisionId"].as_str().unwrap();
    let (_, one) = s.call("GET", &format!("{c}/decisions/{id}"), None, None);
    assert_eq!(one["rewards"].as_array().map(Vec::len), Some(1), "{one}");
    assert_eq!(
        s.call("GET", &format!("{c}/decisions/nope"), None, None).0,
        404
    );
    let (_, live) = s.call("GET", &format!("{c}/model"), None, None);
    assert!(live.get("snapshot").is_none(), "{live}");
    let snapshot_url = format!("{c}/model?snapshot=true");
    let ((_, model), resp) = s.send_with("GET", &snapshot_url, None, None, &[]);
    assert!(model["snapshot"].is_string(), "{model}");
    let etag = format!("\"{}\"", model["modelTag"].as_str().expect("modelTag"));
    assert_eq!(header(&resp, "etag"), Some(etag.as_str()));
    let ((status, _), resp) = s.send_with(
        "GET",
        &snapshot_url,
        None,
        None,
        &[("If-None-Match", &etag)],
    );
    assert_eq!((status, header(&resp, "etag")), (304, Some(etag.as_str())));
    let (_, audits) = s.call("GET", &format!("{c}/audits?limit=1000"), None, None);
    let events: Vec<&str> = audits["audits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["event"].as_str())
        .collect();
    assert!(events.contains(&"execution_denied"), "{events:?}");
    assert_eq!(
        s.call(
            "GET",
            "/v1/tenants/bad%20name/jobs/j/capsules/c/audits",
            None,
            None
        )
        .0,
        400
    );

    // Mode and program.
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/mode"),
            Some(json!({"mode": "frozen"})),
            None
        )
        .0,
        200
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{c}/mode"),
            Some(json!({"mode": "frozen", "x": 1})),
            None
        )
        .0,
        400
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{missing}/mode"),
            Some(json!({"mode": "frozen"})),
            None
        )
        .0,
        404
    );
    // Local evaluation is refused while a feature program is installed.
    let uploads = format!("{c}/decisions:batch");
    assert_eq!(
        s.call("POST", &uploads, Some(json!({"decisions": []})), None)
            .0,
        400
    );
    s.call("DELETE", &format!("{c}/program"), None, None);
    s.call(
        "POST",
        &format!("{c}/mode"),
        Some(json!({"mode": "learner"})),
        None,
    );
    let input = json!({"context": {"task": "code"}});
    let item = local_decision(&s.srv, Some(KEY), c, "loc_session", &input, 42);
    let (_, up) = s.call(
        "POST",
        &uploads,
        Some(json!({"decisions": [item, {}]})),
        None,
    );
    assert_eq!(up["accepted"], 1, "{up}");
    assert_eq!(up["rejected"][0]["index"], 1, "{up}");
    assert_eq!(
        s.call("POST", &uploads, Some(json!({"items": []})), None).0,
        400
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{missing}/decisions:batch"),
            Some(json!({"decisions": []})),
            None
        )
        .0,
        404
    );
    let rewards = json!({"rewards": [{"decisionId": "loc_session", "reward": 1}, {"decisionId": "nope", "reward": 1}]});
    let (_, rw) = s.call("POST", &format!("{c}/rewards:batch"), Some(rewards), None);
    assert_eq!(rw["results"][0]["applied"], true, "{rw}");
    assert_eq!(rw["results"][1]["status"], 404, "{rw}");
    assert_eq!(
        s.call("POST", &format!("{c}/rewards:batch"), Some(json!([])), None)
            .0,
        400
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{missing}/rewards:batch"),
            Some(json!({"rewards": []})),
            None
        )
        .0,
        404
    );

    // Off-policy evaluation of the log, and promotion gated on it.
    for i in 0..20 {
        let (_, d) = s.call(
            "POST",
            &format!("{c}/decide"),
            Some(json!({"context": {"i": i}})),
            None,
        );
        let r = if d["action"] == "small" { 1.0 } else { 0.0 };
        let body = json!({"decisionId": d["decisionId"], "reward": r});
        s.call("POST", &format!("{c}/reward"), Some(body), None);
    }
    let settings = json!({"folds": 2, "bootstrap": 100});
    let mut eval = settings.clone();
    eval["policy"] = json!("greedy");
    let (status, report) = s.call("POST", &format!("{c}/evaluate"), Some(eval), None);
    assert_eq!(status, 200, "{report}");
    let patch = json!({"exploration": {"floor": 0.2}});
    let mut refused = settings.clone();
    refused["spec"] = patch.clone();
    refused["gates"] = json!(["n >= 1000000"]);
    let (status, body) = s.call("POST", &format!("{c}/promote"), Some(refused), None);
    assert_eq!((status, &body["promoted"]), (409, &json!(false)), "{body}");
    let mut passed = settings.clone();
    passed["spec"] = patch;
    passed["gates"] = json!(["n >= 2"]);
    let (status, body) = s.call("POST", &format!("{c}/promote"), Some(passed), None);
    assert_eq!(
        (status, &body["spec"]["exploration"]["floor"]),
        (200, &json!(0.2)),
        "{body}"
    );

    // Tenants and jobs.
    let jobs = "/v1/tenants/acme/jobs";
    assert_eq!(
        s.call(
            "POST",
            jobs,
            Some(json!({"id": "extra", "name": "Extra"})),
            None
        )
        .0,
        201
    );
    assert_eq!(
        s.call("POST", jobs, Some(json!({"id": "extra"})), None).0,
        200
    );
    assert_eq!(
        s.call("POST", jobs, Some(json!({"name": "no id"})), None).0,
        400
    );
    s.call("GET", jobs, None, None);
    s.call("GET", &format!("{jobs}/routing"), None, None);
    assert_eq!(s.call("GET", &format!("{jobs}/nope"), None, None).0, 404);
    s.call("GET", &format!("{jobs}/routing/capsules"), None, None);
    s.call("GET", "/v1/tenants", None, None);
    s.call("GET", "/v1/admin/capsules", None, None);

    // Tokens and scopes.
    let (_, issued) = s.call(
        "POST",
        "/v1/admin/tokens",
        Some(schema("IssueTokenRequest")["example"].clone()),
        None,
    );
    let read_token = issued["token"].as_str().unwrap().to_string();
    let (_, tenant_token) = s.call(
        "POST",
        "/v1/admin/tokens",
        Some(json!({"scope": {"kind": "tenant_admin", "tenant": "acme"}})),
        None,
    );
    let tenant_token = tenant_token["token"].as_str().unwrap().to_string();
    assert_eq!(
        s.call(
            "POST",
            "/v1/admin/tokens",
            Some(json!({"scope": {"kind": "root"}})),
            None
        )
        .0,
        400
    );
    s.call("GET", "/v1/admin/tokens", None, None);
    let (_, who) = s.call("GET", "/v1/auth/whoami", None, Some(&read_token));
    assert_eq!(who["kind"], "scoped_token");
    assert_eq!(
        s.call("GET", &format!("{c}/spec"), None, Some(&read_token))
            .0,
        200
    );
    assert_eq!(
        s.call(
            "PUT",
            &format!("{c}/spec"),
            Some(json!({})),
            Some(&read_token)
        )
        .0,
        403
    );
    let open = json!({"deny_private_networks": false});
    assert_eq!(
        s.call(
            "PUT",
            &format!("{c}/policy"),
            Some(open),
            Some(&tenant_token)
        )
        .0,
        403
    );
    let hash = issued["hash"].as_str().unwrap();
    assert_eq!(
        s.call("DELETE", &format!("/v1/admin/tokens/{hash}"), None, None)
            .0,
        200
    );
    assert_eq!(
        s.call("DELETE", &format!("/v1/admin/tokens/{hash}"), None, None)
            .0,
        404
    );

    // Personalizer.
    let px = format!("{c}/personalizer/v1.0");
    let rank = |event: &str, defer: bool| {
        json!({"contextFeatures": [{"user": {"tier": "pro"}}],
               "actions": [{"id": "x"}, {"id": "y", "features": [{"size": 2}]}],
               "excludedActions": ["y"], "eventId": event, "deferActivation": defer})
    };
    assert_eq!(
        s.call(
            "POST",
            &format!("{px}/rank"),
            Some(rank("px-1", false)),
            None
        )
        .0,
        201
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{px}/events/px-1/reward"),
            Some(json!({"value": 1})),
            None
        )
        .0,
        204
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{px}/rank"),
            Some(rank("px-2", true)),
            None
        )
        .0,
        201
    );
    assert_eq!(
        s.call("POST", &format!("{px}/events/px-2/activate"), None, None)
            .0,
        204
    );
    assert_eq!(
        s.call("POST", &format!("{px}/events/px-9/activate"), None, None)
            .0,
        404
    );
    assert_eq!(
        s.call(
            "POST",
            &format!("{px}/rank"),
            Some(json!({"actions": []})),
            None
        )
        .0,
        400
    );
    assert_eq!(
        s.call("GET", &format!("{px}/configurations/service"), None, None)
            .0,
        200
    );
    assert_eq!(
        s.call(
            "PUT",
            &format!("{px}/configurations/service"),
            Some(json!({"rewardWaitTime": "PT5M", "logRetentionDays": 30})),
            None
        )
        .0,
        200
    );

    // Erasure.
    s.call("DELETE", &format!("{c}/logs"), None, None);
    assert_eq!(
        s.call("DELETE", &format!("{c}/logs"), None, Some(&tenant_token))
            .0,
        200
    );
    assert_eq!(s.call("DELETE", c, None, None).0, 200);
    assert_eq!(s.call("DELETE", c, None, None).0, 404);
    assert_eq!(s.call("GET", c, None, None).0, 404);
    assert_eq!(
        s.call("DELETE", &format!("{jobs}/extra"), None, None).0,
        200
    );
    assert_eq!(
        s.call("DELETE", &format!("{jobs}/extra"), None, None).0,
        404
    );
    assert_eq!(s.call("DELETE", "/v1/tenants/acme", None, None).0, 200);
    assert_eq!(s.call("DELETE", "/v1/tenants/acme", None, None).0, 404);
    assert_eq!(s.call("DELETE", "/v1/tenants/.bad", None, None).0, 400);

    // Every documented success response was observed and validated.
    let mut missing = Vec::new();
    for op in operations() {
        for status in op.op["responses"].as_object().unwrap().keys() {
            let status: u16 = status.parse().unwrap();
            if (200..300).contains(&status)
                && !s
                    .seen
                    .contains(&(op.method.clone(), op.path.clone(), status))
            {
                missing.push(format!("{} {status}", op.label()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "success responses never exercised: {missing:?}"
    );
}

// ── 6. x-scopes ──────────────────────────────────────────────────────────

#[test]
fn documented_scopes_match_authorization() {
    const KEY: &str = "openapi-scope-key";
    let srv = Srv::new("scopes", Some(KEY));
    let issue = |scope: Value| {
        let r = srv.call(
            "POST",
            "/v1/admin/tokens",
            Some(json!({ "scope": scope }).to_string().as_bytes()),
            Some(KEY),
        );
        assert_eq!(r.status, 200);
        body_json(&r)["token"].as_str().unwrap().to_string()
    };
    // The same names `concrete` gives path parameters.
    let tenant_admin = issue(json!({"kind": "tenant_admin", "tenant": "probe-tenant"}));
    let read = issue(json!({
        "kind": "read", "tenant": "probe-tenant", "job": "probe-job", "capsule": "probe-capsule"
    }));
    let keys = [
        ("admin", KEY),
        ("tenant_admin", tenant_admin.as_str()),
        ("read", read.as_str()),
    ];

    for op in operations().into_iter().filter(|o| !o.infra()) {
        let path = concrete(&op.path);
        let allowed = op.scopes();
        // Least privileged first, so destructive calls run last.
        for &(scope, key) in keys.iter().rev() {
            let resp = srv.call(&op.method, &path, probe_body(&op.method), Some(key));
            assert_eq!(
                resp.status != 403,
                allowed.contains(scope),
                "{} with a {scope} token: {} {}",
                op.label(),
                resp.status,
                String::from_utf8_lossy(&resp.body)
            );
        }
    }
}
