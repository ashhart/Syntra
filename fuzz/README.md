# Fuzzing

Coverage-guided fuzzing for every parser that consumes untrusted bytes.
Built on `cargo-fuzz` (libFuzzer + ASan) against the `syntra` crate.

## Targets

| Target | Entry point | Reachable from |
|---|---|---|
| `graph_from_bytes` | `graph::NeuralGraph::from_bytes` | **Server install path** — tenants POST `.lyc` bytes to `/install` |
| `lycs_parse` | lexer → parser → graph compiler | `lycan compile` / `syntra author` on caller-supplied source |
| `binary_decode` | `binary::decode` (AST-level `.lyc`) | `lycan dump/inspect/stats` on untrusted files |
| `learning_config_json` | `LearningConfig::from_json` + reward computation | tenant-supplied `learning.json` / reward specs |
| `hierarchical_spec_json` | `HierarchicalSpec::from_json` + `validate` | tenant-supplied capsule sidecar |

## Running

```bash
# 2-minute smoke per target (nightly toolchain required)
cargo fuzz run graph_from_bytes      -- -max_total_time=120 -rss_limit_mb=4096
cargo fuzz run lycs_parse            -- -max_total_time=120 -rss_limit_mb=4096
cargo fuzz run binary_decode         -- -max_total_time=120 -rss_limit_mb=4096
cargo fuzz run learning_config_json  -- -max_total_time=120 -rss_limit_mb=4096
cargo fuzz run hierarchical_spec_json -- -max_total_time=120 -rss_limit_mb=4096

# Replay a saved crashing input
./fuzz/target/<triple>/release/<target> fuzz/artifacts/<target>/<file>
```

Crashing inputs land in `fuzz/artifacts/<target>/` (gitignored). Seed
corpora live in `fuzz/corpus/<target>/`.

## Results

- 2026-07-07 initial campaign (60s/target): `lycs_parse`,
  `learning_config_json`, `hierarchical_spec_json` clean
  (7M+ combined execs). `graph_from_bytes` found a **64 GB single
  allocation from a 2,163-byte input**: per-node `operand_count` /
  `weight_count` were raw `u32`s feeding `Vec::with_capacity` before the
  decode loop could error — the header-level DoS guards never saw
  per-node counts. Fixed in `src/graph.rs` (per-node `check_count`
  guards) with regression tests
  (`rejects_node_with_u32_max_operand_count`,
  `rejects_node_with_u32_max_weight_count`).
- The same audit found `binary::decode` entirely unhardened (panicking
  primitive readers, unbounded `read_str`, unguarded counts). Fixed with
  bounds-checked readers and `check_count` at every allocation site,
  mirroring `graph.rs`.

## Known limitations

- `lycs_parse` and `binary_decode` are recursive-descent; a pathologically
  deep input can overflow the stack. The CLI allocates a 64 MB stack
  (50k-deep nesting verified compiling); libFuzzer targets run on the
  default stack. A parser depth cap is the proper fix if a campaign ever
  trips this. `graph_from_bytes` is iterative and not exposed.
- CI does not run fuzzing (nondeterministic wall-time); run the smoke
  campaign locally before touching any parser.
