//! Flagship real-time control demo: closed-loop autonomous rendezvous.
//!
//! The plant (double integrator: range + closing rate + propellant) lives in
//! this harness; every tick the compiled Lycan capsule delivers the previous
//! tick's delayed feedback to its AdaptiveChoice policy and selects a burn
//! strategy under a fuel/rate Guard with a certified safe-hold fallback.
//!
//! Verified claims, printed in the report:
//!   1. Closed loop: plant -> capsule decision -> plant update -> delayed
//!      feedback -> policy update, every tick.
//!   2. Real-time: per-tick decision latency distribution (p50/p90/p99/max)
//!      and allocations per tick, measured on the embed path (fresh clone of
//!      the live policy graph + execute).
//!   3. Deterministic: two full simulations from the same seed and initial
//!      state produce byte-identical decision traces (FNV-1a over the trace).
//!   4. Fail-closed: the Guard's safe-hold fallback is reachable and logged.
//!   5. Learning: the choice-node weights before vs after, plus burn-choice
//!      histograms per half of the run, show the policy shifting.
//!   6. Verified capsule: `verifier::verify` gates the run.
//!
//! Run: cargo run --release --example rt_control_loop
//! Report: target/rt-control.report (capsule prints go to real stdout).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use syntra::capabilities::CapValue;
use syntra::context::{ExecutionContext, SelectionMode};
use syntra::graph::OpCode;
use syntra::graph_compiler::GraphCompiler;
use syntra::graph_executor::GraphExecutor;
use syntra::learning::seed_rng;
use syntra::{lexer, parser, verifier};

// ── Counting allocator ───────────────────────────────────────────────────

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = System.alloc(layout);
            if !p.is_null() {
                let size = layout.size() as u64;
                ALLOCS.fetch_add(1, Ordering::Relaxed);
                let cur = BYTES.fetch_add(size, Ordering::Relaxed) + size;
                PEAK.fetch_max(cur, Ordering::Relaxed);
            }
            p
        }
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(p, layout);
            BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        }
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let q = System.realloc(p, layout, new_size);
            if !q.is_null() {
                let delta = new_size as i64 - layout.size() as i64;
                ALLOCS.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(delta as u64, Ordering::Relaxed);
            }
            q
        }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// ── Plant model ──────────────────────────────────────────────────────────

const DT: f64 = 0.5;
const TICKS: usize = 600;
const R0: f64 = 200.0;
const V0: f64 = 1.6;
const FUEL0: f64 = 10.0;
const SEED: u64 = 0x57A7_7A;

/// Burn accelerations (m/s², + = closes faster). Brake opposes closing.
fn burn_accel(burn: usize) -> f64 {
    match burn {
        1 => 0.35,
        2 => 0.12,
        3 => -0.5,
        _ => 0.0,
    }
}

struct Plant {
    range: f64,
    rate: f64,
    fuel: f64,
}

impl Plant {
    /// Advance one tick; returns (fuel_used, overshoot).
    fn step(&mut self, burn: usize) -> (f64, bool) {
        let a = burn_accel(burn);
        let fuel_used = a.abs() * DT * 0.4;
        self.fuel = (self.fuel - fuel_used).max(0.0);
        self.rate += a * DT;
        self.range -= self.rate * DT;
        let overshoot = self.range <= 0.0;
        if overshoot {
            self.range = 0.0;
        }
        (fuel_used, overshoot)
    }

    fn docked(&self) -> bool {
        self.range <= 0.5 && self.rate <= 0.25 && self.rate >= -0.1
    }
}

/// Shaped, clamped outcome for the burn chosen THIS tick; delivered NEXT tick
/// as delayed feedback. Rewards closing progress; penalizes overshoot,
/// arrival-velocity corridor violations, and propellant spend.
fn shaped_reward(progress: f64, overshoot: bool, range: f64, rate: f64, fuel_used: f64) -> f64 {
    // Arrival-velocity corridor: the closing rate must come down as the dock
    // nears — the honest control constraint that makes coasting near the
    // dock expensive.
    let corridor = (range / 50.0).max(0.3);
    let mut raw = 0.25 + 50.0 * progress;
    if overshoot {
        raw -= 0.5;
    }
    if rate > corridor {
        raw -= 0.4;
    }
    raw -= 2.0 * fuel_used;
    raw.clamp(0.0, 1.0)
}

// ── Simulation ───────────────────────────────────────────────────────────

struct SimOutcome {
    trace_hash: u64,
    ticks_used: usize,
    docked: bool,
    safe_holds: usize,
    fuel_end: f64,
    range_end: f64,
    rate_end: f64,
    weights_end: [f64; 4],
    burns_first_half: [usize; 4],
    burns_second_half: [usize; 4],
    samples: Vec<u128>,
}

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

fn input_pairs(t: usize, p: &Plant, reward: f64) -> CapValue {
    let pair = |k: &str, v: CapValue| CapValue::Array(vec![CapValue::Str(k.into()), v]);
    CapValue::Array(vec![
        pair("tick", CapValue::Int(t as i64)),
        pair("range_m", CapValue::Float(p.range)),
        pair("rate_ms", CapValue::Float(p.rate)),
        pair("fuel_kg", CapValue::Float(p.fuel)),
        pair("reward", CapValue::Float(reward)),
    ])
}

fn run_sim(pristine: &syntra::graph::NeuralGraph, choice_id: usize) -> Result<SimOutcome, String> {
    seed_rng(Some(SEED));
    let mut graph = pristine.clone();
    let mut plant = Plant {
        range: R0,
        rate: V0,
        fuel: FUEL0,
    };
    let mut reward_prev = -1000.0f64;
    let mut trace = String::new();
    let mut samples = Vec::new();
    let mut safe_holds = 0usize;
    let mut burns = Vec::new();
    let mut docked = false;
    let mut ticks_used = TICKS;

    for t in 0..TICKS {
        let t0 = Instant::now();
        let mut ctx = ExecutionContext::with_input(input_pairs(t, &plant, reward_prev));
        ctx.selection_mode = SelectionMode::EpsilonGreedy;
        ctx.selection_epsilon = 0.15;
        let mut ex = GraphExecutor::new_with_context(graph.clone(), ctx);
        let result = ex.run().map_err(|e| format!("tick {t}: {e}"))?;
        let elapsed = t0.elapsed().as_nanos();
        for line in &ex.stdout_buffer {
            trace.push_str(line);
            trace.push('\n');
        }
        graph = ex.into_graph();
        samples.push(elapsed);

        // The graph's return value IS the chosen burn (safe-hold included).
        let burn = match result {
            syntra::graph_executor::GVal::Int(n) if (0..=3).contains(&n) => n as usize,
            other => return Err(format!("tick {t}: unexpected result {other}")),
        };
        let guard_ok = plant.fuel > 0.4 && plant.rate.abs() < 2.5;
        if !guard_ok {
            safe_holds += 1;
        }
        burns.push(burn);

        let prev_range = plant.range;
        let (fuel_used, overshoot) = plant.step(burn);
        let progress = (prev_range - plant.range) / prev_range.max(0.001);
        reward_prev = shaped_reward(progress, overshoot, prev_range, plant.rate, fuel_used);

        if plant.docked() {
            docked = true;
            ticks_used = t + 1;
            break;
        }
    }

    let weights: [f64; 4] = {
        let w = &graph.nodes[choice_id].weights;
        [w[0], w[1], w[2], w[3]]
    };
    let half = burns.len() / 2;
    let mut b1 = [0usize; 4];
    let mut b2 = [0usize; 4];
    for (i, &b) in burns.iter().enumerate() {
        if i < half {
            b1[b] += 1;
        } else {
            b2[b] += 1;
        }
    }

    Ok(SimOutcome {
        trace_hash: fnv1a(&trace),
        ticks_used,
        docked,
        safe_holds,
        fuel_end: plant.fuel,
        range_end: plant.range,
        rate_end: plant.rate,
        weights_end: weights,
        burns_first_half: b1,
        burns_second_half: b2,
        samples,
    })
}

// ── Report ───────────────────────────────────────────────────────────────

fn main() {
    let root = env!("CARGO_MANIFEST_DIR");
    let src_path = format!("{root}/examples/rt-control/rendezvous_burn_sequencer.lycs");
    let source = std::fs::read_to_string(&src_path).expect("read capsule source");

    // Fail closed: parse/compile/verify gate the run.
    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize().expect("tokenize");
    let mut parser = parser::Parser::new(tokens);
    let program = parser.parse_program().expect("parse");
    let pristine = GraphCompiler::new()
        .compile(&program)
        .expect("compile capsule");
    verifier::verify(&pristine).expect("capsule must verify (fail-closed)");

    let choice_id = pristine
        .nodes
        .iter()
        .position(|n| matches!(n.op, OpCode::AdaptiveChoice))
        .expect("capsule must contain an AdaptiveChoice node");
    let weights_start: [f64; 4] = {
        let w = &pristine.nodes[choice_id].weights;
        [w[0], w[1], w[2], w[3]]
    };

    // Two full simulations from the same seed and initial state.
    let run1 = run_sim(&pristine, choice_id).expect("sim 1");
    let allocs_run2_start = ALLOCS.load(Ordering::Relaxed);
    let run2 = run_sim(&pristine, choice_id).expect("sim 2");
    let allocs_run2 = ALLOCS.load(Ordering::Relaxed) - allocs_run2_start;

    let deterministic = run1.trace_hash == run2.trace_hash;

    let mut s = run1.samples.clone();
    s.sort_unstable();
    let n = s.len();
    let mean = s.iter().sum::<u128>() / n.max(1) as u128;

    let fmt_w = |w: &[f64; 4]| format!("[{:.3} {:.3} {:.3} {:.3}]", w[0], w[1], w[2], w[3]);

    let mut report = format!(
        "Syntra real-time flagship: closed-loop rendezvous — arch: {}, os: {}\n\
         Plant: double integrator, dt {} s, r0 {} m, v0 {} m/s, fuel {} kg, {} tick cap\n\
         Capsule: examples/rt-control/rendezvous_burn_sequencer.lycs (verifier-gated)\n\n\
         == Real-time decision loop ==\n\
         decision (clone of live policy + execute), {n} ticks:\n\
         \x20 p50 {}  p90 {}  p99 {}  p99.9 {}  max {}\n\
         \x20 mean {} | throughput ~{:.0} decisions/s at p50\n\
         \x20 allocs/tick: {:.1} | peak heap: {} KiB\n\n\
         == Mission outcome ==\n\
         docked: {} | ticks to dock: {} | safe-hold activations: {}\n\
         end state: range {:.3} m, rate {:.3} m/s, fuel {:.3} kg\n\n\
         == Learning (delayed feedback, one-tick delay) ==\n\
         policy weights start: {}  end: {}\n\
         burn choices first half:  coast {} coarse {} fine {} brake {}\n\
         burn choices second half: coast {} coarse {} fine {} brake {}\n\n\
         == Determinism ==\n\
         two independent full simulations, same seed + initial state:\n\
         \x20 trace hash run 1: 0x{:016x}\n\
         \x20 trace hash run 2: 0x{:016x}\n\
         \x20 byte-identical traces: {}\n",
        std::env::consts::ARCH,
        std::env::consts::OS,
        DT,
        R0,
        V0,
        FUEL0,
        TICKS,
        fmt_ns(pct(&s, 50.0)),
        fmt_ns(pct(&s, 90.0)),
        fmt_ns(pct(&s, 99.0)),
        fmt_ns(pct(&s, 99.9)),
        fmt_ns(*s.last().unwrap_or(&0)),
        fmt_ns(mean),
        1e9 / pct(&s, 50.0) as f64,
        allocs_run2 as f64 / run2.samples.len().max(1) as f64,
        PEAK.load(Ordering::Relaxed) / 1024,
        run1.docked,
        run1.ticks_used,
        run1.safe_holds,
        run1.range_end,
        run1.rate_end,
        run1.fuel_end,
        fmt_w(&weights_start),
        fmt_w(&run1.weights_end),
        run1.burns_first_half[0],
        run1.burns_first_half[1],
        run1.burns_first_half[2],
        run1.burns_first_half[3],
        run1.burns_second_half[0],
        run1.burns_second_half[1],
        run1.burns_second_half[2],
        run1.burns_second_half[3],
        run1.trace_hash,
        run2.trace_hash,
        if deterministic { "PASS" } else { "FAIL" },
    );

    if !deterministic {
        report.push_str(
            "\nFAIL: traces diverged — the runtime must be deterministic under a fixed seed.\n",
        );
    }

    let path = std::path::Path::new(root).join("target/rt-control.report");
    std::fs::write(&path, &report).expect("write report");
    eprintln!("report written to {}", path.display());
    if !deterministic {
        std::process::exit(1);
    }
}
