//! Syntra v2 decision core: a contextual-bandit learner per capsule.
//!
//! - [`features`]: JSON flattening, stable FNV-1a feature hashing, and the
//!   per-pair feature vector `phi(x, a) = [bias, a, c x a, d x a]`.
//! - [`learner`]: online least squares with Normalized Adaptive Gradient
//!   updates over `2^bits` hashed slots, with a checksummed snapshot format.
//! - [`explore`]: SquareCB (inverse gap weighting), epsilon-greedy and
//!   baseline exploration, the exploration floor, and PMF sampling.
//! - [`spec`]: the strict, validated capsule spec and RFC 7396 merge patch.
//! - [`engine`]: decide and learn, tying the pieces together.
//! - [`rng`]: SplitMix64 seeded per decision, and OS-random seeds.
//!
//! Every decision records the PMF it was sampled from, and every eligible
//! action gets at least `floor / K` probability, so the logs support
//! off-policy evaluation.

pub mod engine;
pub mod explore;
pub mod features;
pub mod learner;
pub mod rng;
pub mod spec;

pub use engine::{DecideError, DecideInput, Decision, Engine};
pub use features::{ContextFeatures, Feature, FeatureError, Featurizer};
pub use learner::LinearModel;
pub use rng::{SplitMix64, random_seed};
pub use spec::{ActionSpec, DecisionSpec, ExplorationKind, Importance, Mode};
