//! Fuzz learning-config parsing and reward computation on arbitrary JSON.
//! `learning.json` and reward specs are tenant-supplied capsule artifacts.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let _config = syntra::learning::LearningConfig::from_json(&v);
    let _reward = syntra::learning::compute_reward_from_components(
        &v,
        &serde_json::json!({"quality": 0.5, "latency_ms": 100, "cost_usd": 0.01}),
    );
});
