/// Native capability registry — hardened runtime kernels with metadata.

use std::io::Read;
use std::time::Duration;

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
                if let Some(i) = n.as_i64() { CapValue::Int(i) }
                else { CapValue::Float(n.as_f64().unwrap_or(0.0)) }
            }
            serde_json::Value::String(s) => CapValue::Str(s.clone()),
            serde_json::Value::Array(a) => CapValue::Array(a.iter().map(Self::from_json).collect()),
            serde_json::Value::Object(o) => CapValue::Array(
                o.iter().map(|(k, v)| CapValue::Array(vec![
                    CapValue::Str(k.clone()),
                    Self::from_json(v),
                ])).collect()
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
        inputs: &["predicted_load:number", "target_per_instance:number", "min_instances:int", "max_instances:int"],
        output: "int",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "invalid numeric input, non-positive capacity, or min > max",
        safety: "pure decision helper; caller owns actual infrastructure changes",
    },
    CapabilitySpec {
        name: "nav.norm3",
        version: "1.0.0",
        package: "nav",
        summary: "Euclidean norm of a 3D vector.",
        inputs: &["x:number", "y:number", "z:number"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "non-finite or non-numeric input",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "nav.distance3",
        version: "1.0.0",
        package: "nav",
        summary: "Euclidean distance between two 3D positions.",
        inputs: &["x:number", "y:number", "z:number", "rx:number", "ry:number", "rz:number"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "non-finite or non-numeric input",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "nav.dot3",
        version: "1.0.0",
        package: "nav",
        summary: "Dot product of two 3D vectors.",
        inputs: &["ax:number", "ay:number", "az:number", "bx:number", "by:number", "bz:number"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "non-finite or non-numeric input",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "nav.radialVelocity",
        version: "1.0.0",
        package: "nav",
        summary: "Radial velocity component of a state vector.",
        inputs: &["x:number", "y:number", "z:number", "vx:number", "vy:number", "vz:number"],
        output: "number",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "O(1)",
        failure: "zero position vector or invalid numeric input",
        safety: "pure numeric kernel",
    },
    CapabilitySpec {
        name: "nav.ephemerisState",
        version: "1.0.0",
        package: "nav",
        summary: "Read a local ephemeris table and interpolate a 6D state vector.",
        inputs: &["path:string", "body:string", "et:number"],
        output: "array<number>[6]",
        purity: Purity::ReadOnlyEffect,
        deterministic: true,
        effects: &["file_read"],
        cost: "O(rows_in_ephemeris_file)",
        failure: "file missing, body mismatch, invalid rows, or time outside coverage",
        safety: "read-only local file access; capsule policy must permit file_read",
    },
    CapabilitySpec {
        name: "nav.horizonsVectors",
        version: "1.0.0",
        package: "nav",
        summary: "Fetch geometric heliocentric state vectors from NASA/JPL Horizons.",
        inputs: &["body:string", "start:string", "stop:string", "step_days:number"],
        output: "flat array [jd,x,y,z,vx,vy,vz,...] in AU and AU/day",
        purity: Purity::ReadOnlyEffect,
        deterministic: false,
        effects: &["network"],
        cost: "NASA/JPL Horizons HTTPS request, 10 second timeout, 1 MiB response cap",
        failure: "network error, Horizons error payload, unsupported body/date/step, or parse failure",
        safety: "outbound request to NASA/JPL Horizons; capsule policy must permit network and host allowlist",
    },
    CapabilitySpec {
        name: "astro.lambertSolve",
        version: "0.1.0",
        package: "astro",
        summary: "Solve the 3D Lambert transfer problem and return departure/arrival velocity vectors.",
        inputs: &[
            "r1x:number", "r1y:number", "r1z:number",
            "r2x:number", "r2y:number", "r2z:number",
            "tof_days:number", "mu:number",
        ],
        output: "array<number>[7] = [v1x,v1y,v1z,v2x,v2y,v2z,status]",
        purity: Purity::Pure,
        deterministic: true,
        effects: &[],
        cost: "iterative numeric solve, bounded by Rust kernel",
        failure: "status=0 on invalid geometry, impossible transfer, or non-convergence",
        safety: "pure numeric kernel; no panics; returns status",
    },
];

/// Resolve a file path inside the sandbox root. Returns the canonical path or error.
fn resolve_sandbox_path(
    ctx: Option<&crate::context::ExecutionContext>,
    requested: &str,
    effect: &str,
) -> Result<std::path::PathBuf, String> {
    // Determine root — no context or no policy = unrestricted
    let root = match ctx {
        Some(context) => match &context.policy {
            Some(pol) => {
                if let Some(ref fr) = pol.file_root {
                    let root = std::path::PathBuf::from(fr);
                    if root.is_relative() {
                        if let Some(ref wd) = context.working_dir {
                            wd.join(root)
                        } else {
                            root
                        }
                    } else {
                        root
                    }
                } else if let Some(ref wd) = context.working_dir {
                    wd.clone()
                } else {
                    // Policy exists but no root configured — deny file access
                    return Err(format!("capability={effect}: no file_root or working_dir configured"));
                }
            }
            None => return Ok(std::path::PathBuf::from(requested)), // no policy = unrestricted
        }
        None => return Ok(std::path::PathBuf::from(requested)), // no context = unrestricted
    };

    // Sandbox active: reject absolute paths and traversal
    if requested.starts_with('/') || requested.starts_with('\\') {
        return Err(format!("capability={effect}: absolute paths denied by sandbox"));
    }
    if requested.contains("..") {
        return Err(format!("capability={effect}: path traversal denied by sandbox"));
    }

    let target = root.join(requested);

    // For reads: canonicalize and verify inside root
    let read_like = effect.contains("read")
        || effect == "file.exists"
        || effect == "nav.ephemerisState";
    if read_like {
        if target.exists() {
            let canon = target.canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve path: {e}"))?;
            let canon_root = root.canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve root: {e}"))?;
            if !canon.starts_with(&canon_root) {
                return Err(format!("capability={effect}: path escapes sandbox"));
            }
            Ok(canon)
        } else {
            // File doesn't exist — for file.exists that's fine, return the joined path
            Ok(target)
        }
    } else {
        // For writes: must defeat symlink-escape attacks.
        //
        // Two cases:
        //   (a) target file already exists — it might be a symlink whose
        //       resolved target points outside the sandbox root. Canonicalize
        //       the *target itself* (which resolves any symlinks) and assert
        //       the canonical path is inside the canonical root. Otherwise a
        //       writer would clobber whatever the symlink points to.
        //   (b) target file does not yet exist — the filename component
        //       cannot itself be a symlink because the file doesn't exist,
        //       but the parent dir might be a symlink. Canonicalize the
        //       parent and re-form the write path as
        //       `canonical_parent.join(filename)` so the eventual write
        //       lands in the real directory. The parent must exist and be
        //       inside the canonical root.
        let canon_root = root.canonicalize()
            .map_err(|e| format!("capability={effect}: cannot resolve root: {e}"))?;
        if target.exists() {
            // Existing file/symlink — canonicalize the target itself,
            // which follows symlinks. If it lands outside the sandbox the
            // write would escape, so refuse.
            let canon_target = target.canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve path: {e}"))?;
            if !canon_target.starts_with(&canon_root) {
                return Err(format!("capability={effect}: path escapes sandbox"));
            }
            Ok(canon_target)
        } else {
            // New file — parent must exist (otherwise we can't write).
            let parent = target.parent()
                .ok_or_else(|| format!("capability={effect}: target has no parent directory"))?;
            if !parent.exists() {
                return Err(format!(
                    "capability={effect}: parent directory does not exist"
                ));
            }
            let canon_parent = parent.canonicalize()
                .map_err(|e| format!("capability={effect}: cannot resolve parent: {e}"))?;
            if !canon_parent.starts_with(&canon_root) {
                return Err(format!("capability={effect}: path escapes sandbox"));
            }
            let filename = target.file_name()
                .ok_or_else(|| format!("capability={effect}: target has no filename"))?;
            Ok(canon_parent.join(filename))
        }
    }
}

/// Check if a URL host is allowed by the network sandbox.
/// Returns Ok(true) if sandbox is active, Ok(false) if unrestricted.
fn check_network_sandbox(
    ctx: Option<&crate::context::ExecutionContext>,
    url: &str,
    cap_name: &str,
) -> Result<bool, String> {
    let context = match ctx {
        Some(c) => c,
        None => return Ok(false), // no context = unrestricted
    };
    let policy = match &context.policy {
        Some(p) => p,
        None => return Ok(false), // no policy = unrestricted
    };

    // Extract host from URL, handling IPv6 bracket syntax
    let authority = url.split("://").nth(1).unwrap_or(url)
        .split('/').next().unwrap_or("");
    let host = if authority.starts_with('[') {
        // IPv6: [::1] or [::1]:port
        authority.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        authority.split(':').next().unwrap_or("")
    };

    if host.is_empty() {
        return Err(format!("capability={cap_name}: cannot parse host from URL"));
    }

    // Check allowed_hosts (exact match only)
    if !policy.allowed_hosts.is_empty() {
        let allowed = policy.allowed_hosts.iter().any(|h| {
            if h.starts_with("*.") {
                // Wildcard: *.example.com matches sub.example.com and example.com
                host.ends_with(&h[1..]) || host == &h[2..]
            } else {
                host == h // exact match only — evil-example.com != example.com
            }
        });
        if !allowed {
            return Err(format!("capability={cap_name}: host '{host}' not in allowed_hosts"));
        }
    } else {
        return Err(format!("capability={cap_name}: no allowed_hosts configured — outbound HTTP denied"));
    }

    // Check private networks
    if policy.deny_private_networks {
        let lower = host.to_lowercase();
        if lower == "localhost" {
            return Err(format!("capability={cap_name}: private/local host denied by policy"));
        }

        // Check if host is a literal IP
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if is_private_ip(&ip) {
                return Err(format!("capability={cap_name}: private network denied by policy"));
            }
        }

        // DNS resolution check — resolve hostname and check all IPs
        if host.parse::<std::net::IpAddr>().is_err() {
            // It's a hostname, try to resolve
            if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, 80)) {
                for addr in addrs {
                    if is_private_ip(&addr.ip()) {
                        return Err(format!(
                            "capability={cap_name}: host '{host}' resolves to private IP {} — denied by policy",
                            addr.ip()
                        ));
                    }
                }
            }
        }
    }

    Ok(true) // sandbox is active
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_unspecified() || v4.is_broadcast()
            || v4.octets()[0] == 10
            || (v4.octets()[0] == 172 && v4.octets()[1] >= 16 && v4.octets()[1] <= 31)
            || (v4.octets()[0] == 192 && v4.octets()[1] == 168)
            || (v4.octets()[0] == 169 && v4.octets()[1] == 254)
            || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified()
            || (v6.segments()[0] & 0xfe00) == 0xfc00  // unique local
            || (v6.segments()[0] & 0xffc0) == 0xfe80  // link-local
            || v6.is_multicast()
        }
    }
}

pub fn execute(name: &str, args: &[CapValue], ctx: Option<&crate::context::ExecutionContext>) -> Result<CapValue, String> {
    // ── Central policy enforcement ──
    if let Some(context) = ctx {
        if let Some(pol) = &context.policy {
            if let Some(spec) = get(name) {
                for effect in spec.effects {
                    let denied = match *effect {
                        "file_read" => !pol.allow_file_read,
                        "file_write" => !pol.allow_file_write,
                        "network" => !pol.allow_network,
                        _ => false,
                    };
                    if denied {
                        return Err(format!("capability={name} effect={effect} denied by policy"));
                    }
                }
            }
        }
    }

    match name {
        "runtime.capabilities" => Ok(CapValue::Array(
            names().into_iter().map(|name| CapValue::Str(name.to_string())).collect(),
        )),
        "runtime.input" => {
            let input = ctx.and_then(|c| c.input.as_ref());
            Ok(input.cloned().unwrap_or(CapValue::Null))
        }
        "runtime.inputGet" => {
            expect_arity(args, 1, name)?;
            let path = expect_str(args, 0, name)?;
            let input = ctx.and_then(|c| c.input.as_ref()).cloned().unwrap_or(CapValue::Null);
            Ok(navigate_input_path(&input, path))
        }
        "runtime.publish" => {
            expect_arity(args, 2, name)?;
            let key = expect_str(args, 0, name)?.to_string();
            let v = match args.get(1) {
                Some(CapValue::Int(n)) => serde_json::json!(*n),
                Some(CapValue::Float(f)) => {
                    if !f.is_finite() {
                        return Err(format!("{name}: value is non-finite"));
                    }
                    serde_json::json!(*f)
                }
                Some(CapValue::Str(s)) => serde_json::json!(s),
                Some(CapValue::Bool(b)) => serde_json::json!(*b),
                Some(CapValue::Null) => serde_json::Value::Null,
                Some(CapValue::Array(_)) => return Err(format!("{name}: array values not supported in v1")),
                None => return Err(format!("{name}: missing value argument")),
            };
            if let Some(context) = ctx {
                if let Some(buf) = &context.published {
                    buf.borrow_mut().insert(key, v);
                }
                // If `published` is None, the call is silently a no-op — this
                // is intentional so a capsule using `runtime.publish` still
                // runs in CLI / test contexts without requiring a buffer to
                // be set up.
            }
            Ok(CapValue::Null)
        }
        "file.exists" => {
            expect_arity(args, 1, name)?;
            let requested = expect_str(args, 0, name)?;
            let resolved = resolve_sandbox_path(ctx, requested, "file.exists")?;
            Ok(CapValue::Bool(resolved.exists()))
        }
        "file.readText" => {
            expect_arity(args, 1, name)?;
            let requested = expect_str(args, 0, name)?;
            let resolved = resolve_sandbox_path(ctx, requested, "file.readText")?;
            let metadata = std::fs::metadata(&resolved)
                .map_err(|e| format!("file.readText could not stat: {e}"))?;
            if metadata.len() > MAX_BYTES as u64 {
                return Err(format!("file.readText refuses files larger than {MAX_BYTES} bytes"));
            }
            let text = std::fs::read_to_string(&resolved)
                .map_err(|e| format!("file.readText could not read: {e}"))?;
            Ok(CapValue::Str(text))
        }
        "file.writeText" => {
            expect_arity(args, 2, name)?;
            let requested = expect_str(args, 0, name)?;
            let contents = expect_str(args, 1, name)?;
            let resolved = resolve_sandbox_path(ctx, requested, "file.writeText")?;
            if contents.len() > MAX_BYTES {
                return Err(format!("file.writeText refuses contents larger than {MAX_BYTES} bytes"));
            }
            std::fs::write(&resolved, contents)
                .map_err(|e| format!("file.writeText could not write: {e}"))?;
            Ok(CapValue::Bool(true))
        }
        "http.get" => {
            expect_arity(args, 1, name)?;
            let url = expect_url(expect_str(args, 0, name)?, name)?;
            let has_sandbox = check_network_sandbox(ctx, url, name)?;
            let agent = if has_sandbox {
                // Sandboxed: disable redirects to prevent redirect escape to private hosts
                ureq::AgentBuilder::new().redirects(0).build()
            } else {
                ureq::AgentBuilder::new().build()
            };
            let response = agent.get(url)
                .timeout(Duration::from_secs(10))
                .call()
                .map_err(http_error)?;
            read_http_response(response, name)
        }
        "http.post" => {
            expect_arity(args, 3, name)?;
            let url = expect_url(expect_str(args, 0, name)?, name)?;
            let has_sandbox = check_network_sandbox(ctx, url, name)?;
            let body = expect_str(args, 1, name)?;
            let content_type = expect_str(args, 2, name)?;
            if body.len() > MAX_BYTES {
                return Err(format!("http.post refuses bodies larger than {MAX_BYTES} bytes"));
            }
            let agent = if has_sandbox {
                ureq::AgentBuilder::new().redirects(0).build()
            } else {
                ureq::AgentBuilder::new().build()
            };
            let response = agent.post(url)
                .timeout(Duration::from_secs(10))
                .set("Content-Type", content_type)
                .send_string(body)
                .map_err(http_error)?;
            read_http_response(response, name)
        }
        "json.get" => {
            expect_arity(args, 2, name)?;
            let root = parse_json(expect_str(args, 0, name)?, name)?;
            let path = expect_str(args, 1, name)?;
            let value = json_path(&root, path)
                .ok_or_else(|| format!("json.get path '{path}' not found"))?;
            Ok(json_to_cap(value))
        }
        "json.has" => {
            expect_arity(args, 2, name)?;
            let root = parse_json(expect_str(args, 0, name)?, name)?;
            let path = expect_str(args, 1, name)?;
            Ok(CapValue::Bool(json_path(&root, path).is_some()))
        }
        "json.len" => {
            expect_arity(args, 2, name)?;
            let root = parse_json(expect_str(args, 0, name)?, name)?;
            let path = expect_str(args, 1, name)?;
            let value = json_path(&root, path)
                .ok_or_else(|| format!("json.len path '{path}' not found"))?;
            let len = match value {
                serde_json::Value::Array(items) => items.len(),
                serde_json::Value::Object(map) => map.len(),
                serde_json::Value::String(s) => s.chars().count(),
                other => return Err(format!("json.len cannot measure {}", json_kind(other))),
            };
            Ok(CapValue::Int(len as i64))
        }
        "sql.sqliteQuery" => {
            expect_arity(args, 2, name)?;
            let raw_path = expect_str(args, 0, name)?;
            let resolved = resolve_sandbox_path(ctx, raw_path, "sql.sqliteQuery")?;
            let resolved_str = resolved.to_string_lossy().to_string();
            let sql = expect_str(args, 1, name)?;
            sqlite_query_resolved(&resolved_str, sql, name)
        }
        "stats.mean" => {
            let values = numeric_array(args, 0, name)?;
            Ok(CapValue::Float(values.iter().sum::<f64>() / values.len() as f64))
        }
        "stats.stdDev" => {
            let values = numeric_array(args, 0, name)?;
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let var = values.iter().map(|v| {
                let d = v - mean;
                d * d
            }).sum::<f64>() / values.len() as f64;
            Ok(CapValue::Float(var.sqrt()))
        }
        "stats.min" => {
            let values = numeric_array(args, 0, name)?;
            Ok(CapValue::Float(values.iter().copied().fold(f64::INFINITY, f64::min)))
        }
        "stats.max" => {
            let values = numeric_array(args, 0, name)?;
            Ok(CapValue::Float(values.iter().copied().fold(f64::NEG_INFINITY, f64::max)))
        }
        "stats.percentile" => {
            expect_arity(args, 2, name)?;
            let mut values = numeric_array(args, 0, name)?;
            let p = number(args, 1, name)?;
            if !(0.0..=100.0).contains(&p) {
                return Err("stats.percentile expects percentile in 0..100".to_string());
            }
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let rank = (p / 100.0) * (values.len().saturating_sub(1) as f64);
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            let t = rank - lo as f64;
            Ok(CapValue::Float(values[lo] * (1.0 - t) + values[hi] * t))
        }
        "series.ewmaForecast" => {
            expect_arity(args, 2, name)?;
            let values = numeric_array(args, 0, name)?;
            let alpha = number(args, 1, name)?;
            if !(0.0..=1.0).contains(&alpha) {
                return Err("series.ewmaForecast expects alpha in 0..1".to_string());
            }
            let mut forecast = values[0];
            for value in values.iter().skip(1) {
                forecast = alpha * value + (1.0 - alpha) * forecast;
            }
            Ok(CapValue::Float(forecast))
        }
        "ops.autoScaleRecommend" => {
            expect_arity(args, 4, name)?;
            let load = number(args, 0, name)?;
            let target = number(args, 1, name)?;
            let min = integer(args, 2, name)?;
            let max = integer(args, 3, name)?;
            if target <= 0.0 || min < 0 || max < min {
                return Err("ops.autoScaleRecommend expects target > 0 and 0 <= min <= max".to_string());
            }
            let needed = (load / target).ceil() as i64;
            Ok(CapValue::Int(needed.clamp(min, max)))
        }
        "nav.ephemerisState" => {
            let (path, body, et) = ephemeris_args(args, name)?;
            let resolved = resolve_sandbox_path(ctx, &path, "nav.ephemerisState")?;
            let resolved_str = resolved.to_string_lossy().to_string();
            let state = load_ephemeris_state(&resolved_str, &body, et)?;
            Ok(CapValue::Array(state.into_iter().map(CapValue::Float).collect()))
        }
        "nav.horizonsVectors" => horizons_vectors(args, ctx, name),
        "nav.norm3" => {
            let nums = numbers(args, 3, name)?;
            Ok(CapValue::Float((nums[0] * nums[0] + nums[1] * nums[1] + nums[2] * nums[2]).sqrt()))
        }
        "nav.distance3" => {
            let nums = numbers(args, 6, name)?;
            let dx = nums[0] - nums[3];
            let dy = nums[1] - nums[4];
            let dz = nums[2] - nums[5];
            Ok(CapValue::Float((dx * dx + dy * dy + dz * dz).sqrt()))
        }
        "nav.dot3" => {
            let nums = numbers(args, 6, name)?;
            Ok(CapValue::Float(nums[0] * nums[3] + nums[1] * nums[4] + nums[2] * nums[5]))
        }
        "nav.radialVelocity" => {
            let nums = numbers(args, 6, name)?;
            let r = (nums[0] * nums[0] + nums[1] * nums[1] + nums[2] * nums[2]).sqrt();
            if r == 0.0 {
                return Err("nav.radialVelocity requires non-zero position".to_string());
            }
            Ok(CapValue::Float((nums[0] * nums[3] + nums[1] * nums[4] + nums[2] * nums[5]) / r))
        }
        "astro.lambertSolve" => {
            let nums = numbers(args, 8, name)?;
            let r1 = [nums[0], nums[1], nums[2]];
            let r2 = [nums[3], nums[4], nums[5]];
            let result = crate::lambert::solve(r1, r2, nums[6], nums[7], true);
            let status = if result.converged { 1.0 } else { 0.0 };
            Ok(CapValue::Array(vec![
                CapValue::Float(result.v1[0]), CapValue::Float(result.v1[1]), CapValue::Float(result.v1[2]),
                CapValue::Float(result.v2[0]), CapValue::Float(result.v2[1]), CapValue::Float(result.v2[2]),
                CapValue::Float(status),
            ]))
        }
        _ => Err(format!("unknown capability '{name}'")),
    }
}

/// Walk a dot-separated path through a CapValue.
/// Supports key lookup in object-like pair arrays and numeric indexes.
/// Missing paths return Null.
fn navigate_input_path(value: &CapValue, path: &str) -> CapValue {
    let mut current = value.clone();
    for segment in path.split('.') {
        current = match current {
            CapValue::Array(ref items) => {
                // Try numeric index first
                if let Ok(idx) = segment.parse::<usize>() {
                    items.get(idx).cloned().unwrap_or(CapValue::Null)
                } else {
                    // Key lookup in object-like pair array: [[key, val], [key, val], ...]
                    let mut found = CapValue::Null;
                    for item in items {
                        if let CapValue::Array(pair) = item {
                            if pair.len() == 2 {
                                if let CapValue::Str(k) = &pair[0] {
                                    if k == segment {
                                        found = pair[1].clone();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    found
                }
            }
            _ => return CapValue::Null,
        };
        if matches!(current, CapValue::Null) {
            return CapValue::Null;
        }
    }
    current
}

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

const MAX_BYTES: usize = 1024 * 1024;
const MAX_SQL_ROWS: usize = 1000;

fn expect_arity(args: &[CapValue], expected: usize, capability: &str) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("{capability} expects {expected} arguments, got {}", args.len()));
    }
    Ok(())
}

fn expect_str<'a>(args: &'a [CapValue], idx: usize, capability: &str) -> Result<&'a str, String> {
    match args.get(idx) {
        Some(CapValue::Str(s)) => Ok(s),
        Some(other) => Err(format!("{capability} argument {} must be string, got {}", idx + 1, other.type_name())),
        None => Err(format!("{capability} missing argument {}", idx + 1)),
    }
}

fn integer(args: &[CapValue], idx: usize, capability: &str) -> Result<i64, String> {
    match args.get(idx) {
        Some(CapValue::Int(n)) => Ok(*n),
        Some(CapValue::Float(n)) if n.fract() == 0.0 && n.is_finite() => Ok(*n as i64),
        Some(other) => Err(format!("{capability} argument {} must be int, got {}", idx + 1, other.type_name())),
        None => Err(format!("{capability} missing argument {}", idx + 1)),
    }
}

fn number(args: &[CapValue], idx: usize, capability: &str) -> Result<f64, String> {
    let n = match args.get(idx) {
        Some(CapValue::Int(n)) => *n as f64,
        Some(CapValue::Float(n)) => *n,
        Some(other) => return Err(format!("{capability} argument {} must be number, got {}", idx + 1, other.type_name())),
        None => return Err(format!("{capability} missing argument {}", idx + 1)),
    };
    if !n.is_finite() {
        return Err(format!("{capability} argument {} must be finite", idx + 1));
    }
    Ok(n)
}

fn numbers(args: &[CapValue], count: usize, capability: &str) -> Result<Vec<f64>, String> {
    expect_arity(args, count, capability)?;
    (0..count).map(|i| number(args, i, capability)).collect()
}

fn numeric_array(args: &[CapValue], idx: usize, capability: &str) -> Result<Vec<f64>, String> {
    let values = match args.get(idx) {
        Some(CapValue::Array(items)) => items,
        Some(other) => return Err(format!("{capability} argument {} must be array, got {}", idx + 1, other.type_name())),
        None => return Err(format!("{capability} missing argument {}", idx + 1)),
    };
    if values.is_empty() {
        return Err(format!("{capability} requires a non-empty numeric array"));
    }
    values.iter().enumerate().map(|(i, value)| {
        let n = match value {
            CapValue::Int(n) => *n as f64,
            CapValue::Float(n) => *n,
            other => return Err(format!("{capability} array item {} must be number, got {}", i + 1, other.type_name())),
        };
        if !n.is_finite() {
            return Err(format!("{capability} array item {} must be finite", i + 1));
        }
        Ok(n)
    }).collect()
}

fn expect_url<'a>(url: &'a str, capability: &str) -> Result<&'a str, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("{capability} only accepts http:// or https:// URLs"));
    }
    Ok(url)
}

fn http_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, response) => {
            format!("http request failed with status {} {}", code, response.status_text())
        }
        ureq::Error::Transport(e) => format!("http transport error: {e}"),
    }
}

fn read_http_response(response: ureq::Response, capability: &str) -> Result<CapValue, String> {
    let mut reader = response.into_reader().take((MAX_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)
        .map_err(|e| format!("{capability} failed reading response: {e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("{capability} refuses responses larger than {MAX_BYTES} bytes"));
    }
    let body = String::from_utf8(bytes)
        .map_err(|e| format!("{capability} response was not UTF-8: {e}"))?;
    Ok(CapValue::Str(body))
}

fn parse_json(text: &str, capability: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{capability} invalid JSON: {e}"))
}

fn json_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    if path.is_empty() || path == "$" {
        return Some(root);
    }
    let mut current = root;
    for segment in path.trim_start_matches("$.").split('.') {
        if segment.is_empty() {
            continue;
        }
        current = match current {
            serde_json::Value::Object(map) => map.get(segment)?,
            serde_json::Value::Array(items) => {
                let idx = segment.parse::<usize>().ok()?;
                items.get(idx)?
            }
            _ => return None,
        };
    }
    Some(current)
}

fn json_to_cap(value: &serde_json::Value) -> CapValue {
    match value {
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
        serde_json::Value::Array(items) => CapValue::Array(items.iter().map(json_to_cap).collect()),
        serde_json::Value::Object(_) => CapValue::Str(value.to_string()),
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn sqlite_query_resolved(db_path: &str, sql: &str, _capability: &str) -> Result<CapValue, String> {
    let lower = sql.trim_start().to_ascii_lowercase();
    if !(lower.starts_with("select") || lower.starts_with("with") || lower.starts_with("pragma")) {
        return Err("sql.sqliteQuery only allows SELECT, WITH, or PRAGMA statements".to_string());
    }

    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(db_path, flags)
        .map_err(|e| format!("sql.sqliteQuery could not open '{db_path}' read-only: {e}"))?;
    let mut stmt = conn.prepare(sql)
        .map_err(|e| format!("sql.sqliteQuery could not prepare query: {e}"))?;
    if !stmt.readonly() {
        return Err("sql.sqliteQuery rejected non-read-only statement".to_string());
    }
    let col_count = stmt.column_count();
    let mut query = stmt.query([])
        .map_err(|e| format!("sql.sqliteQuery failed: {e}"))?;
    let mut rows = Vec::new();
    while let Some(row) = query.next()
        .map_err(|e| format!("sql.sqliteQuery failed reading row: {e}"))?
    {
        if rows.len() >= MAX_SQL_ROWS {
            break;
        }
        let mut values = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let value = row.get_ref(i)
                .map_err(|e| format!("sql.sqliteQuery failed reading column {}: {e}", i + 1))?;
            values.push(sql_value_to_cap(value));
        }
        rows.push(CapValue::Array(values));
    }
    Ok(CapValue::Array(rows))
}

fn sql_value_to_cap(value: rusqlite::types::ValueRef<'_>) -> CapValue {
    match value {
        rusqlite::types::ValueRef::Null => CapValue::Null,
        rusqlite::types::ValueRef::Integer(n) => CapValue::Int(n),
        rusqlite::types::ValueRef::Real(n) => CapValue::Float(n),
        rusqlite::types::ValueRef::Text(bytes) => {
            CapValue::Str(String::from_utf8_lossy(bytes).to_string())
        }
        rusqlite::types::ValueRef::Blob(bytes) => CapValue::Str(hex(bytes)),
    }
}

fn horizons_vectors(
    args: &[CapValue],
    ctx: Option<&crate::context::ExecutionContext>,
    capability: &str,
) -> Result<CapValue, String> {
    expect_arity(args, 4, capability)?;
    let body = expect_str(args, 0, capability)?;
    let start = expect_str(args, 1, capability)?;
    let stop = expect_str(args, 2, capability)?;
    let step_days = number(args, 3, capability)?;
    if !(step_days > 0.0 && step_days <= 365.0) {
        return Err(format!("{capability} expects step_days in 0..365"));
    }
    if !body.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("{capability} body must be a Horizons command id/name"));
    }

    let api_url = "https://ssd.jpl.nasa.gov/api/horizons.api";
    let has_sandbox = check_network_sandbox(ctx, api_url, capability)?;
    let agent = if has_sandbox {
        ureq::AgentBuilder::new().redirects(0).build()
    } else {
        ureq::AgentBuilder::new().build()
    };

    let step_value = if step_days.fract() == 0.0 {
        format!("{}", step_days as i64)
    } else {
        format!("{step_days}")
    };
    let step = format!("{step_value}d");
    let response = agent
        .get(api_url)
        .timeout(Duration::from_secs(10))
        .query("format", "json")
        .query("COMMAND", body)
        .query("OBJ_DATA", "NO")
        .query("MAKE_EPHEM", "YES")
        .query("EPHEM_TYPE", "VECTORS")
        .query("CENTER", "@sun")
        .query("REF_PLANE", "ECLIPTIC")
        .query("START_TIME", start)
        .query("STOP_TIME", stop)
        .query("STEP_SIZE", &step)
        .query("OUT_UNITS", "AU-D")
        .query("VEC_TABLE", "2")
        .query("CSV_FORMAT", "NO")
        .call()
        .map_err(http_error)?;

    let body_text = match read_http_response(response, capability)? {
        CapValue::Str(text) => text,
        _ => unreachable!("read_http_response always returns a string"),
    };
    let root = parse_json(&body_text, capability)?;
    if let Some(err) = root.get("error").and_then(|v| v.as_str()) {
        return Err(format!("{capability} Horizons error: {err}"));
    }
    let result = root.get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{capability} Horizons response missing result text"))?;
    let rows = parse_horizons_vector_result(result, capability)?;
    let mut flat = Vec::with_capacity(rows.len() * 7);
    for (jd, state) in rows {
        flat.push(CapValue::Float(jd));
        flat.extend(state.into_iter().map(CapValue::Float));
    }
    Ok(CapValue::Array(flat))
}

fn parse_horizons_vector_result(result: &str, capability: &str) -> Result<Vec<(f64, [f64; 6])>, String> {
    let mut rows = Vec::new();
    let mut in_table = false;
    let mut lines = result.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed == "$$SOE" {
            in_table = true;
            continue;
        }
        if trimmed == "$$EOE" {
            break;
        }
        if !in_table || trimmed.is_empty() {
            continue;
        }
        if !trimmed.contains(" = ") {
            continue;
        }
        let jd = trimmed.split_whitespace().next()
            .ok_or_else(|| format!("{capability} could not read Horizons JD"))?
            .parse::<f64>()
            .map_err(|e| format!("{capability} invalid Horizons JD: {e}"))?;
        let pos = lines.next()
            .ok_or_else(|| format!("{capability} Horizons vector missing position line"))?;
        let vel = lines.next()
            .ok_or_else(|| format!("{capability} Horizons vector missing velocity line"))?;
        rows.push((jd, [
            parse_horizons_component(pos, "X =", capability)?,
            parse_horizons_component(pos, "Y =", capability)?,
            parse_horizons_component(pos, "Z =", capability)?,
            parse_horizons_component(vel, "VX=", capability)?,
            parse_horizons_component(vel, "VY=", capability)?,
            parse_horizons_component(vel, "VZ=", capability)?,
        ]));
    }
    if rows.is_empty() {
        return Err(format!("{capability} Horizons result contained no vector rows"));
    }
    Ok(rows)
}

fn parse_horizons_component(line: &str, label: &str, capability: &str) -> Result<f64, String> {
    let start = line.find(label)
        .ok_or_else(|| format!("{capability} Horizons row missing {label}"))?
        + label.len();
    let token = line[start..].trim_start().split_whitespace().next()
        .ok_or_else(|| format!("{capability} Horizons row missing value after {label}"))?;
    token.parse::<f64>()
        .map_err(|e| format!("{capability} invalid Horizons value after {label}: {e}"))
}

fn ephemeris_args(args: &[CapValue], capability: &str) -> Result<(String, String, f64), String> {
    expect_arity(args, 3, capability)?;
    Ok((
        expect_str(args, 0, capability)?.to_string(),
        expect_str(args, 1, capability)?.to_string(),
        number(args, 2, capability)?,
    ))
}

fn load_ephemeris_state(path: &str, body: &str, et: f64) -> Result<[f64; 6], String> {
    let table = std::fs::read_to_string(path)
        .map_err(|e| format!("nav.ephemerisState could not read '{path}': {e}"))?;
    let mut declared_body: Option<String> = None;
    let mut rows: Vec<(f64, [f64; 6])> = Vec::new();

    for (line_no, line) in table.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.first() == Some(&"BODY") {
            if parts.len() < 2 {
                return Err(format!("invalid ephemeris BODY line {} in '{path}'", line_no + 1));
            }
            declared_body = Some(parts[1].to_string());
            continue;
        }
        if parts.first().is_some_and(|p| p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())) {
            continue;
        }
        if parts.len() != 7 {
            return Err(format!("invalid ephemeris row {} in '{path}'", line_no + 1));
        }
        let mut nums = [0.0; 7];
        for (i, part) in parts.iter().enumerate() {
            nums[i] = part.parse::<f64>()
                .map_err(|_| format!("invalid ephemeris number '{}' on line {}", part, line_no + 1))?;
            if !nums[i].is_finite() {
                return Err(format!("non-finite ephemeris number on line {}", line_no + 1));
            }
        }
        rows.push((nums[0], [nums[1], nums[2], nums[3], nums[4], nums[5], nums[6]]));
    }

    if let Some(declared) = declared_body {
        if declared != body {
            return Err(format!("ephemeris body mismatch: file has {declared}, requested {body}"));
        }
    }
    if rows.is_empty() {
        return Err(format!("ephemeris '{path}' contains no state rows"));
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    if et < rows[0].0 || et > rows[rows.len() - 1].0 {
        return Err(format!(
            "ephemeris time {et} outside coverage {}..{}",
            rows[0].0,
            rows[rows.len() - 1].0
        ));
    }
    if let Some((_, state)) = rows.iter().find(|(row_et, _)| (*row_et - et).abs() < 1e-9) {
        return Ok(*state);
    }
    for pair in rows.windows(2) {
        let (t0, s0) = pair[0];
        let (t1, s1) = pair[1];
        if et >= t0 && et <= t1 {
            let alpha = (et - t0) / (t1 - t0);
            let mut out = [0.0; 6];
            for i in 0..6 {
                out[i] = s0[i] + (s1[i] - s0[i]) * alpha;
            }
            return Ok(out);
        }
    }
    Err(format!("ephemeris time {et} could not be interpolated"))
}

fn quoted(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| format!("\"{}\"", esc(s))).collect()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn hex(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(LUT[(byte >> 4) as usize] as char);
        out.push(LUT[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{new_published_buffer, ExecutionContext, SelectionMode};

    fn ctx_with_buffer() -> ExecutionContext {
        ExecutionContext {
            policy: None,
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: Some(new_published_buffer()),
        }
    }

    fn ctx_no_buffer() -> ExecutionContext {
        ExecutionContext {
            policy: None,
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    #[test]
    fn runtime_publish_roundtrips_each_supported_type() {
        let ctx = ctx_with_buffer();

        execute(
            "runtime.publish",
            &[CapValue::Str("forecast".into()), CapValue::Float(142.7)],
            Some(&ctx),
        )
        .expect("publish float");

        execute(
            "runtime.publish",
            &[CapValue::Str("recommended_count".into()), CapValue::Int(7)],
            Some(&ctx),
        )
        .expect("publish int");

        execute(
            "runtime.publish",
            &[CapValue::Str("policy".into()), CapValue::Str("forecast_match".into())],
            Some(&ctx),
        )
        .expect("publish str");

        execute(
            "runtime.publish",
            &[CapValue::Str("override".into()), CapValue::Bool(true)],
            Some(&ctx),
        )
        .expect("publish bool");

        execute(
            "runtime.publish",
            &[CapValue::Str("unset".into()), CapValue::Null],
            Some(&ctx),
        )
        .expect("publish null");

        let buf = ctx.published.as_ref().expect("buffer present");
        let map = buf.borrow();

        assert_eq!(map.get("forecast"), Some(&serde_json::json!(142.7)));
        assert_eq!(map.get("recommended_count"), Some(&serde_json::json!(7)));
        assert_eq!(map.get("policy"), Some(&serde_json::json!("forecast_match")));
        assert_eq!(map.get("override"), Some(&serde_json::json!(true)));
        assert_eq!(map.get("unset"), Some(&serde_json::Value::Null));
        // BTreeMap => deterministic, sorted-by-name iteration
        let keys: Vec<&String> = map.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn runtime_publish_overwrites_same_key() {
        let ctx = ctx_with_buffer();
        execute(
            "runtime.publish",
            &[CapValue::Str("k".into()), CapValue::Int(1)],
            Some(&ctx),
        ).unwrap();
        execute(
            "runtime.publish",
            &[CapValue::Str("k".into()), CapValue::Int(2)],
            Some(&ctx),
        ).unwrap();
        let buf = ctx.published.as_ref().unwrap();
        assert_eq!(buf.borrow().get("k"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn runtime_publish_is_noop_when_buffer_absent() {
        let ctx = ctx_no_buffer();
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("forecast".into()), CapValue::Float(1.0)],
            Some(&ctx),
        );
        assert!(matches!(r, Ok(CapValue::Null)));
    }

    #[test]
    fn runtime_publish_noop_with_no_context() {
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("forecast".into()), CapValue::Float(1.0)],
            None,
        );
        assert!(matches!(r, Ok(CapValue::Null)));
    }

    #[test]
    fn runtime_publish_rejects_array_values() {
        let ctx = ctx_with_buffer();
        let r = execute(
            "runtime.publish",
            &[
                CapValue::Str("xs".into()),
                CapValue::Array(vec![CapValue::Int(1), CapValue::Int(2)]),
            ],
            Some(&ctx),
        );
        match r {
            Err(msg) => assert!(msg.to_lowercase().contains("array"), "expected 'array' in error, got: {msg}"),
            Ok(_) => panic!("expected error for array value"),
        }
    }

    #[test]
    fn runtime_publish_rejects_non_finite_float() {
        let ctx = ctx_with_buffer();
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("nan".into()), CapValue::Float(f64::NAN)],
            Some(&ctx),
        );
        assert!(r.is_err(), "NaN must be rejected");
        let r2 = execute(
            "runtime.publish",
            &[CapValue::Str("inf".into()), CapValue::Float(f64::INFINITY)],
            Some(&ctx),
        );
        assert!(r2.is_err(), "Infinity must be rejected");
    }

    #[test]
    fn runtime_publish_requires_two_args() {
        let ctx = ctx_with_buffer();
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("only_name".into())],
            Some(&ctx),
        );
        assert!(r.is_err());
    }

    #[test]
    fn runtime_publish_registered_in_catalog() {
        assert!(get("runtime.publish").is_some());
        assert!(names().iter().any(|n| *n == "runtime.publish"));
    }

    // ── file.writeText sandbox tests ──
    //
    // The write handler must defeat symlink-escape: an existing symlink
    // inside the sandbox pointing at a file outside the sandbox must not
    // be writable through the capability.

    use crate::context::ExecutionPolicy;

    fn fresh_tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lycan-cap-write-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn sandboxed_ctx(root: &std::path::Path) -> ExecutionContext {
        let mut policy = ExecutionPolicy::default();
        policy.file_root = Some(root.to_string_lossy().to_string());
        policy.allow_file_read = true;
        policy.allow_file_write = true;
        ExecutionContext {
            policy: Some(policy),
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    #[test]
    fn write_in_sandbox_succeeds() {
        let root = fresh_tempdir("ok");
        let ctx = sandboxed_ctx(&root);

        let result = execute(
            "file.writeText",
            &[CapValue::Str("hello.txt".into()), CapValue::Str("greetings".into())],
            Some(&ctx),
        );
        assert!(matches!(result, Ok(CapValue::Bool(true))), "expected Ok(true), got {result:?}");

        let body = std::fs::read_to_string(root.join("hello.txt")).expect("file written");
        assert_eq!(body, "greetings");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn write_through_symlink_pointing_outside_fails() {
        // Layout:
        //   tempdir/
        //     secret.txt             ← outside sandbox, must NOT be modified
        //     sandbox/                ← sandbox root
        //       escape.txt → ../secret.txt
        let base = fresh_tempdir("symlink");
        let sandbox = base.join("sandbox");
        std::fs::create_dir_all(&sandbox).expect("create sandbox");
        let secret = base.join("secret.txt");
        std::fs::write(&secret, "original").expect("seed secret");

        let escape = sandbox.join("escape.txt");
        std::os::unix::fs::symlink(std::path::Path::new("../secret.txt"), &escape)
            .expect("create symlink");

        let ctx = sandboxed_ctx(&sandbox);

        let result = execute(
            "file.writeText",
            &[CapValue::Str("escape.txt".into()), CapValue::Str("evil".into())],
            Some(&ctx),
        );

        assert!(result.is_err(), "expected Err for symlink escape, got {result:?}");
        let msg = result.err().unwrap();
        assert!(
            msg.contains("escapes sandbox"),
            "expected 'escapes sandbox' in error, got: {msg}"
        );

        // The crucial assertion: the file outside the sandbox MUST be untouched.
        let after = std::fs::read_to_string(&secret).expect("secret still readable");
        assert_eq!(after, "original", "symlink target was clobbered — sandbox escape!");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_to_nonexistent_path_creates_file_inside_sandbox() {
        let root = fresh_tempdir("nonexistent");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        let ctx = sandboxed_ctx(&root);

        let result = execute(
            "file.writeText",
            &[CapValue::Str("nested/new.txt".into()), CapValue::Str("fresh".into())],
            Some(&ctx),
        );
        assert!(matches!(result, Ok(CapValue::Bool(true))), "expected Ok(true), got {result:?}");

        let body = std::fs::read_to_string(nested.join("new.txt")).expect("file written");
        assert_eq!(body, "fresh");

        let _ = std::fs::remove_dir_all(&root);
    }
}
