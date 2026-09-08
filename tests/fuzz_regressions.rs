//! Byte-exact regressions for real fuzzer finds.
//!
//! Corpus-seeded fuzzing runs in CI (`.github/workflows/ci.yml`); this file
//! pins the specific crashes that were already fixed in the library, so a
//! guard regression fails a named test instead of relying on fuzz time.

/// OOM find (2026-07, `graph_from_bytes`): a 2163-byte input whose header
/// counts drove multi-GB `Vec::with_capacity` allocations. Fixed by the
/// `check_count` guard (`src/graph.rs`: hard ceilings + remaining-buffer
/// min-bytes check). The artifact must now be rejected, not allocated.
#[test]
fn graph_from_bytes_rejects_oom_trigger_input() {
    let data = include_bytes!("fixtures/fuzz_graph_from_bytes_oom.bin");
    let result = syntra::graph::NeuralGraph::from_bytes(data);
    assert!(
        result.is_err(),
        "header-count guard regressed: the known OOM-trigger input parsed"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("maximum") || err.contains("input size") || err.contains("out of bounds"),
        "expected a header-count guard rejection, got: {err}"
    );
}
