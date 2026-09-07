//! Capsule fixture drift guard.
//!
//! Every committed `examples/lycan/**/*.lyc` must byte-match a fresh
//! `lycan compile` of its sibling `.lycs`. The compiler is deterministic
//! (same source -> same bytes), so any mismatch means the fixture is
//! stale — the bug class that broke the SPICE navigation tests during
//! the repo merge.
//!
//! Regenerate a stale fixture with:
//!   ./target/release/lycan compile examples/lycan/<name>.lycs

use std::path::{Path, PathBuf};
use std::process::Command;

fn walk_lyc_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk_lyc_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("lyc") {
            out.push(path);
        }
    }
}

#[test]
fn committed_lyc_fixtures_match_fresh_compile() {
    let root = Path::new("examples/lycan");
    let mut fixtures = Vec::new();
    walk_lyc_files(root, &mut fixtures);
    assert!(
        fixtures.len() > 20,
        "expected the committed fixture set, found {} files under {}",
        fixtures.len(),
        root.display()
    );

    let work = std::env::temp_dir().join(format!("syntra-fixture-drift-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();

    let mut failures = Vec::new();
    for (i, fixture) in fixtures.iter().enumerate() {
        let lycs = fixture.with_extension("lycs");
        assert!(
            lycs.exists(),
            "orphan .lyc without sibling .lycs: {}",
            fixture.display()
        );

        let dir = work.join(i.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("program.lycs");
        std::fs::copy(&lycs, &src).unwrap();

        let output = Command::new(env!("CARGO_BIN_EXE_lycan"))
            .args(["compile", src.to_str().unwrap()])
            .output()
            .expect("run lycan compile");
        assert!(
            output.status.success(),
            "compile failed for {}: {}",
            lycs.display(),
            String::from_utf8_lossy(&output.stderr)
        );

        let fresh = std::fs::read(dir.join("program.lyc")).unwrap();
        let committed = std::fs::read(fixture).unwrap();
        if fresh != committed {
            failures.push(format!(
                "{}: committed fixture is stale ({} bytes vs fresh {} bytes)",
                fixture.display(),
                committed.len(),
                fresh.len()
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&work);

    assert!(
        failures.is_empty(),
        "stale .lyc fixtures detected — regenerate with `lycan compile <name>.lycs`:\n{}",
        failures.join("\n")
    );
}
