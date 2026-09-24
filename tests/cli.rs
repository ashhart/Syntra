//! The operator CLI (`syntra`) and the language CLI (`lycan`) as a user
//! runs them: help, unknown commands, `status`/`stop` against a running
//! server, and running a program with injected input.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};

fn json_line(out: &std::process::Output) -> Value {
    serde_json::from_str(stdout(out).trim())
        .unwrap_or_else(|e| panic!("not one JSON line ({e}): {:?}", stdout(out)))
}

#[test]
fn every_subcommand_answers_help_with_exit_0() {
    for cmd in [
        "serve", "demo", "health", "status", "stop", "doctor", "backup", "restore", "import",
        "evaluate",
    ] {
        let out = run(SYNTRA, &[cmd, "--help"]);
        assert_eq!(out.status.code(), Some(0), "syntra {cmd} --help");
        assert!(
            stderr(&out).contains("syntra"),
            "syntra {cmd} --help printed no usage"
        );
    }
}

#[test]
fn syntra_serve_help_prints_its_options() {
    for flag in ["--help", "-h"] {
        let out = run(SYNTRA, &["serve", flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let usage = stderr(&out);
        for want in ["syntra serve", "SYNTRA_ADMIN_KEY", "--dev-mode", "--specs"] {
            assert!(usage.contains(want), "{flag}: missing {want:?} in\n{usage}");
        }
    }
}

#[test]
fn syntra_version_prints_the_crate_version() {
    for flag in ["--version", "-V", "version"] {
        let out = run(SYNTRA, &[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(
            stdout(&out).trim(),
            format!("syntra {}", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}

#[test]
fn syntra_help_lists_every_command() {
    for args in [vec!["--help"], vec!["-h"], vec![]] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(stdout(&out).is_empty(), "{args:?}: usage goes to stderr");
        let usage = stderr(&out);
        for want in [
            "Usage:",
            "syntra serve",
            "--dev-mode",
            "syntra status",
            "syntra stop",
            "syntra doctor --store",
            "syntra backup --store",
            "syntra restore --from",
            "`lycan` binary",
        ] {
            assert!(
                usage.contains(want),
                "{args:?}: missing {want:?} in\n{usage}"
            );
        }
    }
    for cmd in ["status", "stop", "doctor"] {
        let out = run(SYNTRA, &[cmd, "--help"]);
        assert_eq!(out.status.code(), Some(0), "{cmd} --help");
        assert!(
            stderr(&out).contains(&format!("Usage: syntra {cmd}")),
            "{cmd}: {}",
            stderr(&out)
        );
    }
}

/// An unknown command must fail: a script calling `syntra migrate` (which
/// the v1-store refusal recommends) or a typo must not see success.
#[test]
fn unknown_subcommands_exit_nonzero() {
    for args in [
        vec!["frobnicate"],
        vec!["srve", "--addr", "127.0.0.1:0"],
        vec!["migrate", "--from", "old", "--to", "new"],
        vec!["author", "capsule.yaml"],
        vec!["SERVE"],
        vec!["--bogus"],
    ] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(
            err.contains(&format!("unknown command {:?}", args[0])),
            "{args:?}: {err}"
        );
        assert!(err.contains("Usage:"), "{args:?}: {err}");
        assert!(stdout(&out).is_empty());
    }
}

fn lsof_available() -> bool {
    std::process::Command::new("lsof")
        .arg("-v")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[test]
fn status_and_stop_find_the_server_by_port() {
    if !lsof_available() {
        eprintln!("skipping: `syntra status` and `syntra stop` need lsof");
        return;
    }
    let dir = TempDir::new("cli-status");
    let store = dir.join("store");
    let mut srv = Server::start(&store, Some("cli-suite-key"));
    let port = srv.addr.rsplit(':').next().unwrap().to_string();
    let port_num: u16 = port.parse().unwrap();
    let pid = srv.pid();

    for args in [
        vec!["status", "--port", port.as_str()],
        vec!["status", "--addr", srv.addr.as_str()],
    ] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(
            json_line(&out),
            json!({"running": true, "port": port_num, "pid": pid}),
            "{args:?}"
        );
    }

    // Leave some state to flush, then stop through the CLI.
    srv.ok(
        "PUT",
        &cap("acme", "j", "c", "/spec"),
        Some(json!({"actions": [{"id": "a"}, {"id": "b"}]})),
        201,
    );
    let d = srv.ok(
        "POST",
        &cap("acme", "j", "c", "/decide"),
        Some(json!({})),
        200,
    );
    srv.ok(
        "POST",
        &cap("acme", "j", "c", "/reward"),
        Some(json!({"decisionId": d["decisionId"], "reward": 1})),
        200,
    );
    let out = run(SYNTRA, &["stop", "--port", &port]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        json_line(&out),
        json!({"stopped": true, "port": port_num, "pid": pid, "signal": "TERM"})
    );
    // SIGTERM drains, flushes, snapshots and exits 0.
    assert!(
        wait_until(Duration::from_secs(40), || srv
            .child
            .try_wait()
            .unwrap()
            .is_some()),
        "server did not exit after `syntra stop`"
    );
    assert_eq!(srv.child.try_wait().unwrap().unwrap().code(), Some(0));
    assert!(!store.join("server.pid").exists());
    assert!(!store.join("syntra.db-wal").exists());
    assert_eq!(doctor(&store), (0, vec![]));

    // Nothing listens now.
    let out = run(SYNTRA, &["status", "--port", &port]);
    assert_eq!(json_line(&out), json!({"running": false, "port": port_num}));
    let out = run(SYNTRA, &["stop", "--port", &port]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        json_line(&out),
        json!({"stopped": false, "port": port_num, "reason": "no listener"})
    );

    // The model survived the CLI stop.
    srv.restart();
    let v = srv.ok("GET", &cap("acme", "j", "c", ""), None, 200);
    assert_eq!(v["modelVersion"], json!(1));
    assert_eq!(v["stats"]["rewards"], json!(1));
}

#[test]
fn lycan_help() {
    for flag in ["--help", "-h"] {
        let out = run(LYCAN, &[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let usage = stderr(&out);
        for want in [
            "lycan <file.lycs>",
            "lycan <file> --input <request.json>",
            "lycan compile <file.lycs>",
            "lycan capabilities",
        ] {
            assert!(usage.contains(want), "{flag}: missing {want:?} in\n{usage}");
        }
    }
    let out = run(LYCAN, &["frobnicate", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown command 'frobnicate'"));
}

const INPUT_PROGRAM: &str = r#"
($ tier (!cap "runtime.inputGet" "user.tier"))
($ tokens (!cap "runtime.inputGet" "tokens"))
($ first (!cap "runtime.inputGet" "tags.0"))
(!p "tier" tier)
(!p "double" (* tokens 2))
(!p "first" first)
(!p "missing" (!cap "runtime.inputGet" "nope.deeper"))
(? (== tier "pro") (!p "route" "large") (!p "route" "small"))
"#;

#[test]
fn lycan_runs_a_program_with_injected_input() {
    let dir = TempDir::new("lycan-input");
    let src = dir.join("route.lycs");
    std::fs::write(&src, INPUT_PROGRAM).unwrap();
    let pro = dir.join("pro.json");
    std::fs::write(
        &pro,
        r#"{"user": {"tier": "pro"}, "tokens": 21, "tags": ["fast", "cheap"]}"#,
    )
    .unwrap();
    let free = dir.join("free.json");
    std::fs::write(
        &free,
        r#"{"user": {"tier": "free"}, "tokens": 5, "tags": []}"#,
    )
    .unwrap();
    let s = |p: &std::path::Path| p.to_str().unwrap().to_string();

    let out = run(LYCAN, &[&s(&src), "--input", &s(&pro)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "tier pro\ndouble 42\nfirst fast\nmissing null\nroute large\n"
    );
    let out = run(LYCAN, &[&s(&src), "--input", &s(&free)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "tier free\ndouble 10\nfirst null\nmissing null\nroute small\n"
    );

    // The compiled graph behaves the same.
    let out = run(LYCAN, &["compile", &s(&src)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lyc = dir.join("route.lyc");
    assert!(lyc.exists());
    let out = run(LYCAN, &[&s(&lyc), "--input", &s(&pro)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "tier pro\ndouble 42\nfirst fast\nmissing null\nroute large\n"
    );

    // Failures exit 1 with a reason.
    let bad_json = dir.join("bad.json");
    std::fs::write(&bad_json, "{\"user\": ").unwrap();
    let garbage = dir.join("garbage.lyc");
    std::fs::write(&garbage, b"LYCN\x00\x01garbage").unwrap();
    let unverifiable = dir.join("arity.lycs");
    std::fs::write(&unverifiable, "(!p (+ 1))").unwrap();
    for (args, want) in [
        (
            vec![s(&src), "--input".into(), s(&bad_json)],
            "invalid JSON",
        ),
        (
            vec![s(&src), "--input".into(), s(&dir.join("absent.json"))],
            "error reading",
        ),
        (
            vec![s(&dir.join("absent.lycs")), "--input".into(), s(&pro)],
            "error reading",
        ),
        (vec![s(&garbage), "--input".into(), s(&pro)], ""),
        (vec![s(&unverifiable), "--input".into(), s(&pro)], ""),
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(LYCAN, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        assert!(!stderr(&out).trim().is_empty(), "{args:?}: no reason given");
        assert!(stderr(&out).contains(want), "{args:?}: {}", stderr(&out));
    }
}

#[test]
fn serve_refuses_unknown_options() {
    for args in [
        vec!["serve", "--stroe", "/tmp/x", "--dev-mode"],
        vec!["serve", "--dev-mode", "--addr"],
    ] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
}

#[test]
fn health_asks_the_running_server() {
    let dir = TempDir::new("cli-health");
    let srv = Server::start(&dir.join("store"), Some("cli-health-key"));
    let out = run(SYNTRA, &["health", "--addr", &srv.addr]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(json_line(&out)["ok"], json!(true));
    let out = run(SYNTRA, &["health", "--port", &free_port().to_string()]);
    assert_eq!(out.status.code(), Some(1), "nothing listens there");
}

#[test]
fn stop_refuses_a_listener_that_is_not_syntra() {
    if !lsof_available() {
        eprintln!("skipping: `syntra stop` needs lsof");
        return;
    }
    // This test process listens; `stop` must not signal it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let out = run(SYNTRA, &["stop", "--port", &port]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("not syntra"), "{}", stderr(&out));
    drop(listener);
}

#[test]
fn metrics_needs_an_admin_credential_unless_public() {
    let dir = TempDir::new("cli-metrics");
    let srv = Server::start(&dir.join("store"), Some("cli-metrics-key"));
    let anon = try_http(&agent(), "GET", &srv.url("/metrics"), &[], None).unwrap();
    assert_eq!(anon.status, 401);
    assert_eq!(srv.http("GET", "/metrics", None).status, 200);

    let dir = TempDir::new("cli-metrics-public");
    let srv = Server::start_with(
        &dir.join("store"),
        Some("cli-metrics-key"),
        &["--metrics-public"],
    );
    let anon = try_http(&agent(), "GET", &srv.url("/metrics"), &[], None).unwrap();
    assert_eq!(anon.status, 200);
}

#[test]
fn demo_validates_its_options() {
    let out = run(SYNTRA, &["demo", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("simulated traffic"),
        "{}",
        stderr(&out)
    );
    for args in [vec!["demo", "--rate", "0"], vec!["demo", "--bogus", "1"]] {
        let out = run(SYNTRA, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}
