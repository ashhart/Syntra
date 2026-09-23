//! Operational edges: repairing a corrupt spec, audit trails that outlive
//! their capsule, and strict idempotency keys in every reward mode.

mod common;

use common::*;
use serde_json::json;

const T: &str = "acme";
const J: &str = "prod";
const C: &str = "router";

#[test]
fn a_corrupt_spec_is_repaired_with_replace() {
    let app = App::dev("repair");
    app.put_spec(
        T,
        J,
        C,
        json!({"actions": [{"id": "a"}, {"id": "b"}], "seed": 7}),
    );
    let spec_file = app
        .store()
        .join(format!("tenants/{T}/jobs/{J}/capsules/{C}/spec.json"));
    assert!(spec_file.exists(), "{spec_file:?}");
    std::fs::write(&spec_file, "{ not json").unwrap();

    // A patch needs the stored spec, so it cannot help.
    let (st, _) = app.call(
        "PUT",
        &cap(T, J, C, "/spec"),
        Some(json!({"mode": "frozen"})),
    );
    assert_eq!(st, 500);
    // A replacement does not.
    let fixed = app.ok(
        "PUT",
        &cap(T, J, C, "/spec?replace=true"),
        Some(json!({"actions": [{"id": "x"}]})),
        200,
    );
    assert_eq!(fixed["actions"], json!([{"id": "x"}]));
    assert!(
        fixed.get("seed").is_none(),
        "replace starts from the defaults: {fixed}"
    );
    let (st, _) = app.call("GET", &cap(T, J, C, "/spec"), None);
    assert_eq!(st, 200);
    let d = app.decide(T, J, C, json!({"context": {}}));
    assert_eq!(d["action"], "x");

    let (_, audits) = app.call("GET", &cap(T, J, C, "/audits"), None);
    let events: Vec<&str> = audits["audits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap())
        .collect();
    assert!(events.contains(&"spec_replaced"), "{events:?}");

    let (st, _) = app.call("PUT", &cap(T, J, C, "/spec?replace=maybe"), Some(json!({})));
    assert_eq!(st, 400);
}

#[test]
fn audit_trails_outlive_their_capsule() {
    let app = App::dev("audit-retention");
    app.put_spec(T, J, C, json!({"actions": [{"id": "a"}]}));
    app.ok(
        "POST",
        &cap(T, J, C, "/mode"),
        Some(json!({"mode": "frozen"})),
        200,
    );
    app.ok("DELETE", &cap(T, J, C, ""), None, 200);

    let (st, v) = app.call("GET", &cap(T, J, C, "/audits"), None);
    assert_eq!(st, 200, "{v}");
    let events: Vec<&str> = v["audits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        ["capsule_created", "mode_changed", "capsule_deleted"]
    );
    // A capsule that never existed has no trail.
    let (st, _) = app.call("GET", &cap(T, J, "never", "/audits"), None);
    assert_eq!(st, 404);
}

#[test]
fn malformed_idempotency_keys_are_refused_in_every_mode() {
    let app = App::dev("keys");
    for (capsule, mode) in [("first", "first"), ("sum", "sum")] {
        app.put_spec(
            T,
            J,
            capsule,
            json!({"actions": [{"id": "a"}], "rewards": mode}),
        );
        let d = app.decide(T, J, capsule, json!({"context": {}}));
        let id = d["decisionId"].as_str().unwrap();
        for bad in ["", "k\u{0}ey"] {
            let (st, v) = app.call(
                "POST",
                &cap(T, J, capsule, "/reward"),
                Some(json!({"decisionId": id, "reward": 1, "idempotencyKey": bad})),
            );
            assert_eq!(st, 400, "{mode} {bad:?}: {v}");
        }
        let long = "k".repeat(257);
        let (st, _) = app.call(
            "POST",
            &cap(T, J, capsule, "/reward"),
            Some(json!({"decisionId": id, "reward": 1, "idempotencyKey": long})),
        );
        assert_eq!(st, 400, "{mode}");
    }
}

#[test]
fn escaped_path_segments_are_decoded_one_segment_at_a_time() {
    let app = App::dev("escapes");
    app.put_spec(T, J, C, json!({"actions": [{"id": "a"}]}));
    let d = app.decide(T, J, C, json!({"context": {}, "eventId": "order:42"}));
    assert_eq!(d["decisionId"], "order:42");
    for target in [
        cap(T, J, C, "/decisions/order:42"),
        cap(T, J, C, "/decisions/order%3A42"),
        cap(T, J, C, "/decisions/order%3a42"),
    ] {
        let (st, v) = app.call("GET", &target, None);
        assert_eq!(st, 200, "{target}: {v}");
        assert_eq!(v["decisionId"], "order:42");
    }
    // An escaped `/` stays inside its segment, where names refuse it.
    let (st, _) = app.call("GET", &cap(T, J, "rou%2Fter", "/spec"), None);
    assert_eq!(st, 400);
    // A retried durable decide answers only once its decision is on disk.
    let body = json!({"context": {"k": 1}, "eventId": "durable-1", "durable": true});
    let first = app.decide(T, J, C, body.clone());
    let again = app.decide(T, J, C, body);
    assert_eq!(again["decisionId"], first["decisionId"]);
    assert_eq!(again["replayed"], true);
    let key = syntra::eventstore::CapsuleKey::new(T, J, C).unwrap();
    assert!(
        app.state
            .events
            .get_decision(&key, "durable-1")
            .unwrap()
            .is_some(),
        "committed, not just queued"
    );
}
