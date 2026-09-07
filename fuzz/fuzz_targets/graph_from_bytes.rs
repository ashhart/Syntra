//! Fuzz the `.lyc` graph binary parser — the hostile install path.
//! Tenants upload compiled capsule bytes over HTTP; parsing must never
//! panic (returning an error is fine and expected for garbage input).

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = syntra::graph::NeuralGraph::from_bytes(data);
});
