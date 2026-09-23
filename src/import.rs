//! `syntra import dsjson`: bring decision logs exported from Azure AI
//! Personalizer or a Vowpal Wabbit decision service (DSJSON, one JSON
//! event per line) into a capsule's event store, so off-policy evaluation
//! runs on that history from day one.
//!
//! Each event becomes a decision with its logged propensities and, when it
//! has one, its reward (`-_label_cost`, or the sum of its `o` outcomes).
//! Rewards are stored as not learned, so the capsule's model ignores them,
//! unless `--learn` is given; then the model replays them the next time the
//! capsule loads (a warm start). Import into a store no server is running
//! on (the command refuses a live one unless `--force`).
//!
//! Mapping (see the DSJSON description in the Vowpal Wabbit wiki):
//!
//! - `EventId` → decision id; `Timestamp` (ISO 8601, UTC) → `tsMs`.
//! - `c`: keys not starting with `_` are the context. A namespace given as
//!   an array of objects is merged into one object, as the Personalizer
//!   `rank` route merges `contextFeatures`, so imported and live decisions
//!   featurize alike.
//! - `c._multi[i]` → action `i`: its id from `_tag`, else `i.id`, else
//!   `id`, else `action<i>`; its other non-`_` namespaces (arrays of
//!   objects merged) are the features. The `i` namespace holding only
//!   `constant` and `id` (the action id as a feature) is dropped.
//! - `a` (1-based, ranked) and `p` → the eligible actions and PMF; `a[0]`
//!   is the chosen action with probability `p[0]`.
//! - `DeferredAction: true` events without an `ActionTaken` outcome were
//!   never activated and are skipped; so are multi-slot (`_slots`) events.
//! - `_skipLearn: true` events keep their reward, not learned even with
//!   `--learn`.
//!
//! Re-importing the same file is harmless: decisions and rewards already
//! present (same ids) are skipped.

use std::io::BufRead;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::decision::{ActionSpec, DecisionSpec};
use crate::eventstore::{
    CapsuleKey, DecisionRecord, EventStore, InsertOutcome, RewardOutcome, RewardRecord, SqliteStore,
};
use crate::store::{Store, sha256_hex};

/// Counts reported by an import.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub lines: usize,
    pub decisions: usize,
    pub rewards: usize,
    pub already_present: usize,
    pub skipped_deferred: usize,
    pub skipped_multislot: usize,
    pub invalid: usize,
    /// The first few invalid lines, with the reason.
    pub errors: Vec<String>,
}

impl ImportReport {
    pub fn to_json(&self) -> Value {
        json!({
            "lines": self.lines,
            "decisions": self.decisions,
            "rewards": self.rewards,
            "alreadyPresent": self.already_present,
            "skipped": {
                "deferredNotActivated": self.skipped_deferred,
                "multiSlot": self.skipped_multislot,
                "invalid": self.invalid,
            },
            "errors": self.errors,
        })
    }
}

/// One event, parsed.
struct Event {
    decision: DecisionRecord,
    reward: Option<f64>,
    learn: bool,
}

enum Parsed {
    Event(Box<Event>),
    Deferred,
    MultiSlot,
}

/// Merge an array of objects into one object (later keys win); objects
/// are kept, other values are returned as they are.
fn merge_namespace(v: &Value) -> Value {
    match v {
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            let mut out = Map::new();
            for item in items {
                if let Value::Object(m) = item {
                    for (k, x) in m {
                        out.insert(k.clone(), x.clone());
                    }
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The non-`_` keys of an object, with namespace arrays merged.
fn namespaces(obj: &Map<String, Value>, skip: &[&str]) -> Map<String, Value> {
    obj.iter()
        .filter(|(k, _)| !k.starts_with('_') && !skip.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), merge_namespace(v)))
        .collect()
}

/// `YYYY-MM-DDTHH:MM:SS[.fraction]Z` (UTC) to milliseconds since the epoch.
pub fn parse_timestamp(s: &str) -> Result<i64, String> {
    let bad = || format!("timestamp {s:?} is not ISO 8601 UTC (YYYY-MM-DDTHH:MM:SS[.f]Z)");
    let s = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix("+00:00"))
        .ok_or_else(bad)?;
    let (date, time) = s.split_once('T').ok_or_else(bad)?;
    let mut d = date.splitn(3, '-');
    let (y, m, day): (i64, i64, i64) = (
        d.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
        d.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
        d.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
    );
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, ""),
    };
    let mut t = hms.splitn(3, ':');
    let (hh, mm, ss): (i64, i64, i64) = (
        t.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
        t.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
        t.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return Err(bad());
    }
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let ms: i64 = frac
        .chars()
        .chain("000".chars())
        .take(3)
        .collect::<String>()
        .parse()
        .map_err(|_| bad())?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

fn parse_event(key: &CapsuleKey, line: &str) -> Result<Parsed, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let obj = v.as_object().ok_or("not a JSON object")?;
    let c = obj
        .get("c")
        .and_then(Value::as_object)
        .ok_or("no context object `c`")?;
    if c.contains_key("_slots") || obj.contains_key("_outcomes") {
        return Ok(Parsed::MultiSlot);
    }
    let outcomes: &[Value] = obj.get("o").and_then(Value::as_array).map_or(&[], |o| o);
    let activated = outcomes
        .iter()
        .any(|o| o.get("ActionTaken").and_then(Value::as_bool) == Some(true));
    if obj.get("DeferredAction").and_then(Value::as_bool) == Some(true) && !activated {
        return Ok(Parsed::Deferred);
    }

    let id = obj
        .get("EventId")
        .and_then(Value::as_str)
        .ok_or("no EventId")?
        .to_string();
    crate::server::decide::validate_event_id_str(&id)?;
    let ts_ms = match obj.get("Timestamp").and_then(Value::as_str) {
        Some(t) => parse_timestamp(t)?,
        None => return Err("no Timestamp".into()),
    };

    let multi = c
        .get("_multi")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or("no actions (`c._multi`)")?;
    let mut actions = Vec::with_capacity(multi.len());
    for (i, a) in multi.iter().enumerate() {
        let a = a
            .as_object()
            .ok_or_else(|| format!("c._multi[{i}] is not an object"))?;
        let id = a
            .get("_tag")
            .and_then(Value::as_str)
            .or_else(|| a.get("i").and_then(|x| x.get("id")).and_then(Value::as_str))
            .or_else(|| a.get("id").and_then(Value::as_str))
            .map(String::from)
            .unwrap_or_else(|| format!("action{i}"));
        // `i: {constant, id}` is the action id as a feature; the action's
        // own id feature replaces it.
        let id_only = a
            .get("i")
            .and_then(Value::as_object)
            .is_some_and(|m| m.keys().all(|k| k == "constant" || k == "id"));
        let skip: &[&str] = if id_only { &["i", "id"] } else { &["id"] };
        actions.push(ActionSpec {
            id,
            features: namespaces(a, skip),
        });
    }
    let n = actions.len();

    let ranked: Vec<usize> = obj
        .get("a")
        .and_then(Value::as_array)
        .ok_or("no ranked actions `a`")?
        .iter()
        .map(|x| {
            x.as_u64()
                .filter(|&k| k >= 1 && (k as usize) <= n)
                .map(|k| k as usize - 1)
                .ok_or_else(|| format!("`a` holds {x}, not an action number in 1..={n}"))
        })
        .collect::<Result<_, _>>()?;
    let probs: Vec<f64> = obj
        .get("p")
        .and_then(Value::as_array)
        .ok_or("no probabilities `p`")?
        .iter()
        .map(|x| x.as_f64().ok_or_else(|| format!("`p` holds {x}")))
        .collect::<Result<_, _>>()?;
    if ranked.is_empty() || ranked.len() != probs.len() {
        return Err(format!(
            "`a` ({}) and `p` ({}) must be non-empty and the same length",
            ranked.len(),
            probs.len()
        ));
    }
    let mut pmf = vec![0.0; n];
    for (&i, &p) in ranked.iter().zip(&probs) {
        if !(0.0..=1.0).contains(&p) {
            return Err(format!("probability {p} is outside [0, 1]"));
        }
        pmf[i] = p;
    }
    let total: f64 = pmf.iter().sum();
    if (total - 1.0).abs() > 1e-3 {
        return Err(format!("probabilities sum to {total}, not 1"));
    }
    let chosen = ranked[0];
    let probability = probs[0];
    if probability <= 0.0 {
        return Err("the chosen action has probability 0".into());
    }
    let mut eligible = ranked.clone();
    eligible.sort_unstable();

    let reward = match obj.get("_label_cost").and_then(Value::as_f64) {
        Some(cost) => Some(-cost),
        None => {
            let values: Vec<f64> = outcomes
                .iter()
                .filter_map(|o| o.get("v").and_then(Value::as_f64))
                .collect();
            (!values.is_empty()).then(|| values.iter().sum())
        }
    };
    if reward.is_some_and(|r| !r.is_finite()) {
        return Err("reward is not finite".into());
    }
    let learn = obj.get("_skipLearn").and_then(Value::as_bool) != Some(true);

    let context = Value::Object(namespaces(c, &[]));
    let decision = DecisionRecord {
        id,
        key: key.clone(),
        ts_ms,
        model_version: 0,
        mode: "imported".into(),
        context: context.to_string(),
        actions: serde_json::to_string(&actions).map_err(|e| e.to_string())?,
        eligible: serde_json::to_string(&eligible).map_err(|e| e.to_string())?,
        pmf: Some(serde_json::to_string(&pmf).map_err(|e| e.to_string())?),
        chosen_index: chosen as i64,
        chosen_id: actions[chosen].id.clone(),
        probability: Some(probability),
        seed: 0,
        derived: Value::Null.to_string(),
        reason: None,
        request_sha256: sha256_hex(line.as_bytes()),
        program_sha256: None,
    };
    Ok(Parsed::Event(Box::new(Event {
        decision,
        reward,
        learn,
    })))
}

/// Import DSJSON lines from `input` into capsule `t/j/c` of the store at
/// `root`. The capsule is created (actions come with each event) if it
/// does not exist.
pub fn import_dsjson(
    root: &Path,
    capsule: &str,
    input: impl BufRead,
    learn: bool,
    force: bool,
) -> Result<ImportReport, String> {
    if !force && let Some(why) = crate::backup::live_server(root) {
        return Err(format!(
            "refusing to import into live store {}: {why} (stop it, or pass --force and restart it after)",
            root.display()
        ));
    }
    let mut parts = capsule.splitn(3, '/');
    let (Some(t), Some(j), Some(c)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!(
            "--capsule {capsule:?}: expected tenant/job/capsule"
        ));
    };
    let key = CapsuleKey::new(t, j, c).map_err(|e| format!("--capsule {capsule:?}: {e}"))?;
    let store = Store::open_or_init(&root.to_string_lossy())?;
    let spec = if store.capsule_exists(t, j, c) {
        store
            .load_spec(t, j, c)?
            .ok_or_else(|| format!("capsule {capsule} has no spec"))?
    } else {
        let spec = DecisionSpec::default();
        store.save_spec(t, j, c, &spec)?;
        spec
    };
    let events = SqliteStore::open(store.events_path()).map_err(|e| e.to_string())?;

    let mut report = ImportReport::default();
    let mut batch: Vec<Event> = Vec::new();
    let flush = |batch: &mut Vec<Event>, report: &mut ImportReport| -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        let decisions: Vec<DecisionRecord> = batch.iter().map(|e| e.decision.clone()).collect();
        let outcomes = events
            .insert_decisions(&decisions)
            .map_err(|e| e.to_string())?;
        let mut rewards = Vec::new();
        for (e, o) in batch.iter().zip(outcomes) {
            match o {
                InsertOutcome::Inserted => report.decisions += 1,
                InsertOutcome::Duplicate(_) => report.already_present += 1,
                InsertOutcome::Invalid(msg) => {
                    report.invalid += 1;
                    if report.errors.len() < 10 {
                        report.errors.push(format!("{}: {msg}", e.decision.id));
                    }
                    continue;
                }
            }
            if let Some(value) = e.reward {
                let learned = learn && e.learn;
                let mut detail = json!({ "imported": "dsjson" });
                if !learned {
                    detail["learned"] = json!(false);
                }
                rewards.push(RewardRecord {
                    seq: 0,
                    decision_id: e.decision.id.clone(),
                    key: key.clone(),
                    ts_ms: e.decision.ts_ms,
                    value,
                    value_norm: spec.reward.normalize(value),
                    idempotency_key: e.decision.id.clone(),
                    detail: Some(detail.to_string()),
                });
            }
        }
        for o in events.insert_rewards(&rewards).map_err(|e| e.to_string())? {
            if matches!(o, RewardOutcome::Applied(_)) {
                report.rewards += 1;
            }
        }
        batch.clear();
        Ok(())
    };

    for (n, line) in input.lines().enumerate() {
        let line = line.map_err(|e| format!("reading line {}: {e}", n + 1))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        report.lines += 1;
        match parse_event(&key, line) {
            Ok(Parsed::Event(e)) => batch.push(*e),
            Ok(Parsed::Deferred) => report.skipped_deferred += 1,
            Ok(Parsed::MultiSlot) => report.skipped_multislot += 1,
            Err(msg) => {
                report.invalid += 1;
                if report.errors.len() < 10 {
                    report.errors.push(format!("line {}: {msg}", n + 1));
                }
            }
        }
        if batch.len() >= 1000 {
            flush(&mut batch, &mut report)?;
        }
    }
    flush(&mut batch, &mut report)?;
    Ok(report)
}

/// `syntra import dsjson ...`; returns the process exit code.
pub fn cli(args: &[String]) -> i32 {
    const USAGE: &str = "\
Usage:
  syntra import dsjson --store <root> --capsule <tenant/job/capsule> [--learn] [--force] <file.json | ->

Imports decision logs exported from Azure AI Personalizer or a Vowpal Wabbit
decision service (DSJSON, one event per line) with their propensities and
rewards, for `syntra evaluate` and `POST .../evaluate`. Rewards are not
learned unless --learn (then the model replays them when the capsule next
loads). Refuses a store a server is running on unless --force.";
    if args.first().map(String::as_str) != Some("dsjson")
        || args.iter().any(|a| a == "--help" || a == "-h")
    {
        eprintln!("{USAGE}");
        return if args.iter().any(|a| a == "--help" || a == "-h") {
            0
        } else {
            2
        };
    }
    let mut store = None;
    let mut capsule = None;
    let mut file = None;
    let (mut learn, mut force) = (false, false);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--store" => {
                i += 1;
                store = args.get(i).cloned();
            }
            "--capsule" => {
                i += 1;
                capsule = args.get(i).cloned();
            }
            "--learn" => learn = true,
            "--force" => force = true,
            f if f.starts_with("--") => {
                eprintln!("syntra import: unknown option {f:?}\n{USAGE}");
                return 2;
            }
            f => file = Some(f.to_string()),
        }
        i += 1;
    }
    let (Some(store), Some(capsule), Some(file)) = (store, capsule, file) else {
        eprintln!("syntra import: --store, --capsule and a file are required\n{USAGE}");
        return 2;
    };
    let result = if file == "-" {
        import_dsjson(
            Path::new(&store),
            &capsule,
            std::io::stdin().lock(),
            learn,
            force,
        )
    } else {
        match std::fs::File::open(&file) {
            Ok(f) => import_dsjson(
                Path::new(&store),
                &capsule,
                std::io::BufReader::new(f),
                learn,
                force,
            ),
            Err(e) => Err(format!("{file}: {e}")),
        }
    };
    match result {
        Ok(report) => {
            println!("{}", report.to_json());
            0
        }
        Err(e) => {
            eprintln!("syntra import: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(
            parse_timestamp("2019-08-13T20:40:28.7880000Z").unwrap(),
            1_565_728_828_788
        );
        assert_eq!(
            parse_timestamp("2000-02-29T12:00:00.5Z").unwrap(),
            951_825_600_500
        );
        assert_eq!(
            parse_timestamp("2024-12-31T23:59:59+00:00").unwrap(),
            1_735_689_599_000
        );
        for bad in [
            "2019-08-13 20:40:28Z",
            "2019-13-01T00:00:00Z",
            "2019-08-13T20:40:28",
            "x",
        ] {
            assert!(parse_timestamp(bad).is_err(), "{bad}");
        }
    }

    fn key() -> CapsuleKey {
        CapsuleKey::new("t", "j", "c").unwrap()
    }

    const EVENT: &str = r#"{"_label_cost":-1,"_label_probability":0.8,"_label_Action":2,"_labelIndex":1,"Timestamp":"2019-08-13T20:40:28.7880000Z","Version":"1","EventId":"13c3e3f18b2d4e70","a":[2,1,3],"c":{"User":[{"tier":"pro"},{"geo":"UK"}],"_synthetic":false,"_multi":[{"_tag":"news","i":{"constant":1,"id":"news"},"Topic":[{"kind":"news"}]},{"_tag":"sports","i":{"constant":1,"id":"sports"},"Topic":[{"kind":"sports"}]},{"_tag":"music","i":{"constant":1,"id":"music"}}]},"p":[0.8,0.1,0.1],"VWState":{"m":"x"}}"#;

    #[test]
    fn an_event_maps_to_a_decision() {
        let Parsed::Event(e) = parse_event(&key(), EVENT).unwrap() else {
            panic!("not an event")
        };
        let d = &e.decision;
        assert_eq!(d.id, "13c3e3f18b2d4e70");
        assert_eq!(d.ts_ms, 1_565_728_828_788);
        assert_eq!(d.chosen_id, "sports");
        assert_eq!(d.chosen_index, 1);
        assert_eq!(d.probability, Some(0.8));
        assert_eq!(d.pmf.as_deref(), Some("[0.1,0.8,0.1]"));
        assert_eq!(d.eligible, "[0,1,2]");
        let ctx: Value = serde_json::from_str(&d.context).unwrap();
        assert_eq!(ctx, json!({"User": {"tier": "pro", "geo": "UK"}}));
        let actions: Vec<ActionSpec> = serde_json::from_str(&d.actions).unwrap();
        assert_eq!(actions[0].id, "news");
        assert_eq!(
            Value::Object(actions[0].features.clone()),
            json!({"Topic": {"kind": "news"}})
        );
        assert!(actions[2].features.is_empty());
        assert_eq!(e.reward, Some(1.0));
        assert!(e.learn);
    }

    #[test]
    fn deferred_multislot_and_bad_events() {
        let mut deferred: Value = serde_json::from_str(EVENT).unwrap();
        deferred["DeferredAction"] = json!(true);
        assert!(matches!(
            parse_event(&key(), &deferred.to_string()).unwrap(),
            Parsed::Deferred
        ));
        deferred["o"] = json!([{"EventId": "x", "ActionTaken": true}]);
        assert!(matches!(
            parse_event(&key(), &deferred.to_string()).unwrap(),
            Parsed::Event(_)
        ));
        let mut slots: Value = serde_json::from_str(EVENT).unwrap();
        slots["c"]["_slots"] = json!([{}]);
        assert!(matches!(
            parse_event(&key(), &slots.to_string()).unwrap(),
            Parsed::MultiSlot
        ));
        for (field, value) in [
            ("p", json!([0.5, 0.1, 0.1])),
            ("a", json!([4, 1, 2])),
            ("EventId", json!("bad id with spaces")),
            ("Timestamp", json!("yesterday")),
        ] {
            let mut bad: Value = serde_json::from_str(EVENT).unwrap();
            bad[field] = value;
            assert!(parse_event(&key(), &bad.to_string()).is_err(), "{field}");
        }
        assert!(parse_event(&key(), "not json").is_err());
    }
}
