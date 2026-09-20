//! LLM-free phishing decisions: selected-action feedback, then frozen evaluation.
//! Consumes only numeric features prepared by scripts/demo-email-fraud.py.
use serde::Deserialize;
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

#[derive(Deserialize)]
struct Row {
    x: Vec<f64>,
    y: usize,
    baseline: usize,
    reference: usize,
    #[serde(default)]
    review_guard: bool,
}
#[derive(Deserialize)]
struct Data {
    train: Vec<Row>,
    calibration: Vec<Row>,
    test: Vec<Row>,
    #[serde(default)]
    risk_bias: f64,
}

fn random(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
fn choose(
    states: &[LinUcbState],
    x: &[f64],
    alpha: f64,
    g: &NeuralGraph,
    node: usize,
    risk_bias: f64,
) -> (usize, f64) {
    let mut scores: Vec<_> = states.iter().map(|s| s.ucb_score(x, alpha).0).collect();
    scores[1] += risk_bias;
    assert!(scores.iter().all(|s| s.is_finite()));
    let best = usize::from(scores[1] > scores[0]);
    let mut graph = g.clone();
    for (i, w) in graph.nodes[node].weights.iter_mut().take(2).enumerate() {
        *w = f64::from(i == best);
    }
    let mut ctx = ExecutionContext::with_input(syntra::capabilities::CapValue::Null);
    ctx.selection_mode = SelectionMode::Greedy;
    ctx.policy = Some(ExecutionPolicy::deny_all());
    let mut ex = GraphExecutor::new_with_context(graph, ctx);
    let action = match ex.run().expect("decision graph") {
        GVal::Int(a) if (0..2).contains(&a) => a as usize,
        _ => panic!("invalid classification action"),
    };
    assert_eq!(action, best);
    (action, (scores[1] - scores[0]).abs())
}
fn metrics(rows: &[Row], predictions: &[Option<usize>]) -> Value {
    assert_eq!(rows.len(), predictions.len());
    let mut c = [[0usize; 2]; 2];
    let mut review_by_class = [0usize; 2];
    for (r, p) in rows.iter().zip(predictions) {
        if let Some(a) = p {
            c[r.y][*a] += 1;
        } else {
            review_by_class[r.y] += 1;
        }
    }
    let n: usize = c.iter().flatten().sum();
    let correct = c[0][0] + c[1][1];
    let ratio = |a: usize, b: usize| {
        if b == 0 {
            None
        } else {
            Some(a as f64 / b as f64)
        }
    };
    let accuracy = ratio(correct, n);
    let interval = accuracy.map(|p| {
        let z: f64 = 1.96;
        let n = n as f64;
        let den = 1.0 + z * z / n;
        let center = (p + z * z / (2.0 * n)) / den;
        let radius = z * ((p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt()) / den;
        [center - radius, center + radius]
    });
    json!({"samples":rows.len(),"automated":n,"review":rows.len()-n,
        "coverage":n as f64/rows.len() as f64,"correct":correct,"accuracy":accuracy,
        "accuracy_wilson95":interval,"confusion_matrix":c,"review_by_class":review_by_class,
        "phishing_precision":ratio(c[1][1],c[0][1]+c[1][1]),
        "phishing_recall_automated":ratio(c[1][1],c[1][0]+c[1][1]),
        "phishing_caught_fraction_all":ratio(c[1][1],c[1][0]+c[1][1]+review_by_class[1]),
        "false_positive_rate_automated":ratio(c[0][1],c[0][0]+c[0][1])})
}
fn latency(times: &[u64]) -> Value {
    let mut t = times.to_vec();
    t.sort_unstable();
    let p = |q: f64| t[((t.len() as f64 * q).ceil() as usize).saturating_sub(1)] as f64 / 1000.;
    json!({"p50_us":p(0.5),"p99_us":p(0.99),"max_us":p(1.),
        "sum_ms":times.iter().sum::<u64>() as f64/1e6,
        "over_1ms":times.iter().filter(|&&v|v>=1_000_000).count()})
}
fn run(data: &Data, g: &NeuralGraph, node: usize, seed: u64) -> Value {
    let mut rng = seed;
    let mut order: Vec<_> = (0..data.train.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, random(&mut rng) as usize % (i + 1));
    }
    let mut states: Vec<_> = (0..2)
        .map(|_| LinUcbState::new(data.train[0].x.len(), 1.))
        .collect();
    let mut pending = VecDeque::new();
    let update = |s: &mut Vec<LinUcbState>, a: usize, x: Vec<f64>, r: f64| {
        s[a].update(&x, r);
        if s[a].rebuild_due(256) {
            s[a].rebuild_inverse();
        }
    };
    for i in order {
        let r = &data.train[i];
        let (a, _) = choose(&states, &r.x, 1., g, node, 0.);
        pending.push_back((a, r.x.clone(), f64::from(a == r.y)));
        if pending.len() > 32 {
            let (a, x, reward) = pending.pop_front().unwrap();
            update(&mut states, a, x, reward);
        }
    }
    for (a, x, reward) in pending {
        update(&mut states, a, x, reward);
    }
    // Review threshold is the predeclared bottom 10% of calibration margins.
    // This uses no calibration labels and no test observations.
    let mut margins: Vec<_> = data
        .calibration
        .iter()
        .map(|r| choose(&states, &r.x, 0., g, node, data.risk_bias).1)
        .collect();
    margins.sort_by(f64::total_cmp);
    let threshold = margins[margins.len() / 10];
    let mut predictions = Vec::new();
    let mut selective = Vec::new();
    let mut times = Vec::new();
    let mut digest = 0xcbf29ce484222325u64;
    for r in &data.test {
        let t = Instant::now();
        let (a, margin) = choose(&states, &r.x, 0., g, node, data.risk_bias);
        times.push(t.elapsed().as_nanos() as u64);
        predictions.push(Some(a));
        let review = margin < threshold || r.review_guard;
        selective.push(if review { None } else { Some(a) });
        digest = (digest ^ (a as u64)).wrapping_mul(0x100000001b3);
        digest = (digest ^ u64::from(review)).wrapping_mul(0x100000001b3);
    }
    let n = 100.min(data.test.len());
    json!({"seed":seed,"risk_bias":data.risk_bias,"selected_action_reward_delay":32,"review_margin_threshold":threshold,
        "guard_deferred":data.test.iter().filter(|r| r.review_guard).count(),
        "full":metrics(&data.test,&predictions),"with_review":metrics(&data.test,&selective),
        "first100":metrics(&data.test[..n],&predictions[..n]),
        "first100_with_review":metrics(&data.test[..n],&selective[..n]),
        "decision_latency":latency(&times),"first100_decision_latency":latency(&times[..n]),
        "prediction_digest":format!("{digest:016x}")})
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("numeric dataset path required");
    let data: Data = serde_json::from_slice(&std::fs::read(path).expect("read features"))
        .expect("parse features");
    assert!(!data.train.is_empty() && !data.calibration.is_empty() && data.test.len() >= 100);
    assert!(data.risk_bias.is_finite() && (0.0..=0.2).contains(&data.risk_bias));
    let d = data.train[0].x.len();
    assert!(d > 0 && d <= 64);
    for row in data.train.iter().chain(&data.calibration).chain(&data.test) {
        assert_eq!(row.x.len(), d);
        assert!(row.x.iter().all(|x| x.is_finite()));
        assert!(row.y < 2 && row.baseline < 2 && row.reference < 2);
    }
    let program = Parser::new(Lexer::new("(choice 0 1)").tokenize().unwrap())
        .parse_program()
        .unwrap();
    let graph = GraphCompiler::new().compile(&program).unwrap();
    verifier::verify(&graph).unwrap();
    let node = graph
        .nodes
        .iter()
        .position(|n| matches!(n.op, OpCode::AdaptiveChoice))
        .unwrap();
    let baseline: Vec<_> = data.test.iter().map(|r| Some(r.baseline)).collect();
    let reference: Vec<_> = data.test.iter().map(|r| Some(r.reference)).collect();
    let majority =
        usize::from(data.train.iter().filter(|r| r.y == 1).count() * 2 > data.train.len());
    let majority_predictions = vec![Some(majority); data.test.len()];
    println!("{}",serde_json::to_string_pretty(&json!({"features":d,
        "baselines":{"naive_bayes":metrics(&data.test,&baseline),"naive_bayes_equal_data":metrics(&data.test,&reference),"naive_bayes_first100":metrics(&data.test[..100],&baseline[..100]),"majority":metrics(&data.test,&majority_predictions)},
        "runs":[run(&data,&graph,node,7),run(&data,&graph,node,42),run(&data,&graph,node,2026)]})).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_is_not_counted_as_a_correct_prediction() {
        let rows = vec![
            Row {
                x: vec![1.],
                y: 0,
                baseline: 0,
                reference: 0,
                review_guard: false,
            },
            Row {
                x: vec![1.],
                y: 1,
                baseline: 0,
                reference: 0,
                review_guard: false,
            },
        ];
        let result = metrics(&rows, &[Some(0), None]);
        assert_eq!(result["correct"], 1);
        assert_eq!(result["automated"], 1);
        assert_eq!(result["review"], 1);
        assert_eq!(result["coverage"], 0.5);
        assert_eq!(result["phishing_caught_fraction_all"], 0.0);
        assert!(result["phishing_recall_automated"].is_null());
    }
    #[test]
    fn guard_defers_without_changing_classifier_predictions() {
        let rows = |guard| {
            (0..100)
                .map(|_| Row {
                    x: vec![1.],
                    y: 0,
                    baseline: 0,
                    reference: 0,
                    review_guard: guard,
                })
                .collect()
        };
        let data = Data {
            train: rows(false),
            calibration: rows(false),
            test: rows(true),
            risk_bias: 0.,
        };
        let program = Parser::new(Lexer::new("(choice 0 1)").tokenize().unwrap())
            .parse_program()
            .unwrap();
        let graph = GraphCompiler::new().compile(&program).unwrap();
        let node = graph
            .nodes
            .iter()
            .position(|n| matches!(n.op, OpCode::AdaptiveChoice))
            .unwrap();
        let result = run(&data, &graph, node, 7);
        assert_eq!(result["full"]["correct"], 100);
        assert_eq!(result["with_review"]["review"], 100);
        assert_eq!(result["with_review"]["coverage"], 0.0);
        assert!(result["with_review"]["accuracy"].is_null());
    }

    #[test]
    fn uncertainty_interval_does_not_claim_certainty_for_100_correct() {
        let rows: Vec<_> = (0..100)
            .map(|_| Row {
                x: vec![1.],
                y: 1,
                baseline: 1,
                reference: 1,
                review_guard: false,
            })
            .collect();
        let result = metrics(&rows, &vec![Some(1); 100]);
        assert!(result["accuracy_wilson95"][0].as_f64().unwrap() < 0.97);
        assert_eq!(result["accuracy"], 1.0);
    }
}
