//! Integration tests for the v2 event store (`syntra::eventstore`).
//!
//! Every test opens its own database in a fresh temp directory, removed on
//! drop, so tests are independent and safe to run in parallel.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use syntra::eventstore::{
    CapsuleKey, DecisionRecord, EventStore, InsertOutcome, LoggedRow, MAX_BATCH_ROWS, MAX_ID_CHARS,
    ModelSnapshot, PruneCounts, RewardOutcome, RewardRecord, RewardsMode, SCHEMA_VERSION,
    SqliteOptions, SqliteStore, StoreError,
};

// ---------------------------------------------------------------------------
// Fixtures

struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "syntra-eventstore-it-{tag}-{}-{nanos}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }

    fn db(&self) -> PathBuf {
        self.0.join("syntra.db")
    }

    fn open(&self) -> SqliteStore {
        SqliteStore::open(self.db()).unwrap()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wal(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", db.display()))
}

fn key(tenant: &str) -> CapsuleKey {
    CapsuleKey::new(tenant, "default", "router").unwrap()
}

/// A decision with a realistically sized context and action set.
fn decision(key: &CapsuleKey, id: &str, ts_ms: i64) -> DecisionRecord {
    DecisionRecord {
        id: id.to_string(),
        key: key.clone(),
        ts_ms,
        model_version: 42,
        mode: "learner".into(),
        context: format!(
            r#"{{"task":"code","prompt_tokens":{},"user":{{"tier":"pro","region":"eu-west"}},"flags":["a","b"]}}"#,
            ts_ms.rem_euclid(4096)
        ),
        actions:
            r#"[{"id":"small","features":{"cost":0.2}},{"id":"large","features":{"cost":1.0}}]"#
                .into(),
        eligible: r#"["small","large"]"#.into(),
        pmf: Some("[0.93,0.07]".into()),
        chosen_index: 0,
        chosen_id: "small".into(),
        probability: Some(0.93),
        seed: 0x9E37_79B9_7F4A_7C15,
        derived: r#"{"len_bucket":"m"}"#.into(),
        reason: None,
        request_sha256: "5f2b".repeat(16),
        program_sha256: None,
    }
}

fn reward(key: &CapsuleKey, decision_id: &str, idempotency_key: &str, value: f64) -> RewardRecord {
    RewardRecord {
        seq: 0,
        decision_id: decision_id.to_string(),
        key: key.clone(),
        ts_ms: 1_000_000,
        value,
        value_norm: value / 10.0,
        idempotency_key: idempotency_key.to_string(),
        detail: None,
    }
}

fn applied(outcome: RewardOutcome) -> i64 {
    match outcome {
        RewardOutcome::Applied(seq) => seq,
        other => panic!("expected Applied, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Round-trips

#[test]
fn decision_round_trip_preserves_every_field() {
    let dir = TestDir::new("roundtrip");
    let store = dir.open();
    let k = key("acme");
    let seeds = [
        0,
        1,
        i64::MAX as u64,
        i64::MAX as u64 + 1,
        0xFEDC_BA98_7654_3210,
        u64::MAX,
    ];
    let mut expected = Vec::new();
    for (i, seed) in seeds.into_iter().enumerate() {
        let mut d = decision(&k, &format!("dec-é-{i}"), 1_700_000_000_000 + i as i64);
        d.seed = seed;
        d.model_version = if i % 2 == 0 { 0 } else { i64::MAX as u64 };
        d.context = r#"{"user":"Zoë","city":"東京","emoji":"🚀","quote":"say \"hi\"","ctl":"\u0001","nested":{"é":[1,2.5,null,true]}}"#.into();
        d.actions = r#"[{"id":"größe","features":{"ß":1}},{"id":"大","features":{}}]"#.into();
        d.eligible = r#"["größe","大"]"#.into();
        d.chosen_id = "大".into();
        d.chosen_index = 1;
        d.derived = r#"{"sprache":"deutsch ✓"}"#.into();
        if i % 2 == 1 {
            d.pmf = None;
            d.probability = None;
            d.reason = Some("règle: écarté ✓".into());
            d.program_sha256 = Some("ab".repeat(32));
        }
        assert_eq!(store.insert_decision(&d).unwrap(), InsertOutcome::Inserted);
        expected.push(d);
    }
    for d in &expected {
        let got = store.get_decision(&k, &d.id).unwrap().expect("stored");
        assert_eq!(&got, d);
        assert_eq!(got.seed, d.seed, "seed must round-trip bit-exactly");
    }
    // The other read paths decode the same columns.
    let listed = store.list_decisions(&k, None, None, 100, None).unwrap();
    assert_eq!(listed, expected);
    let logged: Vec<DecisionRecord> = store
        .logged_rows(&k, None, None, RewardsMode::First)
        .unwrap()
        .into_iter()
        .map(|row| row.decision)
        .collect();
    assert_eq!(logged, expected);
    assert!(store.get_decision(&k, "absent").unwrap().is_none());

    // The batch path writes the same bits.
    let batch_key = key("batch");
    let batch: Vec<DecisionRecord> = expected
        .iter()
        .map(|d| DecisionRecord {
            key: batch_key.clone(),
            ..d.clone()
        })
        .collect();
    let outcomes = store.insert_decisions(&batch).unwrap();
    assert!(
        outcomes.iter().all(|o| *o == InsertOutcome::Inserted),
        "{outcomes:?}"
    );
    assert_eq!(
        store
            .list_decisions(&batch_key, None, None, 100, None)
            .unwrap(),
        batch
    );
}

#[test]
fn reward_snapshot_and_audit_round_trip() {
    let dir = TestDir::new("roundtrip2");
    let store = dir.open();
    let k = key("acme");
    store.insert_decision(&decision(&k, "d1", 10)).unwrap();

    let mut r = reward(&k, "d1", "idem-ü", -3.25);
    r.value_norm = 0.0;
    r.ts_ms = -5; // before the epoch is still a valid instant
    r.detail = Some(r#"{"source":"ẞ-pipeline","ok":true}"#.into());
    let seq = applied(store.insert_reward(&r).unwrap());
    assert!(seq > 0);
    let stored = store.rewards_for_decision(&k, "d1").unwrap();
    assert_eq!(stored, vec![RewardRecord { seq, ..r.clone() }]);

    let state: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect();
    let snap = ModelSnapshot {
        key: k.clone(),
        version: i64::MAX as u64,
        reward_seq: seq,
        ts_ms: 99,
        state,
    };
    store.save_model(&snap).unwrap();
    assert_eq!(store.load_latest_model(&k).unwrap(), Some(snap));

    let audit_seq = store
        .append_audit(&k, 7, "spec.updated", r#"{"by":"José","diff":["ε"]}"#)
        .unwrap();
    let audit = store.list_audit(&k, 10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].seq, audit_seq);
    assert_eq!(audit[0].key, k);
    assert_eq!(audit[0].ts_ms, 7);
    assert_eq!(audit[0].event, "spec.updated");
    assert_eq!(audit[0].detail, r#"{"by":"José","diff":["ε"]}"#);
}

// ---------------------------------------------------------------------------
// Idempotency and referential integrity

#[test]
fn duplicate_decision_returns_the_stored_row_untouched() {
    let dir = TestDir::new("dup-decision");
    let store = dir.open();
    let k = key("acme");
    let original = decision(&k, "evt-1", 100);
    assert_eq!(
        store.insert_decision(&original).unwrap(),
        InsertOutcome::Inserted
    );

    // An exact retry collides on the primary key and on the unique
    // (ts_ms, id) index at once; it must still come back as Duplicate.
    assert_eq!(
        store.insert_decision(&original).unwrap(),
        InsertOutcome::Duplicate(Box::new(original.clone()))
    );

    // Same id, different content (as a conflicting client retry would send).
    let mut retry = decision(&k, "evt-1", 200);
    retry.request_sha256 = "different".into();
    retry.seed = 1;
    match store.insert_decision(&retry).unwrap() {
        InsertOutcome::Duplicate(existing) => assert_eq!(*existing, original),
        other => panic!("expected Duplicate, got {other:?}"),
    }
    assert_eq!(store.get_decision(&k, "evt-1").unwrap(), Some(original));
    assert_eq!(store.stats(&k).unwrap().decisions, 1);
}

#[test]
fn duplicate_reward_idempotency_keys_apply_once() {
    let dir = TestDir::new("dup-reward");
    let store = dir.open();
    let k = key("acme");
    store.insert_decision(&decision(&k, "d1", 1)).unwrap();

    // Default mode: idempotency key == decision id, so one reward per decision.
    let first = applied(store.insert_reward(&reward(&k, "d1", "d1", 1.0)).unwrap());
    assert_eq!(
        store.insert_reward(&reward(&k, "d1", "d1", 5.0)).unwrap(),
        RewardOutcome::Duplicate(first)
    );
    // Sum mode: distinct keys add rewards; a retried key still counts once.
    let second = applied(
        store
            .insert_reward(&reward(&k, "d1", "d1#click", 2.0))
            .unwrap(),
    );
    assert!(second > first);
    assert_eq!(
        store
            .insert_reward(&reward(&k, "d1", "d1#click", 2.0))
            .unwrap(),
        RewardOutcome::Duplicate(second)
    );
    let stored = store.rewards_for_decision(&k, "d1").unwrap();
    let values: Vec<(i64, f64)> = stored.iter().map(|r| (r.seq, r.value)).collect();
    assert_eq!(values, vec![(first, 1.0), (second, 2.0)]);
}

#[test]
fn reward_for_unknown_decision_is_rejected() {
    let dir = TestDir::new("unknown-decision");
    let store = dir.open();
    let k = key("acme");
    let err = store
        .insert_reward(&reward(&k, "never-decided", "never-decided", 1.0))
        .unwrap_err();
    match &err {
        StoreError::UnknownDecision { key, decision_id } => {
            assert_eq!(key, &k);
            assert_eq!(decision_id, "never-decided");
        }
        other => panic!("expected UnknownDecision, got {other}"),
    }
    assert!(err.to_string().contains("never-decided"), "{err}");
    assert_eq!(store.stats(&k).unwrap().rewards, 0);
    // The key was not consumed: once the decision exists, the reward applies.
    store
        .insert_decision(&decision(&k, "never-decided", 1))
        .unwrap();
    applied(
        store
            .insert_reward(&reward(&k, "never-decided", "never-decided", 1.0))
            .unwrap(),
    );
}

// ---------------------------------------------------------------------------
// Batches

#[test]
fn batch_decisions_report_per_row_outcomes() {
    let dir = TestDir::new("batch-decisions");
    let store = dir.open();
    let a = key("acme");
    let b = key("globex");
    let stored = decision(&a, "already-stored", 1);
    store.insert_decision(&stored).unwrap();

    let mut bad = decision(&a, "bad-json", 2);
    bad.context = "{nope".into();
    let batch = vec![
        decision(&a, "a1", 10),
        decision(&b, "b1", 11),
        decision(&a, "already-stored", 12), // conflicts with a stored row
        bad,
        decision(&a, "a2", 13),
        decision(&a, "a1", 14), // repeats a row earlier in this batch
        decision(&b, "a1", 15), // same id in another capsule: independent
    ];
    let outcomes = store.insert_decisions(&batch).unwrap();
    assert_eq!(outcomes.len(), batch.len());
    assert_eq!(outcomes[0], InsertOutcome::Inserted);
    assert_eq!(outcomes[1], InsertOutcome::Inserted);
    assert_eq!(
        outcomes[2],
        InsertOutcome::Duplicate(Box::new(stored.clone()))
    );
    match &outcomes[3] {
        InsertOutcome::Invalid(message) => {
            assert!(message.contains("context is not valid JSON"), "{message}")
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert_eq!(outcomes[4], InsertOutcome::Inserted);
    assert_eq!(
        outcomes[5],
        InsertOutcome::Duplicate(Box::new(batch[0].clone()))
    );
    assert_eq!(outcomes[6], InsertOutcome::Inserted);

    assert_eq!(
        store.get_decision(&a, "a1").unwrap(),
        Some(batch[0].clone())
    );
    assert_eq!(
        store.get_decision(&b, "a1").unwrap(),
        Some(batch[6].clone())
    );
    assert_eq!(
        store.get_decision(&a, "already-stored").unwrap(),
        Some(stored)
    );
    assert!(store.get_decision(&a, "bad-json").unwrap().is_none());
    assert_eq!(store.stats(&a).unwrap().decisions, 3);
    assert_eq!(store.stats(&b).unwrap().decisions, 2);

    assert!(store.insert_decisions(&[]).unwrap().is_empty());
    let only_invalid = store.insert_decisions(&[decision(&a, "", 1)]).unwrap();
    assert!(
        matches!(only_invalid[..], [InsertOutcome::Invalid(_)]),
        "{only_invalid:?}"
    );

    // An oversized batch is refused whole, before anything is written.
    let too_many: Vec<DecisionRecord> = (0..=MAX_BATCH_ROWS)
        .map(|i| decision(&a, &format!("big-{i}"), i as i64))
        .collect();
    let err = store.insert_decisions(&too_many).unwrap_err();
    assert!(matches!(err, StoreError::InvalidInput(_)), "{err}");
    assert!(store.get_decision(&a, "big-0").unwrap().is_none());
    assert_eq!(store.stats(&a).unwrap().decisions, 3);
}

#[test]
fn batch_rewards_report_per_row_outcomes() {
    let dir = TestDir::new("batch-rewards");
    let store = dir.open();
    let k = key("acme");
    let other = key("globex");
    for id in ["d1", "d2", "d3"] {
        store.insert_decision(&decision(&k, id, 1)).unwrap();
    }
    store.insert_decision(&decision(&other, "o1", 1)).unwrap();
    let earlier = applied(store.insert_reward(&reward(&k, "d3", "d3", 1.0)).unwrap());

    let batch = vec![
        reward(&k, "d1", "d1", 1.0),       // applied
        reward(&k, "ghost", "ghost", 1.0), // no such decision
        reward(&k, "d1", "d1", 9.0),       // repeats row 0's key
        RewardRecord {
            value: f64::NAN,
            ..reward(&k, "d2", "d2-nan", 1.0)
        }, // invalid
        reward(&k, "d3", "d3", 2.0),       // key stored before the batch
        reward(&k, "o1", "o1", 1.0),       // o1 exists only in another capsule
        reward(&other, "o1", "o1", 3.0),   // applied, other capsule
        reward(&k, "d2", "d2", 4.0),       // applied after all the failures
    ];
    let outcomes = store.insert_rewards(&batch).unwrap();
    assert_eq!(outcomes.len(), batch.len());
    let s0 = applied(outcomes[0].clone());
    assert_eq!(outcomes[1], RewardOutcome::UnknownDecision);
    assert_eq!(outcomes[2], RewardOutcome::Duplicate(s0));
    match &outcomes[3] {
        RewardOutcome::Invalid(message) => {
            assert!(message.contains("value must be finite"), "{message}")
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert_eq!(outcomes[4], RewardOutcome::Duplicate(earlier));
    assert_eq!(outcomes[5], RewardOutcome::UnknownDecision);
    let s6 = applied(outcomes[6].clone());
    let s7 = applied(outcomes[7].clone());
    assert!(
        earlier < s0 && s0 < s6 && s6 < s7,
        "{earlier} {s0} {s6} {s7}"
    );

    // Every accepted row committed, and nothing else.
    let values = |k: &CapsuleKey, id: &str| -> Vec<f64> {
        store
            .rewards_for_decision(k, id)
            .unwrap()
            .iter()
            .map(|r| r.value)
            .collect()
    };
    assert_eq!(values(&k, "d1"), vec![1.0]);
    assert_eq!(values(&k, "d2"), vec![4.0]);
    assert_eq!(values(&k, "d3"), vec![1.0]);
    assert_eq!(values(&other, "o1"), vec![3.0]);
    assert_eq!(store.stats(&k).unwrap().rewards, 3);
    let replay: Vec<i64> = store
        .rewards_since(&k, earlier, 10)
        .unwrap()
        .iter()
        .map(|(r, _)| r.seq)
        .collect();
    assert_eq!(replay, vec![s0, s7]);
    assert!(store.integrity_check().unwrap().is_ok());
    assert!(store.insert_rewards(&[]).unwrap().is_empty());
}

#[test]
fn batch_rolls_back_entirely_on_an_unexpected_error() {
    let dir = TestDir::new("batch-rollback");
    let store = dir.open();
    let k = key("acme");
    store.insert_decision(&decision(&k, "d0", 0)).unwrap();
    // Inject a failure that is not a per-row outcome: a trigger, added by a
    // second connection, that fails one specific row mid-batch.
    let side = rusqlite::Connection::open(dir.db()).unwrap();
    side.execute_batch(
        "CREATE TRIGGER fail_decision BEFORE INSERT ON decisions WHEN NEW.id = 'boom'
           BEGIN SELECT RAISE(FAIL, 'injected failure'); END;
         CREATE TRIGGER fail_reward BEFORE INSERT ON rewards WHEN NEW.idempotency_key = 'boom'
           BEGIN SELECT RAISE(FAIL, 'injected failure'); END;",
    )
    .unwrap();

    let decisions = vec![
        decision(&k, "d1", 1),
        decision(&k, "boom", 2),
        decision(&k, "d3", 3),
    ];
    let err = store.insert_decisions(&decisions).unwrap_err();
    assert!(err.to_string().contains("injected failure"), "{err}");
    assert!(
        store.get_decision(&k, "d1").unwrap().is_none(),
        "rows before the failure must roll back"
    );
    assert_eq!(store.stats(&k).unwrap().decisions, 1);

    let rewards = vec![reward(&k, "d0", "r1", 1.0), reward(&k, "d0", "boom", 1.0)];
    let err = store.insert_rewards(&rewards).unwrap_err();
    assert!(err.to_string().contains("injected failure"), "{err}");
    assert_eq!(store.stats(&k).unwrap().rewards, 0);

    // The writer is usable afterwards, and the retried batches commit.
    side.execute_batch("DROP TRIGGER fail_decision; DROP TRIGGER fail_reward;")
        .unwrap();
    let outcomes = store.insert_decisions(&decisions).unwrap();
    assert!(
        outcomes.iter().all(|o| *o == InsertOutcome::Inserted),
        "{outcomes:?}"
    );
    let outcomes = store.insert_rewards(&rewards).unwrap();
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, RewardOutcome::Applied(_))),
        "{outcomes:?}"
    );
    assert!(store.integrity_check().unwrap().is_ok());
}

#[test]
fn batches_are_atomic_for_concurrent_readers() {
    const BATCH: usize = 512;
    const BATCHES: usize = 20;
    let dir = TestDir::new("batch-atomic");
    let store = Arc::new(dir.open());
    let k = key("acme");
    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (store, k, done) = (store.clone(), k.clone(), done.clone());
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !done.load(Ordering::Acquire) {
                seen.push(store.stats(&k).unwrap().decisions);
            }
            seen
        })
    };
    for b in 0..BATCHES {
        let batch: Vec<DecisionRecord> = (0..BATCH)
            .map(|i| decision(&k, &format!("b{b:02}-{i:03}"), (b * BATCH + i) as i64))
            .collect();
        let outcomes = store.insert_decisions(&batch).unwrap();
        assert!(outcomes.iter().all(|o| *o == InsertOutcome::Inserted));
    }
    done.store(true, Ordering::Release);
    let seen = reader.join().unwrap();
    let partial = seen.iter().find(|n| **n % BATCH as u64 != 0);
    assert!(
        partial.is_none(),
        "a reader saw a partial batch: {partial:?} rows"
    );
    assert!(seen.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(store.stats(&k).unwrap().decisions, (BATCH * BATCHES) as u64);
}

// ---------------------------------------------------------------------------
// Replay and OPE reads

#[test]
fn rewards_since_replays_after_a_snapshot_watermark() {
    let dir = TestDir::new("replay");
    let store = dir.open();
    let k = key("acme");
    let decisions: Vec<DecisionRecord> = (0..6)
        .map(|i| decision(&k, &format!("d{i}"), 100 + i))
        .collect();
    for d in &decisions {
        store.insert_decision(d).unwrap();
    }
    // Rewards arrive out of decision order; seq follows arrival.
    let arrivals = [
        ("d3", "d3"),
        ("d0", "d0"),
        ("d5", "d5"),
        ("d0", "d0#2"),
        ("d1", "d1"),
        ("d4", "d4"),
    ];
    let mut seqs = Vec::new();
    for (i, (decision_id, idem)) in arrivals.iter().enumerate() {
        seqs.push(applied(
            store
                .insert_reward(&reward(&k, decision_id, idem, i as f64))
                .unwrap(),
        ));
    }
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seqs increase: {seqs:?}"
    );

    // Snapshot after the third reward.
    let watermark = seqs[2];
    store
        .save_model(&ModelSnapshot {
            key: k.clone(),
            version: 3,
            reward_seq: watermark,
            ts_ms: 1,
            state: vec![1, 2, 3],
        })
        .unwrap();
    let snap = store.load_latest_model(&k).unwrap().unwrap();
    assert_eq!(snap.reward_seq, watermark);

    let replay = store.rewards_since(&k, snap.reward_seq, 100).unwrap();
    let got: Vec<(i64, &str)> = replay
        .iter()
        .map(|(r, _)| (r.seq, r.idempotency_key.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![(seqs[3], "d0#2"), (seqs[4], "d1"), (seqs[5], "d4")]
    );
    for (r, d) in &replay {
        let expected = decisions.iter().find(|x| x.id == r.decision_id).unwrap();
        assert_eq!(d, expected, "reward {} joined to the wrong decision", r.seq);
    }

    // Paging by the last seq returns everything, in order, exactly once.
    let mut paged = Vec::new();
    let mut after = 0;
    loop {
        let page = store.rewards_since(&k, after, 2).unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().unwrap().0.seq;
        paged.extend(page.into_iter().map(|(r, _)| r.seq));
    }
    assert_eq!(paged, seqs);
    assert!(
        store
            .rewards_since(&k, *seqs.last().unwrap(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(store.rewards_since(&k, 0, 0).unwrap().is_empty());
}

#[test]
fn logged_rows_aggregate_first_and_sum() {
    let dir = TestDir::new("logged-rows");
    let store = dir.open();
    let k = key("acme");
    // Window [100, 200). "b" and "a" tie on ts; ids break the tie bytewise.
    let mut legacy = decision(&k, "legacy", 150);
    legacy.pmf = None;
    legacy.probability = None;
    for d in [
        decision(&k, "before", 99),
        decision(&k, "none", 100),
        decision(&k, "b", 120),
        decision(&k, "a", 120),
        legacy,
        decision(&k, "at-until", 200),
    ] {
        store.insert_decision(&d).unwrap();
    }
    let add = |id: &str, idem: &str, value: f64, norm: f64| {
        let mut r = reward(&k, id, idem, value);
        r.value_norm = norm;
        applied(store.insert_reward(&r).unwrap());
    };
    add("b", "b1", 1.0, 0.1);
    add("a", "a1", 5.0, 0.5);
    add("b", "b2", 2.0, 0.2);
    add("b", "b3", 4.0, 0.4);
    add("before", "x", 9.0, 0.9);
    add("at-until", "y", 9.0, 0.9);
    add("legacy", "l", 3.0, 0.3);

    let summary = |rows: &[LoggedRow]| -> Vec<(String, Option<f64>, Option<f64>, u64)> {
        rows.iter()
            .map(|r| {
                (
                    r.decision.id.clone(),
                    r.reward,
                    r.reward_norm,
                    r.reward_count,
                )
            })
            .collect()
    };
    let first = store
        .logged_rows(&k, Some(100), Some(200), RewardsMode::First)
        .unwrap();
    assert_eq!(
        summary(&first),
        vec![
            ("none".into(), None, None, 0),
            ("a".into(), Some(5.0), Some(0.5), 1),
            ("b".into(), Some(1.0), Some(0.1), 3), // lowest seq wins
            ("legacy".into(), Some(3.0), Some(0.3), 1),
        ]
    );
    let sum = store
        .logged_rows(&k, Some(100), Some(200), RewardsMode::Sum)
        .unwrap();
    assert_eq!(
        summary(&sum),
        vec![
            ("none".into(), None, None, 0),
            ("a".into(), Some(5.0), Some(0.5), 1),
            // Added in seq order, so the float result is reproducible.
            ("b".into(), Some(1.0 + 2.0 + 4.0), Some(0.1 + 0.2 + 0.4), 3),
            ("legacy".into(), Some(3.0), Some(0.3), 1),
        ]
    );
    let propensity: Vec<bool> = sum.iter().map(LoggedRow::has_propensity).collect();
    assert_eq!(propensity, vec![true, true, true, false]);

    // Unbounded and empty windows.
    assert_eq!(
        store
            .logged_rows(&k, None, None, RewardsMode::First)
            .unwrap()
            .len(),
        6
    );
    assert!(
        store
            .logged_rows(&k, Some(200), Some(200), RewardsMode::Sum)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn list_decisions_pages_in_ts_then_id_order() {
    let dir = TestDir::new("paging");
    let store = dir.open();
    let k = key("acme");
    // 25 decisions over 5 timestamps, inserted in scrambled order.
    let mut ids: Vec<(i64, String)> = (0..25)
        .map(|i| (10 * (i % 5), format!("id-{:02}", (i * 7) % 25)))
        .collect();
    for (ts, id) in &ids {
        store.insert_decision(&decision(&k, id, *ts)).unwrap();
    }
    ids.sort();
    let expected: Vec<String> = ids.iter().map(|(_, id)| id.clone()).collect();

    let mut pages = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .list_decisions(&k, None, None, 7, after.as_deref())
            .unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|d| d.id.clone());
        pages.extend(page.into_iter().map(|d| d.id));
    }
    assert_eq!(pages, expected);

    // [10, 30) holds the ts 10 and ts 20 decisions only.
    let window: Vec<i64> = store
        .list_decisions(&k, Some(10), Some(30), 100, None)
        .unwrap()
        .iter()
        .map(|d| d.ts_ms)
        .collect();
    assert_eq!(
        window,
        vec![10; 5]
            .into_iter()
            .chain(vec![20; 5])
            .collect::<Vec<_>>()
    );

    // A cursor before the window starts at the window.
    let first_id = &expected[0]; // ts 0
    let from_cursor = store
        .list_decisions(&k, Some(20), None, 3, Some(first_id))
        .unwrap();
    assert!(from_cursor.iter().all(|d| d.ts_ms >= 20));
    assert_eq!(from_cursor[0].id, expected[10]);

    let err = store
        .list_decisions(&k, None, None, 10, Some("pruned-long-ago"))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownCursor { .. }), "{err}");
    assert!(
        store
            .list_decisions(&k, None, None, 0, None)
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Concurrency

#[test]
fn concurrent_writers_and_readers_lose_nothing() {
    const WRITERS: usize = 8;
    const PER_WRITER: usize = 2000;
    const READERS: usize = 4;
    let dir = TestDir::new("concurrency");
    let store = Arc::new(dir.open());
    let k = key("acme");
    let seed_for = |w: usize, i: usize| u64::MAX - (w * PER_WRITER + i) as u64;
    // published[w] = number of writer w's decisions whose insert returned.
    let published: Arc<Vec<AtomicUsize>> =
        Arc::new((0..WRITERS).map(|_| AtomicUsize::new(0)).collect());
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let done = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));

    let readers: Vec<_> = (0..READERS)
        .map(|r| {
            let (store, k, published, errors, done, reads) = (
                store.clone(),
                k.clone(),
                published.clone(),
                errors.clone(),
                done.clone(),
                reads.clone(),
            );
            std::thread::spawn(move || {
                let mut x = 0x2545_F491_4F6C_DD1D_u64 ^ (r as u64 + 1);
                let mut next = move || {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as usize
                };
                while !done.load(Ordering::Acquire) {
                    let w = next() % WRITERS;
                    let n = published[w].load(Ordering::Acquire);
                    if n == 0 {
                        std::thread::yield_now();
                        continue;
                    }
                    let i = next() % n;
                    let id = format!("w{w}-{i:05}");
                    match store.get_decision(&k, &id) {
                        Ok(Some(d)) if d.id == id && d.seed == seed_for(w, i) => {}
                        Ok(other) => errors.lock().unwrap().push(format!("read {id}: {other:?}")),
                        Err(e) => errors.lock().unwrap().push(format!("read {id}: {e}")),
                    }
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    let started = Instant::now();
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (store, k, published, errors) =
                (store.clone(), k.clone(), published.clone(), errors.clone());
            std::thread::spawn(move || {
                for i in 0..PER_WRITER {
                    let id = format!("w{w}-{i:05}");
                    let mut d = decision(&k, &id, (i * WRITERS + w) as i64);
                    d.seed = seed_for(w, i);
                    match store.insert_decision(&d) {
                        Ok(InsertOutcome::Inserted) => {}
                        other => {
                            errors
                                .lock()
                                .unwrap()
                                .push(format!("insert {id}: {other:?}"));
                            continue;
                        }
                    }
                    match store.insert_reward(&reward(&k, &id, &id, 1.0)) {
                        Ok(RewardOutcome::Applied(_)) => {}
                        other => errors
                            .lock()
                            .unwrap()
                            .push(format!("reward {id}: {other:?}")),
                    }
                    published[w].store(i + 1, Ordering::Release);
                }
            })
        })
        .collect();
    for t in writers {
        t.join().unwrap();
    }
    let write_time = started.elapsed();
    done.store(true, Ordering::Release);
    for t in readers {
        t.join().unwrap();
    }
    let errors = errors.lock().unwrap();
    assert!(
        errors.is_empty(),
        "{} errors, first: {:?}",
        errors.len(),
        errors.first()
    );
    println!(
        "concurrency: {} decisions + {} rewards from {WRITERS} threads in {write_time:.2?}, \
         {} concurrent reads",
        WRITERS * PER_WRITER,
        WRITERS * PER_WRITER,
        reads.load(Ordering::Relaxed)
    );
    assert!(reads.load(Ordering::Relaxed) > 0, "readers never ran");

    let total = WRITERS * PER_WRITER;
    let stats = store.stats(&k).unwrap();
    assert_eq!(stats.decisions, total as u64);
    assert_eq!(stats.rewards, total as u64);

    // Every decision exactly once.
    let mut decision_ids = HashSet::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .list_decisions(&k, None, None, 1000, after.as_deref())
            .unwrap();
        let Some(last) = page.last() else { break };
        after = Some(last.id.clone());
        for d in &page {
            assert!(
                decision_ids.insert(d.id.clone()),
                "decision {} listed twice",
                d.id
            );
        }
    }
    assert_eq!(decision_ids.len(), total);

    // Every reward exactly once, strictly increasing seq, joined to its own
    // decision.
    let mut rewarded = HashSet::new();
    let mut last_seq = 0;
    loop {
        let batch = store.rewards_since(&k, last_seq, 1000).unwrap();
        if batch.is_empty() {
            break;
        }
        for (r, d) in &batch {
            assert!(r.seq > last_seq, "seq {} after {last_seq}", r.seq);
            last_seq = r.seq;
            assert_eq!(r.decision_id, d.id);
            assert!(
                rewarded.insert(r.decision_id.clone()),
                "reward for {} twice",
                d.id
            );
        }
    }
    assert_eq!(rewarded, decision_ids);
    assert!(store.integrity_check().unwrap().is_ok());
}

#[test]
fn concurrent_duplicate_rewards_apply_exactly_once() {
    const THREADS: usize = 16;
    const ROUNDS: usize = 25;
    let dir = TestDir::new("dup-race");
    // Two stores on one file stand in for two processes: the in-process
    // writer mutex cannot serialize them, only the UNIQUE constraint can.
    let stores = [Arc::new(dir.open()), Arc::new(dir.open())];
    let k = key("acme");
    for round in 0..ROUNDS {
        stores[0]
            .insert_decision(&decision(&k, &format!("d{round}"), round as i64))
            .unwrap();
    }
    let barrier = Arc::new(Barrier::new(THREADS));
    let threads: Vec<_> = (0..THREADS)
        .map(|t| {
            let (store, k, barrier) = (stores[t % 2].clone(), k.clone(), barrier.clone());
            std::thread::spawn(move || {
                (0..ROUNDS)
                    .map(|round| {
                        let r =
                            reward(&k, &format!("d{round}"), &format!("idem-{round}"), t as f64);
                        barrier.wait();
                        // Half the threads go through the batch path.
                        if t % 4 < 2 {
                            store.insert_reward(&r).unwrap()
                        } else {
                            let mut outcomes = store.insert_rewards(&[r]).unwrap();
                            outcomes.remove(0)
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let outcomes: Vec<Vec<RewardOutcome>> =
        threads.into_iter().map(|t| t.join().unwrap()).collect();
    for round in 0..ROUNDS {
        let this_round: Vec<&RewardOutcome> = outcomes.iter().map(|o| &o[round]).collect();
        let winners: Vec<i64> = this_round
            .iter()
            .filter_map(|o| match o {
                RewardOutcome::Applied(seq) => Some(*seq),
                _ => None,
            })
            .collect();
        assert_eq!(winners.len(), 1, "round {round}: {this_round:?}");
        assert!(
            this_round.iter().all(|o| matches!(
                o,
                RewardOutcome::Applied(seq) | RewardOutcome::Duplicate(seq) if *seq == winners[0]
            )),
            "round {round}: {this_round:?}"
        );
        let stored = stores[1]
            .rewards_for_decision(&k, &format!("d{round}"))
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].seq, winners[0]);
    }
}

#[test]
fn reads_do_not_wait_for_an_open_write_transaction() {
    let dir = TestDir::new("nonblocking");
    let store = dir.open();
    let k = key("acme");
    store.insert_decision(&decision(&k, "before", 1)).unwrap();

    // Another connection (think: a second process) holds the write lock.
    let other = rusqlite::Connection::open(dir.db()).unwrap();
    other
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO audit (tenant, job, capsule, ts_ms, event, detail)
             VALUES ('x', 'y', 'z', 1, 'held', '{}');",
        )
        .unwrap();
    let started = Instant::now();
    assert!(store.get_decision(&k, "before").unwrap().is_some());
    assert!(store.rewards_for_decision(&k, "before").unwrap().is_empty());
    assert_eq!(store.stats(&k).unwrap().decisions, 1);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "reads waited {:?} behind a writer",
        started.elapsed()
    );

    // Writes queue behind the lock (busy_timeout) and then succeed.
    let started = Instant::now();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        other.execute_batch("COMMIT").unwrap();
    });
    store.insert_decision(&decision(&k, "after", 2)).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
    releaser.join().unwrap();
}

// ---------------------------------------------------------------------------
// Persistence, migration, crash safety

#[test]
fn reopen_preserves_data_and_migration_is_idempotent() {
    let dir = TestDir::new("reopen");
    let k = key("acme");
    let d = decision(&k, "d1", 5);
    let seq;
    {
        let store = dir.open();
        store.insert_decision(&d).unwrap();
        seq = applied(store.insert_reward(&reward(&k, "d1", "d1", 2.0)).unwrap());
        store
            .save_model(&ModelSnapshot {
                key: k.clone(),
                version: 1,
                reward_seq: seq,
                ts_ms: 6,
                state: b"weights".to_vec(),
            })
            .unwrap();
        store
            .append_audit(&k, 7, "capsule.installed", "{}")
            .unwrap();
    }
    // A clean close checkpoints everything and removes the WAL.
    assert!(
        !wal(&dir.db()).exists(),
        "clean close must fold the WAL into the database"
    );

    for _ in 0..3 {
        let store = dir.open();
        assert_eq!(store.get_decision(&k, "d1").unwrap(), Some(d.clone()));
        assert_eq!(store.rewards_for_decision(&k, "d1").unwrap()[0].seq, seq);
        assert_eq!(
            store.load_latest_model(&k).unwrap().unwrap().state,
            b"weights"
        );
        assert_eq!(store.list_audit(&k, 10).unwrap().len(), 1);
        assert!(store.integrity_check().unwrap().is_ok());
    }
    let raw = rusqlite::Connection::open(dir.db()).unwrap();
    let versions: Vec<i64> = raw
        .prepare("SELECT version FROM schema_version ORDER BY version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(versions, (1..=SCHEMA_VERSION).collect::<Vec<_>>());

    // A file from a newer build is refused, not downgraded.
    raw.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        [SCHEMA_VERSION + 1],
    )
    .unwrap();
    drop(raw);
    let err = SqliteStore::open(dir.db()).unwrap_err();
    assert!(matches!(err, StoreError::Schema(_)), "{err}");
}

const CRASH_CHILD_ENV: &str = "SYNTRA_EVENTSTORE_CRASH_CHILD_DB";
const CRASH_ROWS: usize = 300;

/// Child half of `committed_writes_survive_sigkill`. A no-op in normal runs.
#[test]
fn crash_child_writer() {
    let Some(path) = std::env::var_os(CRASH_CHILD_ENV) else {
        return;
    };
    // No checkpoints: after the kill, the rows exist only in the WAL.
    let options = SqliteOptions {
        checkpoint_after_pages: u64::MAX,
        checkpoint_interval: Duration::from_secs(3600),
        ..SqliteOptions::default()
    };
    let store = SqliteStore::open_with(PathBuf::from(path), options).unwrap();
    let k = key("crash");
    // Half single-row commits, half one batch, as the server issues both.
    for i in 0..CRASH_ROWS / 2 {
        let id = format!("c{i}");
        store.insert_decision(&decision(&k, &id, i as i64)).unwrap();
        applied(store.insert_reward(&reward(&k, &id, &id, 1.0)).unwrap());
    }
    let batch: Vec<DecisionRecord> = (CRASH_ROWS / 2..CRASH_ROWS)
        .map(|i| decision(&k, &format!("c{i}"), i as i64))
        .collect();
    let outcomes = store.insert_decisions(&batch).unwrap();
    assert!(outcomes.iter().all(|o| *o == InsertOutcome::Inserted));
    let rewards: Vec<RewardRecord> = batch
        .iter()
        .map(|d| reward(&k, &d.id, &d.id, 1.0))
        .collect();
    let outcomes = store.insert_rewards(&rewards).unwrap();
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, RewardOutcome::Applied(_)))
    );
    println!("CHILD_COMMITTED {CRASH_ROWS}");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(60)); // wait for SIGKILL
    std::process::exit(3);
}

#[test]
fn committed_writes_survive_sigkill() {
    let dir = TestDir::new("crash");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "crash_child_writer",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CRASH_CHILD_ENV, dir.db())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let committed = BufReader::new(stdout)
        .lines()
        .map_while(|line| line.ok())
        .any(|line| line.contains("CHILD_COMMITTED"));
    child.kill().unwrap(); // SIGKILL: no destructors, no close, no checkpoint
    let status = child.wait().unwrap();
    assert!(committed, "child exited before committing: {status:?}");
    assert!(!status.success());
    assert!(
        wal(&dir.db()).exists(),
        "the rows should be in an uncheckpointed WAL"
    );

    let store = dir.open();
    let k = key("crash");
    let stats = store.stats(&k).unwrap();
    assert_eq!(stats.decisions, CRASH_ROWS as u64);
    assert_eq!(stats.rewards, CRASH_ROWS as u64);
    assert!(store.integrity_check().unwrap().is_ok());
}

#[test]
fn backup_is_a_consistent_standalone_copy() {
    let dir = TestDir::new("backup");
    let store = Arc::new(dir.open());
    let k = key("acme");
    for i in 0..300 {
        let id = format!("seed-{i}");
        store.insert_decision(&decision(&k, &id, i)).unwrap();
        store.insert_reward(&reward(&k, &id, &id, 1.0)).unwrap();
    }
    // Keep writing while the backup runs.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (store, k, stop) = (store.clone(), k.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 300u64;
            while !stop.load(Ordering::Acquire) {
                let id = format!("live-{n}");
                store.insert_decision(&decision(&k, &id, n as i64)).unwrap();
                store.insert_reward(&reward(&k, &id, &id, 1.0)).unwrap();
                n += 1;
            }
            n
        })
    };
    let backups = dir.0.join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let dest = backups.join("syntra-backup.db");
    store.backup_to(&dest).unwrap();
    store.backup_to(&dest).unwrap(); // replacing an earlier backup works
    stop.store(true, Ordering::Release);
    let written = writer.join().unwrap();

    assert!(!wal(&dest).exists(), "a backup is one self-contained file");
    let leftovers: Vec<_> = std::fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "syntra-backup.db")
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );

    let copy = SqliteStore::open(&dest).unwrap();
    assert!(copy.integrity_check().unwrap().is_ok());
    let s = copy.stats(&k).unwrap();
    assert!(s.decisions >= 300 && s.decisions <= written, "{s:?}");
    // One snapshot: the writer adds a decision then its reward, so the copy
    // may hold at most one decision whose reward came later.
    assert!(
        s.rewards == s.decisions || s.rewards + 1 == s.decisions,
        "{s:?}"
    );

    // Guards.
    let err = store.backup_to(dir.db()).unwrap_err();
    assert!(matches!(err, StoreError::InvalidInput(_)), "{err}");
    let in_use = backups.join("in-use.db");
    std::fs::write(&in_use, b"").unwrap();
    std::fs::write(wal(&in_use), b"").unwrap();
    let err = store.backup_to(&in_use).unwrap_err();
    assert!(matches!(err, StoreError::InvalidInput(_)), "{err}");
}

// ---------------------------------------------------------------------------
// Isolation, retention, validation

#[test]
fn capsules_are_isolated() {
    let dir = TestDir::new("isolation");
    let store = dir.open();
    let a = CapsuleKey::new("tenant-a", "default", "router").unwrap();
    let b = CapsuleKey::new("tenant-b", "default", "router").unwrap();
    let a_other = CapsuleKey::new("tenant-a", "default", "ranker").unwrap();

    let mut da = decision(&a, "shared-id", 1);
    da.context = r#"{"owner":"a"}"#.into();
    let mut db = decision(&b, "shared-id", 1);
    db.context = r#"{"owner":"b"}"#.into();
    assert_eq!(store.insert_decision(&da).unwrap(), InsertOutcome::Inserted);
    assert_eq!(store.insert_decision(&db).unwrap(), InsertOutcome::Inserted);
    assert_eq!(
        store.get_decision(&a, "shared-id").unwrap(),
        Some(da.clone())
    );
    assert_eq!(
        store.get_decision(&b, "shared-id").unwrap(),
        Some(db.clone())
    );
    assert_eq!(store.get_decision(&a_other, "shared-id").unwrap(), None);

    // Same idempotency key in both capsules: two independent rewards.
    let seq_a = applied(
        store
            .insert_reward(&reward(&a, "shared-id", "shared-id", 1.0))
            .unwrap(),
    );
    let seq_b = applied(
        store
            .insert_reward(&reward(&b, "shared-id", "shared-id", 2.0))
            .unwrap(),
    );
    assert_ne!(seq_a, seq_b);
    // A decision id that exists only in another capsule is unknown here.
    let err = store
        .insert_reward(&reward(&a_other, "shared-id", "shared-id", 3.0))
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownDecision { .. }), "{err}");

    let values = |k: &CapsuleKey| -> Vec<f64> {
        store
            .rewards_for_decision(k, "shared-id")
            .unwrap()
            .iter()
            .map(|r| r.value)
            .collect()
    };
    assert_eq!(values(&a), vec![1.0]);
    assert_eq!(values(&b), vec![2.0]);
    let replay_a: Vec<(f64, String)> = store
        .rewards_since(&a, 0, 10)
        .unwrap()
        .into_iter()
        .map(|(r, d)| (r.value, d.context))
        .collect();
    assert_eq!(replay_a, vec![(1.0, r#"{"owner":"a"}"#.to_string())]);
    let logged_b = store.logged_rows(&b, None, None, RewardsMode::Sum).unwrap();
    assert_eq!(logged_b.len(), 1);
    assert_eq!(logged_b[0].decision, db);
    assert_eq!(logged_b[0].reward, Some(2.0));

    for (k, version) in [(&a, 10u64), (&b, 20u64)] {
        store
            .save_model(&ModelSnapshot {
                key: k.clone(),
                version,
                reward_seq: 0,
                ts_ms: 0,
                state: vec![version as u8],
            })
            .unwrap();
        store.append_audit(k, 0, "installed", "{}").unwrap();
    }
    assert_eq!(store.load_latest_model(&a).unwrap().unwrap().version, 10);
    assert_eq!(store.load_latest_model(&b).unwrap().unwrap().version, 20);
    assert!(store.load_latest_model(&a_other).unwrap().is_none());

    // Deleting capsule a leaves b whole; a's audit trail stays.
    assert_eq!(store.delete_capsule(&a).unwrap(), 3);
    assert_eq!(store.stats(&a).unwrap(), Default::default());
    assert_eq!(store.list_audit(&a, 10).unwrap().len(), 1);
    let sb = store.stats(&b).unwrap();
    assert_eq!((sb.decisions, sb.rewards), (1, 1));
    assert_eq!(store.list_audit(&b, 10).unwrap().len(), 1);
    assert!(store.load_latest_model(&b).unwrap().is_some());
}

#[test]
fn prune_and_delete_work_in_batches() {
    let dir = TestDir::new("retention");
    let store = dir.open();
    let k = key("acme");
    let other = key("other");
    const N: i64 = 2_600; // crosses the 1000-row delete batch twice
    for i in 0..N {
        let id = format!("d{i:05}");
        store.insert_decision(&decision(&k, &id, i)).unwrap();
        if i % 2 == 0 {
            store.insert_reward(&reward(&k, &id, &id, 1.0)).unwrap();
        }
    }
    store
        .insert_decision(&decision(&other, "d00000", 0))
        .unwrap();

    let pruned = store.prune_decisions_before(&k, 2_100).unwrap();
    assert_eq!(
        pruned,
        PruneCounts {
            decisions: 2_100,
            rewards: 1_050
        }
    );
    let s = store.stats(&k).unwrap();
    assert_eq!((s.decisions, s.rewards), (500, 250));
    assert_eq!(s.first_decision_ms, Some(2_100));
    assert_eq!(s.last_decision_ms, Some(N - 1));
    assert!(store.get_decision(&k, "d02099").unwrap().is_none());
    assert!(store.rewards_for_decision(&k, "d02098").unwrap().is_empty());
    assert_eq!(store.rewards_for_decision(&k, "d02100").unwrap().len(), 1);
    assert_eq!(store.stats(&other).unwrap().decisions, 1);
    assert_eq!(
        store.prune_decisions_before(&k, 2_100).unwrap(),
        PruneCounts::default()
    );
    assert_eq!(
        store.prune_decisions_before(&k, i64::MIN).unwrap(),
        PruneCounts::default()
    );

    store
        .save_model(&ModelSnapshot {
            key: k.clone(),
            version: 1,
            reward_seq: 0,
            ts_ms: 0,
            state: vec![0; 16],
        })
        .unwrap();
    store.append_audit(&k, 0, "pruned", "{}").unwrap();
    // Decisions, rewards and the model go; the audit event stays.
    assert_eq!(store.delete_capsule(&k).unwrap(), 500 + 250 + 1);
    assert_eq!(store.delete_capsule(&k).unwrap(), 0, "delete is idempotent");
    assert_eq!(store.stats(&k).unwrap(), Default::default());
    assert_eq!(store.stats(&other).unwrap().decisions, 1);
    assert!(store.integrity_check().unwrap().is_ok());
}

#[test]
fn model_snapshots_latest_replace_and_prune() {
    let dir = TestDir::new("models");
    let store = dir.open();
    let k = key("acme");
    let snap = |version: u64, state: &[u8]| ModelSnapshot {
        key: k.clone(),
        version,
        reward_seq: version as i64 * 10,
        ts_ms: version as i64,
        state: state.to_vec(),
    };
    assert!(store.load_latest_model(&k).unwrap().is_none());
    for v in [1, 5, 3] {
        store.save_model(&snap(v, b"s")).unwrap();
    }
    assert_eq!(store.load_latest_model(&k).unwrap().unwrap().version, 5);
    // Saving an existing version replaces it.
    store.save_model(&snap(5, b"replaced")).unwrap();
    assert_eq!(
        store.load_latest_model(&k).unwrap().unwrap().state,
        b"replaced"
    );

    assert_eq!(store.prune_models(&k, 2).unwrap(), 1);
    assert_eq!(store.load_latest_model(&k).unwrap().unwrap().version, 5);
    assert_eq!(store.prune_models(&k, 2).unwrap(), 0);
    assert_eq!(store.prune_models(&k, 0).unwrap(), 2);
    assert!(store.load_latest_model(&k).unwrap().is_none());
}

#[test]
fn audit_lists_the_latest_events_oldest_first() {
    let dir = TestDir::new("audit");
    let store = dir.open();
    let k = key("acme");
    let seqs: Vec<i64> = (0..10)
        .map(|i| {
            store
                .append_audit(&k, i, &format!("event-{i}"), &format!(r#"{{"i":{i}}}"#))
                .unwrap()
        })
        .collect();
    let latest = store.list_audit(&k, 3).unwrap();
    let got: Vec<(i64, &str)> = latest.iter().map(|a| (a.seq, a.event.as_str())).collect();
    assert_eq!(
        got,
        vec![
            (seqs[7], "event-7"),
            (seqs[8], "event-8"),
            (seqs[9], "event-9")
        ]
    );
    assert_eq!(store.list_audit(&k, 100).unwrap().len(), 10);
    assert!(store.list_audit(&k, 0).unwrap().is_empty());
}

#[test]
fn invalid_records_are_rejected_before_any_write() {
    let dir = TestDir::new("invalid");
    let store = dir.open();
    let k = key("acme");
    let base = decision(&k, "d1", 1);
    let bad_decisions: Vec<(&str, DecisionRecord)> = vec![
        (
            "empty id",
            DecisionRecord {
                id: String::new(),
                ..base.clone()
            },
        ),
        (
            "long id",
            DecisionRecord {
                id: "x".repeat(MAX_ID_CHARS + 1),
                ..base.clone()
            },
        ),
        (
            "NUL id",
            DecisionRecord {
                id: "a\0b".into(),
                ..base.clone()
            },
        ),
        (
            "context",
            DecisionRecord {
                context: "{not json".into(),
                ..base.clone()
            },
        ),
        (
            "actions",
            DecisionRecord {
                actions: String::new(),
                ..base.clone()
            },
        ),
        (
            "eligible",
            DecisionRecord {
                eligible: "[1,]".into(),
                ..base.clone()
            },
        ),
        (
            "pmf",
            DecisionRecord {
                pmf: Some("[0.5,".into()),
                ..base.clone()
            },
        ),
        (
            "derived",
            DecisionRecord {
                derived: "{} {}".into(),
                ..base.clone()
            },
        ),
        (
            "probability > 1",
            DecisionRecord {
                probability: Some(1.5),
                ..base.clone()
            },
        ),
        (
            "probability NaN",
            DecisionRecord {
                probability: Some(f64::NAN),
                ..base.clone()
            },
        ),
        (
            "model_version",
            DecisionRecord {
                model_version: u64::MAX,
                ..base.clone()
            },
        ),
        (
            "mode",
            DecisionRecord {
                mode: String::new(),
                ..base.clone()
            },
        ),
        (
            "chosen_id",
            DecisionRecord {
                chosen_id: String::new(),
                ..base.clone()
            },
        ),
    ];
    for (what, d) in &bad_decisions {
        let err = store.insert_decision(d).unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)), "{what}: {err}");
    }
    // The batch path reports the same rows as Invalid, one by one.
    let batch: Vec<DecisionRecord> = bad_decisions.iter().map(|(_, d)| d.clone()).collect();
    let outcomes = store.insert_decisions(&batch).unwrap();
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, InsertOutcome::Invalid(_))),
        "{outcomes:?}"
    );
    store.insert_decision(&base).unwrap();

    let good = reward(&k, "d1", "d1", 1.0);
    let bad_rewards: Vec<(&str, RewardRecord)> = vec![
        (
            "NaN value",
            RewardRecord {
                value: f64::NAN,
                ..good.clone()
            },
        ),
        (
            "infinite norm",
            RewardRecord {
                value_norm: f64::INFINITY,
                ..good.clone()
            },
        ),
        (
            "detail",
            RewardRecord {
                detail: Some("{".into()),
                ..good.clone()
            },
        ),
        (
            "empty key",
            RewardRecord {
                idempotency_key: String::new(),
                ..good.clone()
            },
        ),
        (
            "empty decision",
            RewardRecord {
                decision_id: String::new(),
                ..good.clone()
            },
        ),
    ];
    for (what, r) in &bad_rewards {
        let err = store.insert_reward(r).unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)), "{what}: {err}");
    }

    let snap = ModelSnapshot {
        key: k.clone(),
        version: 1,
        reward_seq: 0,
        ts_ms: 0,
        state: vec![],
    };
    for bad in [
        ModelSnapshot {
            version: i64::MAX as u64 + 1,
            ..snap.clone()
        },
        ModelSnapshot {
            reward_seq: -1,
            ..snap.clone()
        },
    ] {
        let err = store.save_model(&bad).unwrap_err();
        assert!(matches!(err, StoreError::InvalidInput(_)), "{err}");
    }
    assert!(matches!(
        store.append_audit(&k, 0, "", "{}").unwrap_err(),
        StoreError::InvalidInput(_)
    ));
    assert!(matches!(
        store.append_audit(&k, 0, "event", "not json").unwrap_err(),
        StoreError::InvalidInput(_)
    ));

    let s = store.stats(&k).unwrap();
    assert_eq!((s.decisions, s.rewards), (1, 0));
    assert!(store.load_latest_model(&k).unwrap().is_none());
    assert!(store.list_audit(&k, 10).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Performance smoke test

#[test]
fn insert_throughput_smoke() {
    const N: usize = 20_000;
    const BATCH: usize = 512; // the write-behind queue's commit size
    let dir = TestDir::new("throughput");
    let store = dir.open();
    let single_key = key("perf-single");
    let batch_key = key("perf-batch");
    let singles: Vec<DecisionRecord> = (0..N)
        .map(|i| decision(&single_key, &format!("dec-{i:06}"), i as i64))
        .collect();
    let batched: Vec<DecisionRecord> = (0..N)
        .map(|i| decision(&batch_key, &format!("dec-{i:06}"), i as i64))
        .collect();

    let started = Instant::now();
    for d in &singles {
        assert_eq!(store.insert_decision(d).unwrap(), InsertOutcome::Inserted);
    }
    let single = started.elapsed();

    let started = Instant::now();
    for chunk in batched.chunks(BATCH) {
        let outcomes = store.insert_decisions(chunk).unwrap();
        assert!(outcomes.iter().all(|o| *o == InsertOutcome::Inserted));
    }
    let batch = started.elapsed();

    let single_rate = N as f64 / single.as_secs_f64();
    let batch_rate = N as f64 / batch.as_secs_f64();
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!(
        "eventstore insert throughput ({build} build, {N} decisions each): \
         single {single_rate:.0}/s ({single:.2?}, one transaction per decision); \
         batched {batch_rate:.0}/s ({batch:.2?}, {BATCH} per transaction)"
    );
    assert_eq!(store.stats(&single_key).unwrap().decisions, N as u64);
    assert_eq!(store.stats(&batch_key).unwrap().decisions, N as u64);
    // Lenient floors so a regression (an fsync or a table scan on the insert
    // path) fails loudly without flaking on slow CI machines.
    assert!(
        single_rate > 2_000.0,
        "single: only {single_rate:.0} inserts/s"
    );
    assert!(
        batch_rate > 2_000.0,
        "batched: only {batch_rate:.0} inserts/s"
    );
}
