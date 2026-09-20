//! Real-data decision benchmark, not an aerospace control validation.
//! UCI Statlog Shuttle: official train/test split, reward-only selected-arm
//! feedback, delayed updates, frozen held-out evaluation, seven actions.
//! Usage: cargo run --release --example shuttle_decisions -- TRAIN TEST
//! Data and preprocessing stay outside the repository; see the runner script.

use serde_json::{Value, json};
use std::{collections::VecDeque, time::Instant};
use syntra::{
    context::{ExecutionContext, ExecutionPolicy, SelectionMode},
    graph::{NeuralGraph, OpCode},
    graph_compiler::GraphCompiler,
    graph_executor::{GVal, GraphExecutor},
    lexer::Lexer,
    linucb::LinUcbState,
    parser::Parser,
    verifier,
};

const K: usize = 7;
type Row = ([f64; 9], usize);

fn read(path: &str, expected: usize) -> Vec<Row> {
    let rows: Vec<_> = std::fs::read_to_string(path)
        .expect("read dataset")
        .lines()
        .map(|line| {
            let v: Vec<f64> = line
                .split_whitespace()
                .map(|s| s.parse().expect("numeric data"))
                .collect();
            assert_eq!(v.len(), 10);
            assert!(v.iter().all(|x| x.is_finite()));
            assert!(v[9].fract() == 0.0 && (1.0..=7.0).contains(&v[9]));
            (v[..9].try_into().unwrap(), v[9] as usize - 1)
        })
        .collect();
    assert_eq!(rows.len(), expected, "must use the full official split");
    rows
}

struct Scale {
    mean: [f64; 9],
    std: [f64; 9],
}
impl Scale {
    fn train(rows: &[Row]) -> Self {
        let mut mean = [0.0; 9];
        for (x, _) in rows {
            for j in 0..9 {
                mean[j] += x[j] / rows.len() as f64;
            }
        }
        let mut std = [0.0; 9];
        for (x, _) in rows {
            for j in 0..9 {
                std[j] += (x[j] - mean[j]).powi(2) / rows.len() as f64;
            }
        }
        for s in &mut std {
            *s = s.sqrt().max(1e-12);
        }
        Self { mean, std }
    }
    fn encode(&self, raw: &[f64; 9], quadratic: bool) -> Vec<f64> {
        let z: Vec<f64> = (0..9)
            .map(|j| ((raw[j] - self.mean[j]) / self.std[j]).clamp(-5.0, 5.0))
            .collect();
        let mut x = vec![1.0];
        x.extend_from_slice(&z);
        if quadratic {
            for i in 0..9 {
                for j in i..9 {
                    x.push(z[i] * z[j]);
                }
            }
        }
        x
    }
}

fn random(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

fn scores(confusion: &[[usize; K]; K]) -> Value {
    let n: usize = confusion.iter().flatten().sum();
    let correct: usize = (0..K).map(|i| confusion[i][i]).sum();
    let support: Vec<usize> = confusion.iter().map(|r| r.iter().sum()).collect();
    let recalls: Vec<Option<f64>> = (0..K)
        .map(|i| (support[i] > 0).then(|| confusion[i][i] as f64 / support[i] as f64))
        .collect();
    let present: Vec<f64> = recalls.iter().flatten().copied().collect();
    json!({"accuracy":correct as f64 / n as f64, "balanced_accuracy":present.iter().sum::<f64>() / present.len() as f64,
        "per_class_recall":recalls,"support":support,"confusion_matrix":confusion,"samples":n})
}

fn latency(samples: &mut [u64]) -> Value {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let p = |q: f64| {
        samples[((samples.len() as f64 * q).ceil() as usize).saturating_sub(1)] as f64 / 1000.0
    };
    json!({"samples":samples.len(),"p50_us":p(0.5),"p95_us":p(0.95),"p99_us":p(0.99),"p999_us":p(0.999),
        "max_us":samples[samples.len()-1] as f64 / 1000.0,"over_1ms":samples.iter().filter(|&&x|x>=1_000_000).count()})
}

fn select(
    states: &[LinUcbState],
    x: &[f64],
    alpha: f64,
    graph: &NeuralGraph,
    node: usize,
) -> usize {
    let mut best = 0;
    let mut best_score = f64::NEG_INFINITY;
    for (i, s) in states.iter().enumerate() {
        let score = s.ucb_score(x, alpha).0;
        assert!(score.is_finite());
        if score > best_score {
            best = i;
            best_score = score;
        }
    }
    // Same one-hot handoff used by the HTTP feature-context LinUCB path.
    let mut g = graph.clone();
    for (i, w) in g.nodes[node].weights.iter_mut().take(K).enumerate() {
        *w = if i == best { 1.0 } else { 0.0 };
    }
    let mut ctx = ExecutionContext::with_input(syntra::capabilities::CapValue::Null);
    ctx.selection_mode = SelectionMode::Greedy;
    ctx.policy = Some(ExecutionPolicy::deny_all());
    let mut ex = GraphExecutor::new_with_context(g, ctx);
    match ex.run().expect("execute decision graph") {
        GVal::Int(a) if a >= 0 && (a as usize) < K => {
            assert_eq!(a as usize, best);
            a as usize
        }
        other => panic!("invalid action {other}"),
    }
}

fn experiment(
    train: &[Row],
    test: &[Row],
    scale: &Scale,
    graph: &NeuralGraph,
    node: usize,
    seed: u64,
    delay: usize,
    quadratic: bool,
) -> Value {
    let mut rng = seed;
    let mut order: Vec<usize> = (0..train.len()).collect();
    for i in (1..order.len()).rev() {
        let j = random(&mut rng) as usize % (i + 1);
        order.swap(i, j);
    }
    let d = if quadratic { 55 } else { 10 };
    let mut states: Vec<_> = (0..K).map(|_| LinUcbState::new(d, 1.0)).collect();
    let mut pending = VecDeque::new();
    let mut train_confusion = [[0; K]; K];
    let mut train_times = Vec::new();
    let apply = |states: &mut Vec<LinUcbState>, a: usize, x: Vec<f64>, r: f64| {
        states[a].update(&x, r);
        if states[a].rebuild_due(256) {
            states[a].rebuild_inverse();
        }
    };
    for i in order {
        let (raw, label) = &train[i];
        let t = Instant::now();
        let x = scale.encode(raw, quadratic);
        let a = select(&states, &x, 1.0, graph, node);
        train_times.push(t.elapsed().as_nanos() as u64);
        train_confusion[*label][a] += 1;
        // The learner receives only the reward for the action it selected.
        pending.push_back((a, x, f64::from(a == *label)));
        if pending.len() > delay {
            let (a, x, r) = pending.pop_front().unwrap();
            apply(&mut states, a, x, r);
        }
    }
    for (a, x, r) in pending {
        apply(&mut states, a, x, r);
    }
    let mut confusion = [[0; K]; K];
    let mut times = Vec::new();
    let mut digest = 0xcbf29ce484222325u64;
    for (raw, label) in test {
        let t = Instant::now();
        let x = scale.encode(raw, quadratic);
        let a = select(&states, &x, 0.0, graph, node);
        times.push(t.elapsed().as_nanos() as u64);
        confusion[*label][a] += 1;
        digest = (digest ^ a as u64).wrapping_mul(0x100000001b3);
    }
    json!({"seed":seed,"delay_decisions":delay,"features":d,"training":scores(&train_confusion),
        "held_out":scores(&confusion),"training_decision_latency":latency(&mut train_times),
        "held_out_decision_latency":latency(&mut times),"prediction_digest":format!("{digest:016x}")})
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3, "provide official train and test paths");
    let train = read(&args[1], 43500);
    let test = read(&args[2], 14500);
    let scale = Scale::train(&train);
    let program = Parser::new(Lexer::new("(choice 0 1 2 3 4 5 6)").tokenize().unwrap())
        .parse_program()
        .unwrap();
    let graph = GraphCompiler::new().compile(&program).unwrap();
    verifier::verify(&graph).unwrap();
    let node = graph
        .nodes
        .iter()
        .position(|n| matches!(n.op, OpCode::AdaptiveChoice))
        .unwrap();
    let mut counts = [0; K];
    let mut centers = vec![vec![0.0; 10]; K];
    for (x, y) in &train {
        counts[*y] += 1;
        for (a, b) in centers[*y].iter_mut().zip(scale.encode(x, false)) {
            *a += b;
        }
    }
    for i in 0..K {
        for x in &mut centers[i] {
            *x /= counts[i].max(1) as f64;
        }
    }
    let majority = (0..K).max_by_key(|&i| counts[i]).unwrap();
    let mut baseline = vec![[[0; K]; K]; 3];
    let mut rng = 20260920;
    for (raw, label) in &test {
        let x = scale.encode(raw, false);
        let nearest = (0..K)
            .filter(|&i| counts[i] > 0)
            .min_by(|&a, &b| {
                let distance = |i: usize| {
                    x.iter()
                        .zip(&centers[i])
                        .map(|(x, c)| (x - c).powi(2))
                        .sum::<f64>()
                };
                distance(a).total_cmp(&distance(b))
            })
            .unwrap();
        for (i, a) in [majority, random(&mut rng) as usize % K, nearest]
            .into_iter()
            .enumerate()
        {
            baseline[i][*label][a] += 1;
        }
    }
    let mut runs = Vec::new();
    for quadratic in [false, true] {
        for delay in [32, 256] {
            for seed in [7, 42, 2026] {
                runs.push(experiment(
                    &train, &test, &scale, &graph, node, seed, delay, quadratic,
                ));
            }
        }
    }
    println!("{}",serde_json::to_string_pretty(&json!({"dataset":"UCI Statlog Shuttle","train_rows":train.len(),"test_rows":test.len(),
        "baselines":{"majority":scores(&baseline[0]),"uniform_random":scores(&baseline[1]),"nearest_centroid_full_labels":scores(&baseline[2])},"runs":runs})).unwrap());
}
