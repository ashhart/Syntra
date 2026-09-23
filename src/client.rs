//! Local evaluation: decide in-process, learn on the server.
//!
//! A [`LocalDecider`] holds a copy of a capsule's published model and runs
//! the same [`Engine::decide`] the server runs, so a decision costs
//! microseconds and needs no network round trip; it keeps working through a
//! server outage with the last model it synced. Decisions and rewards are
//! queued and uploaded in batches (`decisions:batch`, then
//! `rewards:batch`); the server verifies every uploaded decision by
//! replaying it against the same published model and seed, stores it with
//! its propensity, and learns from its rewards exactly as if it had served
//! it. [`LocalDecider::sync`] picks up newer published models.
//!
//! ```no_run
//! use syntra::client::LocalDecider;
//! let decider = LocalDecider::connect("http://127.0.0.1:8787", "token", "acme", "prod", "router")?;
//! let d = decider.decide(serde_json::json!({"task": "code", "tokens": 812}))?;
//! // ... act on d.action, observe the outcome ...
//! decider.reward(&d.decision_id, 0.8)?;
//! decider.flush()?; // or run `start_background` once at startup
//! # Ok::<(), syntra::client::ClientError>(())
//! ```
//!
//! A decider is shared freely across threads. The decide path takes no
//! shared lock and writes no shared cache line: each thread keeps its own
//! reference to the current model (revalidated with one atomic load), its
//! own random stream, and appends to one of several queue shards.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::{Value, json};

use crate::decision::spec::RewardAggregation;
use crate::decision::{ActionSpec, DecideInput, DecisionSpec, Engine, SplitMix64};

/// An error from the client.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientError(pub String);

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientError {}

fn err(msg: impl Into<String>) -> ClientError {
    ClientError(msg.into())
}

/// A decision made locally.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalDecision {
    pub decision_id: String,
    pub action: String,
    pub action_index: usize,
    pub probability: f64,
    /// Eligible actions with their probabilities, most probable first.
    pub ranking: Vec<(String, f64)>,
    pub model_version: u64,
}

/// Optional parts of a decide request.
#[derive(Debug, Clone, Default)]
pub struct DecideOptions {
    /// Per-request actions; the spec's actions when `None`.
    pub actions: Option<Vec<ActionSpec>>,
    pub excluded_actions: Vec<String>,
    /// The incumbent action, required in `baselineExplore` mode.
    pub baseline_action: Option<String>,
}

/// What [`LocalDecider::flush`] uploaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlushReport {
    /// Decisions the server verified and stored (including retried uploads
    /// of ones it already had).
    pub decisions_accepted: usize,
    /// Decisions refused for good (did not replay, retired model, ...).
    pub decisions_rejected: usize,
    pub rewards_applied: usize,
    /// Rewards refused for good (unknown decision, invalid, ...).
    pub rewards_failed: usize,
    /// Events the server could not take right now (it is backlogged); they
    /// are queued again for the next flush.
    pub requeued: usize,
    /// The first few refusal messages, for logs.
    pub errors: Vec<String>,
}

impl FlushReport {
    fn note(&mut self, error: &Value) {
        if self.errors.len() < 10 {
            self.errors
                .push(error.as_str().unwrap_or("rejected").to_string());
        }
    }
}

/// Items per upload request (the server accepts up to 4096).
const UPLOAD_CHUNK: usize = 1000;
/// Queue shards; threads are spread over them round-robin.
const SHARDS: usize = 16;
/// Default bound on queued decisions plus rewards.
pub const DEFAULT_MAX_QUEUE: usize = 100_000;

struct Model {
    engine: Engine,
    version: u64,
    tag: Arc<str>,
}

/// A decision waiting for upload, kept as a struct so deciding does no
/// serialization.
struct PendingDecision {
    decision_id: String,
    ts_ms: i64,
    tag: Arc<str>,
    version: u64,
    seed: u64,
    context: Value,
    actions: Option<Vec<ActionSpec>>,
    excluded: Vec<String>,
    baseline: Option<String>,
    chosen: usize,
    probability: f64,
    pmf: Vec<f64>,
    eligible: Vec<usize>,
}

impl PendingDecision {
    /// The upload item. Deterministic, so a retried upload hashes the same.
    fn to_json(&self) -> Value {
        let mut input = json!({ "context": self.context });
        if let Some(a) = &self.actions {
            input["actions"] = json!(a);
        }
        if !self.excluded.is_empty() {
            input["excludedActions"] = json!(self.excluded);
        }
        if let Some(b) = &self.baseline {
            input["baselineAction"] = json!(b);
        }
        json!({
            "decisionId": self.decision_id,
            "tsMs": self.ts_ms,
            "modelTag": &*self.tag,
            "modelVersion": self.version,
            "seed": self.seed.to_string(),
            "input": input,
            "chosenIndex": self.chosen,
            "probability": self.probability,
            "pmf": self.pmf,
            "eligible": self.eligible,
        })
    }
}

struct PendingReward {
    decision_id: String,
    reward: f64,
    idempotency_key: Option<String>,
    detail: Option<Value>,
}

impl PendingReward {
    fn to_json(&self) -> Value {
        let mut item = json!({ "decisionId": self.decision_id, "reward": self.reward });
        if let Some(k) = &self.idempotency_key {
            item["idempotencyKey"] = json!(k);
        }
        if let Some(d) = &self.detail {
            item["detail"] = d.clone();
        }
        item
    }
}

#[derive(Default)]
struct Queue {
    decisions: Vec<PendingDecision>,
    rewards: Vec<PendingReward>,
}

/// One queue shard, on its own cache lines.
#[repr(align(128))]
#[derive(Default)]
struct Shard {
    queue: Mutex<Queue>,
    /// Mirror of the queue's length, readable without the lock.
    len: AtomicUsize,
}

struct CachedModel {
    decider: u64,
    generation: u64,
    model: Arc<Model>,
}

thread_local! {
    /// This thread's reference to its decider's current model.
    static MODEL: RefCell<Option<CachedModel>> = const { RefCell::new(None) };
    /// Seeds and decision-id bits for this thread, from the OS CSPRNG once.
    static RNG: RefCell<SplitMix64> = RefCell::new(SplitMix64::new(crate::decision::random_seed()));
    static SHARD: usize = NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % SHARDS;
}

static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);
static NEXT_DECIDER: AtomicU64 = AtomicU64::new(1);

pub struct LocalDecider {
    /// Process-unique; keys the thread-local model cache.
    id: u64,
    capsule_url: String,
    token: String,
    agent: ureq::Agent,
    model: RwLock<Arc<Model>>,
    /// Bumped after `model` changes.
    generation: AtomicU64,
    shards: Box<[Shard]>,
    max_queue: usize,
    /// One flush at a time (see `flush` for why).
    flush_lock: Mutex<()>,
    /// Draw counter for capsules whose spec fixes a seed.
    seed_counter: AtomicU64,
}

impl LocalDecider {
    /// Connect to a capsule and fetch its published model.
    pub fn connect(
        server: &str,
        token: &str,
        tenant: &str,
        job: &str,
        capsule: &str,
    ) -> Result<Self, ClientError> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build();
        let capsule_url = format!(
            "{}/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}",
            server.trim_end_matches('/')
        );
        let model = fetch_model(&agent, &capsule_url, token, None)?
            .ok_or_else(|| err("server returned no model"))?;
        Ok(LocalDecider {
            id: NEXT_DECIDER.fetch_add(1, Ordering::Relaxed),
            capsule_url,
            token: token.to_string(),
            agent,
            model: RwLock::new(Arc::new(model)),
            generation: AtomicU64::new(0),
            shards: (0..SHARDS).map(|_| Shard::default()).collect(),
            max_queue: DEFAULT_MAX_QUEUE,
            flush_lock: Mutex::new(()),
            seed_counter: AtomicU64::new(0),
        })
    }

    /// Bound the upload queue (decisions plus rewards); beyond it `decide`
    /// and `reward` return an error instead of dropping events.
    pub fn with_max_queue(mut self, max: usize) -> Self {
        self.max_queue = max.max(1);
        self
    }

    /// The model version decisions are currently made with.
    pub fn model_version(&self) -> u64 {
        self.model.read().unwrap().version
    }

    /// The server's tag for the model decisions are currently made with.
    pub fn model_tag(&self) -> String {
        self.model.read().unwrap().tag.to_string()
    }

    /// Fetch the server's current published model if it differs from the
    /// one in use. Returns true when the model changed.
    pub fn sync(&self) -> Result<bool, ClientError> {
        let current = self.model_tag();
        match fetch_model(&self.agent, &self.capsule_url, &self.token, Some(&current))? {
            Some(m) if *m.tag != *current => {
                *self.model.write().unwrap() = Arc::new(m);
                self.generation.fetch_add(1, Ordering::Release);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Run `f` with the current model, through this thread's cache.
    fn with_model<R>(&self, f: impl FnOnce(&Arc<Model>) -> R) -> R {
        let generation = self.generation.load(Ordering::Acquire);
        MODEL.with(|cell| {
            let mut cache = cell.borrow_mut();
            let fresh =
                matches!(&*cache, Some(c) if c.decider == self.id && c.generation == generation);
            if !fresh {
                // A sync between the load above and this read caches a
                // newer model under the older generation; the next call
                // reloads it, which is harmless.
                *cache = Some(CachedModel {
                    decider: self.id,
                    generation,
                    model: self.model.read().unwrap().clone(),
                });
            }
            f(&cache.as_ref().expect("cached model").model)
        })
    }

    /// Decide over the spec's actions.
    pub fn decide(&self, context: Value) -> Result<LocalDecision, ClientError> {
        self.decide_with(context, DecideOptions::default())
    }

    /// Decide with per-request actions, exclusions or a baseline.
    pub fn decide_with(
        &self,
        context: Value,
        options: DecideOptions,
    ) -> Result<LocalDecision, ClientError> {
        let context = match context {
            Value::Null => Value::Object(serde_json::Map::new()),
            v @ Value::Object(_) => v,
            _ => return Err(err("context must be a JSON object")),
        };
        let shard = &self.shards[SHARD.with(|s| *s)];
        if shard.len.load(Ordering::Relaxed) >= self.max_queue / SHARDS
            && self.pending() >= self.max_queue
        {
            return Err(err("upload queue is full; call flush() or raise the bound"));
        }
        let input = DecideInput {
            context,
            derived: Value::Null,
            actions: options.actions,
            excluded: options.excluded_actions,
            eligible: None,
            baseline: options.baseline_action,
        };
        let (pending, decision) = self.with_model(|model| {
            let (seed, id_bits) = RNG.with(|rng| {
                let mut rng = rng.borrow_mut();
                (rng.next_u64(), rng.next_u64())
            });
            // A capsule with a fixed seed gets reproducible local draws too.
            let seed = match model.engine.spec().seed {
                Some(base) => {
                    let n = self.seed_counter.fetch_add(1, Ordering::Relaxed);
                    SplitMix64::new(base ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)).next_u64()
                }
                None => seed,
            };
            let d = model
                .engine
                .decide(&input, seed)
                .map_err(|e| err(e.to_string()))?;
            let ts_ms = crate::store::now_ms();
            let decision_id = format!("loc_{:011x}{id_bits:016x}", ts_ms.max(0));
            let decision = LocalDecision {
                decision_id: decision_id.clone(),
                action: d.chosen_action().id.clone(),
                action_index: d.chosen,
                probability: d.probability,
                ranking: d
                    .ranking()
                    .into_iter()
                    .map(|(i, p)| (d.actions[i].id.clone(), p))
                    .collect(),
                model_version: model.version,
            };
            let pending = PendingDecision {
                decision_id,
                ts_ms,
                tag: model.tag.clone(),
                version: model.version,
                seed,
                context: Value::Null,
                actions: None,
                excluded: Vec::new(),
                baseline: None,
                chosen: d.chosen,
                probability: d.probability,
                pmf: d.pmf,
                eligible: d.eligible,
            };
            Ok::<_, ClientError>((pending, decision))
        })?;
        let pending = PendingDecision {
            context: input.context,
            actions: input.actions,
            excluded: input.excluded,
            baseline: input.baseline,
            ..pending
        };
        let mut q = shard.queue.lock().unwrap();
        q.decisions.push(pending);
        shard
            .len
            .store(q.decisions.len() + q.rewards.len(), Ordering::Relaxed);
        Ok(decision)
    }

    /// Queue a reward for a decision (made locally or on the server).
    pub fn reward(&self, decision_id: &str, reward: f64) -> Result<(), ClientError> {
        self.reward_with(decision_id, reward, None, None)
    }

    pub fn reward_with(
        &self,
        decision_id: &str,
        reward: f64,
        idempotency_key: Option<&str>,
        detail: Option<Value>,
    ) -> Result<(), ClientError> {
        if !reward.is_finite() {
            return Err(err("reward must be finite"));
        }
        let shard = &self.shards[SHARD.with(|s| *s)];
        if shard.len.load(Ordering::Relaxed) >= self.max_queue / SHARDS
            && self.pending() >= self.max_queue
        {
            return Err(err("upload queue is full; call flush() or raise the bound"));
        }
        let idempotency_key = match idempotency_key {
            Some(k) => Some(k.to_string()),
            // Under `sum` every reward counts, so give each one a key now:
            // a flush retried after a lost response must not count it twice.
            // (Under `first` the server keys rewards by decision id.)
            None if self.with_model(|m| m.engine.spec().rewards == RewardAggregation::Sum) => {
                let k = RNG.with(|rng| rng.borrow_mut().next_u64());
                Some(format!("{decision_id}:{k:016x}"))
            }
            None => None,
        };
        let mut q = shard.queue.lock().unwrap();
        q.rewards.push(PendingReward {
            decision_id: decision_id.to_string(),
            reward,
            idempotency_key,
            detail,
        });
        shard
            .len
            .store(q.decisions.len() + q.rewards.len(), Ordering::Relaxed);
        Ok(())
    }

    /// Events waiting to be uploaded.
    pub fn pending(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.len.load(Ordering::Relaxed))
            .sum()
    }

    /// Upload queued decisions, then queued rewards. On a transport error
    /// the events stay queued for the next flush; so do events the server
    /// was too busy to take (see [`FlushReport::requeued`]).
    pub fn flush(&self) -> Result<FlushReport, ClientError> {
        // Rewards are taken before decisions, one flush at a time: a reward
        // is always queued after its decision, so any reward taken here has
        // its decision taken here too (or uploaded by an earlier flush) and
        // never reaches the server first.
        let _one = self.flush_lock.lock().unwrap();
        let mut rewards = Vec::new();
        for shard in self.shards.iter() {
            let mut q = shard.queue.lock().unwrap();
            rewards.append(&mut q.rewards);
            shard.len.store(q.decisions.len(), Ordering::Relaxed);
        }
        let mut decisions = Vec::new();
        for shard in self.shards.iter() {
            let mut q = shard.queue.lock().unwrap();
            decisions.append(&mut q.decisions);
            shard.len.store(q.rewards.len(), Ordering::Relaxed);
        }

        let mut report = FlushReport::default();
        let mut retry_decisions = Vec::new();
        let mut decisions = decisions.into_iter();
        loop {
            let chunk: Vec<PendingDecision> = decisions.by_ref().take(UPLOAD_CHUNK).collect();
            if chunk.is_empty() {
                break;
            }
            let body = json!({ "decisions": chunk.iter().map(PendingDecision::to_json).collect::<Vec<_>>() });
            let v = match self.post("decisions:batch", &body) {
                Ok(v) => v,
                Err(e) => {
                    retry_decisions.extend(chunk);
                    retry_decisions.extend(decisions);
                    self.requeue(retry_decisions, rewards);
                    return Err(e);
                }
            };
            report.decisions_accepted += v["accepted"].as_u64().unwrap_or(0) as usize;
            let mut retry_index = Vec::new();
            for r in v["rejected"].as_array().into_iter().flatten() {
                match r["index"].as_u64() {
                    Some(k) if r["retryable"].as_bool() == Some(true) => {
                        retry_index.push(k as usize);
                    }
                    _ => {
                        report.decisions_rejected += 1;
                        report.note(&r["error"]);
                    }
                }
            }
            if !retry_index.is_empty() {
                retry_decisions.extend(
                    chunk
                        .into_iter()
                        .enumerate()
                        .filter(|(k, _)| retry_index.contains(k))
                        .map(|(_, d)| d),
                );
            }
        }
        // Rewards wait while any decision is waiting: a reward uploaded
        // ahead of its decision would be refused as unknown.
        if !retry_decisions.is_empty() {
            report.requeued = retry_decisions.len() + rewards.len();
            self.requeue(retry_decisions, rewards);
            return Ok(report);
        }

        let mut retry_rewards = Vec::new();
        let mut rewards = rewards.into_iter();
        loop {
            let chunk: Vec<PendingReward> = rewards.by_ref().take(UPLOAD_CHUNK).collect();
            if chunk.is_empty() {
                break;
            }
            let body =
                json!({ "rewards": chunk.iter().map(PendingReward::to_json).collect::<Vec<_>>() });
            let v = match self.post("rewards:batch", &body) {
                Ok(v) => v,
                Err(e) => {
                    retry_rewards.extend(chunk);
                    retry_rewards.extend(rewards);
                    self.requeue(Vec::new(), retry_rewards);
                    return Err(e);
                }
            };
            let results = v["results"].as_array().cloned().unwrap_or_default();
            for (item, r) in chunk.into_iter().zip(results.iter()) {
                if r["ok"].as_bool() == Some(true) {
                    report.rewards_applied += 1;
                } else if r["status"].as_u64() == Some(503) {
                    retry_rewards.push(item);
                } else {
                    report.rewards_failed += 1;
                    report.note(&r["error"]);
                }
            }
        }
        report.requeued = retry_rewards.len();
        self.requeue(Vec::new(), retry_rewards);
        Ok(report)
    }

    /// Put events back at the front of the first shard.
    fn requeue(&self, decisions: Vec<PendingDecision>, rewards: Vec<PendingReward>) {
        if decisions.is_empty() && rewards.is_empty() {
            return;
        }
        let shard = &self.shards[0];
        let mut q = shard.queue.lock().unwrap();
        let mut d = decisions;
        d.append(&mut q.decisions);
        q.decisions = d;
        let mut r = rewards;
        r.append(&mut q.rewards);
        q.rewards = r;
        shard
            .len
            .store(q.decisions.len() + q.rewards.len(), Ordering::Relaxed);
    }

    fn post(&self, route: &str, body: &Value) -> Result<Value, ClientError> {
        let resp = self
            .agent
            .post(&format!("{}/{route}", self.capsule_url))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string());
        match resp {
            Ok(r) => read_json(r).map_err(|e| err(format!("{route}: {e}"))),
            Err(ureq::Error::Status(code, r)) => Err(err(format!(
                "{route}: HTTP {code}: {}",
                r.into_string().unwrap_or_default()
            ))),
            Err(e) => Err(err(format!("{route}: {e}"))),
        }
    }

    /// Flush and sync every `every` on a background thread until the
    /// returned handle is dropped (which flushes once more).
    pub fn start_background(self: &Arc<Self>, every: Duration) -> Background {
        let stop = Arc::new(AtomicBool::new(false));
        let me = Arc::clone(self);
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("syntra-local-sync".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    std::thread::park_timeout(every);
                    if me.pending() > 0 {
                        let _ = me.flush();
                    }
                    let _ = me.sync();
                }
                let _ = me.flush();
            })
            .expect("spawn background sync");
        Background {
            stop,
            handle: Some(handle),
        }
    }
}

/// Stops the background thread (after a final flush) when dropped.
pub struct Background {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}

/// `GET .../model?snapshot=true`; `None` when the server answers 304.
fn fetch_model(
    agent: &ureq::Agent,
    capsule_url: &str,
    token: &str,
    current: Option<&str>,
) -> Result<Option<Model>, ClientError> {
    let mut req = agent
        .get(&format!("{capsule_url}/model?snapshot=true"))
        .set("Authorization", &format!("Bearer {token}"));
    if let Some(tag) = current {
        req = req.set("If-None-Match", &format!("\"{tag}\""));
    }
    let resp = match req.call() {
        Ok(r) if r.status() == 304 => return Ok(None),
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            return Err(err(format!(
                "model: HTTP {code}: {}",
                r.into_string().unwrap_or_default()
            )));
        }
        Err(e) => return Err(err(format!("model: {e}"))),
    };
    let v = read_json(resp).map_err(|e| err(format!("model: {e}")))?;
    let spec = DecisionSpec::from_json(&v["spec"]).map_err(err)?;
    let bytes = base64_decode(
        v["snapshot"]
            .as_str()
            .ok_or_else(|| err("model: no snapshot"))?,
    )?;
    let tag = v["modelTag"]
        .as_str()
        .ok_or_else(|| err("model: no modelTag"))?
        .to_string();
    if crate::server::runtime::model_tag(&spec, &bytes) != tag {
        return Err(err("model: snapshot does not match its tag"));
    }
    let engine = Engine::restore(spec, &bytes).map_err(err)?;
    let version = engine.model_version();
    Ok(Some(Model {
        engine,
        version,
        tag: tag.into(),
    }))
}

fn read_json(resp: ureq::Response) -> Result<Value, String> {
    let text = resp
        .into_string()
        .map_err(|e| format!("reading response: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("bad response: {e}"))
}

/// Standard base64 with padding.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, ClientError> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return Err(err("invalid base64 length"));
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for chunk in s.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 {
            return Err(err("invalid base64 padding"));
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c).ok_or_else(|| err("invalid base64 character"))?;
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

    #[test]
    fn base64_roundtrips_with_the_server_encoder() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let enc = crate::server::query::base64_encode(&data);
            assert_eq!(base64_decode(&enc).unwrap(), data);
        }
        assert!(base64_decode("abc").is_err());
        assert!(base64_decode("a===").is_err());
        assert!(base64_decode("ab!=").is_err());
    }
}
