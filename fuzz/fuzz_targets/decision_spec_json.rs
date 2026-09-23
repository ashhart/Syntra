//! Fuzz decision specs, merge patches and decide inputs on arbitrary JSON.
//! Specs, spec patches, contexts and per-request actions are all
//! caller-supplied.

#![no_main]
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use syntra::decision::{ActionSpec, DecideInput, DecisionSpec, Engine};

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    let _ = DecisionSpec::from_json(&v);
    let base = DecisionSpec {
        actions: vec![ActionSpec::new("a"), ActionSpec::new("b")],
        ..DecisionSpec::default()
    };
    let Ok(mut spec) = base.merge_patch(&v) else {
        return;
    };
    // Keep each iteration's weight array small.
    spec.learner.bits = spec.learner.bits.min(14);
    let Ok(mut engine) = Engine::new(spec) else {
        return;
    };
    let input = DecideInput {
        context: v.get("context").cloned().unwrap_or(Value::Null),
        derived: Value::Null,
        actions: v
            .get("requestActions")
            .and_then(|a| serde_json::from_value(a.clone()).ok()),
        excluded: Vec::new(),
        eligible: None,
        baseline: v.get("baseline").and_then(|b| b.as_str()).map(String::from),
    };
    let Ok(d) = engine.decide(&input, 42) else {
        return;
    };
    let total: f64 = d.pmf.iter().sum();
    assert!((total - 1.0).abs() < 1e-9, "pmf sums to {total}");
    assert!(d.probability > 0.0 && d.probability <= 1.0);
    assert!(d.eligible.contains(&d.chosen));
    // The same draw replays exactly.
    assert_eq!(engine.decide(&input, 42).unwrap().chosen, d.chosen);
    // Learning from any finite reward keeps the model usable.
    let reward = v.get("reward").and_then(Value::as_f64).unwrap_or(1.0);
    let action = d.chosen_action().clone();
    if engine
        .learn(&input.context, &Value::Null, &action, reward, d.probability)
        .is_ok()
    {
        let _ = engine.decide(&input, 43);
    }
});
