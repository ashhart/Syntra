//! Local-evaluation benchmark: in-process decide latency with
//! [`LocalDecider`], then how fast the server verifies and stores the
//! uploaded decisions and rewards.
//!
//! ```text
//! syntra serve --addr 127.0.0.1:8787 --store ./bench-store --admin-key bench
//! cargo run --release --example bench_local -- \
//!     --addr 127.0.0.1:8787 --key bench --threads 8 --decisions 100000
//! ```
//!
//! It creates (or reuses) capsule `bench/bench/local` with three actions,
//! trains it briefly through the decider so the model is not trivial, then
//! times `decide` calls (each includes queueing the upload record) on
//! `--threads` threads sharing one decider, and finally uploads everything
//! with `flush`. Numbers depend on the machine; report them with the
//! hardware.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use syntra::client::LocalDecider;

struct Args {
    addr: String,
    key: String,
    threads: usize,
    decisions: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        addr: "127.0.0.1:8787".into(),
        key: String::new(),
        threads: 1,
        decisions: 100_000,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--addr" => a.addr = v,
            "--key" => a.key = v,
            "--threads" => a.threads = v.parse().expect("--threads"),
            "--decisions" => a.decisions = v.parse().expect("--decisions"),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    a
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let a = parse_args();
    let spec = r#"{"actions":[{"id":"small","features":{"cost":0.1}},{"id":"medium","features":{"cost":0.4}},{"id":"large","features":{"cost":1.0}}]}"#;
    let url = format!(
        "http://{}/v1/tenants/bench/jobs/bench/capsules/local/spec",
        a.addr
    );
    let put = ureq::put(&url)
        .set("Authorization", &format!("Bearer {}", a.key))
        .send_string(spec);
    if let Err(e) = put {
        panic!("spec PUT failed: {e}");
    }
    let server = format!("http://{}", a.addr);
    let decider = Arc::new(
        LocalDecider::connect(&server, &a.key, "bench", "bench", "local")
            .expect("connect")
            .with_max_queue(usize::MAX),
    );

    let tasks = ["chat", "code", "extract", "summarize"];
    let tiers = ["free", "pro", "enterprise"];
    let context = |i: usize| {
        json!({
            "task": tasks[i % tasks.len()],
            "tier": tiers[(i / 4) % tiers.len()],
            "promptTokens": 50 + (i * 37) % 4000,
        })
    };

    // Train a little so predictions are not all zero, then sync the model.
    for i in 0..2_000 {
        let d = decider.decide(context(i)).unwrap();
        let reward = match (d.action.as_str(), tasks[i % tasks.len()]) {
            ("large", "code") | ("small", "chat") | ("medium", _) => 1.0,
            _ => 0.0,
        };
        decider.reward(&d.decision_id, reward).unwrap();
    }
    let warm = decider.flush().expect("warmup flush");
    assert_eq!(warm.decisions_rejected, 0, "{warm:?}");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    decider.sync().expect("sync");
    println!(
        "model version {} ({})",
        decider.model_version(),
        decider.model_tag()
    );

    let per_thread = a.decisions / a.threads.max(1);
    let contexts: Arc<Vec<serde_json::Value>> = Arc::new((0..4096).map(context).collect());
    let started = Instant::now();
    let handles: Vec<_> = (0..a.threads.max(1))
        .map(|t| {
            let decider = decider.clone();
            let contexts = contexts.clone();
            std::thread::spawn(move || {
                let mut lat = Vec::with_capacity(per_thread);
                for i in 0..per_thread {
                    let ctx = contexts[(t * 7919 + i) % contexts.len()].clone();
                    let t0 = Instant::now();
                    let d = decider.decide(ctx).unwrap();
                    lat.push(t0.elapsed().as_nanos() as u64);
                    std::hint::black_box(d);
                }
                lat
            })
        })
        .collect();
    let mut lat: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let wall = started.elapsed();
    lat.sort_unstable();
    let us = |ns: u64| ns as f64 / 1000.0;
    println!(
        "local decide ({} threads, {} decisions): p50 {:.2} us  p90 {:.2} us  p99 {:.2} us  p99.9 {:.2} us  max {:.1} us",
        a.threads,
        lat.len(),
        us(percentile(&lat, 50.0)),
        us(percentile(&lat, 90.0)),
        us(percentile(&lat, 99.0)),
        us(percentile(&lat, 99.9)),
        us(*lat.last().unwrap_or(&0)),
    );
    println!(
        "throughput: {:.0} decisions/s",
        lat.len() as f64 / wall.as_secs_f64()
    );

    let t0 = Instant::now();
    let report = decider.flush().expect("flush");
    let upload = t0.elapsed();
    println!(
        "upload: {} accepted, {} rejected in {:.2} s ({:.0} verified decisions/s)",
        report.decisions_accepted,
        report.decisions_rejected,
        upload.as_secs_f64(),
        report.decisions_accepted as f64 / upload.as_secs_f64()
    );
}
