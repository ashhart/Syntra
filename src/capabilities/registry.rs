//! Capability specifications, values, and the static registry.

#[derive(Debug, Clone, Copy)]
pub struct CapabilitySpec {
    pub name: &'static str,
    pub version: &'static str,
    pub package: &'static str,
    pub summary: &'static str,
    pub inputs: &'static [&'static str],
    pub output: &'static str,
    pub purity: Purity,
    pub deterministic: bool,
    pub effects: &'static [&'static str],
    pub cost: &'static str,
    pub failure: &'static str,
    pub safety: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Purity {
    Pure,
    ReadOnlyEffect,
    Effectful,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CapValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Array(Vec<CapValue>),
}

impl CapValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            CapValue::Int(_) => "int",
            CapValue::Float(_) => "float",
            CapValue::Str(_) => "str",
            CapValue::Bool(_) => "bool",
            CapValue::Null => "null",
            CapValue::Array(_) => "array",
        }
    }

    /// Convert from serde_json::Value (used by server for JSON input).
    pub fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => CapValue::Null,
            serde_json::Value::Bool(b) => CapValue::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    CapValue::Int(i)
                } else {
                    CapValue::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => CapValue::Str(s.clone()),
            serde_json::Value::Array(a) => CapValue::Array(a.iter().map(Self::from_json).collect()),
            serde_json::Value::Object(o) => CapValue::Array(
                o.iter()
                    .map(|(k, v)| {
                        CapValue::Array(vec![CapValue::Str(k.clone()), Self::from_json(v)])
                    })
                    .collect(),
            ),
        }
    }
}

pub const REGISTRY: &[CapabilitySpec] = &[
    CapabilitySpec {
        name: "runtime.capabilities",
        version: "1.0.0",
        package: "runtime",
        summary: "Return the names of capabilities available in this runtime.",
        inputs: &[],
        output: "array<string>",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(number_of_capabilities)",
        failure: "never for a valid runtime",
        safety: "introspection only",
    },
    CapabilitySpec {
        name: "runtime.input",
        version: "1.0.0",
        package: "runtime",
        summary: "Return the full JSON input injected via --input flag.",
        inputs: &[],
        output: "any",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "returns null if no input was provided",
        safety: "read-only access to injected input",
    },
    CapabilitySpec {
        name: "runtime.inputGet",
        version: "1.0.0",
        package: "runtime",
        summary: "Access a nested field in the injected input by dot-path (e.g. 'request.body.items.2.symbol').",
        inputs: &["path:string"],
        output: "any",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(depth_of_path)",
        failure: "returns null if path does not exist",
        safety: "read-only access to injected input",
    },
    CapabilitySpec {
        name: "runtime.publish",
        version: "0.1.0",
        package: "runtime",
        summary: "Publish a named computed value into the current decision's journal entry for inspection.",
        inputs: &["name:string", "value:number|string|bool"],
        output: "null",
        purity: Purity::Effectful,
        deterministic: true,
        effects: &["publish"],
        cost: "O(1)",
        failure: "non-string name, non-finite numeric value, or buffer not initialised",
        safety: "values stored in the per-decision journal only; not user-visible filesystem or network state",
    },
    CapabilitySpec {
        name: "file.exists",
        version: "0.1.0",
        package: "io",
        summary: "Check whether a local path exists.",
        inputs: &["path:string"],
        output: "bool",
        purity: Purity::ReadOnlyEffect,
        deterministic: true,
        effects: &["file_read"],
        cost: "O(1) metadata lookup",
        failure: "path argument is not a string",
        safety: "read-only local filesystem metadata; capsule policy must permit file_read",
    },
    CapabilitySpec {
        name: "file.readText",
        version: "0.1.0",
        package: "io",
        summary: "Read a UTF-8 text file from the local filesystem.",
        inputs: &["path:string"],
        output: "string",
        purity: Purity::ReadOnlyEffect,
        deterministic: true,
        effects: &["file_read"],
        cost: "O(file_size), capped at 1 MiB",
        failure: "missing file, non-UTF-8 data, or file exceeds size cap",
        safety: "read-only local filesystem access; capsule policy must permit file_read",
    },
    CapabilitySpec {
        name: "file.writeText",
        version: "0.1.0",
        package: "io",
        summary: "Write UTF-8 text to a local filesystem path.",
        inputs: &["path:string", "contents:string"],
        output: "bool",
        purity: Purity::Effectful,
        deterministic: false,
        effects: &["file_write"],
        cost: "O(contents_size), capped at 1 MiB",
        failure: "write denied, parent missing, or contents exceed cap",
        safety: "effectful local filesystem write; capsule policy must permit file_write",
    },
    CapabilitySpec {
        name: "http.get",
        version: "0.1.0",
        package: "net",
        summary: "Fetch a URL and return the response body as UTF-8 text.",
        inputs: &["url:string"],
        output: "string",
        purity: Purity::ReadOnlyEffect,
        deterministic: false,
        effects: &["network"],
        cost: "network request, 10 second timeout, 1 MiB response cap",
        failure: "invalid URL, request failure, non-success HTTP status, or non-UTF-8 body",
        safety: "outbound network read; capsule policy must permit network",
    },
    CapabilitySpec {
        name: "http.post",
        version: "0.1.0",
        package: "net",
        summary: "POST a UTF-8 body to a URL and return the response body as UTF-8 text.",
        inputs: &["url:string", "body:string", "content_type:string"],
        output: "string",
        purity: Purity::Effectful,
        deterministic: false,
        effects: &["network"],
        cost: "network request, 10 second timeout, 1 MiB request/response cap",
        failure: "invalid URL, request failure, non-success HTTP status, or body exceeds cap",
        safety: "outbound network write; capsule policy must permit network",
    },
    CapabilitySpec {
        name: "json.get",
        version: "0.1.0",
        package: "data",
        summary: "Read a dotted path from a JSON string.",
        inputs: &["json:string", "path:string"],
        output: "any primitive, array, or compact JSON string for objects",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(json_size + path_depth)",
        failure: "invalid JSON or missing path",
        safety: "pure parser kernel",
    },
    CapabilitySpec {
        name: "json.has",
        version: "0.1.0",
        package: "data",
        summary: "Return whether a dotted path exists in a JSON string.",
        inputs: &["json:string", "path:string"],
        output: "bool",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(json_size + path_depth)",
        failure: "invalid JSON",
        safety: "pure parser kernel",
    },
    CapabilitySpec {
        name: "json.len",
        version: "0.1.0",
        package: "data",
        summary: "Return the length of an array, object, or string at a JSON path.",
        inputs: &["json:string", "path:string"],
        output: "int",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(json_size + path_depth)",
        failure: "invalid JSON, missing path, or value has no length",
        safety: "pure parser kernel",
    },
    CapabilitySpec {
        name: "sql.sqliteQuery",
        version: "0.1.0",
        package: "data",
        summary: "Run a read-only SQLite SELECT/WITH/PRAGMA query and return rows.",
        inputs: &["database_path:string", "sql:string"],
        output: "array<array<any>>, capped at 1000 rows",
        purity: Purity::ReadOnlyEffect,
        deterministic: true,
        effects: &["file_read"],
        cost: "SQLite query cost, capped at 1000 returned rows",
        failure: "database missing, SQL invalid, or query is not read-only",
        safety: "read-only SQLite connection; rejects mutating SQL; capsule policy must permit file_read",
    },
    CapabilitySpec {
        name: "stats.mean",
        version: "0.1.0",
        package: "math",
        summary: "Arithmetic mean of a numeric array.",
        inputs: &["values:array<number>"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(n)",
        failure: "empty or non-numeric array",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "stats.stdDev",
        version: "0.1.0",
        package: "math",
        summary: "Population standard deviation of a numeric array.",
        inputs: &["values:array<number>"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(n)",
        failure: "empty or non-numeric array",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "stats.min",
        version: "0.1.0",
        package: "math",
        summary: "Minimum value in a numeric array.",
        inputs: &["values:array<number>"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(n)",
        failure: "empty or non-numeric array",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "stats.max",
        version: "0.1.0",
        package: "math",
        summary: "Maximum value in a numeric array.",
        inputs: &["values:array<number>"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(n)",
        failure: "empty or non-numeric array",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "stats.percentile",
        version: "0.1.0",
        package: "math",
        summary: "Interpolated percentile of a numeric array.",
        inputs: &["values:array<number>", "p:number[0..100]"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(n log n)",
        failure: "empty array, invalid percentile, or non-numeric input",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "series.ewmaForecast",
        version: "0.1.0",
        package: "math",
        summary: "One-step exponential weighted moving average forecast.",
        inputs: &["values:array<number>", "alpha:number[0..1]"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(n)",
        failure: "empty array, invalid alpha, or non-numeric input",
        safety: "pure time-series kernel",
    },
    CapabilitySpec {
        name: "ops.autoScaleRecommend",
        version: "0.1.0",
        package: "ops",
        summary: "Recommend instance count from predicted load and per-instance target capacity.",
        inputs: &[
            "predicted_load:number",
            "target_per_instance:number",
            "min_instances:int",
            "max_instances:int",
        ],
        output: "int",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "invalid numeric input, non-positive capacity, or min > max",
        safety: "pure decision helper; caller owns actual infrastructure changes",
    },
];

pub fn get(name: &str) -> Option<&'static CapabilitySpec> {
    REGISTRY.iter().find(|spec| spec.name == name)
}

pub fn names() -> Vec<&'static str> {
    REGISTRY.iter().map(|spec| spec.name).collect()
}

pub fn json_catalog() -> String {
    let mut out = String::new();
    out.push_str("[\n");
    for (i, spec) in REGISTRY.iter().enumerate() {
        out.push_str(&spec_json(spec, 2));
        if i < REGISTRY.len() - 1 {
            out.push(',');
        }
        out.push('\n');
    }
    out.push(']');
    out
}

pub fn spec_json(spec: &CapabilitySpec, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    format!(
        "{pad}{{\n\
{pad2}\"name\": \"{}\",\n\
{pad2}\"version\": \"{}\",\n\
{pad2}\"package\": \"{}\",\n\
{pad2}\"summary\": \"{}\",\n\
{pad2}\"inputs\": [{}],\n\
{pad2}\"output\": \"{}\",\n\
{pad2}\"purity\": \"{}\",\n\
{pad2}\"deterministic\": {},\n\
{pad2}\"effects\": [{}],\n\
{pad2}\"cost\": \"{}\",\n\
{pad2}\"failure\": \"{}\",\n\
{pad2}\"safety\": \"{}\"\n\
{pad}}}",
        esc(spec.name),
        esc(spec.version),
        esc(spec.package),
        esc(spec.summary),
        quoted(spec.inputs).join(", "),
        esc(spec.output),
        match spec.purity {
            Purity::Pure => "pure",
            Purity::ReadOnlyEffect => "read_only_effect",
            Purity::Effectful => "effectful",
        },
        if spec.deterministic { "true" } else { "false" },
        quoted(spec.effects).join(", "),
        esc(spec.cost),
        esc(spec.failure),
        esc(spec.safety),
    )
}

fn quoted(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| format!("\"{}\"", esc(s))).collect()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
