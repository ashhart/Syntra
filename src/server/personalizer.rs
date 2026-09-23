//! Azure AI Personalizer-compatible API (v1.0), for moving a Personalizer
//! loop onto Syntra by changing the endpoint and key.
//!
//! Routes, relative to `/personalizer/v1.0` (the capsule comes from a key
//! scoped to one capsule) or to
//! `/v1/tenants/{t}/jobs/{j}/capsules/{c}/personalizer/v1.0` (any key with
//! access):
//!
//! - `POST rank`: `contextFeatures`, `actions` (`id`, `features`),
//!   `excludedActions`, `eventId`, `deferActivation` →
//!   `{ranking: [{id, probability}], eventId, rewardActionId}`.
//! - `POST events/{eventId}/reward`: `{value}` → 204.
//! - `POST events/{eventId}/activate` → 204.
//! - `GET`/`PUT configurations/service`: reward wait, default reward,
//!   reward aggregation, exploration percentage and learning mode, mapped
//!   onto the capsule's spec.
//!
//! Semantics follow Personalizer: the `features` arrays of objects are
//! merged into one object (their keys act as namespaces); in Apprentice
//! mode (`baselineExplore`) the first action is the baseline; deferred
//! events are neither logged nor learned from until activated, and
//! rewards that arrive first are applied on activation (deferred events
//! are kept across a graceful restart, not a crash); `defaultReward`
//! applies after `rewardWaitTime`. Multi-slot ranking is not supported.
//! Errors use Personalizer's shape: `{"error": {"code", "message"}}`.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::decision::spec::RewardAggregation;
use crate::decision::{ActionSpec, Mode};
use crate::store::sha256_hex;

use super::decide::{DecideRequest, DecisionView, decide_core};
use super::http::{HandlerResult, Request, Response};
use super::state::State;

/// Personalizer's error shape for an error response.
pub fn personalizer_error(resp: Response) -> Response {
    if resp.status < 400 {
        return resp;
    }
    let message = serde_json::from_slice::<Value>(&resp.body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| String::from_utf8_lossy(&resp.body).into_owned());
    let code = match resp.status {
        400 | 413 => "BadArgument",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "ResourceNotFound",
        405 => "MethodNotAllowed",
        409 => "Conflict",
        429 => "TooManyRequests",
        503 => "ServiceUnavailable",
        _ => "InternalServerError",
    };
    let mut out = Response::json(
        resp.status,
        &json!({ "error": { "code": code, "message": message } }),
    );
    // Keep the original headers (Retry-After, Deprecation, Link, ...).
    for (k, v) in resp.headers {
        if !k.eq_ignore_ascii_case("content-type") && !k.eq_ignore_ascii_case("content-length") {
            out = out.with_header(&k, &v);
        }
    }
    out
}

/// Merge Personalizer feature objects (`[{"user": {...}}, {"env": {...}}]`)
/// into one object; objects under the same key merge recursively, other
/// values from later entries win.
fn merge_features(list: &[Value], what: &str) -> Result<Map<String, Value>, Response> {
    fn merge(into: &mut Map<String, Value>, from: &Map<String, Value>) {
        for (k, v) in from {
            match (into.get_mut(k), v) {
                (Some(Value::Object(a)), Value::Object(b)) => merge(a, b),
                _ => {
                    into.insert(k.clone(), v.clone());
                }
            }
        }
    }
    let mut out = Map::new();
    for (i, item) in list.iter().enumerate() {
        let Value::Object(obj) = item else {
            return Err(Response::error(
                400,
                &format!("{what}[{i}] must be a JSON object"),
            ));
        };
        merge(&mut out, obj);
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RankAction {
    id: String,
    #[serde(default)]
    features: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RankRequest {
    #[serde(default)]
    context_features: Vec<Value>,
    actions: Vec<RankAction>,
    #[serde(default)]
    excluded_actions: Vec<String>,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    defer_activation: bool,
}

/// `POST .../personalizer/v1.0/rank`.
pub fn rank(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let started = std::time::Instant::now();
    let body: RankRequest = serde_json::from_value(req.json()?)
        .map_err(|e| Response::error(400, &format!("invalid rank request: {e}")))?;
    if body.actions.is_empty() {
        return Err(Response::error(400, "actions must not be empty"));
    }
    let context = merge_features(&body.context_features, "contextFeatures")?;
    let mut actions = Vec::with_capacity(body.actions.len());
    for (i, a) in body.actions.iter().enumerate() {
        let features = merge_features(&a.features, &format!("actions[{i}].features"))?;
        actions.push(ActionSpec {
            id: a.id.clone(),
            features,
        });
    }
    let rt = state.runtime(t, j, c)?;
    // Apprentice mode: the first action is the baseline, as in Personalizer.
    let baseline = match rt.spec().mode {
        Mode::BaselineExplore => Some(actions[0].id.clone()),
        _ => None,
    };
    let all_ids: Vec<String> = actions.iter().map(|a| a.id.clone()).collect();
    let request = DecideRequest {
        context: Value::Object(context),
        actions: Some(actions),
        excluded: body.excluded_actions,
        baseline,
        event_id: body.event_id,
        request_sha256: sha256_hex(&req.body),
        defer: body.defer_activation,
        durable: false,
    };
    let response = decide_core(state, &rt, request, &req.request_id, |v| {
        rank_response(v, &all_ids)
    })?;
    state.metrics.observe_decide(started.elapsed());
    Ok(Response::json(201, &response))
}

/// Personalizer's rank answer: the chosen action first, then the other
/// eligible actions by probability, then excluded ones at 0.
fn rank_response(v: &DecisionView<'_>, all_ids: &[String]) -> Value {
    let chosen = v.record.chosen_index as usize;
    let mut eligible: Vec<(usize, f64)> = v
        .eligible
        .iter()
        .copied()
        .zip(v.pmf.iter().copied())
        .filter(|(i, _)| *i != chosen)
        .collect();
    eligible.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut ranking = vec![json!({
        "id": v.record.chosen_id,
        "probability": v.record.probability.unwrap_or(1.0),
    })];
    for (i, p) in eligible {
        if let Some(a) = v.actions.get(i) {
            ranking.push(json!({ "id": a.id, "probability": p }));
        }
    }
    let listed: std::collections::HashSet<&str> =
        ranking.iter().filter_map(|r| r["id"].as_str()).collect();
    let excluded: Vec<Value> = all_ids
        .iter()
        .filter(|id| !listed.contains(id.as_str()))
        .map(|id| json!({ "id": id, "probability": 0.0 }))
        .collect();
    ranking.extend(excluded);
    json!({
        "ranking": ranking,
        "eventId": v.record.id,
        "rewardActionId": v.record.chosen_id,
    })
}

/// `POST .../personalizer/v1.0/events/{eventId}/reward`: `{"value": x}`.
pub fn reward(
    state: &State,
    t: &str,
    j: &str,
    c: &str,
    event_id: &str,
    req: &Request,
) -> HandlerResult {
    #[derive(Deserialize)]
    struct Body {
        value: f64,
    }
    let body: Body = serde_json::from_value(req.json()?)
        .map_err(|e| Response::error(400, &format!("invalid reward request: {e}")))?;
    if !body.value.is_finite() {
        return Err(Response::error(400, "value must be a finite number"));
    }
    let rt = state.runtime(t, j, c)?;
    super::reward::apply_reward(state, &rt, event_id, body.value, None, None)?;
    Ok(Response::new(204, "application/json", ""))
}

/// `POST .../personalizer/v1.0/events/{eventId}/activate`.
pub fn activate(state: &State, t: &str, j: &str, c: &str, event_id: &str) -> HandlerResult {
    let rt = state.runtime(t, j, c)?;
    match rt
        .activate(&state.writer, event_id)
        .map_err(|e| Response::error(503, &e).with_header("retry-after", "1"))?
    {
        Some(held) => {
            for r in held {
                super::reward::apply_reward(
                    state,
                    &rt,
                    event_id,
                    r.value,
                    r.idempotency_key,
                    r.detail,
                )?;
            }
        }
        // Already active (logged), or unknown.
        None => {
            if state.find_decision(&rt.key, event_id)?.is_none() {
                return Err(Response::error(
                    404,
                    &format!("event {event_id:?} not found"),
                ));
            }
        }
    }
    Ok(Response::new(204, "application/json", ""))
}

/// ISO 8601 duration (`PT10M`, `PT1H30M`, `P1D`, `PT0.5S`) in seconds.
pub fn parse_duration(text: &str) -> Result<f64, String> {
    let bad = || format!("{text:?} is not an ISO 8601 duration such as PT10M");
    let rest = text.strip_prefix('P').ok_or_else(bad)?;
    let (date, time) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    let mut seconds = 0.0;
    let mut any = false;
    let mut take = |part: &str, units: &[(char, f64)]| -> Result<(), String> {
        let mut num = String::new();
        let mut i = 0;
        for ch in part.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                num.push(ch);
                continue;
            }
            // Units must appear in order, each at most once.
            let Some(pos) = units[i..].iter().position(|(u, _)| *u == ch) else {
                return Err(bad());
            };
            let n: f64 = num.parse().map_err(|_| bad())?;
            seconds += n * units[i + pos].1;
            any = true;
            i += pos + 1;
            num.clear();
        }
        if num.is_empty() { Ok(()) } else { Err(bad()) }
    };
    take(date, &[('D', 86400.0)])?;
    if let Some(t) = time {
        if t.is_empty() {
            return Err(bad());
        }
        take(t, &[('H', 3600.0), ('M', 60.0), ('S', 1.0)])?;
    }
    if !any {
        return Err(bad());
    }
    Ok(seconds)
}

/// Seconds as an ISO 8601 duration (`PT10M`, `PT1H0M30S`, ...).
pub fn format_duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    let mut out = String::from("PT");
    if h > 0 {
        out.push_str(&format!("{h}H"));
    }
    if m > 0 {
        out.push_str(&format!("{m}M"));
    }
    if s > 0 || (h == 0 && m == 0) {
        out.push_str(&format!("{s}S"));
    }
    out
}

/// `GET .../personalizer/v1.0/configurations/service`.
pub fn get_service_config(state: &State, t: &str, j: &str, c: &str) -> HandlerResult {
    let spec = state
        .store
        .load_spec(t, j, c)
        .map_err(|e| Response::error(500, &e.to_string()))?
        .ok_or_else(|| Response::error(404, &format!("capsule {t}/{j}/{c} not found")))?;
    let exploration = match spec.exploration.kind {
        crate::decision::ExplorationKind::EpsilonGreedy => spec.exploration.epsilon,
        // SquareCB has no single percentage; its floor is the least any
        // action gets, spread over the actions.
        crate::decision::ExplorationKind::SquareCb => spec.exploration.floor,
    };
    let learning_mode = match spec.mode {
        Mode::Learner => "Online",
        Mode::BaselineExplore => "Apprentice",
        Mode::Frozen => "Frozen",
    };
    Ok(Response::json(
        200,
        &json!({
            "rewardWaitTime": format_duration(spec.reward.wait_seconds),
            "defaultReward": spec.reward.default,
            "rewardAggregation": match spec.rewards {
                RewardAggregation::First => "earliest",
                RewardAggregation::Sum => "sum",
            },
            "explorationPercentage": exploration,
            "learningMode": learning_mode,
            // Models publish within a second of learning.
            "modelExportFrequency": "PT1S",
            "logRetentionDays": -1,
        }),
    ))
}

/// `PUT .../personalizer/v1.0/configurations/service`: applies the fields
/// that map onto the spec; the others Personalizer clients send
/// (`modelExportFrequency`, `logRetentionDays`, log mirroring, ...) are
/// accepted and ignored.
pub fn put_service_config(
    state: &State,
    t: &str,
    j: &str,
    c: &str,
    req: &Request,
) -> HandlerResult {
    let body = req.json()?;
    let obj = body
        .as_object()
        .ok_or_else(|| Response::error(400, "the configuration must be a JSON object"))?;
    let mut patch = json!({});
    if let Some(v) = obj.get("rewardWaitTime").filter(|v| !v.is_null()) {
        let text = v
            .as_str()
            .ok_or_else(|| Response::error(400, "rewardWaitTime must be a duration string"))?;
        let secs = parse_duration(text).map_err(|e| Response::error(400, &e))?;
        patch["reward"]["waitSeconds"] = json!(secs.round().max(1.0) as u64);
    }
    if let Some(v) = obj.get("defaultReward") {
        if !(v.is_null() || v.is_number()) {
            return Err(Response::error(
                400,
                "defaultReward must be a number or null",
            ));
        }
        patch["reward"]["default"] = v.clone();
    }
    if let Some(v) = obj.get("rewardAggregation").filter(|v| !v.is_null()) {
        let agg = match v.as_str().map(str::to_ascii_lowercase).as_deref() {
            Some("earliest") => "first",
            Some("sum") => "sum",
            _ => {
                return Err(Response::error(
                    400,
                    "rewardAggregation must be earliest or sum",
                ));
            }
        };
        patch["rewards"] = json!(agg);
    }
    if let Some(v) = obj.get("explorationPercentage").filter(|v| !v.is_null()) {
        let p = v
            .as_f64()
            .filter(|p| (0.0..=1.0).contains(p))
            .ok_or_else(|| Response::error(400, "explorationPercentage must be in [0, 1]"))?;
        patch["exploration"] = json!({ "kind": "epsilonGreedy", "epsilon": p });
    }
    if let Some(v) = obj.get("learningMode").filter(|v| !v.is_null()) {
        let mode = match v.as_str() {
            Some("Online") => "learner",
            Some("Apprentice") => "baselineExplore",
            Some("Frozen") => "frozen",
            _ => {
                return Err(Response::error(
                    400,
                    "learningMode must be Online or Apprentice (LoggingOnly is not supported)",
                ));
            }
        };
        patch["mode"] = json!(mode);
    }
    if !state.store.capsule_exists(t, j, c) {
        return Err(Response::error(
            404,
            &format!("capsule {t}/{j}/{c} not found"),
        ));
    }
    super::capsules::change_spec(
        state,
        t,
        j,
        c,
        super::capsules::SpecChange {
            patch: &patch,
            event: "spec_updated",
            expect_base: None,
            audit_extra: json!({ "via": "personalizer configurations/service" }),
            replace: false,
        },
    )?;
    get_service_config(state, t, j, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT10M").unwrap(), 600.0);
        assert_eq!(parse_duration("PT1H30M").unwrap(), 5400.0);
        assert_eq!(parse_duration("P1D").unwrap(), 86400.0);
        assert_eq!(parse_duration("P1DT2H").unwrap(), 93600.0);
        assert_eq!(parse_duration("PT0.5S").unwrap(), 0.5);
        for bad in ["", "P", "PT", "10M", "PT10", "PT10X", "PTM", "PT1M1H"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
        assert_eq!(format_duration(600), "PT10M");
        assert_eq!(format_duration(5430), "PT1H30M30S");
        assert_eq!(format_duration(0), "PT0S");
        for s in [1, 59, 60, 3599, 3600, 86399, 604800] {
            assert_eq!(parse_duration(&format_duration(s)).unwrap(), s as f64);
        }
    }

    #[test]
    fn features_merge_by_namespace() {
        let merged = merge_features(
            &[
                json!({"user": {"tier": "pro", "geo": {"country": "UK"}}}),
                json!({"user": {"geo": {"city": "London"}}, "env": {"hour": 9}}),
            ],
            "contextFeatures",
        )
        .unwrap();
        assert_eq!(
            Value::Object(merged),
            json!({"user": {"tier": "pro", "geo": {"country": "UK", "city": "London"}},
                   "env": {"hour": 9}})
        );
        assert!(merge_features(&[json!(1)], "contextFeatures").is_err());
    }
}
