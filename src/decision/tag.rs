//! The tag that names a published model.
//!
//! The server publishes a model to local-evaluation SDKs as a `decide`
//! section (see [`super::DecisionSpec::decide_json`]) plus a learner
//! snapshot, and names the pair with [`model_tag`]. An SDK recomputes the
//! tag from what it received before deciding with it, and every decision
//! it uploads carries the tag so the server can replay it against exactly
//! that model. The function lives in the decision core so that every SDK
//! build (native, Python, WebAssembly) computes it with the same code.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Tag for a (decide section, snapshot) pair: the first 16 hex digits of
/// SHA-256 over the decide section's JSON text, a zero byte, and the
/// snapshot's trailing 32 bytes (a snapshot ends with a SHA-256 of its
/// contents).
///
/// Both sides hash the decide section as JSON text of the same value, so
/// an SDK that ignores fields it does not know still computes the tag the
/// server did. The text is `serde_json`'s: object keys sorted, compact.
/// An SDK parses the served section with `serde_json` before hashing it,
/// so it must use the server's `serde_json` features (number parsing
/// decides whether a float survives the trip).
pub fn model_tag(decide: &Value, snapshot: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(decide.to_string().as_bytes());
    h.update([0u8]);
    h.update(&snapshot[snapshot.len().saturating_sub(32)..]);
    let digest = h.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tag_is_16_hex_digits_and_depends_on_both_parts() {
        let decide = json!({"version": 1, "actions": [{"id": "a"}], "bits": 10});
        let snapshot = [7u8; 84];
        let tag = model_tag(&decide, &snapshot);
        assert_eq!(tag.len(), 16);
        assert!(
            tag.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(tag, model_tag(&decide, &snapshot));

        let mut other = snapshot;
        other[83] = 8;
        assert_ne!(tag, model_tag(&decide, &other));
        // Only the trailing checksum counts.
        let mut body = snapshot;
        body[0] = 9;
        assert_eq!(tag, model_tag(&decide, &body));
        assert_ne!(
            tag,
            model_tag(&json!({"version": 1, "actions": [], "bits": 10}), &snapshot)
        );
        // Short snapshots hash whatever there is.
        assert_eq!(model_tag(&decide, &[]).len(), 16);
    }

    #[test]
    fn key_order_of_the_source_text_does_not_matter() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":3,"y":2},"b":1}"#).unwrap();
        assert_eq!(model_tag(&a, b"s"), model_tag(&b, b"s"));
    }

    /// Pinned (computed independently with Python's hashlib): deployed SDKs
    /// verify tags the server computes, so the output must never change.
    #[test]
    fn golden() {
        let decide: Value = serde_json::from_str(
            r#"{"version":1,"actions":[{"id":"a"},{"id":"b"}],"bits":10,"seed":18446744073709551615}"#,
        )
        .unwrap();
        assert_eq!(
            decide.to_string(),
            r#"{"actions":[{"id":"a"},{"id":"b"}],"bits":10,"seed":18446744073709551615,"version":1}"#
        );
        let snapshot: Vec<u8> = (0u8..84).collect();
        assert_eq!(model_tag(&decide, &snapshot), "5b441bd2fa0e248f");
    }
}
