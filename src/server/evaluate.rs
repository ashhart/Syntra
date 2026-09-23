//! `POST .../evaluate` and `POST .../promote`: off-policy evaluation on the
//! capsule's own decision log, and spec changes gated on it.
//!
//! `evaluate` estimates what a policy would have earned on the logged
//! decisions (DM, IPS, SNIPS, cross-fitted DR, paired lift over the logged
//! policy) and checks optional gates. `promote` evaluates a candidate spec
//! (a merge patch over the current one) and applies it only when every
//! gate passes; otherwise it answers 409 with the report. Both audit what
//! they decided.
//!
//! A candidate spec is evaluated as it would serve: on each logged row, the
//! probabilities its exploration (and floor) would put on each action,
//! given the predictions of its learner trained with cross-fitting on the
//! other rows, with its declared action features. Exploration settings
//! therefore count: a gate sees what exploring costs. A
//! `baselineExplore` candidate cannot be scored (the logs do not record
//! each request's baseline).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ope::estimators::EvalConfig;
use crate::ope::gates::Gate;
use crate::ope::policy::PolicyChoice;
use crate::ope::report::Report;
use crate::ope::row::{from_logged_rows, rewards_mode};

use super::http::{HandlerResult, Request, Response};
use super::state::State;

/// Most logged decisions one evaluation reads; narrow larger logs with
/// `since`/`until`.
pub const MAX_EVAL_ROWS: u64 = 2_000_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvalBody {
    /// `logged`, `greedy` or `constant:<id>` (for `evaluate`).
    #[serde(default)]
    policy: Option<String>,
    /// A candidate: a merge patch over the current spec.
    #[serde(default)]
    spec: Option<Value>,
    #[serde(default)]
    gates: Vec<String>,
    /// Decision timestamps (ms) to evaluate, inclusive.
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    until: Option<i64>,
    #[serde(default)]
    folds: Option<usize>,
    #[serde(default)]
    bootstrap: Option<usize>,
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default)]
    w_max: Option<f64>,
    #[serde(default)]
    reward_range: Option<[f64; 2]>,
}

struct Evaluated {
    report: Report,
    key: crate::eventstore::CapsuleKey,
    /// The spec the candidate was built on (for `promote`).
    base: crate::decision::DecisionSpec,
}

fn bad(msg: impl AsRef<str>) -> Response {
    Response::error(400, msg.as_ref())
}

fn run(state: &State, t: &str, j: &str, c: &str, body: &EvalBody) -> Result<Evaluated, Response> {
    let rt = state.runtime(t, j, c)?;
    // The stored spec: what `promote` compares against before applying.
    let base = state
        .store
        .load_spec(t, j, c)
        .map_err(|e| Response::error(500, &e.to_string()))?
        .ok_or_else(|| Response::error(404, &format!("capsule {t}/{j}/{c} not found")))?;
    let policy = match (&body.policy, &body.spec) {
        (Some(_), Some(_)) => return Err(bad("give either policy or spec, not both")),
        (None, Some(patch)) => {
            let candidate = base
                .merge_patch(patch)
                .map_err(|e| bad(format!("spec: {e}")))?;
            PolicyChoice::SpecServed {
                label: "candidate spec".into(),
                spec: candidate,
            }
        }
        (Some(p), None) => {
            // Only policies that read nothing from the server's disk.
            if !(p == "logged" || p == "greedy" || p.starts_with("constant:")) {
                return Err(bad(
                    "policy must be logged, greedy or constant:<id> (use spec for a candidate)",
                ));
            }
            PolicyChoice::parse(p).map_err(bad)?
        }
        (None, None) => return Err(bad("give policy or spec")),
    };
    let gates = body
        .gates
        .iter()
        .map(|g| Gate::parse(g))
        .collect::<Result<Vec<_>, _>>()
        .map_err(bad)?;
    let defaults = EvalConfig::default();
    let config = EvalConfig {
        folds: body.folds.unwrap_or(defaults.folds),
        bootstrap: body.bootstrap.unwrap_or(defaults.bootstrap),
        seed: body.seed.unwrap_or(defaults.seed),
        w_max: body.w_max.unwrap_or(defaults.w_max),
        reward_range: body.reward_range.or(defaults.reward_range),
        reward_model: defaults.reward_model,
    };
    config.validate().map_err(bad)?;

    // Include everything acknowledged before this request.
    state.writer.flush(std::time::Duration::from_secs(2));
    let key = rt.key.clone();
    if body.since.is_none() && body.until.is_none() {
        let stats = state
            .events
            .stats(&key)
            .map_err(|e| Response::error(500, &e.to_string()))?;
        if stats.decisions > MAX_EVAL_ROWS {
            return Err(Response::error(
                413,
                &format!(
                    "{} logged decisions; evaluate at most {MAX_EVAL_ROWS} at a time \
                     (narrow with since/until)",
                    stats.decisions
                ),
            ));
        }
    }
    let aggregation = base.rewards;
    let events = state.events.clone();
    let (since, until) = (body.since, body.until);
    let report = super::capsules::tokio_blocking(|| -> Result<Report, Response> {
        let rows = events
            .logged_rows(&key, since, until, rewards_mode(aggregation))
            .map_err(|e| Response::error(500, &e.to_string()))?;
        if rows.len() as u64 > MAX_EVAL_ROWS {
            return Err(Response::error(
                413,
                "too many logged decisions; narrow with since/until",
            ));
        }
        if rows.is_empty() {
            return Err(Response::error(409, "no logged decisions to evaluate"));
        }
        let loaded = from_logged_rows(rows, aggregation).map_err(|e| Response::error(500, &e))?;
        crate::ope::run(loaded, &policy, &config, &gates).map_err(|e| Response::error(409, &e))
    })?;
    Ok(Evaluated { report, key, base })
}

fn parse_body(req: &Request) -> Result<EvalBody, Response> {
    serde_json::from_value(req.json()?).map_err(|e| bad(format!("invalid request: {e}")))
}

fn report_value(report: &Report) -> Value {
    serde_json::from_str(&report.to_json()).unwrap_or(Value::Null)
}

/// `POST .../evaluate`.
pub fn evaluate(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let body = parse_body(req)?;
    let done = run(state, t, j, c, &body)?;
    Ok(Response::json(200, &report_value(&done.report)))
}

/// `POST .../promote`: `{"spec": patch, "gates": [...], ...}`.
pub fn promote(state: &State, t: &str, j: &str, c: &str, req: &Request) -> HandlerResult {
    let body = parse_body(req)?;
    let Some(patch) = body.spec.clone() else {
        return Err(bad(
            "promote needs spec (a merge patch over the current spec)",
        ));
    };
    if body.policy.is_some() {
        return Err(bad("promote evaluates spec; policy is not allowed"));
    }
    if body.gates.is_empty() {
        return Err(bad(
            "promote needs at least one gate, for example \"lift.dr.lower >= 0\"",
        ));
    }
    let done = run(state, t, j, c, &body)?;
    let report = report_value(&done.report);
    let summary = json!({
        "verdict": report["verdict"],
        "gates": report["gates"],
        "rows": report["data"]["rows"],
    });
    if !done.report.gates_passed {
        state.audit(
            &done.key,
            "promotion_refused",
            json!({ "patch": patch, "evaluation": summary }),
        );
        return Ok(Response::json(
            409,
            &json!({ "promoted": false, "error": "a gate failed", "report": report }),
        ));
    }
    let applied = super::capsules::change_spec(
        state,
        t,
        j,
        c,
        super::capsules::SpecChange {
            patch: &patch,
            event: "spec_promoted",
            expect_base: Some(&done.base),
            audit_extra: json!({ "evaluation": summary }),
            replace: false,
        },
    )?;
    let spec: Value = serde_json::from_slice(&applied.body).unwrap_or(Value::Null);
    Ok(Response::json(
        200,
        &json!({ "promoted": true, "spec": spec, "report": report }),
    ))
}
