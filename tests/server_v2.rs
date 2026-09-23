//! Syntra v2 HTTP API: specs, decisions, rewards, modes, feature programs,
//! logs, the model endpoint, metrics and the HTTP surface.
//!
//! Most tests drive the router in process (`build_state` + `handle`), which
//! is the whole API minus the socket. Tests about the hyper adapter itself
//! (body limit, request ids on the wire) boot a real server.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use syntra::decision::{DecideInput, DecisionSpec, Engine, SplitMix64};
use syntra::eventstore::CapsuleKey;
use syntra::server::http::Request;
use syntra::server::state::State;

const T: &str = "acme";
const J: &str = "prod";

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn ranking(d: &Value) -> BTreeMap<String, f64> {
    d["ranking"]
        .as_array()
        .expect("ranking")
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["probability"].as_f64().unwrap(),
            )
        })
        .collect()
}

fn error_of(v: &Value) -> String {
    v["error"].as_str().unwrap_or_default().to_string()
}

fn audit_events(app: &App, c: &str) -> Vec<String> {
    app.ok("GET", &cap(T, J, c, "/audits"), None, 200)["audits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap().to_string())
        .collect()
}

/// POST a JSON body straight through the router; usable from threads.
fn post(state: &State, target: &str, body: &Value) -> (u16, Value) {
    let mut req = Request::new("POST", target);
    req.body = body.to_string().into_bytes().into();
    let r = syntra::server::handle(state, &req);
    (r.status, parse_body(&r.body))
}

fn three_actions() -> Value {
    json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "learner": {"bits": 12}})
}

// ───────────────────────────── spec ──────────────────────────────────────

#[test]
fn spec_put_creates_then_merge_patches() {
    let app = App::dev("spec");
    let path = cap(T, J, "router", "/spec");
    let (st, created) = app.call(
        "PUT",
        &path,
        Some(json!({
            "actions": [{"id": "small", "features": {"cost": 0.2}}, {"id": "large"}],
            "exploration": {"gammaScale": 20},
            "seed": 7
        })),
    );
    assert_eq!(st, 201, "creating a capsule answers 201: {created}");
    // The response is the complete spec with every default filled in.
    assert_eq!(created["exploration"]["gammaScale"], json!(20.0));
    assert_eq!(created["exploration"]["floor"], json!(0.05));
    assert_eq!(created["exploration"]["kind"], json!("squarecb"));
    assert_eq!(created["learner"]["bits"], json!(18));
    assert_eq!(created["mode"], json!("learner"));
    assert_eq!(created["rewards"], json!("first"));
    assert_eq!(created["snapshotEvery"], json!(1000));
    assert_eq!(created["seed"], json!(7));
    assert_eq!(app.ok("GET", &path, None, 200), created);

    // Merge patch: nested objects merge, untouched fields keep their values.
    let (st, patched) = app.call("PUT", &path, Some(json!({"exploration": {"floor": 0.2}})));
    assert_eq!(
        st, 200,
        "patching an existing capsule answers 200: {patched}"
    );
    assert_eq!(patched["exploration"]["floor"], json!(0.2));
    assert_eq!(patched["exploration"]["gammaScale"], json!(20.0));
    assert_eq!(patched["actions"], created["actions"]);
    assert_eq!(patched["seed"], json!(7));

    // Arrays are replaced wholesale; null removes a field, restoring its
    // default.
    let patched = app.ok(
        "PUT",
        &path,
        Some(
            json!({"actions": [{"id": "only"}], "seed": null, "exploration": {"gammaScale": null}}),
        ),
        200,
    );
    assert_eq!(patched["actions"], json!([{"id": "only"}]));
    assert!(patched.get("seed").is_none(), "{patched}");
    assert_eq!(patched["exploration"]["gammaScale"], json!(10.0));
    assert_eq!(patched["exploration"]["floor"], json!(0.2));

    // An empty patch is the identity.
    assert_eq!(app.ok("PUT", &path, Some(json!({})), 200), patched);

    // What GET returns is what is on disk.
    let file = app
        .store()
        .join("tenants/acme/jobs/prod/capsules/router/spec.json");
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(on_disk, app.ok("GET", &path, None, 200));

    // Creating the capsule created its job and a deny-all policy.
    let job = app.ok("GET", &format!("/v1/tenants/{T}/jobs/{J}"), None, 200);
    assert_eq!(job["capsules"], json!(["router"]));
    let policy = app.ok("GET", &cap(T, J, "router", "/policy"), None, 200);
    for flag in [
        "allow_stdout",
        "allow_stdin",
        "allow_file_read",
        "allow_file_write",
        "allow_network",
    ] {
        assert_eq!(policy[flag], json!(false), "{flag}: {policy}");
    }

    // A patch must be a JSON object.
    for bad in [json!([1]), json!("mode"), json!(3)] {
        let (st, v) = app.call("PUT", &path, Some(bad.clone()));
        assert_eq!(st, 400, "{bad}: {v}");
        assert!(error_of(&v).contains("JSON object"), "{v}");
    }
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    let app = App::dev("unknown");
    let path = cap(T, J, "c", "/spec");
    let base = app.put_spec(T, J, "c", three_actions());
    for (patch, field) in [
        (json!({"explore": {}}), "explore"),
        (json!({"exploration": {"gama": 5}}), "gama"),
        (json!({"learner": {"bitz": 18}}), "bitz"),
        (json!({"reward": {"range": [0, 1], "scale": 2}}), "scale"),
        (json!({"actions": [{"id": "a", "weight": 1}]}), "weight"),
    ] {
        let (st, v) = app.call("PUT", &path, Some(patch.clone()));
        assert_eq!(st, 400, "{patch}: {v}");
        let msg = error_of(&v);
        assert!(
            msg.contains("unknown field") && msg.contains(field),
            "{patch}: {msg}"
        );
    }
    // Rejected patches change nothing and are not audited.
    assert_eq!(app.ok("GET", &path, None, 200), base);
    assert_eq!(audit_events(&app, "c"), vec!["capsule_created"]);

    // A create with an unknown field creates nothing.
    let (st, _) = app.call(
        "PUT",
        &cap(T, J, "fresh", "/spec"),
        Some(json!({"acitons": []})),
    );
    assert_eq!(st, 400);
    assert_eq!(app.call("GET", &cap(T, J, "fresh", "/spec"), None).0, 404);
    assert!(
        !app.store()
            .join("tenants/acme/jobs/prod/capsules/fresh")
            .exists()
    );

    // Request bodies are strict as well.
    for (route, body, field) in [
        ("/decide", json!({"context": {}, "contxt": {}}), "contxt"),
        (
            "/decide",
            json!({"actions": [{"id": "x", "weight": 2}]}),
            "weight",
        ),
        (
            "/reward",
            json!({"decisionId": "d", "reward": 1, "extra": true}),
            "extra",
        ),
        (
            "/mode",
            json!({"mode": "frozen", "epsilon": 0.2}),
            "epsilon",
        ),
    ] {
        let (st, v) = app.call("POST", &cap(T, J, "c", route), Some(body.clone()));
        assert_eq!(st, 400, "{route} {body}: {v}");
        let msg = error_of(&v);
        assert!(
            msg.contains("unknown field") && msg.contains(field),
            "{route} {body}: {msg}"
        );
    }
}

#[test]
fn spec_values_are_range_checked() {
    let app = App::dev("ranges");
    let path = cap(T, J, "c", "/spec");
    let base = app.put_spec(T, J, "c", three_actions());
    for (patch, want) in [
        (json!({"reward": {"range": [1, 0]}}), "reward.range"),
        (json!({"reward": {"range": [2, 2]}}), "reward.range"),
        (
            json!({"exploration": {"floor": 0}}),
            "exploration.floor must be in (0, 1] (got 0)",
        ),
        (json!({"exploration": {"floor": 1.5}}), "exploration.floor"),
        (
            json!({"exploration": {"epsilon": 1.5}}),
            "exploration.epsilon",
        ),
        (
            json!({"exploration": {"gammaScale": -1}}),
            "exploration.gammaScale",
        ),
        (
            json!({"exploration": {"gammaExponent": 2}}),
            "exploration.gammaExponent",
        ),
        (json!({"learner": {"bits": 9}}), "learner.bits"),
        (json!({"learner": {"bits": 25}}), "learner.bits"),
        (
            json!({"learner": {"learningRate": 0}}),
            "learner.learningRate",
        ),
        (
            json!({"learner": {"maxImportanceWeight": 0.5}}),
            "learner.maxImportanceWeight",
        ),
        (json!({"baselineEpsilon": 0}), "baselineEpsilon"),
        (json!({"baselineEpsilon": 1.1}), "baselineEpsilon"),
        (json!({"snapshotEvery": 0}), "snapshotEvery"),
        (json!({"mode": "greedy"}), "unknown variant"),
        (
            json!({"exploration": {"kind": "SquareCB"}}),
            "unknown variant",
        ),
        (json!({"rewards": "max"}), "unknown variant"),
        (json!({"seed": -1}), "invalid value"),
        (json!({"learner": {"bits": 18.5}}), "invalid type"),
        (json!({"actions": [{"id": "a"}, {"id": "a"}]}), "duplicates"),
        (json!({"actions": [{"id": ""}]}), "must not be empty"),
    ] {
        let (st, v) = app.call("PUT", &path, Some(patch.clone()));
        assert_eq!(st, 400, "{patch}: {v}");
        assert!(error_of(&v).contains(want), "{patch}: {v}");
    }
    assert_eq!(app.ok("GET", &path, None, 200), base, "nothing was applied");

    // Boundary values are accepted.
    for patch in [
        json!({"exploration": {"floor": 1, "epsilon": 0, "gammaScale": 0, "gammaExponent": 0}}),
        json!({"exploration": {"floor": 0.001, "epsilon": 1, "gammaScale": 1e6, "gammaExponent": 1}}),
        json!({"learner": {"bits": 10, "learningRate": 10, "maxImportanceWeight": 1}}),
        json!({"baselineEpsilon": 1, "snapshotEvery": 1_000_000_000}),
        json!({"reward": {"range": [-5, 5]}, "rewards": "sum", "seed": 18446744073709551615u64}),
    ] {
        let (st, v) = app.call("PUT", &path, Some(patch.clone()));
        assert_eq!(st, 200, "{patch}: {v}");
    }
}

// ───────────────────────────── decide ────────────────────────────────────

#[test]
fn decide_with_spec_actions_logs_the_full_record() {
    let app = App::dev("decide");
    app.put_spec(T, J, "c", three_actions());
    let body = json!({"context": {"user": {"tier": "pro"}, "tokens": 812}}).to_string();
    let (st, d) = app.call_bytes("POST", &cap(T, J, "c", "/decide"), body.as_bytes());
    assert_eq!(st, 200, "{d}");
    let id = d["decisionId"].as_str().unwrap().to_string();
    assert!(id.starts_with("dec_"), "{id}");
    let r = ranking(&d);
    assert_eq!(
        r.keys().cloned().collect::<Vec<_>>(),
        vec!["a", "b", "c"],
        "{d}"
    );
    assert!(close(r.values().sum::<f64>(), 1.0), "{d}");
    // Untrained: uniform.
    for p in r.values() {
        assert!(close(*p, 1.0 / 3.0), "{d}");
    }
    let action = d["action"].as_str().unwrap();
    let index = ["a", "b", "c"].iter().position(|a| *a == action).unwrap();
    assert_eq!(d["actionIndex"], json!(index));
    assert!(close(d["probability"].as_f64().unwrap(), r[action]));
    assert_eq!(d["mode"], json!("learner"));
    assert_eq!(d["modelVersion"], json!(0));
    assert!(d.get("replayed").is_none() && d.get("reason").is_none());

    // The stored record carries everything needed to replay and evaluate.
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(T, J, "c", &format!("/decisions/{id}")),
        None,
        200,
    );
    assert_eq!(rec["decisionId"], json!(id));
    assert_eq!(
        rec["context"],
        json!({"user": {"tier": "pro"}, "tokens": 812})
    );
    assert_eq!(
        rec["actions"],
        json!([{"id": "a"}, {"id": "b"}, {"id": "c"}])
    );
    assert_eq!(rec["eligible"], json!([0, 1, 2]));
    assert_eq!(rec["pmf"].as_array().unwrap().len(), 3);
    assert_eq!(rec["chosenIndex"], json!(index));
    assert_eq!(rec["action"], json!(action));
    assert_eq!(rec["probability"], d["probability"]);
    assert_eq!(rec["modelVersion"], json!(0));
    assert_eq!(rec["mode"], json!("learner"));
    assert!(
        rec["seed"].as_str().unwrap().parse::<u64>().is_ok(),
        "{rec}"
    );
    assert_eq!(
        rec["requestSha256"],
        json!(syntra::store::sha256_hex(body.as_bytes()))
    );
    assert!(
        rec["derived"].is_null(),
        "no feature program, no derived features: {rec}"
    );
    assert_eq!(rec["rewards"], json!([]));

    // v1 compatibility: contextKey and features merge into the context.
    let d = app.decide(
        T,
        J,
        "c",
        json!({"contextKey": "rush_hour", "features": {"load": 0.9}, "context": {"x": 1}}),
    );
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(
            T,
            J,
            "c",
            &format!("/decisions/{}", d["decisionId"].as_str().unwrap()),
        ),
        None,
        200,
    );
    assert_eq!(
        rec["context"],
        json!({"x": 1, "contextKey": "rush_hour", "load": 0.9})
    );

    // Context and features must be objects.
    for body in [
        json!({"context": [1, 2]}),
        json!({"context": "x"}),
        json!({"features": 3}),
    ] {
        let (st, v) = app.call("POST", &cap(T, J, "c", "/decide"), Some(body.clone()));
        assert_eq!(st, 400, "{body}: {v}");
    }
    // A missing or null context is an empty one.
    app.decide(T, J, "c", json!({}));
    app.decide(T, J, "c", json!({"context": null}));
}

#[test]
fn decide_with_request_actions_features_and_exclusions() {
    let app = App::dev("adf");
    app.put_spec(T, J, "adf", json!({"learner": {"bits": 12}}));
    let route = cap(T, J, "adf", "/decide");

    // Neither the spec nor the request has actions.
    let (st, v) = app.call("POST", &route, Some(json!({"context": {}})));
    assert_eq!(st, 400);
    assert!(error_of(&v).contains("no actions"), "{v}");

    let actions = json!([
        {"id": "small", "features": {"cost": 0.2, "family": "s"}},
        {"id": "large", "features": {"cost": 1.0, "tags": ["slow", "smart"]}}
    ]);
    let d = app.decide(
        T,
        J,
        "adf",
        json!({"context": {"task": "code"}, "actions": actions}),
    );
    assert_eq!(ranking(&d).len(), 2, "{d}");
    let id = d["decisionId"].as_str().unwrap().to_string();

    // Exclusions: only the other action remains, with probability 1.
    let d = app.decide(
        T,
        J,
        "adf",
        json!({"context": {}, "actions": actions, "excludedActions": ["large"]}),
    );
    assert_eq!(d["action"], json!("small"));
    assert_eq!(d["probability"], json!(1.0));
    assert_eq!(ranking(&d).len(), 1);

    for (body, want) in [
        (
            json!({"actions": actions, "excludedActions": ["huge"]}),
            "excludedActions names unknown action \"huge\"",
        ),
        (
            json!({"actions": actions, "excludedActions": ["small", "large"]}),
            "no eligible actions",
        ),
        (json!({"actions": [{"id": "x"}, {"id": "x"}]}), "duplicates"),
        (json!({"actions": [{"id": ""}]}), "must not be empty"),
        (json!({"actions": []}), "no actions"),
    ] {
        let (st, v) = app.call("POST", &route, Some(body.clone()));
        assert_eq!(st, 400, "{body}: {v}");
        assert!(error_of(&v).contains(want), "{body}: {v}");
    }

    // The per-request actions, features included, are what the log holds,
    // and a reward trains on them.
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(T, J, "adf", &format!("/decisions/{id}")),
        None,
        200,
    );
    assert_eq!(rec["actions"], actions);
    let r = app.reward(T, J, "adf", json!({"decisionId": id, "reward": 1}));
    assert_eq!(r["modelVersion"], json!(1), "{r}");
}

const FEATURE_PROGRAM: &str = r#"
($ seg (!cap "runtime.inputGet" "segment"))
($ score (!cap "runtime.inputGet" "score"))
(!cap "runtime.publish" "features.risk" (* score 2.0))
(? (== seg "vip") (!cap "runtime.publish" "only.premium" true))
(? (== seg "blocked") (!cap "runtime.publish" "exclude.cheap" true))
(!cap "runtime.publish" "reason" (? (== seg "vip") "vip gets premium" "default rules"))
"#;

#[test]
fn feature_program_restricts_actions_and_publishes_features_and_reason() {
    let app = App::dev("program");
    app.put_spec(
        T,
        J,
        "p",
        json!({"actions": [{"id": "cheap"}, {"id": "standard"}, {"id": "premium"}], "learner": {"bits": 12}}),
    );
    let program = compile_lycan(FEATURE_PROGRAM);
    let (st, v) = app.call_bytes("POST", &cap(T, J, "p", "/install"), &program);
    assert_eq!(st, 200, "{v}");
    let sha = v["programSha256"].as_str().unwrap().to_string();
    assert_eq!(sha, syntra::store::sha256_hex(&program));

    // `only.premium`: the single eligible action gets probability 1.
    let d = app.decide(
        T,
        J,
        "p",
        json!({"context": {"segment": "vip", "score": 0.25}}),
    );
    assert_eq!(d["action"], json!("premium"));
    assert_eq!(d["probability"], json!(1.0));
    assert_eq!(ranking(&d).len(), 1);
    assert_eq!(d["reason"], json!("vip gets premium"), "reason is echoed");
    let vip_id = d["decisionId"].as_str().unwrap().to_string();

    // `exclude.cheap`: the other two remain.
    let d = app.decide(
        T,
        J,
        "p",
        json!({"context": {"segment": "blocked", "score": 1}}),
    );
    assert_eq!(
        ranking(&d).keys().cloned().collect::<Vec<_>>(),
        vec!["premium", "standard"]
    );
    assert_eq!(d["reason"], json!("default rules"));

    // Program exclusions combine with the request's.
    let d = app.decide(
        T,
        J,
        "p",
        json!({"context": {"segment": "blocked", "score": 1}, "excludedActions": ["standard"]}),
    );
    assert_eq!(d["action"], json!("premium"));
    let (st, v) = app.call(
        "POST",
        &cap(T, J, "p", "/decide"),
        Some(json!({"context": {"segment": "vip", "score": 1}, "excludedActions": ["premium"]})),
    );
    assert_eq!(st, 400, "{v}");
    assert!(error_of(&v).contains("no eligible actions"), "{v}");

    // No restriction for other segments.
    let d = app.decide(
        T,
        J,
        "p",
        json!({"context": {"segment": "normal", "score": 0.5}}),
    );
    assert_eq!(ranking(&d).len(), 3);

    // The derived features, eligible set, reason and program hash are
    // logged with the decision.
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(T, J, "p", &format!("/decisions/{vip_id}")),
        None,
        200,
    );
    assert_eq!(rec["derived"], json!({"risk": 0.5}), "{rec}");
    assert_eq!(rec["eligible"], json!([2]));
    assert_eq!(rec["pmf"], json!([1.0]));
    assert_eq!(rec["reason"], json!("vip gets premium"));
    assert_eq!(rec["programSha256"], json!(sha));

    // A reward learns with the derived features, and the capsule reports
    // its program.
    let r = app.reward(T, J, "p", json!({"decisionId": vip_id, "reward": 1}));
    assert_eq!(r["learned"], json!(true));
    let capsule = app.ok("GET", &cap(T, J, "p", ""), None, 200);
    assert_eq!(capsule["program"]["programSha256"], json!(sha), "{capsule}");

    // Removing the program lifts the restriction.
    let v = app.ok("DELETE", &cap(T, J, "p", "/program"), None, 200);
    assert_eq!(v["removed"], json!(true));
    let d = app.decide(
        T,
        J,
        "p",
        json!({"context": {"segment": "vip", "score": 0.25}}),
    );
    assert_eq!(ranking(&d).len(), 3);
    assert!(d.get("reason").is_none(), "{d}");
    let v = app.ok("DELETE", &cap(T, J, "p", "/program"), None, 200);
    assert_eq!(v["removed"], json!(false));
}

#[test]
fn programs_with_learning_nodes_are_refused_at_install() {
    let app = App::dev("choice");
    app.put_spec(T, J, "c", three_actions());
    let good = compile_lycan(FEATURE_PROGRAM);
    app.ok_bytes_install(T, J, "c", &good);
    let before = audit_events(&app, "c");

    for src in [
        "(!p (choice 10 20))",
        "($ x (choice \"a\" \"b\" \"c\"))\n(!p x)",
        "(!p (strategy 1 2))",
        "($ c (choice 0 1 2))\n(feedback c 1.0)\nc",
    ] {
        let (st, v) = app.call_bytes("POST", &cap(T, J, "c", "/install"), &compile_lycan(src));
        assert_eq!(st, 400, "{src}: {v}");
        let msg = error_of(&v);
        assert!(
            msg.contains("docs/design/v2-decision-core.md"),
            "the refusal must point at the design doc: {msg}"
        );
        assert!(
            msg.contains("feature programs compute features"),
            "{src}: {msg}"
        );
    }
    // Garbage is refused as well.
    let (st, v) = app.call_bytes("POST", &cap(T, J, "c", "/install"), b"not a graph");
    assert_eq!(st, 400);
    assert!(error_of(&v).contains("invalid program"), "{v}");

    // The installed program is untouched and nothing was audited.
    let capsule = app.ok("GET", &cap(T, J, "c", ""), None, 200);
    assert_eq!(
        capsule["program"]["programSha256"],
        json!(syntra::store::sha256_hex(&good))
    );
    assert_eq!(audit_events(&app, "c"), before);

    // A refused install does not create a capsule.
    let (st, _) = app.call_bytes(
        "POST",
        &cap(T, J, "ghost", "/install"),
        &compile_lycan("(!p (choice 1 2))"),
    );
    assert_eq!(st, 400);
    assert_eq!(app.call("GET", &cap(T, J, "ghost", ""), None).0, 404);
}

/// Install helper kept next to the test that needs it.
trait InstallExt {
    fn ok_bytes_install(&self, t: &str, j: &str, c: &str, program: &[u8]);
}

impl InstallExt for App {
    fn ok_bytes_install(&self, t: &str, j: &str, c: &str, program: &[u8]) {
        let (st, v) = self.call_bytes("POST", &cap(t, j, c, "/install"), program);
        assert_eq!(st, 200, "install: {v}");
    }
}

// ───────────────────────────── modes ─────────────────────────────────────

#[test]
fn baseline_explore_serves_the_baseline_with_one_minus_epsilon_plus_share() {
    let app = App::dev("baseline");
    app.put_spec(T, J, "c", three_actions());
    let spec = app.ok(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "baselineExplore", "baselineEpsilon": 0.3})),
        200,
    );
    assert_eq!(spec["mode"], json!("baselineExplore"));
    assert_eq!(spec["baselineEpsilon"], json!(0.3));

    let (st, v) = app.call(
        "POST",
        &cap(T, J, "c", "/decide"),
        Some(json!({"context": {}})),
    );
    assert_eq!(st, 400);
    assert!(
        error_of(&v).contains("baselineAction is required in baselineExplore mode"),
        "{v}"
    );

    let eps = 0.3;
    let k = 3.0;
    let mut chose_baseline = 0;
    let mut ids = Vec::new();
    for _ in 0..200 {
        let d = app.decide(
            T,
            J,
            "c",
            json!({"context": {"x": 1}, "baselineAction": "b"}),
        );
        assert_eq!(d["mode"], json!("baselineExplore"));
        let r = ranking(&d);
        assert!(close(r["b"], (1.0 - eps) + eps / k), "{d}");
        assert!(close(r["a"], eps / k) && close(r["c"], eps / k), "{d}");
        assert_eq!(d["ranking"][0]["id"], json!("b"), "baseline ranks first");
        if d["action"] == json!("b") {
            chose_baseline += 1;
        }
        ids.push(d["decisionId"].as_str().unwrap().to_string());
    }
    // 0.8 expected; a loose bound that a fair sampler never misses.
    assert!(
        (120..=190).contains(&chose_baseline),
        "baseline chosen {chose_baseline}/200"
    );

    for (body, want) in [
        (json!({"baselineAction": "zzz"}), "unknown action"),
        (
            json!({"baselineAction": "b", "excludedActions": ["b"]}),
            "not eligible",
        ),
    ] {
        let (st, v) = app.call("POST", &cap(T, J, "c", "/decide"), Some(body.clone()));
        assert_eq!(st, 400, "{body}: {v}");
        assert!(error_of(&v).contains(want), "{body}: {v}");
    }

    // The learner keeps training in baselineExplore mode.
    let r = app.reward(T, J, "c", json!({"decisionId": ids[0], "reward": 1}));
    assert_eq!(r["learned"], json!(true));
    assert_eq!(r["modelVersion"], json!(1));

    // `mode` requires a mode and allows only its two fields.
    for body in [json!({}), json!({"baselineEpsilon": 0.2}), json!([])] {
        assert_eq!(
            app.call("POST", &cap(T, J, "c", "/mode"), Some(body)).0,
            400
        );
    }
    let (st, v) = app.call(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "learner", "baselineEpsilon": 0})),
    );
    assert_eq!(st, 400, "baselineEpsilon is range-checked: {v}");
}

#[test]
fn frozen_mode_records_rewards_without_learning() {
    let mut app = App::dev("frozen");
    app.put_spec(T, J, "c", three_actions());
    for i in 0..3 {
        let d = app.decide(T, J, "c", json!({"context": {"i": i}}));
        app.reward(
            T,
            J,
            "c",
            json!({"decisionId": d["decisionId"], "reward": 1}),
        );
    }
    assert_eq!(app.model_version(T, J, "c"), 3);

    let spec = app.ok(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "frozen"})),
        200,
    );
    assert_eq!(spec["mode"], json!("frozen"));
    let d = app.decide(T, J, "c", json!({"context": {"i": 9}}));
    assert_eq!(d["mode"], json!("frozen"));
    assert_eq!(d["modelVersion"], json!(3));
    let id = d["decisionId"].as_str().unwrap().to_string();
    let r = app.reward(
        T,
        J,
        "c",
        json!({"decisionId": id, "reward": 1, "durable": true}),
    );
    assert_eq!(r["applied"], json!(true), "{r}");
    assert_eq!(r["learned"], json!(false), "{r}");
    assert_eq!(r["modelVersion"], json!(3), "{r}");
    assert_eq!(app.model_version(T, J, "c"), 3);

    // The reward is recorded, marked as not learned.
    let rec = app.ok(
        "GET",
        &cap(T, J, "c", &format!("/decisions/{id}")),
        None,
        200,
    );
    assert_eq!(rec["rewards"].as_array().unwrap().len(), 1, "{rec}");
    assert_eq!(
        rec["rewards"][0]["detail"]["learned"],
        json!(false),
        "{rec}"
    );

    // Rebuilding the model from the log (a reload, then a restart) skips it.
    app.ok(
        "PUT",
        &cap(T, J, "c", "/policy"),
        Some(json!({"allow_stdout": false})),
        200,
    );
    assert_eq!(app.model_version(T, J, "c"), 3, "reload replays the log");
    app.restart(true);
    assert_eq!(app.model_version(T, J, "c"), 3, "restart replays the log");
    let stats = app.ok("GET", &cap(T, J, "c", ""), None, 200)["stats"].clone();
    assert_eq!(stats["rewards"], json!(4), "{stats}");

    // Unfreezing resumes learning.
    app.ok(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "learner"})),
        200,
    );
    let d = app.decide(T, J, "c", json!({"context": {}}));
    let r = app.reward(
        T,
        J,
        "c",
        json!({"decisionId": d["decisionId"], "reward": 0}),
    );
    assert_eq!(r["learned"], json!(true));
    assert_eq!(r["modelVersion"], json!(4));
}

// ───────────────────────────── idempotency ───────────────────────────────

#[test]
fn event_id_retries_return_the_original_decision() {
    let app = App::dev("eventid");
    app.put_spec(T, J, "c", three_actions());
    app.put_spec(T, J, "other", three_actions());
    let route = cap(T, J, "c", "/decide");
    let body = json!({"context": {"user": 1}, "eventId": "evt-1"}).to_string();

    let (st, first) = app.call_bytes("POST", &route, body.as_bytes());
    assert_eq!(st, 200, "{first}");
    assert_eq!(first["decisionId"], json!("evt-1"));
    assert!(first.get("replayed").is_none());

    // Same body while still queued, then after commit: the original answer.
    for flush in [false, true] {
        if flush {
            app.flush();
        }
        let (st, again) = app.call_bytes("POST", &route, body.as_bytes());
        assert_eq!(st, 200, "{again}");
        assert_eq!(again["replayed"], json!(true), "{again}");
        for field in [
            "decisionId",
            "action",
            "actionIndex",
            "probability",
            "ranking",
            "mode",
            "modelVersion",
        ] {
            assert_eq!(again[field], first[field], "{field}");
        }
    }

    // Same id, different request: conflict, and the original stands.
    let (st, v) = app.call(
        "POST",
        &route,
        Some(json!({"context": {"user": 2}, "eventId": "evt-1"})),
    );
    assert_eq!(st, 409, "{v}");
    assert!(error_of(&v).contains("evt-1"), "{v}");
    let rec = app.ok("GET", &cap(T, J, "c", "/decisions/evt-1"), None, 200);
    assert_eq!(rec["context"], json!({"user": 1}));

    // Ids are scoped to the capsule.
    let d = app.decide(
        T,
        J,
        "other",
        json!({"context": {"user": 2}, "eventId": "evt-1"}),
    );
    assert!(d.get("replayed").is_none());

    // A reward joins the client-chosen id.
    let r = app.reward(T, J, "c", json!({"decisionId": "evt-1", "reward": 1}));
    assert_eq!(r["applied"], json!(true));

    // Id syntax: 1-128 characters from [A-Za-z0-9_.:-].
    let long_ok = "x".repeat(128);
    app.decide(T, J, "c", json!({"eventId": long_ok}));
    for bad in ["", "has space", "slash/id", "ünïcode", &"x".repeat(129)] {
        let (st, v) = app.call("POST", &route, Some(json!({"eventId": bad})));
        assert_eq!(st, 400, "{bad:?}: {v}");
    }
}

#[test]
fn rewards_count_once_per_decision_by_default() {
    let app = App::dev("rewards-first");
    app.put_spec(T, J, "c", three_actions());
    let d = app.decide(T, J, "c", json!({"context": {}}));
    let id = d["decisionId"].as_str().unwrap().to_string();

    let r = app.reward(
        T,
        J,
        "c",
        json!({"decisionId": id, "reward": 1, "detail": {"latencyMs": 12}}),
    );
    assert_eq!(
        r,
        json!({"ok": true, "applied": true, "learned": true, "modelVersion": 1})
    );
    // A retry (queued, then committed) is a duplicate and changes nothing,
    // even with another value.
    for (value, flush) in [(1.0, false), (0.0, true)] {
        if flush {
            app.flush();
        }
        let r = app.reward(T, J, "c", json!({"decisionId": id, "reward": value}));
        assert_eq!(r["duplicate"], json!(true), "{r}");
        assert_eq!(r["applied"], json!(false), "{r}");
        assert_eq!(r["modelVersion"], json!(1), "{r}");
    }
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(T, J, "c", &format!("/decisions/{id}")),
        None,
        200,
    );
    let rewards = rec["rewards"].as_array().unwrap();
    assert_eq!(rewards.len(), 1, "{rec}");
    assert_eq!(
        rewards[0]["idempotencyKey"],
        json!(id),
        "default key is the decision id"
    );
    assert_eq!(rewards[0]["reward"], json!(1.0));
    assert_eq!(rewards[0]["rewardNormalized"], json!(1.0));
    assert_eq!(rewards[0]["detail"], json!({"latencyMs": 12}));
    assert!(rewards[0]["seq"].as_i64().unwrap() > 0);

    // `value` is an alias of `reward`, and `/feedback` of `/reward`.
    let d2 = app.decide(T, J, "c", json!({}));
    let r = app.ok(
        "POST",
        &cap(T, J, "c", "/feedback"),
        Some(json!({"decisionId": d2["decisionId"], "value": 0.5})),
        200,
    );
    assert_eq!(r["modelVersion"], json!(2), "{r}");

    // Explicit keys make retries safe too.
    let d3 = app.decide(T, J, "c", json!({}));
    let body = json!({"decisionId": d3["decisionId"], "reward": 1, "idempotencyKey": "order-17"});
    assert_eq!(app.reward(T, J, "c", body.clone())["applied"], json!(true));
    assert_eq!(app.reward(T, J, "c", body)["duplicate"], json!(true));
    assert_eq!(app.model_version(T, J, "c"), 3);

    // Errors.
    let route = cap(T, J, "c", "/reward");
    for (body, status) in [
        (json!({"decisionId": "dec_missing", "reward": 1}), 404),
        (json!({"reward": 1}), 400),
        (json!({"decisionId": id}), 400),
        (json!({"decisionId": id, "reward": "high"}), 400),
    ] {
        let (st, v) = app.call("POST", &route, Some(body.clone()));
        assert_eq!(st, status, "{body}: {v}");
    }
    // Non-finite numbers are not JSON; an overflowing literal is refused.
    let (st, _) = app.call_bytes(
        "POST",
        &route,
        format!("{{\"decisionId\":\"{id}\",\"reward\":1e999}}").as_bytes(),
    );
    assert_eq!(st, 400);
    assert_eq!(app.model_version(T, J, "c"), 3);
}

#[test]
fn first_mode_counts_only_the_first_reward_even_with_explicit_keys() {
    // `rewards: "first"`: "Only the first reward counts; later ones are
    // rejected." A second reward for the same decision under a different
    // idempotency key must not train the model a second time.
    let app = App::dev("rewards-first-keys");
    app.put_spec(T, J, "c", three_actions());
    let d = app.decide(T, J, "c", json!({}));
    let first = app.reward(
        T,
        J,
        "c",
        json!({"decisionId": d["decisionId"], "reward": 1, "idempotencyKey": "k1"}),
    );
    assert_eq!(first["applied"], json!(true));
    let second = app.reward(
        T,
        J,
        "c",
        json!({"decisionId": d["decisionId"], "reward": 0, "idempotencyKey": "k2"}),
    );
    assert_eq!(second["applied"], json!(false), "{second}");
    assert_eq!(app.model_version(T, J, "c"), 1);
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(
            T,
            J,
            "c",
            &format!("/decisions/{}", d["decisionId"].as_str().unwrap()),
        ),
        None,
        200,
    );
    assert_eq!(rec["rewards"].as_array().unwrap().len(), 1, "{rec}");
}

#[test]
fn sum_mode_counts_every_reward() {
    let mut app = App::dev("rewards-sum");
    app.put_spec(
        T,
        J,
        "c",
        json!({"actions": [{"id": "a"}, {"id": "b"}], "rewards": "sum", "reward": {"range": [-1, 1]}, "learner": {"bits": 12}}),
    );
    let d = app.decide(T, J, "c", json!({}));
    let id = d["decisionId"].as_str().unwrap().to_string();
    for (i, value) in [0.0, 1.0, -1.0].into_iter().enumerate() {
        let r = app.reward(T, J, "c", json!({"decisionId": id, "reward": value}));
        assert_eq!(r["applied"], json!(true), "{r}");
        assert_eq!(r["modelVersion"], json!(i + 1));
    }
    // Explicit keys still deduplicate retries in sum mode.
    let body = json!({"decisionId": id, "reward": 1, "idempotencyKey": "click-1"});
    assert_eq!(app.reward(T, J, "c", body.clone())["applied"], json!(true));
    assert_eq!(app.reward(T, J, "c", body)["duplicate"], json!(true));
    // Keys are 1-256 characters.
    for bad in [String::new(), "k".repeat(257)] {
        let (st, v) = app.call(
            "POST",
            &cap(T, J, "c", "/reward"),
            Some(json!({"decisionId": id, "reward": 1, "idempotencyKey": bad})),
        );
        assert_eq!(st, 400, "{v}");
    }
    let long_ok = json!({"decisionId": id, "reward": 1, "idempotencyKey": "k".repeat(256)});
    assert_eq!(app.reward(T, J, "c", long_ok)["applied"], json!(true));
    app.flush();
    let rec = app.ok(
        "GET",
        &cap(T, J, "c", &format!("/decisions/{id}")),
        None,
        200,
    );
    let rewards = rec["rewards"].as_array().unwrap();
    assert_eq!(rewards.len(), 5, "{rec}");
    let keys: BTreeSet<&str> = rewards
        .iter()
        .map(|r| r["idempotencyKey"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 5, "keys are unique: {keys:?}");
    // Normalized by reward.range [-1, 1].
    let norms: Vec<f64> = rewards
        .iter()
        .map(|r| r["rewardNormalized"].as_f64().unwrap())
        .collect();
    assert_eq!(norms, vec![0.5, 1.0, 0.0, 1.0, 1.0]);
    assert_eq!(app.model_version(T, J, "c"), 5);
    app.restart(false);
    assert_eq!(
        app.model_version(T, J, "c"),
        5,
        "replay counts every reward"
    );
}

#[test]
fn durable_requests_are_committed_before_the_answer() {
    let app = App::dev("durable");
    app.put_spec(T, J, "c", three_actions());
    for i in 0..20 {
        let d = app.decide(T, J, "c", json!({"context": {"i": i}, "durable": true}));
        let id = d["decisionId"].as_str().unwrap();
        // `decisions` lists committed rows only.
        let listed = app.ok("GET", &cap(T, J, "c", "/decisions?limit=1000"), None, 200);
        assert!(
            listed["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["decisionId"] == json!(id)),
            "durable decision {id} not committed"
        );
        let r = app.reward(
            T,
            J,
            "c",
            json!({"decisionId": id, "reward": 1, "durable": true}),
        );
        assert_eq!(r["applied"], json!(true));
        // Rewards on a decision are read from the event store.
        let rec = app.ok(
            "GET",
            &cap(T, J, "c", &format!("/decisions/{id}")),
            None,
            200,
        );
        assert_eq!(rec["rewards"].as_array().unwrap().len(), 1, "{rec}");
    }
}

// ───────────────────────────── reads ─────────────────────────────────────

#[test]
fn decisions_page_with_limit_after_and_next() {
    let app = App::dev("paging");
    app.put_spec(T, J, "c", three_actions());
    let mut made = BTreeSet::new();
    for i in 0..7 {
        let d = app.decide(
            T,
            J,
            "c",
            json!({"context": {"i": i}, "eventId": format!("p{i}")}),
        );
        made.insert(d["decisionId"].as_str().unwrap().to_string());
    }
    let d = app.decide(T, J, "c", json!({"context": {}}));
    app.reward(
        T,
        J,
        "c",
        json!({"decisionId": d["decisionId"], "reward": 1, "durable": true}),
    );
    made.insert(d["decisionId"].as_str().unwrap().to_string());

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = Vec::new();
    loop {
        let target = match &after {
            Some(a) => cap(T, J, "c", &format!("/decisions?limit=3&after={a}")),
            None => cap(T, J, "c", "/decisions?limit=3"),
        };
        let page = app.ok("GET", &target, None, 200);
        let rows = page["decisions"].as_array().unwrap().clone();
        pages.push(rows.len());
        for r in &rows {
            seen.push((
                r["tsMs"].as_i64().unwrap(),
                r["decisionId"].as_str().unwrap().to_string(),
            ));
        }
        match page["next"].as_str() {
            Some(n) => {
                assert_eq!(rows.len(), 3, "a full page carries the cursor");
                assert_eq!(n, rows.last().unwrap()["decisionId"].as_str().unwrap());
                after = Some(n.to_string());
            }
            None => break,
        }
    }
    assert_eq!(pages, vec![3, 3, 2]);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "oldest first, by (tsMs, id)");
    let ids: BTreeSet<String> = seen.into_iter().map(|(_, id)| id).collect();
    assert_eq!(ids, made, "every decision exactly once");

    // One decision with its rewards.
    let one = app.ok(
        "GET",
        &cap(
            T,
            J,
            "c",
            &format!("/decisions/{}", d["decisionId"].as_str().unwrap()),
        ),
        None,
        200,
    );
    assert_eq!(one["rewards"][0]["reward"], json!(1.0));
    assert_eq!(
        app.call("GET", &cap(T, J, "c", "/decisions/nope"), None).0,
        404
    );

    // Limits clamp to [1, 1000]; malformed numbers are 400.
    let n = |q: &str| {
        app.ok(
            "GET",
            &cap(T, J, "c", &format!("/decisions?{q}")),
            None,
            200,
        )["decisions"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(n("limit=0"), 1);
    assert_eq!(n("limit=-4"), 1);
    assert_eq!(n("limit=99999"), 8);
    assert_eq!(n("since=99999999999999"), 0);
    assert_eq!(n("until=1"), 0);
    for q in ["limit=ten", "since=yesterday", "until=1.5"] {
        let (st, v) = app.call("GET", &cap(T, J, "c", &format!("/decisions?{q}")), None);
        assert_eq!(st, 400, "{q}: {v}");
    }
    // A cursor that names no decision is the caller's mistake, not a
    // server fault.
    let (st, v) = app.call("GET", &cap(T, J, "c", "/decisions?after=dec_gone"), None);
    assert_eq!(st, 400, "unknown cursor: {v}");
}

#[test]
fn model_snapshot_restores_to_an_identical_engine() {
    let app = App::dev("model");
    app.put_spec(
        T,
        J,
        "m",
        json!({"actions": [{"id": "a", "features": {"size": 1}}, {"id": "b", "features": {"size": 3}}, {"id": "c"}],
               "learner": {"bits": 12}, "seed": 11}),
    );
    let mut rng = SplitMix64::new(5);
    for i in 0..60 {
        let ctx =
            json!({"segment": if i % 2 == 0 { "x" } else { "y" }, "load": (i % 7) as f64 / 7.0});
        let d = app.decide(T, J, "m", json!({"context": ctx}));
        let reward = if rng.next_f64() < 0.5 { 1.0 } else { 0.0 };
        app.reward(
            T,
            J,
            "m",
            json!({"decisionId": d["decisionId"], "reward": reward}),
        );
    }

    let plain = app.raw("GET", &cap(T, J, "m", "/model"), &Cred::None, None);
    let plain_json = parse_body(&plain.body);
    assert!(plain_json.get("snapshot").is_none(), "{plain_json}");
    assert_eq!(plain_json["modelVersion"], json!(60));

    let published = app.raw(
        "GET",
        &cap(T, J, "m", "/model?snapshot=true"),
        &Cred::None,
        None,
    );
    assert_eq!(published.status, 200);
    let v = parse_body(&published.body);
    let bytes = base64_decode(v["snapshot"].as_str().expect("snapshot"));
    assert_eq!(v["snapshotBytes"], json!(bytes.len()));
    assert_eq!(v["modelVersion"], json!(60));
    // SDKs rebuild the spec from the versioned `decide` section.
    let spec = DecisionSpec::from_decide_json(&v["decide"]).expect("the decide section parses");
    assert_eq!(
        DecisionSpec::from_json(&v["spec"]).unwrap().decide_json(),
        v["decide"],
        "the decide section is the live spec's"
    );
    // The tag names this (spec, snapshot) pair and is the ETag; polling
    // with it answers 304.
    let tag = v["modelTag"].as_str().expect("modelTag").to_string();
    assert_eq!(
        tag,
        syntra::server::runtime::model_tag(&v["decide"], &bytes)
    );
    let etag = format!("\"{tag}\"");
    assert_eq!(header(&published, "etag"), Some(etag.as_str()));
    let mut poll = Request::new("GET", &cap(T, J, "m", "/model?snapshot=true"));
    poll.headers.push(("if-none-match".into(), etag.clone()));
    let not_modified = syntra::server::handle(&app.state, &poll);
    assert_eq!(not_modified.status, 304);
    assert!(not_modified.body.is_empty());
    assert_eq!(header(&not_modified, "etag"), Some(etag.as_str()));
    let restored = Engine::restore(spec, &bytes).expect("Engine::restore accepts the snapshot");
    assert_eq!(restored.model_version(), 60);

    let rt = app.state.runtime(T, J, "m").unwrap();
    let live = rt.engine.read().unwrap();
    assert_eq!(restored.snapshot(), live.snapshot(), "byte-identical model");
    for (i, ctx) in [
        json!({"segment": "x", "load": 0.1}),
        json!({"segment": "y", "load": 0.9}),
        json!({"segment": "z"}),
        json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        let input = DecideInput {
            context: ctx.clone(),
            ..DecideInput::default()
        };
        let a = restored.decide(&input, i as u64).unwrap();
        let b = live.decide(&input, i as u64).unwrap();
        assert_eq!(a, b, "restored and live engines disagree on {ctx}");
        assert!(a.predictions.iter().any(|p| *p > 0.0), "the model learned");
    }
    drop(live);

    // An SDK evaluating locally computes the same PMF the server serves.
    let d = app.decide(T, J, "m", json!({"context": {"segment": "x", "load": 0.1}}));
    let local = restored
        .decide(
            &DecideInput {
                context: json!({"segment": "x", "load": 0.1}),
                ..DecideInput::default()
            },
            0,
        )
        .unwrap();
    let served = ranking(&d);
    for (k, &i) in local.eligible.iter().enumerate() {
        let id = &local.actions[i].id;
        assert_eq!(served[id], local.pmf[k], "{id}");
    }
}

#[test]
fn audits_record_every_administrative_change() {
    let app = App::dev("audit");
    app.put_spec(T, J, "c", three_actions());
    app.put_spec(T, J, "c", json!({"exploration": {"floor": 0.1}}));
    app.ok(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "frozen"})),
        200,
    );
    app.ok_bytes_install(T, J, "c", &compile_lycan(FEATURE_PROGRAM));
    app.ok(
        "PUT",
        &cap(T, J, "c", "/policy"),
        Some(json!({"allow_stdout": true})),
        200,
    );
    app.ok("DELETE", &cap(T, J, "c", "/program"), None, 200);
    app.ok("DELETE", &cap(T, J, "c", "/logs"), None, 200);

    let audits = app.ok("GET", &cap(T, J, "c", "/audits"), None, 200)["audits"]
        .as_array()
        .unwrap()
        .clone();
    let events: Vec<&str> = audits
        .iter()
        .map(|a| a["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        vec![
            "capsule_created",
            "spec_updated",
            "mode_changed",
            "program_installed",
            "policy_updated",
            "program_removed",
            "logs_purged"
        ]
    );
    // Audit rows carry their detail as JSON, with a millisecond timestamp.
    assert!(
        audits
            .iter()
            .all(|a| a["tsMs"].as_i64().unwrap() > 0 && a["detail"].is_object())
    );
    let detail = |i: usize| audits[i]["detail"].clone();
    assert!(detail(0)["specSha256"].as_str().unwrap().len() == 64);
    assert_eq!(detail(1)["patch"], json!({"exploration": {"floor": 0.1}}));
    assert_eq!(detail(2)["patch"], json!({"mode": "frozen"}));
    assert_eq!(
        detail(3)["programSha256"],
        json!(syntra::store::sha256_hex(&compile_lycan(FEATURE_PROGRAM)))
    );
    assert!(detail(4)["policySha256"].as_str().unwrap().len() == 64);
    // Newest last, and `limit` keeps the most recent.
    let last2 = app.ok("GET", &cap(T, J, "c", "/audits?limit=2"), None, 200)["audits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(last2, vec!["program_removed", "logs_purged"]);
}

#[test]
fn deleting_logs_keeps_the_learned_model() {
    let mut app = App::dev("purge");
    app.put_spec(T, J, "c", three_actions());
    for i in 0..5 {
        let d = app.decide(T, J, "c", json!({"context": {"i": i}}));
        app.reward(
            T,
            J,
            "c",
            json!({"decisionId": d["decisionId"], "reward": 1}),
        );
    }
    let before = app.ok("GET", &cap(T, J, "c", "/model?snapshot=true"), None, 200);
    assert_eq!(before["modelVersion"], json!(5));

    let v = app.ok("DELETE", &cap(T, J, "c", "/logs"), None, 200);
    assert_eq!(
        v["removed"],
        json!({"removedDecisions": 5, "removedRewards": 5})
    );
    let capsule = app.ok("GET", &cap(T, J, "c", ""), None, 200);
    assert_eq!(capsule["stats"]["decisions"], json!(0));
    assert_eq!(capsule["stats"]["rewards"], json!(0));
    assert_eq!(
        app.ok("GET", &cap(T, J, "c", "/decisions"), None, 200)["decisions"],
        json!([])
    );
    assert_eq!(capsule["modelVersion"], json!(5), "the live model is kept");
    assert_eq!(
        capsule["spec"],
        app.ok("GET", &cap(T, J, "c", "/spec"), None, 200)
    );

    // The model must also survive a reload from the store (any spec,
    // policy or program change) and a restart without a shutdown snapshot
    // (a crash): the erased rewards can no longer be replayed.
    app.ok(
        "PUT",
        &cap(T, J, "c", "/policy"),
        Some(json!({"allow_stdout": false})),
        200,
    );
    let after_reload = app.ok("GET", &cap(T, J, "c", "/model?snapshot=true"), None, 200);
    assert_eq!(
        after_reload["modelVersion"],
        json!(5),
        "reload lost the model"
    );
    assert_eq!(after_reload["snapshot"], before["snapshot"]);
    app.restart(false);
    let after_crash = app.ok("GET", &cap(T, J, "c", "/model?snapshot=true"), None, 200);
    assert_eq!(
        after_crash["modelVersion"],
        json!(5),
        "restart lost the model"
    );
    assert_eq!(after_crash["snapshot"], before["snapshot"]);

    // Learning continues on top of the kept model.
    let d = app.decide(T, J, "c", json!({}));
    let r = app.reward(
        T,
        J,
        "c",
        json!({"decisionId": d["decisionId"], "reward": 1}),
    );
    assert_eq!(r["modelVersion"], json!(6));
    app.restart(false);
    assert_eq!(app.model_version(T, J, "c"), 6);

    assert_eq!(app.call("DELETE", &cap(T, J, "nope", "/logs"), None).0, 404);
}

#[test]
fn deleting_a_capsule_erases_its_files_and_events() {
    let app = App::dev("delete");
    app.put_spec(T, J, "gone", three_actions());
    app.put_spec(T, J, "kept", three_actions());
    app.ok_bytes_install(T, J, "gone", &compile_lycan(FEATURE_PROGRAM));
    for c in ["gone", "kept"] {
        for i in 0..4 {
            let d = app.decide(T, J, c, json!({"context": {"segment": "x", "score": i}}));
            app.reward(T, J, c, json!({"decisionId": d["decisionId"], "reward": 1}));
        }
    }
    app.flush();
    let dir = app.store().join("tenants/acme/jobs/prod/capsules/gone");
    assert!(dir.join("current.lyc").exists());

    let v = app.ok("DELETE", &cap(T, J, "gone", ""), None, 200);
    // 4 decisions + 4 rewards (audit rows are kept).
    assert!(v["removedRows"].as_u64().unwrap() >= 8, "{v}");
    assert!(!dir.exists(), "capsule directory removed");
    for tail in ["", "/spec", "/decisions", "/model", "/policy"] {
        assert_eq!(
            app.call("GET", &cap(T, J, "gone", tail), None).0,
            404,
            "GET {tail}"
        );
    }
    let key = CapsuleKey::new(T, J, "gone").unwrap();
    let stats = app.state.events.stats(&key).unwrap();
    assert_eq!((stats.decisions, stats.rewards), (0, 0));
    // The audit trail outlives the capsule and records its deletion.
    let audit = app.state.events.list_audit(&key, 100).unwrap();
    assert_eq!(
        audit.last().map(|a| a.event.as_str()),
        Some("capsule_deleted")
    );
    assert!(app.state.events.load_latest_model(&key).unwrap().is_none());

    // The neighbour is untouched.
    let kept = app.ok("GET", &cap(T, J, "kept", ""), None, 200);
    assert_eq!(kept["stats"]["decisions"], json!(4));
    assert_eq!(kept["modelVersion"], json!(4));

    // Deleting again is 404; recreating starts from nothing.
    assert_eq!(app.call("DELETE", &cap(T, J, "gone", ""), None).0, 404);
    let (st, _) = app.call("PUT", &cap(T, J, "gone", "/spec"), Some(three_actions()));
    assert_eq!(st, 201);
    let fresh = app.ok("GET", &cap(T, J, "gone", ""), None, 200);
    assert_eq!(fresh["modelVersion"], json!(0));
    assert_eq!(fresh["stats"]["decisions"], json!(0));
    assert!(fresh["program"].is_null(), "{fresh}");
    // The name's audit trail spans both lives.
    assert_eq!(
        audit_events(&app, "gone"),
        vec![
            "capsule_created",
            "program_installed",
            "capsule_deleted",
            "capsule_created"
        ]
    );
}

#[test]
fn deleting_a_job_or_tenant_audits_each_capsule() {
    let app = App::dev("delete-parents");
    for (j, c) in [("prod", "a"), ("prod", "b"), ("staging", "c")] {
        app.put_spec(T, j, c, three_actions());
        let d = app.decide(T, j, c, json!({}));
        app.reward(T, j, c, json!({"decisionId": d["decisionId"], "reward": 1}));
    }
    app.flush();
    let v = app.ok("DELETE", &format!("/v1/tenants/{T}/jobs/prod"), None, 200);
    assert!(v["removedRows"].as_u64().unwrap() >= 4, "{v}");
    let v = app.ok("DELETE", &format!("/v1/tenants/{T}"), None, 200);
    assert!(v["removedRows"].as_u64().unwrap() >= 2, "{v}");
    for (j, c, with) in [
        ("prod", "a", "job"),
        ("prod", "b", "job"),
        ("staging", "c", "tenant"),
    ] {
        let key = CapsuleKey::new(T, j, c).unwrap();
        let stats = app.state.events.stats(&key).unwrap();
        assert_eq!((stats.decisions, stats.rewards), (0, 0), "{j}/{c}");
        let audit = app.state.events.list_audit(&key, 100).unwrap();
        let last = audit.last().expect("audit kept");
        assert_eq!(last.event, "capsule_deleted", "{j}/{c}");
        let detail: Value = serde_json::from_str(&last.detail).unwrap_or_else(|_| json!(null));
        assert_eq!(detail["with"], with, "{j}/{c}: {detail}");
    }
}

#[test]
fn tenants_and_jobs_lifecycle() {
    let app = App::dev("jobs");
    let (st, v) = app.call(
        "POST",
        &format!("/v1/tenants/{T}/jobs"),
        Some(json!({"id": "routing", "name": "Request routing"})),
    );
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["created"], json!(true));
    assert_eq!(v["job"]["name"], json!("Request routing"));
    let (st, v) = app.call(
        "POST",
        &format!("/v1/tenants/{T}/jobs"),
        Some(json!({"id": "routing"})),
    );
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["created"], json!(false));
    for bad in [
        json!({}),
        json!({"id": 5}),
        json!({"id": "../x"}),
        json!({"id": ".hidden"}),
    ] {
        let (st, v) = app.call("POST", &format!("/v1/tenants/{T}/jobs"), Some(bad.clone()));
        assert_eq!(st, 400, "{bad}: {v}");
    }

    app.put_spec(T, "routing", "router", three_actions());
    app.put_spec("globex", "j", "c", three_actions());
    assert_eq!(
        app.ok("GET", "/v1/tenants", None, 200)["tenants"],
        json!(["acme", "globex"])
    );
    let jobs = app.ok("GET", &format!("/v1/tenants/{T}/jobs"), None, 200);
    assert_eq!(jobs["jobs"][0]["id"], json!("routing"));
    assert_eq!(jobs["jobs"][0]["capsules"], json!(["router"]));
    assert_eq!(
        app.ok(
            "GET",
            &format!("/v1/tenants/{T}/jobs/routing/capsules"),
            None,
            200
        )["capsules"],
        json!(["router"])
    );
    assert_eq!(
        app.call("GET", &format!("/v1/tenants/{T}/jobs/none"), None)
            .0,
        404
    );
    let all = app.ok("GET", "/v1/admin/capsules", None, 200)["capsules"].clone();
    assert_eq!(all.as_array().unwrap().len(), 2, "{all}");

    let d = app.decide(T, "routing", "router", json!({}));
    app.reward(
        T,
        "routing",
        "router",
        json!({"decisionId": d["decisionId"], "reward": 1}),
    );
    let v = app.ok(
        "DELETE",
        &format!("/v1/tenants/{T}/jobs/routing"),
        None,
        200,
    );
    assert!(v["removedRows"].as_u64().unwrap() >= 2, "{v}");
    assert_eq!(
        app.call("GET", &format!("/v1/tenants/{T}/jobs/routing"), None)
            .0,
        404
    );
    let key = CapsuleKey::new(T, "routing", "router").unwrap();
    assert_eq!(app.state.events.stats(&key).unwrap().decisions, 0);
    assert_eq!(
        app.call("DELETE", &format!("/v1/tenants/{T}/jobs/routing"), None)
            .0,
        404
    );

    app.ok("DELETE", "/v1/tenants/globex", None, 200);
    assert_eq!(
        app.ok("GET", "/v1/tenants", None, 200)["tenants"],
        json!(["acme"])
    );
    assert_eq!(app.call("DELETE", "/v1/tenants/globex", None).0, 404);
    assert!(!app.store().join("tenants/globex").exists());
}

// ───────────────────────────── HTTP surface ──────────────────────────────

#[test]
fn unknown_capsules_are_404_and_create_nothing() {
    let app = App::dev("404");
    app.put_spec(T, J, "real", three_actions());
    let policy = json!({"allow_stdout": false}).to_string();
    for (method, tail, body) in [
        ("POST", "/decide", Some(json!({"context": {}}).to_string())),
        (
            "POST",
            "/reward",
            Some(json!({"decisionId": "d", "reward": 1}).to_string()),
        ),
        (
            "POST",
            "/feedback",
            Some(json!({"decisionId": "d", "reward": 1}).to_string()),
        ),
        ("GET", "", None),
        ("DELETE", "", None),
        ("GET", "/spec", None),
        ("POST", "/mode", Some(json!({"mode": "frozen"}).to_string())),
        ("DELETE", "/program", None),
        ("GET", "/policy", None),
        ("PUT", "/policy", Some(policy.clone())),
        ("DELETE", "/logs", None),
        ("GET", "/decisions", None),
        ("GET", "/decisions/d", None),
        ("GET", "/model", None),
        ("GET", "/audits", None),
        ("GET", "/nonsense", None),
        ("POST", "/spec", Some("{}".to_string())),
    ] {
        let r = app.raw(
            method,
            &cap(T, J, "missing", tail),
            &Cred::None,
            body.as_deref().map(str::as_bytes),
        );
        assert_eq!(
            r.status,
            404,
            "{method} {tail}: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert!(parse_body(&r.body)["error"].is_string());
    }
    assert!(
        !app.store()
            .join("tenants/acme/jobs/prod/capsules/missing")
            .exists()
    );
    // Unknown routes outside capsules.
    for (method, target) in [
        ("GET", "/v1/nothing"),
        ("POST", "/v1/tenants"),
        ("GET", "/v1"),
    ] {
        assert_eq!(
            app.raw(method, target, &Cred::None, None).status,
            404,
            "{target}"
        );
    }
}

#[test]
fn names_that_cannot_be_stored_are_client_errors() {
    // Tenant, job and capsule names become directory names; one that fails
    // validation is the caller's mistake (400), never a server fault.
    let app = App::dev("names");
    let long = "x".repeat(129);
    for (t, j, c) in [
        ("acme", "prod", "has%20space"),
        ("acme", "prod", ".hidden"),
        ("acme", "pr%2Fod", "c"),
        ("a*b", "prod", "c"),
        ("acme", "prod", long.as_str()),
    ] {
        for (method, tail, body) in [
            ("PUT", "/spec", Some(three_actions().to_string())),
            ("POST", "/decide", Some("{}".to_string())),
            ("GET", "", None),
            ("GET", "/spec", None),
            ("GET", "/model", None),
            ("DELETE", "", None),
            ("POST", "/install", Some("x".to_string())),
        ] {
            let r = app.raw(
                method,
                &cap(t, j, c, tail),
                &Cred::None,
                body.as_deref().map(str::as_bytes),
            );
            assert!(
                r.status == 400 || r.status == 404,
                "{method} {t}/{j}/{c}{tail}: {} {}",
                r.status,
                String::from_utf8_lossy(&r.body)
            );
        }
        for (method, target) in [
            ("DELETE", format!("/v1/tenants/{t}")),
            ("DELETE", format!("/v1/tenants/{t}/jobs/{j}")),
            ("GET", format!("/v1/tenants/{t}/jobs/{j}")),
        ] {
            let r = app.raw(method, &target, &Cred::None, None);
            assert!(
                r.status == 400 || r.status == 404,
                "{method} {target}: {} {}",
                r.status,
                String::from_utf8_lossy(&r.body)
            );
        }
    }
    assert_eq!(
        app.ok("GET", "/v1/tenants", None, 200)["tenants"],
        json!([])
    );
}

#[test]
fn malformed_bodies_are_400() {
    let app = App::dev("json");
    app.put_spec(T, J, "c", three_actions());
    let routes = [
        ("POST", cap(T, J, "c", "/decide")),
        ("POST", cap(T, J, "c", "/reward")),
        ("POST", cap(T, J, "c", "/feedback")),
        ("PUT", cap(T, J, "c", "/spec")),
        ("POST", cap(T, J, "c", "/mode")),
        ("PUT", cap(T, J, "c", "/policy")),
        ("POST", format!("/v1/tenants/{T}/jobs")),
        ("POST", "/v1/admin/tokens".to_string()),
    ];
    for (method, target) in &routes {
        for body in [
            &b""[..],
            b"   \n",
            b"{",
            b"{\"context\": }",
            b"not json",
            b"\xff\xfe",
            b"{} {}",
        ] {
            let (st, v) = app.call_bytes(method, target, body);
            assert_eq!(
                st,
                400,
                "{method} {target} {:?}: {v}",
                String::from_utf8_lossy(body)
            );
            assert!(error_of(&v).len() > 5, "{method} {target}: {v}");
        }
    }
    // The spec is untouched by all of that.
    assert_eq!(
        app.ok("GET", &cap(T, J, "c", "/spec"), None, 200)["actions"],
        three_actions()["actions"]
    );
}

/// Raw HTTP/1.1 exchange, so the request can declare any length.
fn raw_http(addr: &str, head: &str, body_chunks: Vec<Vec<u8>>) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let head = head.to_string();
    // Write from another thread: the server may answer (and close) before
    // the body is complete, which is the point of the test.
    let w = std::thread::spawn(move || {
        let _ = writer.write_all(head.as_bytes());
        for chunk in body_chunks {
            if writer.write_all(&chunk).is_err() {
                break;
            }
        }
        let _ = writer.flush();
    });
    let mut response = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&response);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if response.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = w.join();
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text)
}

#[test]
fn bodies_over_four_mib_are_413_on_the_wire() {
    let dir = TempDir::new("413");
    let srv = Server::start(&dir.join("store"), Some("k-413"));
    srv.ok("PUT", &cap(T, J, "c", "/spec"), Some(three_actions()), 201);
    const MAX: usize = 4 * 1024 * 1024;
    let target = cap(T, J, "c", "/decide");

    // Declared too large: refused before the body is read.
    let head = format!(
        "POST {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-413\r\nContent-Length: {}\r\n\r\n",
        MAX + 1
    );
    let (st, text) = raw_http(&srv.addr, &head, vec![vec![b' '; 1024]]);
    assert_eq!(st, 413, "{text}");
    assert!(text.contains("payload too large"), "{text}");

    // Chunked with no declared length: cut off at the limit.
    let head = format!(
        "POST {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer k-413\r\nTransfer-Encoding: chunked\r\n\r\n"
    );
    let chunk = {
        let data = vec![b' '; 64 * 1024];
        let mut c = format!("{:x}\r\n", data.len()).into_bytes();
        c.extend_from_slice(&data);
        c.extend_from_slice(b"\r\n");
        c
    };
    let chunks = vec![chunk; MAX / (64 * 1024) + 2];
    let (st, text) = raw_http(&srv.addr, &head, chunks);
    assert_eq!(st, 413, "{text}");

    // Exactly at the limit is accepted: a valid decide padded with spaces.
    let json = br#"{"context":{"x":1}}"#;
    let mut body = json.to_vec();
    body.resize(MAX, b' ');
    let r = srv.http("POST", &target, Some(&body));
    assert_eq!(r.status, 200, "{}", r.body);
    // And the server is still healthy.
    assert_eq!(srv.http("GET", "/health", None).status, 200);
}

#[test]
fn request_ids_are_echoed() {
    // In process: the request's id comes back as `x-request-id`.
    let app = App::dev("rid");
    let mut req = Request::new("GET", "/health");
    req.request_id = "trace-abc-123".into();
    let r = syntra::server::handle(&app.state, &req);
    assert!(
        r.headers
            .iter()
            .any(|(k, v)| k == "x-request-id" && v == "trace-abc-123"),
        "{:?}",
        r.headers
    );
    // Errors carry it too.
    let mut req = Request::new("GET", "/v1/no/such/route");
    req.request_id = "trace-err".into();
    let r = syntra::server::handle(&app.state, &req);
    assert_eq!(r.status, 404);
    assert!(
        r.headers
            .iter()
            .any(|(k, v)| k == "x-request-id" && v == "trace-err")
    );

    // On the wire: a caller's id is echoed; a missing or unusable one is
    // replaced by a generated 16-hex-digit id.
    let dir = TempDir::new("rid-wire");
    let srv = Server::start(&dir.join("store"), Some("k-rid"));
    let a = agent();
    let get = |headers: &[(&str, &str)]| {
        let mut h = vec![("Authorization", "Bearer k-rid")];
        h.extend_from_slice(headers);
        try_http(&a, "GET", &srv.url("/v1/tenants"), &h, None).unwrap()
    };
    let r = get(&[("X-Request-Id", "client-42")]);
    assert_eq!(r.header("x-request-id"), Some("client-42"));
    let generated = |r: &HttpResponse| {
        let id = r.header("x-request-id").unwrap().to_string();
        assert!(
            id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()),
            "{id}"
        );
        id
    };
    let one = generated(&get(&[]));
    let two = generated(&get(&[]));
    assert_ne!(one, two);
    generated(&get(&[("X-Request-Id", &"z".repeat(129))]));
    generated(&get(&[("X-Request-Id", "has space")]));
    // 401s are traceable too.
    let r = try_http(
        &a,
        "GET",
        &srv.url("/v1/tenants"),
        &[("X-Request-Id", "unauth-1")],
        None,
    )
    .unwrap();
    assert_eq!(r.status, 401);
    assert_eq!(r.header("x-request-id"), Some("unauth-1"));
}

fn header<'a>(r: &'a syntra::server::http::Response, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[test]
fn unversioned_alias_matches_v1_and_is_marked_deprecated() {
    let app = App::dev("alias");
    app.put_spec(T, J, "c", three_actions());
    let d = app.decide(T, J, "c", json!({"context": {}, "durable": true}));
    let id = d["decisionId"].as_str().unwrap().to_string();
    for target in [
        format!("/tenants/{T}/jobs/{J}/capsules/c"),
        format!("/tenants/{T}/jobs/{J}/capsules/c/spec"),
        format!("/tenants/{T}/jobs/{J}/capsules/c/policy"),
        format!("/tenants/{T}/jobs/{J}/capsules/c/model"),
        format!("/tenants/{T}/jobs/{J}/capsules/c/decisions?limit=5"),
        format!("/tenants/{T}/jobs/{J}/capsules/c/decisions/{id}"),
        format!("/tenants/{T}/jobs/{J}/capsules/c/audits"),
        format!("/tenants/{T}/jobs/{J}/capsules/missing/spec"),
        format!("/tenants/{T}/jobs"),
        "/tenants".to_string(),
        "/auth/whoami".to_string(),
        "/no/such/thing".to_string(),
    ] {
        let v1 = app.raw("GET", &format!("/v1{target}"), &Cred::None, None);
        let alias = app.raw("GET", &target, &Cred::None, None);
        assert_eq!(v1.status, alias.status, "{target}");
        assert_eq!(v1.body, alias.body, "{target}");
        assert_eq!(header(&v1, "deprecation"), None, "{target}");
        assert_eq!(header(&v1, "link"), None, "{target}");
        assert_eq!(header(&alias, "deprecation"), Some("true"), "{target}");
        assert_eq!(
            header(&alias, "link"),
            Some(format!("</v1{target}>; rel=\"successor-version\"").as_str()),
            "{target}"
        );
    }
    // Writes through the alias behave the same.
    let (st, alias_d) = app.call(
        "POST",
        &format!("/tenants/{T}/jobs/{J}/capsules/c/decide"),
        Some(json!({"context": {}})),
    );
    assert_eq!(st, 200);
    assert_eq!(
        alias_d.as_object().unwrap().keys().collect::<Vec<_>>(),
        d.as_object().unwrap().keys().collect::<Vec<_>>()
    );
    // Operational endpoints are unversioned and carry no deprecation.
    for target in ["/health", "/ready", "/metrics", "/admin", "/v1/admin"] {
        let r = app.raw("GET", target, &Cred::None, None);
        assert_eq!(r.status, 200, "{target}");
        assert_eq!(header(&r, "deprecation"), None, "{target}");
    }
}

#[test]
fn metrics_expose_decide_latency_and_model_versions() {
    let app = App::with_key("metrics", "k-metrics");
    app.put_spec(T, J, "c", three_actions());
    app.put_spec("globex", "j", "c2", three_actions());
    for _ in 0..3 {
        let d = app.decide(T, J, "c", json!({"context": {}}));
        app.reward(
            T,
            J,
            "c",
            json!({"decisionId": d["decisionId"], "reward": 1}),
        );
    }
    app.decide("globex", "j", "c2", json!({}));
    app.flush();
    // /metrics takes an admin credential: it names every capsule.
    let r = app.raw("GET", "/metrics", &Cred::None, None);
    assert_eq!(r.status, 401);
    let r = app.raw("GET", "/metrics", &Cred::Bearer("k-metrics".into()), None);
    assert_eq!(r.status, 200);
    assert!(
        header(&r, "content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    let text = String::from_utf8(r.body.to_vec()).unwrap();
    for want in [
        "# TYPE syntra_decide_seconds histogram",
        "syntra_decide_seconds_bucket{le=\"+Inf\"} 4",
        "syntra_decide_seconds_count 4",
        "# TYPE syntra_model_version gauge",
        "syntra_model_version{tenant=\"acme\",job=\"prod\",capsule=\"c\"} 3",
        "syntra_model_version{tenant=\"globex\",job=\"j\",capsule=\"c2\"} 0",
        "syntra_requests_total{route=\"capsule.decide\",status=\"200\"} 4",
        "syntra_requests_total{route=\"capsule.reward\",status=\"200\"} 3",
        "syntra_decisions_committed_total 4",
        "syntra_rewards_committed_total 3",
        "syntra_rewards_refused_after_apply_total 0",
        "syntra_events_lost_total 0",
        "syntra_decision_log_backlog 0",
        "syntra_uptime_seconds",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // Buckets are cumulative.
    let buckets: Vec<u64> = text
        .lines()
        .filter(|l| l.starts_with("syntra_decide_seconds_bucket"))
        .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(buckets.len(), 15);
    assert!(buckets.windows(2).all(|w| w[0] <= w[1]), "{buckets:?}");
}

// ───────────────────────────── learning ──────────────────────────────────

/// Two context segments, three actions, Bernoulli rewards from a fixed
/// seed; each segment has a different best action. Through the API only.
#[test]
fn learns_a_different_best_action_per_segment() {
    let app = App::dev("learn");
    app.put_spec(
        T,
        J,
        "l",
        json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "seed": 20260923, "snapshotEvery": 500}),
    );
    // P(reward = 1) by segment and action.
    let means: [(&str, [f64; 3]); 2] = [("north", [0.8, 0.5, 0.2]), ("south", [0.2, 0.45, 0.75])];
    let best = ["a", "c"];
    let actions = ["a", "b", "c"];
    let mut rng = SplitMix64::new(0x5eed);
    const ROUNDS: usize = 3000;
    const TAIL: usize = 500;
    let mut hits = [0usize; 2];
    let mut seen = [0usize; 2];
    for round in 0..ROUNDS {
        let s = (rng.next_u64() % 2) as usize;
        let d = app.decide(T, J, "l", json!({"context": {"segment": means[s].0}}));
        let action = d["action"].as_str().unwrap();
        let k = actions.iter().position(|a| *a == action).unwrap();
        let reward = if rng.next_f64() < means[s].1[k] {
            1.0
        } else {
            0.0
        };
        app.reward(
            T,
            J,
            "l",
            json!({"decisionId": d["decisionId"], "reward": reward}),
        );
        if round >= ROUNDS - TAIL {
            seen[s] += 1;
            if action == best[s] {
                hits[s] += 1;
            }
        }
    }
    for s in 0..2 {
        let rate = hits[s] as f64 / seen[s] as f64;
        println!(
            "segment {}: best action {} in {:.1}% of the last {} rounds",
            means[s].0,
            best[s],
            rate * 100.0,
            seen[s]
        );
        assert!(rate >= 0.7, "segment {}: {rate:.3}", means[s].0);
    }
    assert_eq!(app.model_version(T, J, "l"), ROUNDS as u64);
}

// ───────────────────── consistency under reloads and races ───────────────

/// A spec, policy or program change reloads the capsule's runtime from the
/// event log. Rewards the old runtime applied that are still in the
/// write-behind queue must not vanish from the reloaded model.
#[test]
fn a_runtime_reload_keeps_rewards_still_in_the_write_queue() {
    let app = App::dev("reload");
    app.put_spec(T, J, "c", three_actions());
    for round in 1..=25u64 {
        let d = app.decide(T, J, "c", json!({"context": {"r": round}, "durable": true}));
        let r = app.reward(
            T,
            J,
            "c",
            json!({"decisionId": d["decisionId"], "reward": 1}),
        );
        assert_eq!(r["modelVersion"], json!(round));
        // Invalidate the runtime (no program to remove: nothing touches
        // the disk, so this is immediate), then load it again.
        app.ok("DELETE", &cap(T, J, "c", "/program"), None, 200);
        assert_eq!(
            app.model_version(T, J, "c"),
            round,
            "round {round}: the reloaded model lost a queued reward"
        );
    }
    app.flush();
    let stats = app.ok("GET", &cap(T, J, "c", ""), None, 200)["stats"].clone();
    assert_eq!(stats["rewards"], json!(25));
}

/// Concurrent retries of one reward race the write-behind commit. Exactly
/// one may train the model, whatever the interleaving.
#[test]
fn concurrent_reward_retries_apply_exactly_once() {
    let app = App::dev("retry-race");
    app.put_spec(T, J, "c", three_actions());
    const ROUNDS: usize = 120;
    const THREADS: usize = 4;
    for round in 0..ROUNDS {
        let d = app.decide(T, J, "c", json!({"context": {"r": round}}));
        let body = json!({"decisionId": d["decisionId"], "reward": 1});
        let barrier = Arc::new(Barrier::new(THREADS));
        let applied: usize = (0..THREADS)
            .map(|_| {
                let state = app.state.clone();
                let barrier = barrier.clone();
                let body = body.clone();
                let target = cap(T, J, "c", "/reward");
                std::thread::spawn(move || {
                    barrier.wait();
                    // Keep retrying across several write-behind commits
                    // (a batch waits at most 2 ms).
                    let until = std::time::Instant::now() + Duration::from_millis(8);
                    let mut applied = 0;
                    while std::time::Instant::now() < until {
                        let (st, v) = post(&state, &target, &body);
                        assert_eq!(st, 200, "{v}");
                        if v["applied"] == json!(true) {
                            applied += 1;
                        }
                    }
                    applied
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum();
        assert_eq!(applied, 1, "round {round}: reward applied {applied} times");
    }
    app.flush();
    let capsule = app.ok("GET", &cap(T, J, "c", ""), None, 200);
    assert_eq!(capsule["stats"]["rewards"], json!(ROUNDS));
    assert_eq!(capsule["modelVersion"], json!(ROUNDS));
    let metrics =
        String::from_utf8(app.raw("GET", "/metrics", &Cred::None, None).body.to_vec()).unwrap();
    assert!(
        metrics.contains("syntra_rewards_refused_after_apply_total 0"),
        "{metrics}"
    );
}

/// Concurrent requests with one eventId (a client retrying while the first
/// attempt is in flight) must all answer with the one logged decision.
#[test]
fn concurrent_event_id_retries_return_one_decision() {
    let app = App::dev("eventid-race");
    app.put_spec(
        T,
        J,
        "c",
        json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}, {"id": "d"}], "learner": {"bits": 12}}),
    );
    const THREADS: usize = 6;
    for round in 0..60 {
        let body = json!({"context": {"r": round}, "eventId": format!("race-{round}")});
        let barrier = Arc::new(Barrier::new(THREADS));
        let answers: Vec<Value> = (0..THREADS)
            .map(|_| {
                let state = app.state.clone();
                let barrier = barrier.clone();
                let body = body.clone();
                let target = cap(T, J, "c", "/decide");
                std::thread::spawn(move || {
                    barrier.wait();
                    let (st, v) = post(&state, &target, &body);
                    assert_eq!(st, 200, "{v}");
                    v
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        app.flush();
        let stored = app.ok(
            "GET",
            &cap(T, J, "c", &format!("/decisions/race-{round}")),
            None,
            200,
        );
        for a in &answers {
            assert_eq!(a["decisionId"], json!(format!("race-{round}")));
            assert_eq!(
                (&a["action"], &a["probability"]),
                (&stored["action"], &stored["probability"]),
                "round {round}: a caller was told {} but the log says {}",
                a["action"],
                stored["action"]
            );
        }
    }
}

/// The model a restart rebuilds (latest snapshot plus replay) must be the
/// live model, bit for bit, whatever happened to the spec in between:
/// learning settings, reward range and mode all change how a reward
/// trains the model.
#[test]
fn restart_rebuilds_exactly_the_live_model_across_spec_changes() {
    let mut app = App::dev("replay-exact");
    app.put_spec(
        T,
        J,
        "c",
        json!({"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}], "learner": {"bits": 12}, "snapshotEvery": 1000}),
    );
    let mut rng = SplitMix64::new(77);
    let mut learn = |app: &App, n: usize, range: f64| {
        for i in 0..n {
            let d = app.decide(
                T,
                J,
                "c",
                json!({"context": {"seg": i % 3, "x": rng.next_f64()}}),
            );
            let reward = (rng.next_f64() * range * 100.0).round() / 100.0;
            app.reward(
                T,
                J,
                "c",
                json!({"decisionId": d["decisionId"], "reward": reward}),
            );
        }
    };
    let snapshot = |app: &App| {
        let v = app.ok("GET", &cap(T, J, "c", "/model?snapshot=true"), None, 200);
        (
            v["modelVersion"].as_u64().unwrap(),
            v["snapshot"].as_str().unwrap().to_string(),
        )
    };

    learn(&app, 15, 1.0);
    // Hot-swapped spec changes that alter how later rewards train.
    app.put_spec(
        T,
        J,
        "c",
        json!({"reward": {"range": [-1, 3]}, "learner": {"learningRate": 0.2, "importance": "mtr"}}),
    );
    learn(&app, 15, 3.0);
    app.ok(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "frozen"})),
        200,
    );
    learn(&app, 5, 3.0);
    app.ok(
        "POST",
        &cap(T, J, "c", "/mode"),
        Some(json!({"mode": "learner"})),
        200,
    );
    app.put_spec(T, J, "c", json!({"learner": {"learningRate": 1.5}}));
    learn(&app, 10, 3.0);
    let live = snapshot(&app);
    assert_eq!(live.0, 40, "frozen rewards are not learned");

    // A reload from the store (policy change) and a restart without a
    // shutdown snapshot (a crash) both rebuild the same bytes.
    app.ok(
        "PUT",
        &cap(T, J, "c", "/policy"),
        Some(json!({"allow_stdout": false})),
        200,
    );
    assert_eq!(
        snapshot(&app),
        live,
        "reloaded model differs from the live one"
    );
    app.restart(false);
    assert_eq!(
        snapshot(&app),
        live,
        "restarted model differs from the live one"
    );
    app.restart(true);
    assert_eq!(snapshot(&app), live, "gracefully restarted model differs");
}

/// `detail` is the caller's free-form JSON. It must not be able to mark a
/// learned reward as unlearned, which would make every restart drop it
/// from the model.
#[test]
fn reward_detail_cannot_change_what_replay_learns() {
    let mut app = App::dev("detail-learned");
    app.put_spec(T, J, "c", three_actions());
    let d = app.decide(T, J, "c", json!({}));
    let (st, v) = app.call(
        "POST",
        &cap(T, J, "c", "/reward"),
        Some(json!({"decisionId": d["decisionId"], "reward": 1, "detail": {"learned": false}})),
    );
    if st == 200 {
        assert_eq!(v["learned"], json!(true), "{v}");
        let live = app.model_version(T, J, "c");
        app.restart(false);
        assert_eq!(
            app.model_version(T, J, "c"),
            live,
            "a caller's detail.learned=false made replay skip a learned reward"
        );
    } else {
        assert_eq!(st, 400, "{v}");
        assert!(error_of(&v).contains("learned"), "{v}");
        assert_eq!(app.model_version(T, J, "c"), 0);
    }
    // Other detail keys are stored as given.
    let d = app.decide(T, J, "c", json!({}));
    app.reward(
        T,
        J,
        "c",
        json!({"decisionId": d["decisionId"], "reward": 1, "detail": {"quality": 0.9, "tags": ["a"]}, "durable": true}),
    );
    let rec = app.ok(
        "GET",
        &cap(
            T,
            J,
            "c",
            &format!("/decisions/{}", d["decisionId"].as_str().unwrap()),
        ),
        None,
        200,
    );
    assert_eq!(
        rec["rewards"][0]["detail"],
        json!({"quality": 0.9, "tags": ["a"]})
    );
}
