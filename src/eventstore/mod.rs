//! Durable event store for the Syntra v2 decision core.
//!
//! The store keeps four kinds of events per capsule: decisions (with the PMF
//! they were sampled from), rewards, model snapshots and audit events. It
//! replaces the v1 JSONL logs and whole-file JSON rewrites: every write is a
//! small SQLite transaction, and rewards join decisions through an index
//! instead of a log scan.
//!
//! [`EventStore`] is the backend-neutral contract; [`SqliteStore`] is the
//! default implementation (`<root>/syntra.db`). The batch methods
//! ([`EventStore::insert_decisions`], [`EventStore::insert_rewards`]) serve
//! the server's write-behind decision queue and SDK batch uploads: one
//! transaction per batch, one outcome per row.
//!
//! # Semantics
//!
//! * **Addressing.** Every row belongs to one [`CapsuleKey`]. Decision ids,
//!   reward idempotency keys and audit/model rows are scoped to that key, so
//!   the same decision id under two capsules names two unrelated decisions.
//! * **Decisions are write-once.** Inserting an id that already exists for the
//!   capsule returns [`InsertOutcome::Duplicate`] with the stored row, never
//!   an overwrite.
//! * **Rewards count once.** `(capsule, idempotency_key)` is unique. The
//!   check is the database constraint itself, so it holds for concurrent
//!   callers and for several processes sharing one file. A reward must name a
//!   decision that exists in the same capsule.
//! * **Reward sequence numbers** are assigned by the store, strictly
//!   increasing and never reused, even after deletes. SQLite has a single
//!   writer, so sequence order is commit order: a reader that has seen reward
//!   `n` has seen every reward below `n`. That property is what makes
//!   [`ModelSnapshot::reward_seq`] a safe replay watermark.
//! * **Time** is milliseconds since the Unix epoch and is always supplied by
//!   the caller; the store never reads the clock. Time windows are half-open,
//!   `[since_ms, until_ms)`, and `None` means unbounded on that side.
//!
//! # Durability ([`SqliteStore`])
//!
//! The database runs in WAL mode with `synchronous = NORMAL`. A write,
//! batch or single, returns once its frames are in the write-ahead log,
//! without an fsync. A batch commits all of its accepted rows or none.
//!
//! * **Process crash** (panic, `kill -9`, OOM kill): every write that returned
//!   `Ok` survives. The WAL is in the OS page cache and is replayed on the next
//!   open.
//! * **Power loss or kernel crash:** the database stays consistent, since WAL
//!   frames are checksummed and recovery keeps a prefix of committed
//!   transactions. Transactions committed after the last WAL sync can be lost.
//!   The WAL is synced by every checkpoint, which a background thread runs
//!   after [`SqliteOptions::checkpoint_after_pages`] WAL pages or
//!   [`SqliteOptions::checkpoint_interval`] after the first unsynced write,
//!   whichever comes first. That bounds the loss window. It does not replace
//!   the drive's own write-cache guarantees, and on macOS `fsync` does not
//!   flush the drive cache.
//!
//! No checkpoint runs on the write path; the writer connection checkpoints
//! only once, when the store closes. A decision insert therefore never waits
//! for a checkpoint. The one fsync left on the write path is SQLite's
//! WAL-header sync: after a completed checkpoint, the next commit restarts
//! the log at the beginning and syncs its new header, at most once per
//! checkpoint cycle.

mod checkpoint;
mod error;
mod ffi;
mod pool;
mod schema;
mod sqlite;
mod validate;

pub use error::{Result, StoreError};
pub use schema::SCHEMA_VERSION;
pub use sqlite::{IntegrityReport, SqliteOptions, SqliteStore, read_logged_rows};

use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum length, in characters, of each capsule key component.
pub const MAX_KEY_PART_CHARS: usize = 128;
/// Maximum length, in characters, of decision ids, idempotency keys and
/// audit event names.
pub const MAX_ID_CHARS: usize = 256;
/// Maximum rows per batch call. A batch holds the single writer for its
/// whole transaction, so this bounds how long one caller can hold it.
pub const MAX_BATCH_ROWS: usize = 4096;

/// Address of one capsule: `tenant / job / capsule`.
///
/// Each component is non-empty, at most [`MAX_KEY_PART_CHARS`] characters,
/// contains no `/`, `\` or NUL, and is not `.` or `..`. The components also
/// name directories under the store root, which is why path syntax is
/// rejected. The fields are private and deserialization validates too, so a
/// `CapsuleKey` value is always valid.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "RawCapsuleKey")]
pub struct CapsuleKey {
    tenant: String,
    job: String,
    capsule: String,
}

#[derive(Deserialize)]
struct RawCapsuleKey {
    tenant: String,
    job: String,
    capsule: String,
}

impl TryFrom<RawCapsuleKey> for CapsuleKey {
    type Error = StoreError;

    fn try_from(raw: RawCapsuleKey) -> Result<Self> {
        CapsuleKey::new(raw.tenant, raw.job, raw.capsule)
    }
}

impl CapsuleKey {
    /// Builds a key, validating every component.
    pub fn new(
        tenant: impl Into<String>,
        job: impl Into<String>,
        capsule: impl Into<String>,
    ) -> Result<Self> {
        let key = CapsuleKey {
            tenant: tenant.into(),
            job: job.into(),
            capsule: capsule.into(),
        };
        validate::key_part("tenant", &key.tenant)?;
        validate::key_part("job", &key.job)?;
        validate::key_part("capsule", &key.capsule)?;
        Ok(key)
    }

    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    pub fn job(&self) -> &str {
        &self.job
    }

    pub fn capsule(&self) -> &str {
        &self.capsule
    }
}

impl fmt::Display for CapsuleKey {
    /// `tenant/job/capsule`. Unambiguous because components cannot contain `/`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.tenant, self.job, self.capsule)
    }
}

/// One logged decision. JSON-typed fields hold JSON text and are validated as
/// JSON on insert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Unique within the capsule (the client `eventId`, or a generated id).
    pub id: String,
    pub key: CapsuleKey,
    pub ts_ms: i64,
    /// Must not exceed `i64::MAX` (SQLite integers are signed).
    pub model_version: u64,
    /// `learner`, `baseline_explore`, `frozen`, ... (not interpreted here).
    pub mode: String,
    /// JSON text: the request context.
    pub context: String,
    /// JSON text: the action set, with features.
    pub actions: String,
    /// JSON text: the eligible action ids, in PMF order.
    pub eligible: String,
    /// JSON text: the sampling distribution over `eligible`. `None` only for
    /// legacy rows imported from v1, which must be excluded from OPE.
    pub pmf: Option<String>,
    pub chosen_index: i64,
    pub chosen_id: String,
    /// Probability of the chosen action, in `[0, 1]` when present.
    pub probability: Option<f64>,
    /// Sampling seed. Every `u64` round-trips exactly: it is stored as the
    /// `i64` with the same bits.
    pub seed: u64,
    /// JSON text: derived features published by the feature program.
    pub derived: String,
    pub reason: Option<String>,
    pub request_sha256: String,
    pub program_sha256: Option<String>,
}

/// One reward event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewardRecord {
    /// Assigned by the store. Ignored on insert and filled in on every read.
    pub seq: i64,
    pub decision_id: String,
    pub key: CapsuleKey,
    pub ts_ms: i64,
    /// Raw reward; must be finite.
    pub value: f64,
    /// Reward normalized with the capsule's reward range; must be finite.
    pub value_norm: f64,
    /// Unique within the capsule. The design defaults it to the decision id,
    /// which yields one reward per decision.
    pub idempotency_key: String,
    /// Optional JSON text.
    pub detail: Option<String>,
}

/// A serialized learner state plus the reward watermark it reflects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub key: CapsuleKey,
    /// Model version; the latest snapshot is the one with the highest version.
    /// Must not exceed `i64::MAX`.
    pub version: u64,
    /// Replay watermark: every reward with `seq <= reward_seq` is reflected in
    /// `state` and no later reward is, so after loading this snapshot the
    /// caller replays [`EventStore::rewards_since`]`(key, reward_seq, ..)`.
    /// The learner must therefore apply rewards in sequence order. `0` means
    /// no reward yet.
    pub reward_seq: i64,
    pub ts_ms: i64,
    pub state: Vec<u8>,
}

/// One audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: i64,
    pub key: CapsuleKey,
    pub ts_ms: i64,
    pub event: String,
    /// JSON text.
    pub detail: String,
}

/// Result of storing one decision.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertOutcome {
    Inserted,
    /// The id already exists for this capsule. Holds the stored row, which
    /// was left untouched. The caller compares `request_sha256` to tell an
    /// idempotent retry from a conflicting reuse of the id.
    Duplicate(Box<DecisionRecord>),
    /// Batch only: the row failed validation (the message says why) and was
    /// skipped. [`EventStore::insert_decision`] reports this as
    /// [`StoreError::InvalidInput`] instead.
    Invalid(String),
}

/// Result of storing one reward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewardOutcome {
    /// Stored under this new sequence number.
    Applied(i64),
    /// A reward with the same idempotency key already exists in the capsule
    /// under this sequence number; nothing was written. The existing reward
    /// may belong to a different decision (a reused key). Callers that must
    /// tell a retry from a collision check whether the sequence number
    /// appears in [`EventStore::rewards_for_decision`].
    Duplicate(i64),
    /// Batch only: the decision does not exist in the capsule, so nothing was
    /// written. [`EventStore::insert_reward`] reports this as
    /// [`StoreError::UnknownDecision`] instead.
    UnknownDecision,
    /// Batch only: the row failed validation (the message says why) and was
    /// skipped. [`EventStore::insert_reward`] reports this as
    /// [`StoreError::InvalidInput`] instead.
    Invalid(String),
}

/// How [`EventStore::logged_rows`] folds several rewards for one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardsMode {
    /// The reward with the lowest sequence number, i.e. the first applied.
    First,
    /// The sum of all rewards, added in sequence order.
    Sum,
}

/// A decision joined with its aggregated reward, as input to off-policy
/// evaluation. See [`EventStore::logged_rows`] for the aggregation rules.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggedRow {
    pub decision: DecisionRecord,
    /// Aggregated raw reward; `None` when the decision has no reward.
    pub reward: Option<f64>,
    /// Aggregated normalized reward; `None` exactly when `reward` is `None`.
    pub reward_norm: Option<f64>,
    /// Number of rewards stored for the decision, whatever the mode.
    pub reward_count: u64,
}

impl LoggedRow {
    /// True when the row can feed importance-weighted estimators: it has a
    /// logged PMF and a chosen-action probability (legacy v1 rows do not).
    pub fn has_propensity(&self) -> bool {
        self.decision.pmf.is_some() && self.decision.probability.is_some()
    }
}

/// Rows removed by [`EventStore::prune_decisions_before`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneCounts {
    pub decisions: u64,
    pub rewards: u64,
}

/// Per-capsule counters from [`EventStore::stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapsuleStats {
    pub decisions: u64,
    pub rewards: u64,
    pub first_decision_ms: Option<i64>,
    pub last_decision_ms: Option<i64>,
    /// Highest reward sequence number in the capsule, for replay-lag checks
    /// against [`ModelSnapshot::reward_seq`].
    pub last_reward_seq: Option<i64>,
}

/// Durable storage for decisions, rewards, model snapshots and audit events.
///
/// Every method is safe to call from many threads at once. Methods take no
/// callbacks, so no lock is ever held while caller code runs. Limits are row
/// counts; a limit of 0 returns nothing.
pub trait EventStore: Send + Sync {
    /// Stores a decision unless its id already exists for the capsule, in
    /// which case the stored row is returned untouched.
    fn insert_decision(&self, decision: &DecisionRecord) -> Result<InsertOutcome>;

    /// Stores a batch of decisions in one transaction and returns one outcome
    /// per input row, in input order. Rows may belong to different capsules.
    ///
    /// * Rows apply in order: an id repeated within the batch is `Inserted`
    ///   the first time and `Duplicate` after that.
    /// * A row that fails validation gets [`InsertOutcome::Invalid`] and is
    ///   skipped; the other rows still commit.
    /// * Any other failure (I/O, lock timeout, corruption) rolls back the
    ///   whole batch and returns the error. Nothing from the batch is stored,
    ///   and retrying it is safe because each row is idempotent.
    /// * At most [`MAX_BATCH_ROWS`] rows, otherwise
    ///   [`StoreError::InvalidInput`] and nothing is written.
    fn insert_decisions(&self, decisions: &[DecisionRecord]) -> Result<Vec<InsertOutcome>>;

    /// Looks up one decision by id within the capsule.
    fn get_decision(&self, key: &CapsuleKey, id: &str) -> Result<Option<DecisionRecord>>;

    /// Pages through decisions with `ts_ms` in `[since_ms, until_ms)`, ordered
    /// by `(ts_ms, id)` ascending (newest last; ids compare bytewise).
    ///
    /// `after_id` is the id of the last row of the previous page; the page
    /// starts strictly after that row's `(ts_ms, id)`. An `after_id` that no
    /// longer exists (for example, pruned) fails with
    /// [`StoreError::UnknownCursor`] rather than silently restarting.
    fn list_decisions(
        &self,
        key: &CapsuleKey,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        limit: usize,
        after_id: Option<&str>,
    ) -> Result<Vec<DecisionRecord>>;

    /// Stores a reward unless its idempotency key already exists in the
    /// capsule. Atomic under concurrency: the uniqueness check is the
    /// database constraint. Fails with [`StoreError::UnknownDecision`] when
    /// the decision does not exist in the same capsule.
    fn insert_reward(&self, reward: &RewardRecord) -> Result<RewardOutcome>;

    /// Stores a batch of rewards in one transaction and returns one outcome
    /// per input row, in input order: `Applied`, `Duplicate`,
    /// `UnknownDecision` or `Invalid`. A bad row never fails the batch; the
    /// other rows still commit. Rows apply in order, so an idempotency key
    /// repeated within the batch is `Applied` once and `Duplicate` after
    /// that, and sequence numbers of applied rows increase in input order.
    /// Other failures roll back the whole batch, as for
    /// [`EventStore::insert_decisions`], and the same size limit applies.
    fn insert_rewards(&self, rewards: &[RewardRecord]) -> Result<Vec<RewardOutcome>>;

    /// All rewards for one decision, ascending by sequence number.
    fn rewards_for_decision(
        &self,
        key: &CapsuleKey,
        decision_id: &str,
    ) -> Result<Vec<RewardRecord>>;

    /// Up to `limit` rewards with `seq > after_seq`, ascending by sequence
    /// number, each joined with its decision. Used to replay the model after
    /// a snapshot; page by passing the last returned `seq` as `after_seq`.
    fn rewards_since(
        &self,
        key: &CapsuleKey,
        after_seq: i64,
        limit: usize,
    ) -> Result<Vec<(RewardRecord, DecisionRecord)>>;

    /// Every decision with `ts_ms` in `[since_ms, until_ms)`, ordered by
    /// `(ts_ms, id)`, each with its rewards folded into one value.
    ///
    /// Aggregation, applied to the raw and the normalized value separately:
    ///
    /// * All rewards stored for the decision at the time of the call count,
    ///   whatever their own timestamps. The window selects decisions, not
    ///   rewards.
    /// * No reward: `reward` and `reward_norm` are `None` and
    ///   `reward_count == 0`. Nothing is imputed; the estimator decides.
    /// * [`RewardsMode::First`]: the value of the reward with the lowest
    ///   sequence number, i.e. the first one applied.
    /// * [`RewardsMode::Sum`]: the sum over all rewards, added in sequence
    ///   order, so results are bit-for-bit reproducible. `reward_norm` is the
    ///   sum of the per-reward normalized values. When the reward range does
    ///   not start at 0, that differs from normalizing the raw sum, so an
    ///   estimator that needs the normalized sum must recompute it from
    ///   `reward`.
    /// * `reward_count` is the number of rewards stored, in both modes.
    ///
    /// Legacy rows (`pmf == None`) are included; see
    /// [`LoggedRow::has_propensity`].
    fn logged_rows(
        &self,
        key: &CapsuleKey,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        rewards: RewardsMode,
    ) -> Result<Vec<LoggedRow>>;

    /// Decisions of one capsule that have no reward, made after `after`
    /// (a `(ts_ms, id)` cursor, exclusive) and at or before `until_ms`,
    /// oldest first, at most `limit`. Returns `(ts_ms, id)` pairs. Used to
    /// apply default rewards once the reward wait has passed.
    fn unrewarded_decisions(
        &self,
        key: &CapsuleKey,
        after: Option<(i64, &str)>,
        until_ms: i64,
        limit: usize,
    ) -> Result<Vec<(i64, String)>>;

    /// Stores a snapshot. Saving a version that already exists replaces it.
    fn save_model(&self, snapshot: &ModelSnapshot) -> Result<()>;

    /// The snapshot with the highest version, if any. Versions must grow
    /// monotonically per capsule; after a model reset, prune old snapshots
    /// first ([`EventStore::prune_models`] with `keep = 0`).
    fn load_latest_model(&self, key: &CapsuleKey) -> Result<Option<ModelSnapshot>>;

    /// Keeps the `keep` highest versions and deletes the rest. Returns the
    /// number of snapshots deleted.
    fn prune_models(&self, key: &CapsuleKey, keep: usize) -> Result<u64>;

    /// Appends an audit event and returns its sequence number.
    fn append_audit(
        &self,
        key: &CapsuleKey,
        ts_ms: i64,
        event: &str,
        detail_json: &str,
    ) -> Result<i64>;

    /// The `limit` most recent audit events, oldest first.
    fn list_audit(&self, key: &CapsuleKey, limit: usize) -> Result<Vec<AuditRecord>>;

    /// Deletes every decision, reward, snapshot and audit event of the
    /// capsule and returns the number of rows removed. The work is done in
    /// bounded batches so other capsules keep writing; stop traffic to the
    /// capsule first. Idempotent, so it is safe to retry after an error.
    fn delete_capsule(&self, key: &CapsuleKey) -> Result<u64>;

    /// Deletes decisions with `ts_ms < before_ms`, together with their
    /// rewards, in bounded batches.
    ///
    /// Deleted rewards can no longer be replayed. Save a snapshot that covers
    /// them first, or a restart will rebuild the model without them.
    fn prune_decisions_before(&self, key: &CapsuleKey, before_ms: i64) -> Result<PruneCounts>;

    /// Row counts and time bounds for the capsule, read from one consistent
    /// snapshot. Cost grows with the capsule's row count (index scans).
    fn stats(&self, key: &CapsuleKey) -> Result<CapsuleStats>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_key_accepts_ordinary_names() {
        let key = CapsuleKey::new("acme", "default", "router-v2.1").unwrap();
        assert_eq!(key.tenant(), "acme");
        assert_eq!(key.job(), "default");
        assert_eq!(key.capsule(), "router-v2.1");
        assert_eq!(key.to_string(), "acme/default/router-v2.1");
        // Non-ASCII is fine; the limit counts characters, not bytes.
        let long_unicode = "é".repeat(MAX_KEY_PART_CHARS);
        assert!(CapsuleKey::new(long_unicode, "j", "c").is_ok());
    }

    #[test]
    fn capsule_key_rejects_invalid_components() {
        let too_long = "x".repeat(MAX_KEY_PART_CHARS + 1);
        let cases: &[(&str, &str, &str)] = &[
            ("", "job", "cap"),
            ("tenant", "", "cap"),
            ("tenant", "job", ""),
            ("ten/ant", "job", "cap"),
            ("tenant", "jo\\b", "cap"),
            ("tenant", "job", "ca\0p"),
            ("..", "job", "cap"),
            ("tenant", ".", "cap"),
            (too_long.as_str(), "job", "cap"),
        ];
        for (t, j, c) in cases {
            let err = CapsuleKey::new(*t, *j, *c).unwrap_err();
            assert!(
                matches!(err, StoreError::InvalidInput(_)),
                "{t:?}/{j:?}/{c:?}: {err}"
            );
        }
    }

    #[test]
    fn capsule_key_deserialization_validates() {
        let ok: CapsuleKey =
            serde_json::from_str(r#"{"tenant":"t","job":"j","capsule":"c"}"#).unwrap();
        assert_eq!(ok, CapsuleKey::new("t", "j", "c").unwrap());
        let round: CapsuleKey = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
        assert_eq!(round, ok);

        let bad =
            serde_json::from_str::<CapsuleKey>(r#"{"tenant":"../x","job":"j","capsule":"c"}"#);
        let msg = bad.unwrap_err().to_string();
        assert!(msg.contains("tenant"), "{msg}");
    }

    #[test]
    fn records_serialize_seed_exactly() {
        let key = CapsuleKey::new("t", "j", "c").unwrap();
        let d = DecisionRecord {
            id: "d1".into(),
            key,
            ts_ms: 1,
            model_version: 2,
            mode: "learner".into(),
            context: "{}".into(),
            actions: "[]".into(),
            eligible: "[]".into(),
            pmf: None,
            chosen_index: 0,
            chosen_id: "a".into(),
            probability: None,
            seed: u64::MAX,
            derived: "{}".into(),
            reason: None,
            request_sha256: "00".into(),
            program_sha256: None,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("18446744073709551615"), "{json}");
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
