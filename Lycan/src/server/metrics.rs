use std::collections::HashMap;
use std::sync::Mutex;

use super::state::State;

/// In-process Prometheus-compatible counters/histograms.
///
/// Implemented manually rather than via the `prometheus` crate so we don't
/// pull in a dep right before the parallel work might also be reaching for
/// the same one. Format matches the Prometheus exposition spec; one-line
/// COUNTER/GAUGE/HISTOGRAM types.
pub(super) struct Metrics {
    /// {kind="decide"|"feedback", status="ok"|"refused"|"err", ...}
    /// → count.
    /// Keyed by `(kind, tenant, job, capsule, status)`.
    pub(super) request_total: Mutex<HashMap<(String, String, String, String, String), u64>>,
    /// `/decide` latency in seconds, bucketed.
    pub(super) decide_latency_seconds: Mutex<LatencyHistogram>,
    /// Capsule lifecycle (0=warmup, 1=active, 2=frozen). Polled at /metrics scrape time.
    /// No accumulator state needed — derived from disk.
    /// `{tenant, job, capsule, candidate}` → trials at most-recent observation.
    /// Polled at scrape time, also from disk.
    /// Refusal counter, keyed by `(tenant, job, capsule, reason)`.
    pub(super) refusals_total: Mutex<HashMap<(String, String, String, String), u64>>,
}

pub(super) struct LatencyHistogram {
    /// Cumulative bucket counts (le=0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, +Inf).
    buckets: [u64; 12],
    sum_seconds: f64,
    count: u64,
}

const LATENCY_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

impl LatencyHistogram {
    fn new() -> Self {
        Self { buckets: [0; 12], sum_seconds: 0.0, count: 0 }
    }

    fn observe(&mut self, seconds: f64) {
        self.sum_seconds += seconds;
        self.count += 1;
        let mut placed = false;
        for (i, le) in LATENCY_BUCKETS.iter().enumerate() {
            if seconds <= *le {
                for j in i..12 { self.buckets[j] += 1; }
                placed = true;
                break;
            }
        }
        if !placed {
            self.buckets[11] += 1;
        }
    }
}

impl Metrics {
    pub(super) fn new() -> Self {
        Self {
            request_total: Mutex::new(HashMap::new()),
            decide_latency_seconds: Mutex::new(LatencyHistogram::new()),
            refusals_total: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn record_request(&self, kind: &str, tenant: &str, job: &str, capsule: &str, status: &str) {
        let key = (kind.to_string(), tenant.to_string(), job.to_string(),
                   capsule.to_string(), status.to_string());
        let mut m = self.request_total.lock().unwrap();
        *m.entry(key).or_insert(0) += 1;
    }

    pub(super) fn observe_decide_latency(&self, seconds: f64) {
        let mut h = self.decide_latency_seconds.lock().unwrap();
        h.observe(seconds);
    }

    pub(super) fn record_refusal(&self, tenant: &str, job: &str, capsule: &str, reason: &str) {
        let key = (tenant.to_string(), job.to_string(),
                   capsule.to_string(), reason.to_string());
        let mut m = self.refusals_total.lock().unwrap();
        *m.entry(key).or_insert(0) += 1;
    }
}

fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Render the current metrics state as Prometheus exposition text.
/// Walks the store on each call to surface lifecycle + meta-bandit trial
/// gauges; only the counters/histogram live in-process.
pub(super) fn render_metrics(state: &State) -> String {
    let mut out = String::new();

    out.push_str("# HELP syntra_requests_total Total Syntra HTTP requests, by kind/status.\n");
    out.push_str("# TYPE syntra_requests_total counter\n");
    {
        let m = state.metrics.request_total.lock().unwrap();
        for ((kind, tenant, job, capsule, status), count) in m.iter() {
            out.push_str(&format!(
                "syntra_requests_total{{kind=\"{}\",tenant=\"{}\",job=\"{}\",capsule=\"{}\",status=\"{}\"}} {}\n",
                escape_label(kind), escape_label(tenant), escape_label(job),
                escape_label(capsule), escape_label(status), count,
            ));
        }
    }

    out.push_str("# HELP syntra_decide_latency_seconds /decide latency histogram.\n");
    out.push_str("# TYPE syntra_decide_latency_seconds histogram\n");
    {
        let h = state.metrics.decide_latency_seconds.lock().unwrap();
        for (i, le) in LATENCY_BUCKETS.iter().enumerate() {
            out.push_str(&format!(
                "syntra_decide_latency_seconds_bucket{{le=\"{}\"}} {}\n",
                le, h.buckets[i],
            ));
        }
        out.push_str(&format!(
            "syntra_decide_latency_seconds_bucket{{le=\"+Inf\"}} {}\n",
            h.buckets[11],
        ));
        out.push_str(&format!(
            "syntra_decide_latency_seconds_sum {}\n", h.sum_seconds,
        ));
        out.push_str(&format!(
            "syntra_decide_latency_seconds_count {}\n", h.count,
        ));
    }

    out.push_str("# HELP syntra_refusals_total Total refused /decide responses, by reason.\n");
    out.push_str("# TYPE syntra_refusals_total counter\n");
    {
        let m = state.metrics.refusals_total.lock().unwrap();
        for ((tenant, job, capsule, reason), count) in m.iter() {
            out.push_str(&format!(
                "syntra_refusals_total{{tenant=\"{}\",job=\"{}\",capsule=\"{}\",reason=\"{}\"}} {}\n",
                escape_label(tenant), escape_label(job), escape_label(capsule),
                escape_label(reason), count,
            ));
        }
    }

    // Lifecycle + meta-bandit trial gauges are derived from on-disk state
    // at scrape time. Walking the entire store on every scrape is cheap
    // for development-scale deployments; if it gets expensive we'll cache.
    out.push_str("# HELP syntra_warmup_state Capsule lifecycle (0=warmup,1=active,2=frozen).\n");
    out.push_str("# TYPE syntra_warmup_state gauge\n");
    out.push_str("# HELP syntra_meta_bandit_trials Meta-bandit trial count per candidate.\n");
    out.push_str("# TYPE syntra_meta_bandit_trials gauge\n");

    for (tenant, job, capsule) in state.store.list_all_capsules() {
        if let Some(w) = state.store.load_warmup_state_in_job(&tenant, &job, &capsule) {
            let v: u8 = if w.is_active() { 1 } else if w.is_frozen() { 2 } else { 0 };
            out.push_str(&format!(
                "syntra_warmup_state{{tenant=\"{}\",job=\"{}\",capsule=\"{}\"}} {}\n",
                escape_label(&tenant), escape_label(&job), escape_label(&capsule), v,
            ));
        }
        if let Ok(mem) = state.store.load_memory_in_job(&tenant, &job, &capsule) {
            for sm in mem.strategies.values() {
                if let Some(mb) = &sm.meta_bandit {
                    for c in &mb.candidates {
                        out.push_str(&format!(
                            "syntra_meta_bandit_trials{{tenant=\"{}\",job=\"{}\",capsule=\"{}\",candidate=\"{}\"}} {}\n",
                            escape_label(&tenant), escape_label(&job),
                            escape_label(&capsule), c.id.as_str(), c.trials,
                        ));
                    }
                }
            }
        }
    }

    out
}
