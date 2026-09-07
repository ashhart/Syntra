//! Fuzz hierarchical capsule spec parsing + validation on arbitrary JSON.
//! `hierarchical_spec.json` is a tenant-supplied capsule sidecar.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    if let Ok(spec) = syntra::hierarchical::HierarchicalSpec::from_json(&v) {
        let _ = spec.validate();
    }
});
