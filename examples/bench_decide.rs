//! HTTP decide benchmark: latency percentiles and throughput against a
//! running server, over persistent keep-alive connections.
//!
//! ```text
//! syntra serve --addr 127.0.0.1:8787 --store ./bench-store --admin-key bench
//! cargo run --release --example bench_decide -- \
//!     --addr 127.0.0.1:8787 --key bench --concurrency 8 --requests 20000 --reward-every 1
//! ```
//!
//! It creates (or reuses) capsule `bench/bench/router` with three actions,
//! then each client thread sends `--requests` decides with a varied context
//! and, with `--reward-every N`, a reward after every Nth decision. Each
//! request's latency is measured from write to fully read response.
//! Numbers depend on the machine; report them with the hardware.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

struct Args {
    addr: String,
    key: String,
    concurrency: usize,
    requests: usize,
    reward_every: usize,
    warmup: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        addr: "127.0.0.1:8787".into(),
        key: String::new(),
        concurrency: 8,
        requests: 10_000,
        reward_every: 0,
        warmup: 1_000,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--addr" => a.addr = v,
            "--key" => a.key = v,
            "--concurrency" => a.concurrency = v.parse().expect("--concurrency"),
            "--requests" => a.requests = v.parse().expect("--requests"),
            "--reward-every" => a.reward_every = v.parse().expect("--reward-every"),
            "--warmup" => a.warmup = v.parse().expect("--warmup"),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    a
}

struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    key: String,
    host: String,
}

impl Conn {
    fn open(addr: &str, key: &str) -> Conn {
        let s = TcpStream::connect(addr).expect("connect");
        s.set_nodelay(true).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Conn {
            reader: BufReader::new(s.try_clone().unwrap()),
            writer: s,
            key: key.to_string(),
            host: addr.to_string(),
        }
    }

    /// Send one request and read the whole response; returns (status, body).
    fn call(&mut self, method: &str, path: &str, body: &str) -> (u16, String) {
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            self.host,
            self.key,
            body.len()
        );
        self.writer.write_all(req.as_bytes()).expect("write");
        let mut status_line = String::new();
        self.reader
            .read_line(&mut status_line)
            .expect("read status");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("status code");
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read header");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).expect("read body");
        (status, String::from_utf8_lossy(&buf).into_owned())
    }
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
    let base = "/v1/tenants/bench/jobs/bench/capsules/router";
    let mut setup = Conn::open(&a.addr, &a.key);
    let (st, body) = setup.call(
        "PUT",
        &format!("{base}/spec"),
        r#"{"actions":[{"id":"small","features":{"cost":0.1}},{"id":"medium","features":{"cost":0.4}},{"id":"large","features":{"cost":1.0}}]}"#,
    );
    assert!(st == 200 || st == 201, "spec PUT failed: {st} {body}");

    let tasks = ["chat", "code", "extract", "summarize"];
    let tiers = ["free", "pro", "enterprise"];
    let run = |n: usize, record: bool| -> (Vec<u64>, Vec<u64>) {
        let handles: Vec<_> = (0..a.concurrency)
            .map(|t| {
                let addr = a.addr.clone();
                let key = a.key.clone();
                let reward_every = a.reward_every;
                std::thread::spawn(move || {
                    let mut c = Conn::open(&addr, &key);
                    let mut decide_ns = Vec::with_capacity(n);
                    let mut reward_ns = Vec::new();
                    for i in 0..n {
                        let body = format!(
                            r#"{{"context":{{"task":"{}","tokens":{},"user":{{"tier":"{}"}}}}}}"#,
                            tasks[(i + t) % tasks.len()],
                            200 + (i * 37 % 4000),
                            tiers[(i / 3 + t) % tiers.len()]
                        );
                        let t0 = Instant::now();
                        let (st, resp) = c.call("POST", &format!("{base}/decide"), &body);
                        decide_ns.push(t0.elapsed().as_nanos() as u64);
                        assert_eq!(st, 200, "decide failed: {resp}");
                        if reward_every > 0 && i % reward_every == 0 {
                            let id = resp
                                .split("\"decisionId\":\"")
                                .nth(1)
                                .and_then(|s| s.split('"').next())
                                .expect("decisionId")
                                .to_string();
                            let reward = if resp.contains("\"action\":\"small\"") {
                                0.9
                            } else {
                                0.4
                            };
                            let rb = format!(r#"{{"decisionId":"{id}","reward":{reward}}}"#);
                            let t1 = Instant::now();
                            let (st, r) = c.call("POST", &format!("{base}/reward"), &rb);
                            reward_ns.push(t1.elapsed().as_nanos() as u64);
                            assert_eq!(st, 200, "reward failed: {r}");
                        }
                    }
                    (decide_ns, reward_ns)
                })
            })
            .collect();
        let mut all_d = Vec::new();
        let mut all_r = Vec::new();
        for h in handles {
            let (d, r) = h.join().unwrap();
            if record {
                all_d.extend(d);
                all_r.extend(r);
            }
        }
        (all_d, all_r)
    };

    run(a.warmup.max(1) / a.concurrency.max(1) + 1, false);
    let started = Instant::now();
    let (mut d, mut r) = run(a.requests, true);
    let elapsed = started.elapsed();
    d.sort_unstable();
    r.sort_unstable();
    let us = |ns: u64| ns as f64 / 1000.0;
    println!(
        "decide: n={} concurrency={} throughput={:.0}/s p50={:.1}us p90={:.1}us p99={:.1}us p99.9={:.1}us max={:.1}us",
        d.len(),
        a.concurrency,
        (d.len() + r.len()) as f64 / elapsed.as_secs_f64(),
        us(percentile(&d, 50.0)),
        us(percentile(&d, 90.0)),
        us(percentile(&d, 99.0)),
        us(percentile(&d, 99.9)),
        us(*d.last().unwrap_or(&0)),
    );
    if !r.is_empty() {
        println!(
            "reward: n={} p50={:.1}us p90={:.1}us p99={:.1}us p99.9={:.1}us max={:.1}us",
            r.len(),
            us(percentile(&r, 50.0)),
            us(percentile(&r, 90.0)),
            us(percentile(&r, 99.0)),
            us(percentile(&r, 99.9)),
            us(*r.last().unwrap_or(&0)),
        );
    }
}
