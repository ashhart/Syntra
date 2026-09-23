//! Prometheus exposition for `/metrics`.
//!
//! The decide path only touches atomics. Per-capsule gauges come from the
//! runtimes already in memory; nothing here reads the store.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::state::State;

/// Upper bounds of the decide-latency buckets, in seconds: 5 µs to 100 ms.
const LATENCY_BUCKETS: [f64; 14] = [
    0.000_005, 0.000_010, 0.000_025, 0.000_050, 0.000_100, 0.000_250, 0.000_500, 0.001, 0.0025,
    0.005, 0.010, 0.025, 0.050, 0.100,
];

/// Lock-free cumulative histogram.
pub struct Histogram {
    buckets: [AtomicU64; 14],
    overflow: AtomicU64,
    sum_nanos: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Histogram {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            overflow: AtomicU64::new(0),
            sum_nanos: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, d: Duration) {
        let secs = d.as_secs_f64();
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_nanos
            .fetch_add(d.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
        match LATENCY_BUCKETS.iter().position(|le| secs <= *le) {
            Some(i) => self.buckets[i].fetch_add(1, Ordering::Relaxed),
            None => self.overflow.fetch_add(1, Ordering::Relaxed),
        };
    }

    fn render(&self, name: &str, help: &str, out: &mut String) {
        use std::fmt::Write;
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} histogram");
        let mut cumulative = 0;
        for (i, le) in LATENCY_BUCKETS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            let _ = writeln!(out, "{name}_bucket{{le=\"{le}\"}} {cumulative}");
        }
        cumulative += self.overflow.load(Ordering::Relaxed);
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {cumulative}");
        let sum = self.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9;
        let _ = writeln!(out, "{name}_sum {sum}");
        let _ = writeln!(out, "{name}_count {}", self.count.load(Ordering::Relaxed));
    }
}

pub struct Metrics {
    /// `(route, status)` → count. Route labels are canonical templates such
    /// as `capsule.decide`, never raw paths, so cardinality stays bounded.
    requests: Mutex<HashMap<(&'static str, u16), u64>>,
    /// Server-side time spent in `/decide`, excluding network and HTTP
    /// parsing.
    pub decide_latency: Histogram,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            requests: Mutex::new(HashMap::new()),
            decide_latency: Histogram::new(),
        }
    }
}

impl Metrics {
    pub fn record_request(&self, route: &'static str, status: u16) {
        *self
            .requests
            .lock()
            .unwrap()
            .entry((route, status))
            .or_insert(0) += 1;
    }

    pub fn observe_decide(&self, d: Duration) {
        self.decide_latency.observe(d);
    }
}

fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

pub fn render(state: &State) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(4096);
    let m = &state.metrics;

    let _ = writeln!(
        out,
        "# HELP syntra_requests_total HTTP requests by route and status."
    );
    let _ = writeln!(out, "# TYPE syntra_requests_total counter");
    let mut rows: Vec<_> = m
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect();
    rows.sort();
    for ((route, status), n) in rows {
        let _ = writeln!(
            out,
            "syntra_requests_total{{route=\"{route}\",status=\"{status}\"}} {n}"
        );
    }

    m.decide_latency.render(
        "syntra_decide_seconds",
        "Server-side time spent choosing and logging one decision.",
        &mut out,
    );

    let w = &state.writer.stats;
    for (name, help, v) in [
        (
            "syntra_decisions_committed_total",
            "Decision records committed to the event store.",
            w.committed.load(Ordering::Relaxed),
        ),
        (
            "syntra_decision_batches_total",
            "Write-behind batches committed.",
            w.batches.load(Ordering::Relaxed),
        ),
        (
            "syntra_decisions_rejected_backlog_total",
            "Decide calls refused with 503 because the decision log queue was full.",
            w.rejected_full.load(Ordering::Relaxed),
        ),
        (
            "syntra_rewards_committed_total",
            "Reward records committed to the event store.",
            w.rewards_committed.load(Ordering::Relaxed),
        ),
        (
            "syntra_events_lost_total",
            "Decision or reward records dropped after repeated commit failures.",
            w.failed.load(Ordering::Relaxed),
        ),
        (
            "syntra_rewards_refused_after_apply_total",
            "Rewards applied to a model but refused by the event store (should stay 0).",
            w.rewards_refused_after_apply.load(Ordering::Relaxed),
        ),
    ] {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} counter");
        let _ = writeln!(out, "{name} {v}");
    }
    let _ = writeln!(
        out,
        "# HELP syntra_decision_log_backlog Decisions queued but not yet committed."
    );
    let _ = writeln!(out, "# TYPE syntra_decision_log_backlog gauge");
    let _ = writeln!(
        out,
        "syntra_decision_log_backlog {}",
        state.writer.backlog()
    );

    let _ = writeln!(
        out,
        "# HELP syntra_model_version Updates applied to a loaded capsule's model."
    );
    let _ = writeln!(out, "# TYPE syntra_model_version gauge");
    let mut runtimes = state.runtimes.loaded();
    runtimes.sort_by(|a, b| a.key.cmp(&b.key));
    for rt in &runtimes {
        let version = rt.engine.read().unwrap().model_version();
        let _ = writeln!(
            out,
            "syntra_model_version{{tenant=\"{}\",job=\"{}\",capsule=\"{}\"}} {version}",
            escape_label(rt.key.tenant()),
            escape_label(rt.key.job()),
            escape_label(rt.key.capsule())
        );
    }
    let _ = writeln!(
        out,
        "# HELP syntra_uptime_seconds Seconds since the server started."
    );
    let _ = writeln!(out, "# TYPE syntra_uptime_seconds gauge");
    let _ = writeln!(
        out,
        "syntra_uptime_seconds {}",
        state.started_at.elapsed().as_secs()
    );
    out
}
