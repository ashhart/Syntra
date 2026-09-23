//! `syntra serve --specs <dir>`: capsule specs from files at startup,
//! applied only when they differ, and a bad file stops the server.

mod common;

use common::*;
use serde_json::{Value, json};

const KEY: &str = "specs-dir-key";

fn audit_events(srv: &Server, t: &str, j: &str, c: &str) -> Vec<String> {
    let v = srv.ok("GET", &cap(t, j, c, "/audits"), None, 200);
    v["audits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn specs_from_files_apply_at_startup() {
    let dir = TempDir::new("specs");
    let specs = dir.join("specs");
    std::fs::create_dir_all(&specs).unwrap();
    std::fs::write(
        specs.join("routing.yaml"),
        "tenant: acme\njob: prod\ncapsule: router\nspec:\n  actions: [{id: small}, {id: large}]\n  reward: {default: 0, waitSeconds: 60}\n---\ntenant: acme\njob: prod\ncapsule: ranker\nspec:\n  actions: []\n  rewards: sum\n",
    )
    .unwrap();
    std::fs::write(
        specs.join("other.json"),
        json!([{"tenant": "globex", "job": "j", "capsule": "c",
                "spec": {"actions": [{"id": "a"}], "mode": "frozen"}}])
        .to_string(),
    )
    .unwrap();
    std::fs::write(specs.join("README.txt"), "not a spec file; ignored").unwrap();
    let specs_arg = specs.to_string_lossy().into_owned();

    let store = dir.join("store");
    let mut srv = Server::start_with(&store, Some(KEY), &["--specs", &specs_arg]);
    let router = srv.ok("GET", &cap("acme", "prod", "router", "/spec"), None, 200);
    assert_eq!(router["actions"], json!([{"id": "small"}, {"id": "large"}]));
    assert_eq!(router["reward"]["waitSeconds"], 60);
    let ranker = srv.ok("GET", &cap("acme", "prod", "ranker", "/spec"), None, 200);
    assert_eq!(ranker["rewards"], "sum");
    let other = srv.ok("GET", &cap("globex", "j", "c", "/spec"), None, 200);
    assert_eq!(other["mode"], "frozen");
    assert_eq!(
        audit_events(&srv, "acme", "prod", "router"),
        ["capsule_created"]
    );

    // Someone patches the live spec; a restart restores the file's.
    srv.ok(
        "PUT",
        &cap("acme", "prod", "router", "/spec"),
        Some(json!({"exploration": {"floor": 0.2}})),
        200,
    );
    srv.restart();
    let router = srv.ok("GET", &cap("acme", "prod", "router", "/spec"), None, 200);
    assert_eq!(router["exploration"]["floor"], 0.05, "the file wins");
    let events = audit_events(&srv, "acme", "prod", "router");
    assert_eq!(
        events.last().map(String::as_str),
        Some("spec_applied"),
        "{events:?}"
    );

    // An unchanged file changes nothing on the next restart.
    let before = audit_events(&srv, "globex", "j", "c").len();
    srv.restart();
    assert_eq!(audit_events(&srv, "globex", "j", "c").len(), before);
}

#[test]
fn a_bad_spec_file_stops_the_server() {
    for (name, content) in [
        (
            "typo.yaml",
            "tenant: acme\njob: prod\ncapsule: router\nspec:\n  actoins: []\n",
        ),
        ("missing.yaml", "tenant: acme\njob: prod\nspec: {}\n"),
        (
            "dup.yaml",
            "tenant: a\njob: b\ncapsule: c\nspec: {}\n---\ntenant: a\njob: b\ncapsule: c\nspec: {}\n",
        ),
        (
            "badname.json",
            r#"{"tenant": ".hidden", "job": "j", "capsule": "c", "spec": {}}"#,
        ),
        ("broken.json", "{not json"),
    ] {
        let dir = TempDir::new("specs-bad");
        let specs = dir.join("specs");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(specs.join(name), content).unwrap();
        let out = run(
            SYNTRA,
            &[
                "serve",
                "--addr",
                &format!("127.0.0.1:{}", free_port()),
                "--store",
                &dir.join("store").to_string_lossy(),
                "--admin-key",
                KEY,
                "--specs",
                &specs.to_string_lossy(),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{name}: {}", stderr(&out));
        let err: Value = Value::String(stderr(&out));
        assert!(err.as_str().unwrap().contains(name), "{name}: {err}");
    }
}
