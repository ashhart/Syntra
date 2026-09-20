//! Real-time baseline bench for the Lycan graph executor.
//!
//! Measures, at the library embed level (not the HTTP appliance):
//!   1. Per-decision latency distribution: p50 / p90 / p99 / p99.9 / max.
//!      A "decision" is the honest embed cost: clone the pristine graph,
//!      construct a fresh executor, run to completion.
//!   2. Heap allocations per decision (counting global allocator).
//!   3. Run-to-run determinism: seeded RNG + pristine graph per decision;
//!      32 repetitions must produce an identical result+stdout hash.
//!
//! Run: cargo run --release --example rt_baseline
//! Scope: single process, single thread, release build. Report hardware in
//! the surrounding material; these numbers are machine-relative.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use syntra::capabilities::CapValue;
use syntra::context::ExecutionContext;
use syntra::graph::NeuralGraph;
use syntra::graph_executor::GraphExecutor;
use syntra::learning::seed_rng;
use syntra::verifier;

// ── Counting global allocator ────────────────────────────────────────────

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static DEALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
/// Allocation size histogram: index = log2 ceiling bucket of requested size.
static SIZES: [AtomicU64; 17] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn size_bucket(size: usize) -> usize {
    let bits = usize::BITS - size.max(1).leading_zeros();
    bits.min(16) as usize
}

/// Pass-through to System until a dhat Profiler is alive (RT_DHAT mode);
/// forwarding here lets the counting wrapper and dhat compose.
static DHAT: dhat::Alloc = dhat::Alloc;

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = DHAT.alloc(layout);
            if !p.is_null() {
                let size = layout.size() as u64;
                ALLOCS.fetch_add(1, Ordering::Relaxed);
                SIZES[size_bucket(layout.size())].fetch_add(1, Ordering::Relaxed);
                let cur = BYTES.fetch_add(size, Ordering::Relaxed) + size;
                PEAK.fetch_max(cur, Ordering::Relaxed);
            }
            p
        }
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe {
            DHAT.dealloc(p, layout);
            DEALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        }
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let q = DHAT.realloc(p, layout, new_size);
            if !q.is_null() {
                let delta = new_size as i64 - layout.size() as i64;
                ALLOCS.fetch_add(1, Ordering::Relaxed);
                let cur = BYTES.fetch_add(delta as u64, Ordering::Relaxed) as i64 + delta;
                if cur >= 0 {
                    PEAK.fetch_max(cur as u64, Ordering::Relaxed);
                }
            }
            q
        }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ── Capsule registry ─────────────────────────────────────────────────────

struct CapsuleSpec {
    name: &'static str,
    path: String,
    input: Option<CapValue>,
}

fn anomaly_input() -> CapValue {
    // Shape matches `json_to_capvalue` on JSON objects: array of [key, value].
    let history: Vec<CapValue> = (0..30)
        .map(|i| CapValue::Float(100.0 + (i % 7) as f64 * 3.0))
        .collect();
    CapValue::Array(vec![
        CapValue::Array(vec![
            CapValue::Str("latency_history".into()),
            CapValue::Array(history),
        ]),
        CapValue::Array(vec![
            CapValue::Str("current_latency".into()),
            CapValue::Float(240.0),
        ]),
    ])
}

fn capsules() -> Vec<CapsuleSpec> {
    let root = env!("CARGO_MANIFEST_DIR");
    let specs = [
        (
            "adaptive-router (2 KB)",
            "examples/lycan-internals/demo_adaptive.lyc",
            None,
        ),
        (
            "chaos-control (31 KB)",
            "examples/lycan-internals/demo_control_chaos.lyc",
            None,
        ),
        (
            "anomaly-zscore-router",
            "examples/anomaly-routing/program.lyc",
            Some(anomaly_input()),
        ),
    ];
    specs
        .into_iter()
        .map(|(name, path, input)| CapsuleSpec {
            name,
            path: format!("{root}/{path}"),
            input,
        })
        .collect()
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn fnv1a(data: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn pct(sorted: &[u128], p: f64) -> u128 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    let idx = ((p / 100.0) * (n - 1) as f64).round() as usize;
    sorted[idx.min(n - 1)]
}

fn fmt_ns(ns: u128) -> String {
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

/// One full decision at the embed level: pristine graph → executor → run.
/// Returns (elapsed_ns, result_display, stdout_text).
fn decision_once(
    pristine: &NeuralGraph,
    input: Option<&CapValue>,
) -> Result<(u128, String, String), String> {
    let t0 = Instant::now();
    let ctx = match input {
        Some(v) => ExecutionContext::with_input(v.clone()),
        None => ExecutionContext::with_input(CapValue::Null),
    };
    let mut ex = GraphExecutor::new_with_context(pristine.clone(), ctx);
    let result = ex.run().map_err(|e| format!("{e}"))?;
    let elapsed = t0.elapsed().as_nanos();
    let stdout = ex.stdout_buffer.join("\n");
    Ok((elapsed, format!("{result}"), stdout))
}

struct CapsuleReport {
    name: String,
    nodes: usize,
    decode_verify_us: u128,
    samples: Vec<u128>,
    clone_p50: u128,
    allocs_per_decision: f64,
    bytes_per_decision: f64,
    size_hist: [u64; 17],
    deterministic: bool,
    reps_identical: usize,
    hash: u64,
    error: Option<String>,
}

fn bench_capsule(spec: &CapsuleSpec) -> CapsuleReport {
    let mut r = CapsuleReport {
        name: spec.name.into(),
        nodes: 0,
        decode_verify_us: 0,
        samples: vec![],
        clone_p50: 0,
        allocs_per_decision: 0.0,
        bytes_per_decision: 0.0,
        size_hist: [0; 17],
        deterministic: false,
        reps_identical: 0,
        hash: 0,
        error: None,
    };

    let data = match std::fs::read(&spec.path) {
        Ok(d) => d,
        Err(e) => {
            r.error = Some(format!("read: {e}"));
            return r;
        }
    };

    // One-time cost: decode + verify.
    let t0 = Instant::now();
    let pristine = match NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => {
            r.error = Some(format!("decode: {e}"));
            return r;
        }
    };
    if let Err(e) = verifier::verify(&pristine) {
        r.error = Some(format!("verify: {e}"));
        return r;
    }
    r.decode_verify_us = t0.elapsed().as_micros();
    r.nodes = pristine.nodes.len();

    // Smoke: the first decision must succeed before anything is measured.
    if let Err(e) = decision_once(&pristine, spec.input.as_ref()) {
        r.error = Some(format!("run: {e}"));
        return r;
    }

    // Determinism: 32 reps, re-seeded RNG each rep, pristine graph each rep.
    let mut identical = 0;
    let mut first_hash = 0u64;
    for i in 0..32 {
        seed_rng(Some(0x57A7_7A));
        let (_, out, stdout) = match decision_once(&pristine, spec.input.as_ref()) {
            Ok(v) => v,
            Err(e) => {
                r.error = Some(format!("det run: {e}"));
                return r;
            }
        };
        let h = fnv1a(&format!("{out}\n{stdout}"));
        if i == 0 {
            first_hash = h;
        }
        if h == first_hash {
            identical += 1;
        }
    }
    r.reps_identical = identical;
    r.deterministic = identical == 32;
    r.hash = first_hash;

    // Calibration: 200 runs → pick N for a ~2 s measurement window.
    let mut cal = Vec::new();
    for _ in 0..200 {
        if let Ok((ns, _, _)) = decision_once(&pristine, spec.input.as_ref()) {
            cal.push(ns);
        }
    }
    let mean_cal = cal.iter().sum::<u128>() / cal.len().max(1) as u128;
    let n = ((2_000_000_000u128 / mean_cal.max(1)) as usize).clamp(1_000, 100_000);

    // Measured loop (fresh clone + exec per decision).
    let a0 = ALLOCS.load(Ordering::Relaxed);
    let b0 = BYTES.load(Ordering::Relaxed);
    let mut s0 = [0u64; 17];
    for (i, b) in SIZES.iter().enumerate() {
        s0[i] = b.load(Ordering::Relaxed);
    }
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        if let Ok((ns, _, _)) = decision_once(&pristine, spec.input.as_ref()) {
            samples.push(ns);
        }
    }
    let a1 = ALLOCS.load(Ordering::Relaxed);
    let b1 = BYTES.load(Ordering::Relaxed);
    for (i, b) in SIZES.iter().enumerate() {
        r.size_hist[i] = b.load(Ordering::Relaxed) - s0[i];
    }
    r.samples = samples;

    // Clone-only decomposition.
    let mut clone_samples = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        let g = pristine.clone();
        std::hint::black_box(&g);
        clone_samples.push(t.elapsed().as_nanos());
    }
    clone_samples.sort_unstable();
    r.clone_p50 = pct(&clone_samples, 50.0);

    r.allocs_per_decision = (a1 - a0) as f64 / n as f64;
    r.bytes_per_decision = (b1.wrapping_sub(b0) as i64) as f64 / n as f64;
    r
}

fn format_report(rep: &CapsuleReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "== {} ==", rep.name);
    if let Some(e) = &rep.error {
        let _ = writeln!(out, "   ERROR: {e}");
        return out;
    }
    let _ = writeln!(
        out,
        "   graph: {} nodes | decode+verify: {} µs (one-time)",
        rep.nodes, rep.decode_verify_us
    );
    let mut s = rep.samples.clone();
    s.sort_unstable();
    let n = s.len();
    let mean = s.iter().sum::<u128>() / n.max(1) as u128;
    let _ = writeln!(out, "   decision (clone+exec), {n} samples:");
    let _ = writeln!(
        out,
        "     p50 {}  p90 {}  p99 {}  p99.9 {}  max {}",
        fmt_ns(pct(&s, 50.0)),
        fmt_ns(pct(&s, 90.0)),
        fmt_ns(pct(&s, 99.0)),
        fmt_ns(pct(&s, 99.9)),
        fmt_ns(*s.last().unwrap_or(&0))
    );
    let _ = writeln!(
        out,
        "     mean {} | throughput ~{:.0}/s at p50 | clone-only p50 {}",
        fmt_ns(mean),
        1e9 / pct(&s, 50.0) as f64,
        fmt_ns(rep.clone_p50)
    );
    let _ = writeln!(
        out,
        "     allocs/decision: {:.1} | bytes/decision: {:.0}",
        rep.allocs_per_decision, rep.bytes_per_decision
    );
    let total: u64 = rep.size_hist.iter().sum();
    if total > 0 {
        let _ = writeln!(
            out,
            "     alloc size buckets (2^lo–2^hi-1 bytes, share of {total}):"
        );
        for (i, c) in rep.size_hist.iter().enumerate() {
            if *c > 0 {
                let _ = writeln!(
                    out,
                    "       {:>2}-{:>2}: {:>10}  {:>5.1}%",
                    if i == 0 { 0 } else { 1 << (i - 1) },
                    (1usize << i).saturating_sub(1).max(1),
                    c,
                    *c as f64 / total as f64 * 100.0
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "     determinism: {}/32 identical reps, fnv1a 0x{:016x} {}",
        rep.reps_identical,
        rep.hash,
        if rep.deterministic { "PASS" } else { "FAIL" }
    );
    out.push('\n');
    out
}

fn main() {
    // RT_DHAT=1: profile ONE decision per capsule with dhat (stack-traced
    // allocations), dump target/dhat-rt.json, skip timing. Run under the dev
    // profile for symbols: cargo run --example rt_baseline (no --release).
    if std::env::var("RT_DHAT").is_ok() {
        let root = env!("CARGO_MANIFEST_DIR");
        {
            let _prof = dhat::Profiler::builder()
                .file_name("target/dhat-rt.json")
                .build();
            for spec in capsules() {
                let data = std::fs::read(&spec.path).expect("read capsule");
                let ng = NeuralGraph::from_bytes(&data).expect("decode");
                verifier::verify(&ng).expect("verify");
                let (ms, _, _) = decision_once(&ng, spec.input.as_ref()).expect("decision");
                eprintln!("{}: one decision, {} ns (dev profile)", spec.name, ms);
            }
        }
        let _ = root;
        return;
    }

    let mut report = format!(
        "Syntra real-time baseline — arch: {}, os: {}, release build, single thread\n\
         Scope: library embed path (pristine clone → execute). Not the HTTP appliance path.\n\n",
        std::env::consts::ARCH,
        std::env::consts::OS
    );

    for spec in capsules() {
        report.push_str(&format_report(&bench_capsule(&spec)));
    }

    report.push_str(&format!(
        "peak heap during run: {} KiB\n",
        PEAK.load(Ordering::Relaxed) / 1024
    ));

    // Capsule Print nodes emit to real stdout during measurement; the report
    // goes to a file so the bench can run with stdout silenced.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/rt-baseline.report");
    std::fs::write(&path, &report).expect("write report");
    eprintln!("report written to {}", path.display());
}
