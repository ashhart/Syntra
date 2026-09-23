//! Fuzz model-snapshot decoding. SDKs decode snapshots the server sends;
//! the server decodes the ones it stored. Arbitrary bytes are given a
//! valid checksum so the fuzzer reaches the parser behind it.

#![no_main]
use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};
use syntra::decision::{ActionSpec, DecisionSpec, Engine, LinearModel};

fuzz_target!(|data: &[u8]| {
    let _ = LinearModel::from_bytes(data);
    let mut sealed = data.to_vec();
    sealed.extend_from_slice(&Sha256::digest(data));
    let Ok(model) = LinearModel::from_bytes(&sealed) else {
        return;
    };
    // A decoded model round-trips and can serve decisions.
    assert!(LinearModel::from_bytes(&model.to_bytes()).is_ok());
    let spec = DecisionSpec {
        actions: vec![ActionSpec::new("a"), ActionSpec::new("b")],
        learner: syntra::decision::spec::LearnerSpec {
            bits: model.bits(),
            ..Default::default()
        },
        ..DecisionSpec::default()
    };
    if let Ok(engine) = Engine::restore(spec, &sealed) {
        let _ = engine.decide(&Default::default(), 7);
    }
});
