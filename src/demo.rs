//! `syntra demo`: a server on a fresh store with simulated traffic, to
//! watch a capsule learn in the admin console.
//!
//! The traffic is simulated: three model routes with made-up quality, cost
//! and latency profiles per task. It shows how the service behaves, not
//! how any real model does.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::decision::SplitMix64;

const TASKS: [&str; 3] = ["chat", "code", "extract"];
const TIERS: [&str; 3] = ["free", "pro", "enterprise"];

/// Simulated (quality, cost in USD) of a route on a task.
fn simulate(route: &str, task: &str) -> (f64, f64) {
    let quality = match (task, route) {
        ("chat", "small") => 0.80,
        ("chat", "medium") => 0.82,
        ("chat", "large") => 0.85,
        ("code", "small") => 0.30,
        ("code", "medium") => 0.60,
        ("code", "large") => 0.95,
        ("extract", "small") => 0.70,
        ("extract", "medium") => 0.90,
        (_, _) => 0.92,
    };
    let cost = match route {
        "small" => 0.001,
        "medium" => 0.004,
        _ => 0.02,
    };
    (quality, cost)
}

pub fn cli(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!(
            "Usage: syntra demo [--addr 127.0.0.1:8787] [--rate <requests/s>]\n\n\
             Starts a server on a fresh temporary store, creates capsule acme/llm/router\n\
             with three model routes, and sends simulated traffic (decide, then reward\n\
             quality - 5 x cost) so you can watch it learn in the admin console."
        );
        return 0;
    }
    let mut addr = "127.0.0.1:8787".to_string();
    let mut rate = 100.0f64;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned();
        match (args[i].as_str(), value) {
            ("--addr", Some(v)) => addr = v,
            ("--rate", Some(v)) => match v.parse::<f64>() {
                Ok(r) if r > 0.0 && r <= 10_000.0 => rate = r,
                _ => {
                    eprintln!("syntra demo: --rate must be in (0, 10000]");
                    return 2;
                }
            },
            (other, _) => {
                eprintln!("syntra demo: unknown or incomplete option {other:?}");
                return 2;
            }
        }
        i += 2;
    }
    let store = std::env::temp_dir().join(format!(
        "syntra-demo-{}-{:x}",
        std::process::id(),
        crate::decision::random_seed() & 0xFFFF_FFFF
    ));
    let key = format!(
        "{:016x}{:016x}",
        crate::decision::random_seed(),
        crate::decision::random_seed()
    );
    let base = format!("http://{addr}");
    eprintln!(
        "\nSyntra demo (simulated traffic)\n\
         \n  admin console  {base}/admin\n  admin key      {key}\n  store          {}\n\
         \nTry, in another terminal:\n\
         \n  curl -s {base}/v1/tenants/acme/jobs/llm/capsules/router -H \"Authorization: Bearer {key}\"\n\
         \nor evaluate the learned policy in the console's Evaluate tab. Ctrl-C stops.\n",
        store.display()
    );
    let traffic_base = base.clone();
    let traffic_key = key.clone();
    std::thread::Builder::new()
        .name("syntra-demo-traffic".into())
        .spawn(move || traffic(&traffic_base, &traffic_key, rate))
        .expect("spawn demo traffic");
    crate::server::run_server(crate::server::ServerConfig {
        addr,
        store_path: store.to_string_lossy().into_owned(),
        admin_key: Some(key),
        service_name: Some("Syntra demo".into()),
        ..Default::default()
    });
    0
}

fn traffic(base: &str, key: &str, rate: f64) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let capsule = format!("{base}/v1/tenants/acme/jobs/llm/capsules/router");
    let call = |method: &str, url: &str, body: &Value| -> Option<Value> {
        agent
            .request(method, url)
            .set("Authorization", &format!("Bearer {key}"))
            .send_string(&body.to_string())
            .ok()?
            .into_string()
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    };
    // Wait for the server.
    let started = Instant::now();
    while agent.get(&format!("{base}/health")).call().is_err() {
        if started.elapsed() > Duration::from_secs(20) {
            eprintln!("syntra demo: the server did not come up");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let spec = json!({
        "actions": [
            {"id": "small", "features": {"costPer1M": 0.15, "tier": "small"}},
            {"id": "medium", "features": {"costPer1M": 1.0, "tier": "medium"}},
            {"id": "large", "features": {"costPer1M": 10.0, "tier": "large"}}
        ],
        "exploration": {"kind": "squarecb"},
        "reward": {"range": [-0.2, 1.0]}
    });
    if call("PUT", &format!("{capsule}/spec"), &spec).is_none() {
        eprintln!("syntra demo: could not create the capsule");
        return;
    }
    let mut rng = SplitMix64::new(crate::decision::random_seed());
    let interval = Duration::from_secs_f64(1.0 / rate);
    let (mut window_reward, mut window_best, mut window_n) = (0.0, 0.0, 0u64);
    let mut last_report = Instant::now();
    let mut next = Instant::now();
    loop {
        let task = TASKS[(rng.next_u64() % 3) as usize];
        let tier = TIERS[(rng.next_u64() % 3) as usize];
        let context = json!({
            "task": task,
            "tier": tier,
            "promptTokens": 50 + rng.next_u64() % 4000,
        });
        if let Some(d) = call(
            "POST",
            &format!("{capsule}/decide"),
            &json!({ "context": context }),
        ) {
            let route = d["action"].as_str().unwrap_or("small");
            let (quality, cost) = simulate(route, task);
            let observed = (quality + (rng.next_f64() - 0.5) * 0.1).clamp(0.0, 1.0);
            let reward = observed - 5.0 * cost;
            let best = ["small", "medium", "large"]
                .iter()
                .map(|r| {
                    let (q, c) = simulate(r, task);
                    q - 5.0 * c
                })
                .fold(f64::MIN, f64::max);
            call(
                "POST",
                &format!("{capsule}/reward"),
                &json!({
                    "decisionId": d["decisionId"],
                    "reward": reward,
                    "detail": {"quality": observed, "costUsd": cost, "task": task}
                }),
            );
            window_reward += quality - 5.0 * cost;
            window_best += best;
            window_n += 1;
        }
        if last_report.elapsed() >= Duration::from_secs(10) && window_n > 0 {
            eprintln!(
                "  last {window_n} decisions: {:.1}% of the best possible expected reward",
                100.0 * window_reward / window_best
            );
            (window_reward, window_best, window_n) = (0.0, 0.0, 0);
            last_report = Instant::now();
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}
