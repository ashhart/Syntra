//! NASA/JPL Horizons and local ephemeris kernels.

use std::time::Duration;

use super::kernels::{
    expect_arity, expect_str, http_error, number, parse_json, read_http_response,
};
use super::registry::CapValue;
use super::sandbox::check_network_sandbox;

pub(crate) fn horizons_vectors(
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
    if !body
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{capability} body must be a Horizons command id/name"
        ));
    }

    let api_url = "https://ssd.jpl.nasa.gov/api/horizons.api";
    let agent = match check_network_sandbox(ctx, api_url, capability)? {
        Some(guard) => guard.agent(),
        None => ureq::AgentBuilder::new().build(),
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
    let result = root
        .get("result")
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

fn parse_horizons_vector_result(
    result: &str,
    capability: &str,
) -> Result<Vec<(f64, [f64; 6])>, String> {
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
        let jd = trimmed
            .split_whitespace()
            .next()
            .ok_or_else(|| format!("{capability} could not read Horizons JD"))?
            .parse::<f64>()
            .map_err(|e| format!("{capability} invalid Horizons JD: {e}"))?;
        let pos = lines
            .next()
            .ok_or_else(|| format!("{capability} Horizons vector missing position line"))?;
        let vel = lines
            .next()
            .ok_or_else(|| format!("{capability} Horizons vector missing velocity line"))?;
        rows.push((
            jd,
            [
                parse_horizons_component(pos, "X =", capability)?,
                parse_horizons_component(pos, "Y =", capability)?,
                parse_horizons_component(pos, "Z =", capability)?,
                parse_horizons_component(vel, "VX=", capability)?,
                parse_horizons_component(vel, "VY=", capability)?,
                parse_horizons_component(vel, "VZ=", capability)?,
            ],
        ));
    }
    if rows.is_empty() {
        return Err(format!(
            "{capability} Horizons result contained no vector rows"
        ));
    }
    Ok(rows)
}

fn parse_horizons_component(line: &str, label: &str, capability: &str) -> Result<f64, String> {
    let start = line
        .find(label)
        .ok_or_else(|| format!("{capability} Horizons row missing {label}"))?
        + label.len();
    let token = line[start..]
        .trim_start()
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("{capability} Horizons row missing value after {label}"))?;
    token
        .parse::<f64>()
        .map_err(|e| format!("{capability} invalid Horizons value after {label}: {e}"))
}

pub(crate) fn ephemeris_args(
    args: &[CapValue],
    capability: &str,
) -> Result<(String, String, f64), String> {
    expect_arity(args, 3, capability)?;
    Ok((
        expect_str(args, 0, capability)?.to_string(),
        expect_str(args, 1, capability)?.to_string(),
        number(args, 2, capability)?,
    ))
}

pub(crate) fn load_ephemeris_state(path: &str, body: &str, et: f64) -> Result<[f64; 6], String> {
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
                return Err(format!(
                    "invalid ephemeris BODY line {} in '{path}'",
                    line_no + 1
                ));
            }
            declared_body = Some(parts[1].to_string());
            continue;
        }
        if parts
            .first()
            .is_some_and(|p| p.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
        {
            continue;
        }
        if parts.len() != 7 {
            return Err(format!("invalid ephemeris row {} in '{path}'", line_no + 1));
        }
        let mut nums = [0.0; 7];
        for (i, part) in parts.iter().enumerate() {
            nums[i] = part.parse::<f64>().map_err(|_| {
                format!(
                    "invalid ephemeris number '{}' on line {}",
                    part,
                    line_no + 1
                )
            })?;
            if !nums[i].is_finite() {
                return Err(format!(
                    "non-finite ephemeris number on line {}",
                    line_no + 1
                ));
            }
        }
        rows.push((
            nums[0],
            [nums[1], nums[2], nums[3], nums[4], nums[5], nums[6]],
        ));
    }

    if let Some(declared) = declared_body {
        if declared != body {
            return Err(format!(
                "ephemeris body mismatch: file has {declared}, requested {body}"
            ));
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
