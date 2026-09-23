//! Capability execution dispatch and argument helpers.

use crate::combinatorics::{self, BadApKind, SearchStatus};
use std::io::Read;
use std::time::Duration;

use super::horizons::{ephemeris_args, horizons_vectors, load_ephemeris_state};
use super::registry::{CapValue, get, names};
use super::sandbox::{check_network_sandbox, resolve_sandbox_path};

pub fn execute(
    name: &str,
    args: &[CapValue],
    ctx: Option<&crate::context::ExecutionContext>,
) -> Result<CapValue, String> {
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
                        return Err(format!(
                            "capability={name} effect={effect} denied by policy"
                        ));
                    }
                }
            }
        }
    }

    match name {
        "runtime.capabilities" => Ok(CapValue::Array(
            names()
                .into_iter()
                .map(|name| CapValue::Str(name.to_string()))
                .collect(),
        )),
        "runtime.input" => {
            let input = ctx.and_then(|c| c.input.as_ref());
            Ok(input.cloned().unwrap_or(CapValue::Null))
        }
        "runtime.inputGet" => {
            expect_arity(args, 1, name)?;
            let path = expect_str(args, 0, name)?;
            let input = ctx
                .and_then(|c| c.input.as_ref())
                .cloned()
                .unwrap_or(CapValue::Null);
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
                Some(CapValue::Array(_)) => {
                    return Err(format!("{name}: array values not supported in v1"));
                }
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
                return Err(format!(
                    "file.readText refuses files larger than {MAX_BYTES} bytes"
                ));
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
                return Err(format!(
                    "file.writeText refuses contents larger than {MAX_BYTES} bytes"
                ));
            }
            std::fs::write(&resolved, contents)
                .map_err(|e| format!("file.writeText could not write: {e}"))?;
            Ok(CapValue::Bool(true))
        }
        "http.get" => {
            expect_arity(args, 1, name)?;
            let url = expect_url(expect_str(args, 0, name)?, name)?;
            // Sandboxed requests go through the guard's agent: no redirects,
            // allow-list and private-address checks at connect time.
            let agent = match check_network_sandbox(ctx, url, name)? {
                Some(guard) => guard.agent(),
                None => ureq::AgentBuilder::new().build(),
            };
            let response = agent
                .get(url)
                .timeout(Duration::from_secs(10))
                .call()
                .map_err(http_error)?;
            read_http_response(response, name)
        }
        "http.post" => {
            expect_arity(args, 3, name)?;
            let url = expect_url(expect_str(args, 0, name)?, name)?;
            let guard = check_network_sandbox(ctx, url, name)?;
            let body = expect_str(args, 1, name)?;
            let content_type = expect_str(args, 2, name)?;
            if body.len() > MAX_BYTES {
                return Err(format!(
                    "http.post refuses bodies larger than {MAX_BYTES} bytes"
                ));
            }
            let agent = match guard {
                Some(guard) => guard.agent(),
                None => ureq::AgentBuilder::new().build(),
            };
            let response = agent
                .post(url)
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
            Ok(CapValue::Float(
                values.iter().sum::<f64>() / values.len() as f64,
            ))
        }
        "stats.stdDev" => {
            let values = numeric_array(args, 0, name)?;
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let var = values
                .iter()
                .map(|v| {
                    let d = v - mean;
                    d * d
                })
                .sum::<f64>()
                / values.len() as f64;
            Ok(CapValue::Float(var.sqrt()))
        }
        "stats.min" => {
            let values = numeric_array(args, 0, name)?;
            Ok(CapValue::Float(
                values.iter().copied().fold(f64::INFINITY, f64::min),
            ))
        }
        "stats.max" => {
            let values = numeric_array(args, 0, name)?;
            Ok(CapValue::Float(
                values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            ))
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
                return Err(
                    "ops.autoScaleRecommend expects target > 0 and 0 <= min <= max".to_string(),
                );
            }
            // load/target can be astronomically large; `as i64` saturates.
            // The clamp hides saturation, so reject non-finite / absurd
            // ratios explicitly instead (decision 2026-09-08, §8 policy).
            let needed_f = (load / target).ceil();
            if !needed_f.is_finite() || needed_f > i64::MAX as f64 {
                return Err("ops.autoScaleRecommend load/target out of range".to_string());
            }
            let needed = needed_f as i64;
            Ok(CapValue::Int(needed.clamp(min, max)))
        }
        "comb.apTuples" => {
            expect_arity(args, 2, name)?;
            let n = bounded_usize(args, 0, name, 512)?;
            let k = bounded_usize(args, 1, name, 64)?;
            let aps = combinatorics::arithmetic_progressions(n, k);
            if aps.len() > 50_000 {
                return Err("comb.apTuples output exceeds 50000 progressions".to_string());
            }
            Ok(CapValue::Array(
                aps.into_iter()
                    .map(|ap| {
                        CapValue::Array(
                            ap.into_iter()
                                .map(|term| CapValue::Int(term as i64))
                                .collect(),
                        )
                    })
                    .collect(),
            ))
        }
        "comb.isGoodColoring" => {
            expect_arity(args, 2, name)?;
            let colors = integer_array(args, 0, name, 512)?;
            let k = bounded_usize(args, 1, name, colors.len().max(1))?;
            Ok(CapValue::Bool(combinatorics::is_good_coloring(&colors, k)))
        }
        "comb.badAp" => {
            expect_arity(args, 2, name)?;
            let colors = integer_array(args, 0, name, 512)?;
            let k = bounded_usize(args, 1, name, colors.len().max(1))?;
            let Some(bad) = combinatorics::bad_arithmetic_progression(&colors, k) else {
                return Ok(CapValue::Array(Vec::new()));
            };
            let kind = match bad.kind {
                BadApKind::Monochromatic => "monochromatic",
                BadApKind::Rainbow => "rainbow",
            };
            Ok(CapValue::Array(vec![
                CapValue::Str(kind.to_string()),
                CapValue::Array(
                    bad.terms
                        .into_iter()
                        .map(|term| CapValue::Int(term as i64))
                        .collect(),
                ),
                CapValue::Array(
                    bad.colors
                        .into_iter()
                        .map(|color| CapValue::Int(color as i64))
                        .collect(),
                ),
            ]))
        }
        "comb.goodColoringWitness" => {
            expect_arity(args, 3, name)?;
            let n = bounded_usize(args, 0, name, 32)?;
            let k = bounded_usize(args, 1, name, n)?;
            let node_limit = bounded_usize(args, 2, name, 5_000_000)?;
            let result = combinatorics::search_good_coloring(n, k, None, node_limit);
            let status = match result.status {
                SearchStatus::Exists => "exists",
                SearchStatus::Unsat => "unsat",
                SearchStatus::Inconclusive => "inconclusive",
            };
            let mut out = vec![
                CapValue::Str(status.to_string()),
                CapValue::Int(result.nodes as i64),
            ];
            if let Some(coloring) = result.coloring {
                out.extend(
                    coloring
                        .into_iter()
                        .map(|color| CapValue::Int((color + 1) as i64)),
                );
            }
            Ok(CapValue::Array(out))
        }
        "comb.hasThreeDistinct4ApColoring" => {
            expect_arity(args, 1, name)?;
            let colors = integer_array(args, 0, name, 512)?;
            Ok(CapValue::Bool(combinatorics::is_good_coloring_160(&colors)))
        }
        "comb.badThreeDistinct4Ap" => {
            expect_arity(args, 1, name)?;
            let colors = integer_array(args, 0, name, 512)?;
            let Some(bad) = combinatorics::bad_arithmetic_progression_160(&colors) else {
                return Ok(CapValue::Array(Vec::new()));
            };
            Ok(CapValue::Array(vec![
                CapValue::Str("low_distinct".to_string()),
                CapValue::Array(
                    bad.terms
                        .into_iter()
                        .map(|term| CapValue::Int(term as i64))
                        .collect(),
                ),
                CapValue::Array(
                    bad.colors
                        .into_iter()
                        .map(|color| CapValue::Int(color as i64))
                        .collect(),
                ),
                CapValue::Int(bad.distinct_colors as i64),
            ]))
        }
        "comb.threeDistinct4ApWitness" => {
            expect_arity(args, 3, name)?;
            let n = bounded_usize(args, 0, name, 64)?;
            let max_colors = bounded_usize(args, 1, name, 64)?;
            let node_limit = bounded_usize(args, 2, name, 5_000_000)?;
            let result = combinatorics::search_coloring_160(n, max_colors, node_limit);
            let status = match result.status {
                SearchStatus::Exists => "exists",
                SearchStatus::Unsat => "unsat",
                SearchStatus::Inconclusive => "inconclusive",
            };
            let mut out = vec![
                CapValue::Str(status.to_string()),
                CapValue::Int(result.nodes as i64),
            ];
            if let Some(coloring) = result.coloring {
                out.extend(
                    coloring
                        .into_iter()
                        .map(|color| CapValue::Int((color + 1) as i64)),
                );
            }
            Ok(CapValue::Array(out))
        }
        "comb.threeDistinct4ApSatWitness" => {
            expect_arity(args, 3, name)?;
            let n = bounded_usize(args, 0, name, 64)?;
            let max_colors = bounded_usize(args, 1, name, 64)?;
            let node_limit = bounded_usize(args, 2, name, 5_000_000)?;
            let result = combinatorics::search_coloring_160_sat(n, max_colors, node_limit);
            let status = match result.status {
                SearchStatus::Exists => "exists",
                SearchStatus::Unsat => "unsat",
                SearchStatus::Inconclusive => "inconclusive",
            };
            let mut out = vec![
                CapValue::Str(status.to_string()),
                CapValue::Int(result.nodes as i64),
                CapValue::Int(result.variables as i64),
                CapValue::Int(result.clauses as i64),
            ];
            if let Some(coloring) = result.coloring {
                out.extend(
                    coloring
                        .into_iter()
                        .map(|color| CapValue::Int((color + 1) as i64)),
                );
            }
            Ok(CapValue::Array(out))
        }
        "nav.ephemerisState" => {
            let (path, body, et) = ephemeris_args(args, name)?;
            let resolved = resolve_sandbox_path(ctx, &path, "nav.ephemerisState")?;
            let resolved_str = resolved.to_string_lossy().to_string();
            let state = load_ephemeris_state(&resolved_str, &body, et)?;
            Ok(CapValue::Array(
                state.into_iter().map(CapValue::Float).collect(),
            ))
        }
        "nav.horizonsVectors" => horizons_vectors(args, ctx, name),
        "nav.norm3" => {
            let nums = numbers(args, 3, name)?;
            Ok(CapValue::Float(
                (nums[0] * nums[0] + nums[1] * nums[1] + nums[2] * nums[2]).sqrt(),
            ))
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
            Ok(CapValue::Float(
                nums[0] * nums[3] + nums[1] * nums[4] + nums[2] * nums[5],
            ))
        }
        "nav.radialVelocity" => {
            let nums = numbers(args, 6, name)?;
            let r = (nums[0] * nums[0] + nums[1] * nums[1] + nums[2] * nums[2]).sqrt();
            if r == 0.0 {
                return Err("nav.radialVelocity requires non-zero position".to_string());
            }
            Ok(CapValue::Float(
                (nums[0] * nums[3] + nums[1] * nums[4] + nums[2] * nums[5]) / r,
            ))
        }
        "astro.lambertSolve" => {
            let nums = numbers(args, 8, name)?;
            let r1 = [nums[0], nums[1], nums[2]];
            let r2 = [nums[3], nums[4], nums[5]];
            let result = crate::lambert::solve(r1, r2, nums[6], nums[7], true);
            let status = if result.converged { 1.0 } else { 0.0 };
            Ok(CapValue::Array(vec![
                CapValue::Float(result.v1[0]),
                CapValue::Float(result.v1[1]),
                CapValue::Float(result.v1[2]),
                CapValue::Float(result.v2[0]),
                CapValue::Float(result.v2[1]),
                CapValue::Float(result.v2[2]),
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

const MAX_BYTES: usize = 1024 * 1024;
const MAX_SQL_ROWS: usize = 1000;

pub(crate) fn expect_arity(
    args: &[CapValue],
    expected: usize,
    capability: &str,
) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!(
            "{capability} expects {expected} arguments, got {}",
            args.len()
        ));
    }
    Ok(())
}

pub(crate) fn expect_str<'a>(
    args: &'a [CapValue],
    idx: usize,
    capability: &str,
) -> Result<&'a str, String> {
    match args.get(idx) {
        Some(CapValue::Str(s)) => Ok(s),
        Some(other) => Err(format!(
            "{capability} argument {} must be string, got {}",
            idx + 1,
            other.type_name()
        )),
        None => Err(format!("{capability} missing argument {}", idx + 1)),
    }
}

fn integer(args: &[CapValue], idx: usize, capability: &str) -> Result<i64, String> {
    match args.get(idx) {
        Some(CapValue::Int(n)) => Ok(*n),
        // Language decision 2026-09-08: out-of-range float->int is an error,
        // never silent saturation (Rust `as i64` saturates; 9.3e18 would
        // otherwise arrive inside a capability as i64::MAX).
        Some(CapValue::Float(n))
            if n.fract() == 0.0
                && n.is_finite()
                && *n >= i64::MIN as f64
                && *n <= i64::MAX as f64 =>
        {
            Ok(*n as i64)
        }
        Some(CapValue::Float(_)) => Err(format!(
            "{capability} argument {} is out of i64 range",
            idx + 1
        )),
        Some(other) => Err(format!(
            "{capability} argument {} must be int, got {}",
            idx + 1,
            other.type_name()
        )),
        None => Err(format!("{capability} missing argument {}", idx + 1)),
    }
}

fn bounded_usize(
    args: &[CapValue],
    idx: usize,
    capability: &str,
    max: usize,
) -> Result<usize, String> {
    let value = integer(args, idx, capability)?;
    if value < 1 || value as usize > max {
        return Err(format!(
            "{capability} argument {} must be in 1..={max}",
            idx + 1
        ));
    }
    Ok(value as usize)
}

pub(crate) fn number(args: &[CapValue], idx: usize, capability: &str) -> Result<f64, String> {
    let n = match args.get(idx) {
        Some(CapValue::Int(n)) => *n as f64,
        Some(CapValue::Float(n)) => *n,
        Some(other) => {
            return Err(format!(
                "{capability} argument {} must be number, got {}",
                idx + 1,
                other.type_name()
            ));
        }
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
        Some(other) => {
            return Err(format!(
                "{capability} argument {} must be array, got {}",
                idx + 1,
                other.type_name()
            ));
        }
        None => return Err(format!("{capability} missing argument {}", idx + 1)),
    };
    if values.is_empty() {
        return Err(format!("{capability} requires a non-empty numeric array"));
    }
    values
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let n = match value {
                CapValue::Int(n) => *n as f64,
                CapValue::Float(n) => *n,
                other => {
                    return Err(format!(
                        "{capability} array item {} must be number, got {}",
                        i + 1,
                        other.type_name()
                    ));
                }
            };
            if !n.is_finite() {
                return Err(format!("{capability} array item {} must be finite", i + 1));
            }
            Ok(n)
        })
        .collect()
}

fn integer_array(
    args: &[CapValue],
    idx: usize,
    capability: &str,
    max_len: usize,
) -> Result<Vec<usize>, String> {
    let values = match args.get(idx) {
        Some(CapValue::Array(items)) => items,
        Some(other) => {
            return Err(format!(
                "{capability} argument {} must be array, got {}",
                idx + 1,
                other.type_name()
            ));
        }
        None => return Err(format!("{capability} missing argument {}", idx + 1)),
    };
    if values.is_empty() {
        return Err(format!("{capability} requires a non-empty integer array"));
    }
    if values.len() > max_len {
        return Err(format!(
            "{capability} array length {} exceeds {max_len}",
            values.len()
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(i, value)| match value {
            CapValue::Int(n) if *n >= 0 => Ok(*n as usize),
            CapValue::Float(n) if n.fract() == 0.0 && *n >= 0.0 && n.is_finite() => Ok(*n as usize),
            other => Err(format!(
                "{capability} array item {} must be non-negative int, got {}",
                i + 1,
                other.type_name()
            )),
        })
        .collect()
}

fn expect_url<'a>(url: &'a str, capability: &str) -> Result<&'a str, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "{capability} only accepts http:// or https:// URLs"
        ));
    }
    Ok(url)
}

pub(crate) fn http_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, response) => {
            format!(
                "http request failed with status {} {}",
                code,
                response.status_text()
            )
        }
        ureq::Error::Transport(e) => format!("http transport error: {e}"),
    }
}

pub(crate) fn read_http_response(
    response: ureq::Response,
    capability: &str,
) -> Result<CapValue, String> {
    let mut reader = response.into_reader().take((MAX_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{capability} failed reading response: {e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "{capability} refuses responses larger than {MAX_BYTES} bytes"
        ));
    }
    let body = String::from_utf8(bytes)
        .map_err(|e| format!("{capability} response was not UTF-8: {e}"))?;
    Ok(CapValue::Str(body))
}

pub(crate) fn parse_json(text: &str, capability: &str) -> Result<serde_json::Value, String> {
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

    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(db_path, flags)
        .map_err(|e| format!("sql.sqliteQuery could not open '{db_path}' read-only: {e}"))?;
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("sql.sqliteQuery could not prepare query: {e}"))?;
    if !stmt.readonly() {
        return Err("sql.sqliteQuery rejected non-read-only statement".to_string());
    }
    let col_count = stmt.column_count();
    let mut query = stmt
        .query([])
        .map_err(|e| format!("sql.sqliteQuery failed: {e}"))?;
    let mut rows = Vec::new();
    while let Some(row) = query
        .next()
        .map_err(|e| format!("sql.sqliteQuery failed reading row: {e}"))?
    {
        if rows.len() >= MAX_SQL_ROWS {
            break;
        }
        let mut values = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let value = row
                .get_ref(i)
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

fn hex(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(LUT[(byte >> 4) as usize] as char);
        out.push(LUT[(byte & 0x0f) as usize] as char);
    }
    out
}
