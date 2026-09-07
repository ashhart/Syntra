//! Fuzz the AST-level `.lyc` decoder (`binary::decode`) — reachable from
//! the `lycan dump/inspect/stats` CLI on untrusted files. Must never panic
//! or allocate unboundedly.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = syntra::binary::decode(data);
});
