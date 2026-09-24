//! The Syntra decision core compiled to WebAssembly, for the TypeScript
//! SDK's `LocalDecider`.
//!
//! `src/decision/` at the repository root is compiled into this crate
//! unchanged, so a decision made here is the decision the server computes
//! when it replays the upload. This file only adds the boundary:
//!
//! - [`Model`] is built from the raw text of `GET .../model?snapshot=true`.
//!   Parsing it here rather than in JavaScript keeps u64 fields (a fixed
//!   seed) exact and verifies the tag over the same `serde_json` value the
//!   server hashed, exactly as the Rust client does.
//! - [`Model::decide`] takes the JSON text of an upload item's `input`,
//!   parsed with the server's rules, and returns the decision as JSON text.
//! - [`fixed_seed`] draws the seeds of capsules whose spec fixes one.
//!
//! u64 values cross the boundary as BigInt, or as decimal strings inside
//! JSON. Errors become JavaScript `Error`s with the message the Rust
//! client would give.

#[path = "../../../../src/decision/mod.rs"]
#[allow(dead_code, unused_imports)]
mod decision;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wasm_bindgen::prelude::*;

use decision::spec::RewardAggregation;
use decision::{ActionSpec, DecideInput, DecisionSpec, Engine, SplitMix64, model_tag};

/// A model published for local evaluation, restored and ready to decide.
#[wasm_bindgen]
pub struct Model {
    engine: Engine,
    tag: String,
    version: u64,
}

impl Model {
    /// Parse and verify the text of `GET .../model?snapshot=true`: the tag
    /// must match the decide section and snapshot, the decide section must
    /// be a version this build understands, and the snapshot must fit it.
    pub fn from_response(text: &str) -> Result<Model, String> {
        let v: Value =
            serde_json::from_str(text).map_err(|e| format!("model: bad response: {e}"))?;
        // Such a capsule decides with features its program derives, which
        // an SDK does not compute, and the server refuses its uploads.
        if v.get("programSha256").is_some_and(|p| !p.is_null()) {
            return Err(
                "model: the capsule has a feature program; local evaluation does not support \
                 feature programs yet"
                    .into(),
            );
        }
        let snapshot = v
            .get("snapshot")
            .and_then(Value::as_str)
            .ok_or("model: no snapshot")?;
        let bytes = base64_decode(snapshot).map_err(|e| format!("model: snapshot: {e}"))?;
        let tag = v
            .get("modelTag")
            .and_then(Value::as_str)
            .ok_or("model: no modelTag")?;
        // The tag covers the decide section as served, so fields this build
        // does not know still count; a newer section version is refused.
        let decide = v.get("decide").unwrap_or(&Value::Null);
        if model_tag(decide, &bytes) != tag {
            return Err("model: snapshot does not match its tag".into());
        }
        let spec = DecisionSpec::from_decide_json(decide).map_err(|e| format!("model: {e}"))?;
        let engine = Engine::restore(spec, &bytes).map_err(|e| format!("model: {e}"))?;
        Ok(Model {
            version: engine.model_version(),
            engine,
            tag: tag.to_string(),
        })
    }

    /// [`Model::decide`] with a Rust error.
    pub fn decide_json(&self, input: &str, seed: u64) -> Result<String, String> {
        let input: Input =
            serde_json::from_str(input).map_err(|e| format!("invalid input: {e}"))?;
        // As the server replays it (src/server/upload.rs).
        let context = match input.context {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(v @ Value::Object(_)) => v,
            Some(_) => return Err("context must be a JSON object".into()),
        };
        let input = DecideInput {
            context,
            derived: Value::Null,
            actions: input.actions,
            excluded: input.excluded_actions,
            eligible: None,
            baseline: input.baseline_action,
        };
        let d = self
            .engine
            .decide(&input, seed)
            .map_err(|e| e.to_string())?;
        let out = Decided {
            chosen_index: d.chosen,
            chosen_id: &d.chosen_action().id,
            probability: d.probability,
            pmf: &d.pmf,
            eligible: &d.eligible,
            ranking: d
                .ranking()
                .into_iter()
                .map(|(i, probability)| Ranked {
                    id: &d.actions[i].id,
                    probability,
                })
                .collect(),
            seed: seed.to_string(),
        };
        serde_json::to_string(&out).map_err(|e| e.to_string())
    }
}

#[wasm_bindgen]
impl Model {
    /// Build a model from the raw response text of
    /// `GET .../model?snapshot=true`. Throws if the response is malformed,
    /// its tag does not match, or its decide section is newer than this
    /// build.
    #[wasm_bindgen(constructor)]
    pub fn new(response: &str) -> Result<Model, JsError> {
        Model::from_response(response).map_err(|e| JsError::new(&e))
    }

    /// The server's tag for this model; uploaded decisions carry it.
    #[wasm_bindgen(getter)]
    pub fn tag(&self) -> String {
        self.tag.clone()
    }

    /// Learner updates applied to this model (a u64).
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The spec's fixed base seed (a u64), when it sets one.
    #[wasm_bindgen(getter)]
    pub fn seed(&self) -> Option<u64> {
        self.engine.spec().seed
    }

    /// True when every reward of a decision counts (`rewards: "sum"`),
    /// false when only the first does.
    #[wasm_bindgen(getter, js_name = rewardsSum)]
    pub fn rewards_sum(&self) -> bool {
        self.engine.spec().rewards == RewardAggregation::Sum
    }

    /// Decide. `input` is the JSON text of an upload item's `input`:
    /// `{"context": {...}, "actions": [...], "excludedActions": [...],
    /// "baselineAction": "..."}`, every field optional, unknown fields
    /// refused. Returns JSON text: `chosenIndex`, `chosenId`,
    /// `probability`, `pmf` and `eligible` (aligned), `ranking` (`{id,
    /// probability}`, most probable first) and `seed` (decimal).
    pub fn decide(&self, input: &str, seed: u64) -> Result<String, JsError> {
        self.decide_json(input, seed).map_err(|e| JsError::new(&e))
    }
}

/// The seed of decision `n` (counting from 0) of a decider whose capsule
/// fixes the base seed `base`, as the Rust client and the server draw it.
#[wasm_bindgen(js_name = fixedSeed)]
pub fn fixed_seed(base: u64, n: u64) -> u64 {
    SplitMix64::new(base ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)).next_u64()
}

/// An upload item's `input`, with the server's field rules.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    #[serde(default)]
    context: Option<Value>,
    #[serde(default)]
    actions: Option<Vec<ActionSpec>>,
    #[serde(default)]
    excluded_actions: Vec<String>,
    #[serde(default)]
    baseline_action: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Decided<'a> {
    chosen_index: usize,
    chosen_id: &'a str,
    probability: f64,
    pmf: &'a [f64],
    eligible: &'a [usize],
    ranking: Vec<Ranked<'a>>,
    /// Decimal: JSON numbers above 2^53 lose precision in JavaScript.
    seed: String,
}

#[derive(Serialize)]
struct Ranked<'a> {
    id: &'a str,
    probability: f64,
}

/// Standard base64 with padding, as the server encodes snapshots.
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return Err("invalid base64 length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let chunks = s.len() / 4;
    for (k, chunk) in s.chunks(4).enumerate() {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && k + 1 != chunks) {
            return Err("invalid base64 padding".into());
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c).ok_or("invalid base64 character")?;
        }
        n <<= 6 * pad as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base64_encode(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A trained engine and the model endpoint's answer for it, built the
    /// way the server builds it (src/server/runtime.rs, query.rs).
    fn published(spec: Value) -> (Engine, String) {
        let full = DecisionSpec::from_json(&spec).unwrap();
        let mut engine = Engine::new(full.clone()).unwrap();
        for i in 0..40 {
            let action = full.actions[i % full.actions.len()].clone();
            let reward = if action.id == "b" { 1.0 } else { 0.2 };
            let tier = ["free", "pro"][i % 2];
            engine
                .learn(
                    &json!({"i": i % 5, "tier": tier}),
                    &Value::Null,
                    &action,
                    reward,
                    0.5,
                )
                .unwrap();
        }
        let decide = full.decide_json();
        let snapshot = engine.snapshot();
        let response = json!({
            "decide": decide,
            "spec": full.to_json(),
            "modelVersion": engine.model_version(),
            "modelTag": model_tag(&decide, &snapshot),
            "snapshotBytes": snapshot.len(),
            "snapshot": base64_encode(&snapshot),
        });
        // The engine SDKs replay against is restored from the published
        // decide section, as `Published::engine` does.
        let replay =
            Engine::restore(DecisionSpec::from_decide_json(&decide).unwrap(), &snapshot).unwrap();
        (replay, response.to_string())
    }

    fn spec() -> Value {
        json!({
            "actions": [
                {"id": "a", "features": {"cost": 0.1}},
                {"id": "b", "features": {"cost": 0.4}},
                {"id": "c", "features": {"cost": 1.0}}
            ],
            "learner": {"bits": 12}
        })
    }

    #[test]
    fn decides_exactly_as_the_engine_it_was_published_from() {
        let (engine, response) = published(spec());
        let model = Model::from_response(&response).unwrap();
        assert_eq!(model.version, 40);
        assert_eq!(model.engine.spec().seed, None);
        for seed in 0..200u64 {
            let tier = ["free", "pro"][(seed % 2) as usize];
            let context = json!({"i": seed % 5, "tier": tier});
            let input = json!({"context": context, "excludedActions": if seed % 7 == 0 { json!(["c"]) } else { json!([]) }});
            let out = model.decide_json(&input.to_string(), seed).unwrap();
            let want = engine
                .decide(
                    &DecideInput {
                        context,
                        excluded: if seed % 7 == 0 {
                            vec!["c".into()]
                        } else {
                            vec![]
                        },
                        ..DecideInput::default()
                    },
                    seed,
                )
                .unwrap();
            // Compared as text: serde_json's default float parsing is not
            // exact, its printing is (JavaScript's JSON.parse is exact too).
            let ranking: Vec<Value> = want
                .ranking()
                .into_iter()
                .map(|(i, p)| json!({"id": want.actions[i].id, "probability": p}))
                .collect();
            let expected = format!(
                r#"{{"chosenIndex":{},"chosenId":{},"probability":{},"pmf":{},"eligible":{},"ranking":{},"seed":"{seed}"}}"#,
                want.chosen,
                text(&want.chosen_action().id),
                text(&want.probability),
                text(&want.pmf),
                text(&want.eligible),
                text(&ranking),
            );
            assert_eq!(out, expected);
        }
    }

    fn text<T: Serialize + ?Sized>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn per_request_actions_and_baseline() {
        let mut s = spec();
        s["mode"] = json!("baselineExplore");
        let (_, response) = published(s);
        let model = Model::from_response(&response).unwrap();
        let err = model.decide_json("{}", 1).unwrap_err();
        assert_eq!(err, "baselineAction is required in baselineExplore mode");
        let input = json!({"actions": [{"id": "x"}, {"id": "y", "features": {"k": 2}}], "baselineAction": "y"});
        let out: Value =
            serde_json::from_str(&model.decide_json(&input.to_string(), 5).unwrap()).unwrap();
        assert_eq!(out["eligible"], json!([0, 1]));
        assert_eq!(out["ranking"][0]["id"], json!("y"));
    }

    #[test]
    fn inputs_follow_the_server_rules() {
        let (_, response) = published(spec());
        let model = Model::from_response(&response).unwrap();
        for ok in [
            "{}",
            r#"{"context": null}"#,
            r#"{"context": {"a": [1, "x", {"b": true}]}}"#,
        ] {
            assert!(model.decide_json(ok, 1).is_ok(), "{ok}");
        }
        let err = |input: &str| model.decide_json(input, 1).unwrap_err();
        assert!(err(r#"{"context": {}, "eventId": "x"}"#).contains("unknown field `eventId`"));
        assert_eq!(err(r#"{"context": [1]}"#), "context must be a JSON object");
        assert!(err("[").starts_with("invalid input: "));
        assert_eq!(
            err(r#"{"excludedActions": ["zzz"]}"#),
            "excludedActions names unknown action \"zzz\""
        );
        assert_eq!(
            err(r#"{"excludedActions": ["a", "b", "c"]}"#),
            "no eligible actions remain after the eligible filter and exclusions"
        );
        assert!(err(r#"{"context": {"x": 1e39}}"#).contains("not a finite 32-bit number"));
    }

    #[test]
    fn a_fixed_seed_survives_as_a_u64() {
        let mut s = spec();
        s["seed"] = json!(u64::MAX);
        s["rewards"] = json!("sum");
        let (_, response) = published(s);
        assert!(response.contains("18446744073709551615"));
        let model = Model::from_response(&response).unwrap();
        assert_eq!(model.engine.spec().seed, Some(u64::MAX));
        assert!(model.rewards_sum());
    }

    #[test]
    fn fixed_seeds_match_the_reference_splitmix64() {
        // Vigna's splitmix64.c from state 1234567 (n = 0 leaves the base).
        assert_eq!(fixed_seed(1_234_567, 0), 6_457_827_717_110_365_317);
        assert_eq!(fixed_seed(0, 0), 0xE220_A839_7B1D_CDAF);
        let base = 42u64;
        for n in [1u64, 2, 1000, u64::MAX] {
            let mut rng = SplitMix64::new(base ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            assert_eq!(fixed_seed(base, n), rng.next_u64());
        }
    }

    #[test]
    fn bad_models_are_refused() {
        let (_, response) = published(spec());
        let good: Value = serde_json::from_str(&response).unwrap();
        let refuse = |v: &Value| Model::from_response(&v.to_string()).err().unwrap();

        let mut v = good.clone();
        v["modelTag"] = json!("0000000000000000");
        assert_eq!(refuse(&v), "model: snapshot does not match its tag");

        let mut v = good.clone();
        v["decide"]["exploration"]["floor"] = json!(0.5);
        assert_eq!(refuse(&v), "model: snapshot does not match its tag");

        let mut v = good.clone();
        v.as_object_mut().unwrap().remove("snapshot");
        assert_eq!(refuse(&v), "model: no snapshot");

        let mut v = good.clone();
        v["snapshot"] = json!("abc");
        assert_eq!(refuse(&v), "model: snapshot: invalid base64 length");

        let mut v = good.clone();
        v.as_object_mut().unwrap().remove("modelTag");
        assert_eq!(refuse(&v), "model: no modelTag");

        // A newer decide section, correctly tagged, is still refused.
        let mut v = good.clone();
        v["decide"]["version"] = json!(99);
        let snapshot = base64_decode(v["snapshot"].as_str().unwrap()).unwrap();
        v["modelTag"] = json!(model_tag(&v["decide"], &snapshot));
        assert!(refuse(&v).contains("upgrade the SDK"), "{}", refuse(&v));

        // A snapshot for other hash bits, correctly tagged.
        let mut v = good.clone();
        v["decide"]["bits"] = json!(14);
        v["modelTag"] = json!(model_tag(&v["decide"], &snapshot));
        assert!(refuse(&v).contains("learner.bits = 12"), "{}", refuse(&v));

        assert!(
            Model::from_response("not json")
                .err()
                .unwrap()
                .starts_with("model: bad response")
        );

        let mut v = good.clone();
        v["programSha256"] = json!("9f2c");
        assert!(refuse(&v).contains("feature program"), "{}", refuse(&v));
        v["programSha256"] = Value::Null;
        assert!(Model::from_response(&v.to_string()).is_ok());
    }

    #[test]
    fn base64_matches_the_encoder_and_is_strict() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        for bad in ["abc", "a===", "ab!=", "ab==abcd", "===="] {
            assert!(base64_decode(bad).is_err(), "{bad}");
        }
    }
}
