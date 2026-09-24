//! Copy `examples/learning_bench.rs` into `OUT_DIR` without its `//!`
//! module docs, which `include!` does not accept, so `src/main.rs` runs the
//! example's own environments and run loop instead of a copy of them.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest.join("../../examples/learning_bench.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let text = std::fs::read_to_string(&source)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", source.display()));
    let body: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//!"))
        .map(|line| format!("{line}\n"))
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("learning_bench.rs");
    std::fs::write(&out, body).unwrap_or_else(|e| panic!("cannot write {}: {e}", out.display()));
}
