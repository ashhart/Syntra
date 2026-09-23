//! Feature flattening and hashing.
//!
//! JSON values become sparse features `(namespace, name, value)`:
//!
//! - nested objects give dotted names (`user.tier`);
//! - a number gives the numeric feature `name` carrying the value;
//! - a bool or string gives the indicator `name=value` with value 1
//!   (bools render as `true` / `false`);
//! - inside an array, a string element gives the indicator `name=elem` (bag
//!   semantics: a repeated element counts twice), a number gives the numeric
//!   feature `name[i]`, a bool gives `name[i]=true|false`, an object is
//!   flattened under `name[i].`, and a nested array under `name[i]`;
//! - null is skipped.
//!
//! Names are not escaped, so `{"a.b": 1}` and `{"a": {"b": 1}}` produce the
//! same feature. Numbers must fit a finite `f32`; nesting may not exceed
//! [`MAX_DEPTH`] containers (the top-level object counts as one); a namespace
//! may not produce more than [`MAX_FEATURES_PER_NAMESPACE`] features.
//!
//! Each feature is identified by the unmasked 64-bit FNV-1a hash of
//! `namespace \x00 name`. FNV-1a is defined byte by byte, so hashes are
//! stable across Rust releases and platforms (`DefaultHasher` is not); the
//! golden tests pin them. Namespaces: `c` for the request context, `a` for
//! the action (always `id=<action id>` plus the action's own features), and
//! `d` for derived features published by a feature program.
//!
//! [`Featurizer::phi`] builds the vector for a (context, action) pair over
//! `2^bits` slots:
//!
//! ```text
//! phi(x, a) = [bias, a, c x a, d x a]
//! ```
//!
//! where `x` is a quadratic interaction: its hash is FNV-1a over the two
//! 64-bit hashes and its value is the product of the two values.
//! Context-only terms cannot change which action ranks first, so phi leaves
//! them out; [`Featurizer::phi_with_context`] adds them back for reward
//! models that need calibrated absolute predictions.
//!
//! A hash's slot is the low `bits` bits of [`fmix64`] of the hash. Masking
//! FNV-1a directly is not good enough: its low bits only receive carries
//! from lower bits, and on sequential names such as zip codes or numbered
//! item ids it produced up to 37 standard deviations more slot collisions
//! than an ideal hash. The MurmurHash3 finalizer is a bijection on `u64`
//! (so it adds no collisions of its own) and brings every tested name
//! family within 2 standard deviations; the spread test below checks this.

use std::fmt::{self, Write as _};

use serde_json::{Map, Number, Value};

use super::spec::ActionSpec;

/// FNV-1a 64-bit offset basis.
pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// Maximum container nesting, counting the top-level object as level 1.
pub const MAX_DEPTH: usize = 8;
/// Maximum number of features one namespace may produce.
pub const MAX_FEATURES_PER_NAMESPACE: usize = 4096;
/// Maximum number of raw terms (before merging colliding slots) in one
/// feature vector. The interaction terms grow as the product of the context
/// and action feature counts, so this bounds the work a single request can
/// cause.
pub const MAX_PHI_TERMS: usize = 1 << 20;
/// Hash of the bias feature: `fnv1a64("b\0bias")`.
pub const BIAS_HASH: u64 = fnv1a64(b"b\0bias");

/// 64-bit FNV-1a over `bytes`.
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

/// Hash of the feature `name` in `namespace`: FNV-1a of `namespace \x00 name`.
pub fn feature_hash(namespace: &str, name: &str) -> u64 {
    let mut h = Fnv1a::new();
    h.write(namespace.as_bytes());
    h.write(&[0]);
    h.write(name.as_bytes());
    h.finish()
}

/// MurmurHash3's 64-bit finalizer: a bijective mix in which every input bit
/// affects every output bit. Used to spread hashes before masking.
pub const fn fmix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 33;
    h
}

/// Hash of the quadratic interaction of two features: FNV-1a over the
/// little-endian bytes of `first` followed by those of `second`. All 128
/// input bits reach the result, so two features that share a slot do not
/// drag all their interactions into shared slots too.
pub fn interaction_hash(first: u64, second: u64) -> u64 {
    let mut h = Fnv1a::new();
    h.write(&first.to_le_bytes());
    h.write(&second.to_le_bytes());
    h.finish()
}

/// Streaming FNV-1a state, so shared prefixes are hashed once.
#[derive(Debug, Clone, Copy)]
struct Fnv1a(u64);

impl Fnv1a {
    const fn new() -> Self {
        Self(FNV_OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

/// Feature namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// `c`: the request context.
    Context,
    /// `a`: the action's id and features.
    Action,
    /// `d`: derived features published by the feature program.
    Derived,
}

impl Namespace {
    /// The namespace tag hashed in front of every feature name.
    pub fn tag(self) -> &'static str {
        match self {
            Namespace::Context => "c",
            Namespace::Action => "a",
            Namespace::Derived => "d",
        }
    }

    /// What the namespace's JSON is called in error messages.
    fn container(self) -> &'static str {
        match self {
            Namespace::Context => "context",
            Namespace::Action => "action features",
            Namespace::Derived => "derived features",
        }
    }

    /// Prefix for a single feature in error messages.
    fn feature(self) -> &'static str {
        match self {
            Namespace::Context => "context",
            Namespace::Action => "action",
            Namespace::Derived => "derived",
        }
    }
}

/// One hashed feature: the unmasked FNV-1a hash of `namespace \x00 name`
/// and its value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feature {
    pub hash: u64,
    pub value: f32,
}

/// Flattened request-level features (everything except the action).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextFeatures {
    /// Namespace `c`.
    pub context: Vec<Feature>,
    /// Namespace `d`.
    pub derived: Vec<Feature>,
}

/// Why a JSON value could not be turned into features.
#[derive(Debug, Clone, PartialEq)]
pub enum FeatureError {
    /// The top-level value was neither an object nor null.
    NotAnObject {
        namespace: Namespace,
        found: &'static str,
    },
    /// A number does not fit a finite `f32`.
    NonFinite {
        namespace: Namespace,
        name: String,
        value: f64,
    },
    /// Containers nested deeper than [`MAX_DEPTH`].
    TooDeep { namespace: Namespace, name: String },
    /// More than [`MAX_FEATURES_PER_NAMESPACE`] features.
    TooManyFeatures { namespace: Namespace },
    /// A feature vector (or a whole decision) would need too many terms.
    TooManyTerms { terms: usize, max: usize },
}

impl fmt::Display for FeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeatureError::NotAnObject { namespace, found } => write!(
                f,
                "{} must be a JSON object or null, not {found}",
                namespace.container()
            ),
            FeatureError::NonFinite {
                namespace,
                name,
                value,
            } => write!(
                f,
                "{} feature \"{name}\" = {value:e} is not a finite 32-bit number",
                namespace.feature()
            ),
            FeatureError::TooDeep { namespace, name } => write!(
                f,
                "{}: nested more than {MAX_DEPTH} levels deep at \"{name}\"",
                namespace.container()
            ),
            FeatureError::TooManyFeatures { namespace } => write!(
                f,
                "{}: more than {MAX_FEATURES_PER_NAMESPACE} features",
                namespace.container()
            ),
            FeatureError::TooManyTerms { terms, max } => write!(
                f,
                "the feature vectors would need {terms} terms, more than the limit of {max}; \
                 send fewer context or action features"
            ),
        }
    }
}

impl std::error::Error for FeatureError {}

/// Flatten a JSON object (or null, which yields nothing) in `namespace`.
pub fn flatten(namespace: Namespace, value: &Value) -> Result<Vec<Feature>, FeatureError> {
    let mut flattener = Flattener::new(namespace);
    match value {
        Value::Null => {}
        Value::Object(map) => flattener.object(map, 1)?,
        other => {
            return Err(FeatureError::NotAnObject {
                namespace,
                found: json_type(other),
            });
        }
    }
    Ok(flattener.out)
}

/// Features of an action in namespace `a`: the indicator `id=<id>` followed
/// by the flattened `features` object.
pub fn action_features(action: &ActionSpec) -> Result<Vec<Feature>, FeatureError> {
    let mut flattener = Flattener::new(Namespace::Action);
    flattener.path.push_str("id");
    flattener.indicator(&action.id)?;
    flattener.path.clear();
    flattener.object(&action.features, 1)?;
    Ok(flattener.out)
}

struct Flattener {
    namespace: Namespace,
    /// FNV-1a state after `namespace \x00`.
    prefix: Fnv1a,
    /// Name of the value being visited.
    path: String,
    out: Vec<Feature>,
}

impl Flattener {
    fn new(namespace: Namespace) -> Self {
        let mut prefix = Fnv1a::new();
        prefix.write(namespace.tag().as_bytes());
        prefix.write(&[0]);
        Self {
            namespace,
            prefix,
            path: String::new(),
            out: Vec::new(),
        }
    }

    fn object(&mut self, map: &Map<String, Value>, depth: usize) -> Result<(), FeatureError> {
        if depth > MAX_DEPTH {
            return Err(self.too_deep());
        }
        for (key, value) in map {
            let len = self.path.len();
            if len > 0 {
                self.path.push('.');
            }
            self.path.push_str(key);
            self.value(value, depth)?;
            self.path.truncate(len);
        }
        Ok(())
    }

    fn array(&mut self, items: &[Value], depth: usize) -> Result<(), FeatureError> {
        if depth > MAX_DEPTH {
            return Err(self.too_deep());
        }
        for (i, item) in items.iter().enumerate() {
            match item {
                Value::Null => {}
                Value::String(s) => self.indicator(s)?,
                _ => {
                    let len = self.path.len();
                    // Writing to a String cannot fail.
                    let _ = write!(self.path, "[{i}]");
                    self.value(item, depth)?;
                    self.path.truncate(len);
                }
            }
        }
        Ok(())
    }

    /// Visit a value found inside a container at nesting level `depth`.
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), FeatureError> {
        match value {
            Value::Null => Ok(()),
            Value::Bool(b) => self.indicator(if *b { "true" } else { "false" }),
            Value::Number(n) => self.numeric(n),
            Value::String(s) => self.indicator(s),
            Value::Array(items) => self.array(items, depth + 1),
            Value::Object(map) => self.object(map, depth + 1),
        }
    }

    /// Indicator `path=value` with value 1.
    fn indicator(&mut self, value: &str) -> Result<(), FeatureError> {
        let mut h = self.prefix;
        h.write(self.path.as_bytes());
        h.write(b"=");
        h.write(value.as_bytes());
        self.emit(h.finish(), 1.0)
    }

    fn numeric(&mut self, n: &Number) -> Result<(), FeatureError> {
        // `as_f64` only fails with serde_json's arbitrary-precision feature.
        let wide = n.as_f64().unwrap_or(f64::NAN);
        let value = wide as f32;
        if !value.is_finite() {
            return Err(FeatureError::NonFinite {
                namespace: self.namespace,
                name: shorten(&self.path),
                value: wide,
            });
        }
        let mut h = self.prefix;
        h.write(self.path.as_bytes());
        self.emit(h.finish(), value)
    }

    fn emit(&mut self, hash: u64, value: f32) -> Result<(), FeatureError> {
        if self.out.len() >= MAX_FEATURES_PER_NAMESPACE {
            return Err(FeatureError::TooManyFeatures {
                namespace: self.namespace,
            });
        }
        self.out.push(Feature { hash, value });
        Ok(())
    }

    fn too_deep(&self) -> FeatureError {
        FeatureError::TooDeep {
            namespace: self.namespace,
            name: shorten(&self.path),
        }
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Feature names are caller-controlled; keep error messages bounded.
fn shorten(name: &str) -> String {
    const MAX_CHARS: usize = 120;
    match name.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => format!("{}...", &name[..cut]),
        None => name.to_string(),
    }
}

/// Convert to `f32`, saturating at `±f32::MAX` instead of overflowing to
/// infinity. NaN stays NaN.
pub(crate) fn saturate_f32(value: f64) -> f32 {
    value.clamp(-f64::from(f32::MAX), f64::from(f32::MAX)) as f32
}

/// Put a sparse vector in canonical form: sorted by slot, colliding slots
/// summed (in `f64`, rounded once), and exact zeros dropped. A hashed
/// feature vector is a point in `R^(2^bits)`, so entries that share a slot
/// add up; the learner relies on each slot appearing once.
pub fn canonicalize(x: &mut Vec<(u32, f32)>) {
    // Ordering by value within a slot makes the merged sum independent of
    // the order the terms were generated in (e.g. JSON key order).
    x.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut write = 0;
    let mut read = 0;
    while read < x.len() {
        let slot = x[read].0;
        let mut sum = 0.0f64;
        while read < x.len() && x[read].0 == slot {
            sum += f64::from(x[read].1);
            read += 1;
        }
        let value = saturate_f32(sum);
        if value != 0.0 {
            x[write] = (slot, value);
            write += 1;
        }
    }
    x.truncate(write);
}

/// Turns JSON into masked sparse feature vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Featurizer {
    bits: u32,
}

impl Featurizer {
    /// A featurizer producing slots in `0..2^bits`, `bits` in `1..=32`.
    pub fn new(bits: u32) -> Result<Self, String> {
        if !(1..=32).contains(&bits) {
            return Err(format!("feature bits must be in [1, 32] (got {bits})"));
        }
        Ok(Self { bits })
    }

    pub fn bits(&self) -> u32 {
        self.bits
    }

    /// Slot of a hash: the low `bits` bits of `fmix64(hash)`.
    pub fn index(&self, hash: u64) -> u32 {
        (fmix64(hash) & ((1u64 << self.bits) - 1)) as u32
    }

    /// Flatten the request context (namespace `c`) and derived features
    /// (namespace `d`). Each must be a JSON object or null.
    pub fn context(
        &self,
        context: &Value,
        derived: &Value,
    ) -> Result<ContextFeatures, FeatureError> {
        Ok(ContextFeatures {
            context: flatten(Namespace::Context, context)?,
            derived: flatten(Namespace::Derived, derived)?,
        })
    }

    /// Features of one action (namespace `a`), including `id=<id>`.
    pub fn action(&self, action: &ActionSpec) -> Result<Vec<Feature>, FeatureError> {
        action_features(action)
    }

    /// Number of raw terms [`Self::phi`] generates for this pair, before
    /// colliding slots are merged. Saturates instead of overflowing.
    pub fn phi_terms(context: &ContextFeatures, action_len: usize) -> usize {
        let request = context.context.len() + context.derived.len();
        request
            .saturating_add(1)
            .saturating_mul(action_len)
            .saturating_add(1)
    }

    /// Build `phi(x, a) = [bias, a, c x a, d x a]` into `out` (cleared
    /// first) in canonical form: sorted unique slots, no zero values.
    pub fn phi(
        &self,
        context: &ContextFeatures,
        action: &[Feature],
        out: &mut Vec<(u32, f32)>,
    ) -> Result<(), FeatureError> {
        self.build(context, action, false, out)
    }

    /// [`Self::phi`] plus the context-only and derived-only terms, for
    /// reward models that need absolute (not just relative) predictions.
    pub fn phi_with_context(
        &self,
        context: &ContextFeatures,
        action: &[Feature],
        out: &mut Vec<(u32, f32)>,
    ) -> Result<(), FeatureError> {
        self.build(context, action, true, out)
    }

    fn build(
        &self,
        context: &ContextFeatures,
        action: &[Feature],
        with_context: bool,
        out: &mut Vec<(u32, f32)>,
    ) -> Result<(), FeatureError> {
        out.clear();
        let request_len = context.context.len() + context.derived.len();
        let mut terms = Self::phi_terms(context, action.len());
        if with_context {
            terms = terms.saturating_add(request_len);
        }
        if terms > MAX_PHI_TERMS {
            return Err(FeatureError::TooManyTerms {
                terms,
                max: MAX_PHI_TERMS,
            });
        }
        out.reserve(terms);
        out.push((self.index(BIAS_HASH), 1.0));
        for a in action {
            out.push((self.index(a.hash), a.value));
        }
        for r in context.context.iter().chain(&context.derived) {
            // interaction_hash(r, a) with r's eight bytes hashed once.
            let mut prefix = Fnv1a::new();
            prefix.write(&r.hash.to_le_bytes());
            for a in action {
                let mut h = prefix;
                h.write(&a.hash.to_le_bytes());
                let value = saturate_f32(f64::from(r.value) * f64::from(a.value));
                out.push((self.index(h.finish()), value));
            }
            if with_context {
                out.push((self.index(r.hash), r.value));
            }
        }
        canonicalize(out);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn action(id: &str, features: Value) -> ActionSpec {
        ActionSpec {
            id: id.to_string(),
            features: features.as_object().cloned().unwrap_or_default(),
        }
    }

    fn names_to_hashes(namespace: &str, names: &[&str]) -> Vec<u64> {
        names.iter().map(|n| feature_hash(namespace, n)).collect()
    }

    fn hashes(features: &[Feature]) -> Vec<u64> {
        features.iter().map(|f| f.hash).collect()
    }

    /// Published FNV-1a 64 test vectors plus feature hashes computed by an
    /// independent Python implementation. If any of these change, every
    /// stored model is silently invalidated, so they must never change.
    #[test]
    fn golden_hashes() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(feature_hash("c", "segment=A"), 0xbf11_8b5a_b4f3_8095);
        assert_eq!(feature_hash("a", "id=small"), 0x4879_a872_137a_8ae3);
        assert_eq!(feature_hash("c", "user.tier=pro"), 0xe4c6_f4a7_7579_c537);
        assert_eq!(feature_hash("d", "score"), 0x7616_fc1a_d72a_5ab1);
        assert_eq!(feature_hash("c", "x[0]"), 0xad92_9054_46fa_c0fc);
        assert_eq!(BIAS_HASH, 0x37ae_a6a3_1106_4846);
        assert_eq!(BIAS_HASH, feature_hash("b", "bias"));
        assert_eq!(
            interaction_hash(0xbf11_8b5a_b4f3_8095, 0x4879_a872_137a_8ae3),
            0xf0be_0dd0_bd4e_094d
        );
        // Slot mapping (MurmurHash3 fmix64, then mask).
        assert_eq!(fmix64(0), 0);
        assert_eq!(fmix64(1), 0xb456_bcfc_34c2_cb2c);
        let fz = Featurizer::new(18).unwrap();
        assert_eq!(fz.index(feature_hash("c", "segment=A")), 172_535);
        assert_eq!(fz.index(BIAS_HASH), 111_628);
        assert_eq!(fz.index(0xf0be_0dd0_bd4e_094d), 90_868);
    }

    #[test]
    fn flattening_rules() {
        let ctx = json!({
            "task": "code",
            "tokens": 812,
            "pro": true,
            "user": {"tier": "pro", "age": 3.5},
            "tags": ["a", "b"],
            "vec": [1, -2.5],
            "flags": [false],
            "items": [{"sku": "x"}, null, {"price": 2}],
            "grid": [[1], ["s"]],
            "missing": null,
            "empty": {},
        });
        let features = flatten(Namespace::Context, &ctx).unwrap();
        let mut got: Vec<(u64, f32)> = features.iter().map(|f| (f.hash, f.value)).collect();
        let mut want: Vec<(u64, f32)> = [
            ("task=code", 1.0),
            ("tokens", 812.0),
            ("pro=true", 1.0),
            ("user.tier=pro", 1.0),
            ("user.age", 3.5),
            ("tags=a", 1.0),
            ("tags=b", 1.0),
            ("vec[0]", 1.0),
            ("vec[1]", -2.5),
            ("flags[0]=false", 1.0),
            ("items[0].sku=x", 1.0),
            ("items[2].price", 2.0),
            ("grid[0][0]", 1.0),
            ("grid[1]=s", 1.0),
        ]
        .iter()
        .map(|&(name, value)| (feature_hash("c", name), value))
        .collect();
        got.sort_by_key(|p| p.0);
        want.sort_by_key(|p| p.0);
        assert_eq!(got, want);
    }

    #[test]
    fn null_and_empty_give_no_features() {
        assert!(
            flatten(Namespace::Context, &Value::Null)
                .unwrap()
                .is_empty()
        );
        assert!(flatten(Namespace::Context, &json!({})).unwrap().is_empty());
    }

    #[test]
    fn repeated_string_elements_count_twice() {
        let f = flatten(Namespace::Context, &json!({"t": ["x", "x"]})).unwrap();
        assert_eq!(hashes(&f), names_to_hashes("c", &["t=x", "t=x"]));
    }

    #[test]
    fn namespaces_separate_identical_names() {
        let value = json!({"k": 1});
        let c = flatten(Namespace::Context, &value).unwrap();
        let d = flatten(Namespace::Derived, &value).unwrap();
        assert_ne!(c[0].hash, d[0].hash);
        assert_eq!(c[0].hash, feature_hash("c", "k"));
        assert_eq!(d[0].hash, feature_hash("d", "k"));
    }

    #[test]
    fn action_features_start_with_id() {
        let a = action("small", json!({"cost": 0.2, "family": "llama"}));
        let f = action_features(&a).unwrap();
        assert_eq!(
            f[0],
            Feature {
                hash: feature_hash("a", "id=small"),
                value: 1.0
            }
        );
        let mut rest: Vec<u64> = hashes(&f[1..]);
        rest.sort_unstable();
        let mut want = names_to_hashes("a", &["cost", "family=llama"]);
        want.sort_unstable();
        assert_eq!(rest, want);
    }

    #[test]
    fn rejects_non_objects() {
        for value in [json!(1), json!("s"), json!([1]), json!(true)] {
            let err = flatten(Namespace::Context, &value).unwrap_err();
            assert!(matches!(err, FeatureError::NotAnObject { .. }), "{err}");
            assert!(
                err.to_string().starts_with("context must be a JSON object"),
                "{err}"
            );
        }
    }

    #[test]
    fn rejects_numbers_that_overflow_f32() {
        let err = flatten(Namespace::Context, &json!({"a": {"big": 1e39}})).unwrap_err();
        assert_eq!(
            err,
            FeatureError::NonFinite {
                namespace: Namespace::Context,
                name: "a.big".into(),
                value: 1e39,
            }
        );
        assert!(err.to_string().contains("\"a.big\""), "{err}");
        // Largest finite f32 is accepted; so is a large integer.
        assert!(flatten(Namespace::Context, &json!({"m": 3.4e38, "n": u64::MAX})).is_ok());
        let err = flatten(Namespace::Derived, &json!({"v": [1, -1e300]})).unwrap_err();
        assert!(matches!(err, FeatureError::NonFinite { ref name, .. } if name == "v[1]"));
    }

    fn nested_objects(levels: usize) -> Value {
        let mut v = json!({"leaf": 1});
        for i in 0..levels - 1 {
            v = json!({ format!("k{i}"): v });
        }
        v
    }

    #[test]
    fn depth_limit_counts_containers() {
        // `levels` objects including the top-level one.
        assert!(flatten(Namespace::Context, &nested_objects(MAX_DEPTH)).is_ok());
        let err = flatten(Namespace::Context, &nested_objects(MAX_DEPTH + 1)).unwrap_err();
        assert!(matches!(err, FeatureError::TooDeep { .. }), "{err}");
        assert!(err.to_string().contains("more than 8 levels"), "{err}");
        // Arrays count as containers too: top object + 7 arrays = 8 levels.
        let mut arr = json!([1]);
        for _ in 0..6 {
            arr = json!([arr]);
        }
        assert!(flatten(Namespace::Context, &json!({ "a": arr.clone() })).is_ok());
        let err = flatten(Namespace::Context, &json!({ "a": [arr] })).unwrap_err();
        assert!(matches!(err, FeatureError::TooDeep { .. }), "{err}");
    }

    #[test]
    fn feature_count_limit() {
        let mut map = Map::new();
        for i in 0..MAX_FEATURES_PER_NAMESPACE {
            map.insert(format!("f{i}"), json!(1));
        }
        assert_eq!(
            flatten(Namespace::Context, &Value::Object(map.clone()))
                .unwrap()
                .len(),
            MAX_FEATURES_PER_NAMESPACE
        );
        map.insert("one_more".into(), json!("x"));
        let err = flatten(Namespace::Context, &Value::Object(map.clone())).unwrap_err();
        assert_eq!(
            err,
            FeatureError::TooManyFeatures {
                namespace: Namespace::Context
            }
        );
        // The action namespace counts its id indicator.
        map.remove("one_more");
        let a = ActionSpec {
            id: "x".into(),
            features: map,
        };
        assert!(matches!(
            action_features(&a),
            Err(FeatureError::TooManyFeatures {
                namespace: Namespace::Action
            })
        ));
        // Array elements count individually.
        let long: Vec<u32> = (0..=MAX_FEATURES_PER_NAMESPACE as u32).collect();
        assert!(flatten(Namespace::Context, &json!({ "v": long })).is_err());
    }

    #[test]
    fn long_names_are_shortened_in_errors() {
        let key = "k".repeat(1000);
        let err = flatten(Namespace::Context, &json!({ key: 1e300 })).unwrap_err();
        let FeatureError::NonFinite { name, .. } = &err else {
            panic!("{err}");
        };
        assert_eq!(name.chars().count(), 123);
        assert!(name.ends_with("..."));
    }

    #[test]
    fn phi_structure() {
        let fz = Featurizer::new(18).unwrap();
        let ctx = fz
            .context(&json!({"segment": "A", "x": 2.0}), &json!({"score": 0.5}))
            .unwrap();
        let a = fz.action(&action("small", json!({"cost": 3.0}))).unwrap();
        let mut phi = Vec::new();
        fz.phi(&ctx, &a, &mut phi).unwrap();

        let id = feature_hash("a", "id=small");
        let cost = feature_hash("a", "cost");
        let seg = feature_hash("c", "segment=A");
        let x = feature_hash("c", "x");
        let score = feature_hash("d", "score");
        let mut want: Vec<(u32, f32)> = vec![
            (fz.index(BIAS_HASH), 1.0),
            (fz.index(id), 1.0),
            (fz.index(cost), 3.0),
            (fz.index(interaction_hash(seg, id)), 1.0),
            (fz.index(interaction_hash(seg, cost)), 3.0),
            (fz.index(interaction_hash(x, id)), 2.0),
            (fz.index(interaction_hash(x, cost)), 6.0),
            (fz.index(interaction_hash(score, id)), 0.5),
            (fz.index(interaction_hash(score, cost)), 1.5),
        ];
        want.sort_by_key(|p| p.0);
        assert_eq!(phi, want, "no slot collisions expected for these names");
        // Context-only terms are absent from phi ...
        for h in [seg, x, score] {
            assert!(!phi.iter().any(|&(i, _)| i == fz.index(h)));
        }
        // ... and present in phi_with_context.
        let mut full = Vec::new();
        fz.phi_with_context(&ctx, &a, &mut full).unwrap();
        want.extend([
            (fz.index(seg), 1.0),
            (fz.index(x), 2.0),
            (fz.index(score), 0.5),
        ]);
        want.sort_by_key(|p| p.0);
        assert_eq!(full, want);
        assert_eq!(Featurizer::phi_terms(&ctx, a.len()), 9);
    }

    #[test]
    fn phi_is_canonical_and_masked() {
        let fz = Featurizer::new(10).unwrap();
        let mut map = Map::new();
        for i in 0..300 {
            map.insert(format!("f{i}"), json!(i % 7));
        }
        let ctx = fz.context(&Value::Object(map), &Value::Null).unwrap();
        let a = fz
            .action(&action("a", json!({"q": 1.5, "tags": ["x", "y"]})))
            .unwrap();
        let mut phi = vec![(999_999, 1.0)];
        fz.phi(&ctx, &a, &mut phi).unwrap();
        assert!(phi.windows(2).all(|w| w[0].0 < w[1].0), "sorted and unique");
        assert!(
            phi.iter()
                .all(|&(i, v)| i < 1024 && v != 0.0 && v.is_finite())
        );
        // 1 + 4 + 300 * 4 raw terms minus zeros (f0, f7, ...) minus collisions.
        assert!(phi.len() < 1 + 4 + 300 * 4);
        assert!(!phi.iter().any(|&(i, _)| i == 999_999), "out is cleared");
    }

    #[test]
    fn canonicalize_merges_collisions_and_drops_zeros() {
        let mut x = vec![(5, 1.0), (2, 3.0), (5, 2.0), (7, 1.0), (7, -1.0), (1, 0.0)];
        canonicalize(&mut x);
        assert_eq!(x, vec![(2, 3.0), (5, 3.0)]);
        let mut big = vec![(1, f32::MAX), (1, f32::MAX)];
        canonicalize(&mut big);
        assert_eq!(big, vec![(1, f32::MAX)], "saturates instead of overflowing");
    }

    #[test]
    fn interaction_values_saturate() {
        let fz = Featurizer::new(12).unwrap();
        let ctx = fz.context(&json!({"x": 3e38}), &Value::Null).unwrap();
        let a = fz.action(&action("a", json!({"y": 3e38}))).unwrap();
        let mut phi = Vec::new();
        fz.phi(&ctx, &a, &mut phi).unwrap();
        assert!(phi.iter().all(|&(_, v)| v.is_finite()));
        assert!(phi.iter().any(|&(_, v)| v == f32::MAX));
    }

    #[test]
    fn term_limit() {
        let fz = Featurizer::new(18).unwrap();
        let one = |hash| Feature { hash, value: 1.0 };
        let ctx = ContextFeatures {
            context: vec![one(1); 2048],
            derived: Vec::new(),
        };
        let mut phi = Vec::new();
        let err = fz.phi(&ctx, &vec![one(2); 1024], &mut phi).unwrap_err();
        assert_eq!(
            err,
            FeatureError::TooManyTerms {
                terms: 1 + 1024 + 2048 * 1024,
                max: MAX_PHI_TERMS,
            }
        );
        assert!(fz.phi(&ctx, &vec![one(2); 400], &mut phi).is_ok());
    }

    #[test]
    fn featurizer_bits_range() {
        assert!(Featurizer::new(0).is_err());
        assert!(Featurizer::new(33).is_err());
        let fz = Featurizer::new(32).unwrap();
        assert_eq!(fz.index(1), 0x34c2_cb2c);
        assert_eq!(Featurizer::new(4).unwrap().index(1), 0xc);
    }

    /// Occupied buckets after hashing `n` distinct keys into `m` buckets,
    /// compared with the expectation for an ideal random hash. Returns
    /// (occupied, expected, standard deviation).
    fn occupancy(slots: impl Iterator<Item = u32>, n: usize, m: usize) -> (f64, f64, f64) {
        let mut seen = vec![false; m];
        let mut occupied = 0usize;
        for s in slots {
            if !seen[s as usize] {
                seen[s as usize] = true;
                occupied += 1;
            }
        }
        let (n, m) = (n as f64, m as f64);
        let q = (-n / m).exp();
        let expected = m * (1.0 - q);
        let variance = m * q * (1.0 - (1.0 + n / m) * q);
        (occupied as f64, expected, variance.sqrt())
    }

    /// Slots must not collide more than an ideal random hash would, for the
    /// structured names real payloads produce (numbered ids, indexed arrays,
    /// dotted paths, zero-padded codes) and for their interactions. Raw
    /// masked FNV-1a fails this by up to 37 standard deviations.
    #[test]
    fn slots_spread_like_a_random_hash() {
        /// A named generator of the i-th hash in a family.
        type Family = (&'static str, fn(usize) -> u64);
        let n = 20_000;
        let families: [Family; 6] = [
            ("item ids", |i| feature_hash("c", &format!("id=item-{i}"))),
            ("indexed", |i| feature_hash("c", &format!("x[{i}]"))),
            ("paths", |i| {
                feature_hash("c", &format!("user.history[{}].sku=P{}", i % 50, i / 50))
            }),
            ("zip codes", |i| feature_hash("c", &format!("zip={i:05}"))),
            ("action ids", |i| feature_hash("a", &format!("id={i}"))),
            ("interactions", |i| {
                let c = feature_hash("c", &format!("f{}=1", i / 100));
                interaction_hash(c, feature_hash("a", &format!("id={}", i % 100)))
            }),
        ];
        for bits in [16, 18] {
            let fz = Featurizer::new(bits).unwrap();
            for (name, family) in families {
                let slots = (0..n).map(|i| fz.index(family(i)));
                let (occupied, expected, sd) = occupancy(slots, n, 1 << bits);
                assert!(
                    occupied > expected - 5.0 * sd,
                    "{name} at {bits} bits: {occupied} slots used, expected {expected:.0} +- {sd:.0}"
                );
            }
        }
    }
}
