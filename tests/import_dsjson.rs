//! `syntra import dsjson`: Personalizer / VW decision logs become a
//! capsule's history, ready for off-policy evaluation, optionally learned.

mod common;

use std::io::Write as _;

use common::*;
use serde_json::{Value, json};

const T: &str = "acme";
const J: &str = "prod";

/// Synthetic Personalizer events: three articles, a uniform-ish logging
/// policy, and rewards where `sports` pays 1, `news` 0.4, `music` 0.
fn events(n: usize) -> Vec<String> {
    let ids = ["news", "sports", "music"];
    let pay = [0.4, 1.0, 0.0];
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    (0..n)
        .map(|i| {
            let chosen = (next() % 3) as usize;
            let mut order = vec![chosen];
            order.extend((0..3).filter(|&k| k != chosen));
            json!({
                "_label_cost": -pay[chosen],
                "_label_probability": 0.5,
                "_label_Action": chosen + 1,
                "_labelIndex": chosen,
                "Timestamp": format!("2026-09-{:02}T10:{:02}:{:02}.1230000Z", 1 + i % 28, i % 60, (i * 7) % 60),
                "Version": "1",
                "EventId": format!("evt-{i:05}"),
                "a": order.iter().map(|k| k + 1).collect::<Vec<_>>(),
                "c": {
                    "User": [{"tier": if i % 2 == 0 { "pro" } else { "free" }}],
                    "_multi": ids.iter().map(|id| json!({
                        "_tag": id, "i": {"constant": 1, "id": id}, "Topic": [{"kind": id}]
                    })).collect::<Vec<_>>()
                },
                "p": [0.5, 0.25, 0.25],
                "VWState": {"m": "model-1"}
            })
            .to_string()
        })
        .collect()
}

fn write_log(dir: &TempDir, lines: &[String]) -> std::path::PathBuf {
    let path = dir.join("log.json");
    let mut f = std::fs::File::create(&path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    path
}

fn import(
    store: &std::path::Path,
    capsule: &str,
    file: &std::path::Path,
    extra: &[&str],
) -> (i32, Value, String) {
    let mut args = vec!["import", "dsjson", "--store"];
    let store_s = store.to_string_lossy().into_owned();
    let file_s = file.to_string_lossy().into_owned();
    args.push(&store_s);
    args.extend(["--capsule", capsule]);
    args.extend(extra);
    args.push(&file_s);
    let out = run(SYNTRA, &args);
    (
        out.status.code().unwrap_or(-1),
        serde_json::from_str(stdout(&out).trim()).unwrap_or(Value::Null),
        stderr(&out),
    )
}

#[test]
fn personalizer_logs_import_for_evaluation() {
    let dir = TempDir::new("dsjson");
    let store = dir.join("store");
    let mut lines = events(600);
    // A deferred event never activated, a multi-slot event, and junk.
    let mut deferred: Value = serde_json::from_str(&lines[0]).unwrap();
    deferred["EventId"] = json!("deferred-1");
    deferred["DeferredAction"] = json!(true);
    lines.push(deferred.to_string());
    let mut slots: Value = serde_json::from_str(&lines[1]).unwrap();
    slots["EventId"] = json!("slots-1");
    slots["c"]["_slots"] = json!([{"_id": "s1"}]);
    lines.push(slots.to_string());
    lines.push("{not json".into());
    let file = write_log(&dir, &lines);

    let (code, report, err) = import(&store, "acme/prod/news", &file, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(report["decisions"], 600, "{report}");
    assert_eq!(report["rewards"], 600);
    assert_eq!(report["skipped"]["deferredNotActivated"], 1);
    assert_eq!(report["skipped"]["multiSlot"], 1);
    assert_eq!(report["skipped"]["invalid"], 1);

    // Importing again changes nothing.
    let (code, again, _) = import(&store, "acme/prod/news", &file, &[]);
    assert_eq!(code, 0);
    assert_eq!(again["decisions"], 0);
    assert_eq!(again["alreadyPresent"], 600);

    // Off-policy evaluation on the imported history: always showing
    // `sports` is worth 1.0; the logging policy much less.
    let out = run(
        SYNTRA,
        &[
            "evaluate",
            "--store",
            &store.to_string_lossy(),
            "--capsule",
            "acme/prod/news",
            "--policy",
            "constant:sports",
            "--bootstrap",
            "200",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let r: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(r["data"]["rows"], 600);
    let dr = r["estimators"]["dr"]["estimate"].as_f64().unwrap();
    assert!((dr - 1.0).abs() < 0.1, "DR estimate of sports {dr}");
    assert!(
        r["lift"]["dr"]["lower"].as_f64().unwrap() > 0.3,
        "{}",
        r["lift"]
    );

    // The server sees the imported decisions, and has not learned them.
    let srv = Server::start(&store, Some("import-key"));
    let d = srv.ok("GET", &cap(T, J, "news", "/decisions/evt-00007"), None, 200);
    assert_eq!(d["mode"], "imported");
    assert_eq!(d["rewards"][0]["detail"]["imported"], "dsjson");
    assert_eq!(d["rewards"][0]["detail"]["learned"], false);
    let m = srv.ok("GET", &cap(T, J, "news", "/model"), None, 200);
    assert_eq!(m["modelVersion"], 0);

    // A live store is refused.
    let (code, _, err) = import(&store, "acme/prod/news", &file, &[]);
    assert_eq!(code, 2);
    assert!(err.contains("live store"), "{err}");
}

#[test]
fn learn_warm_starts_the_model() {
    let dir = TempDir::new("dsjson-learn");
    let store = dir.join("store");
    let file = write_log(&dir, &events(50));
    let (code, report, err) = import(&store, "acme/prod/warm", &file, &["--learn"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(report["rewards"], 50);
    let srv = Server::start(&store, Some("import-key"));
    let m = srv.ok("GET", &cap(T, J, "warm", "/model"), None, 200);
    assert_eq!(
        m["modelVersion"], 50,
        "the model replays imported rewards on load"
    );
}
