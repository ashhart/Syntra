//! Regression tests for BUG-2: verifier must reject Strategy/AdaptiveChoice
//! nodes whose weights count does not match the operand count, and the
//! executor must never panic on such a graph (defense in depth).

use syntra::graph::*;
use syntra::graph_executor::GraphExecutor;

fn const_node(id: u32, n: i64) -> GraphNode {
    GraphNode {
        id,
        op: OpCode::ConstInt,
        operands: vec![Operand::Immediate(ImmValue::Int(n))],
        weights: vec![],
        bias: 0.0,
        activation_count: 0,
        state_slot: None,
        weight_kind: WeightKind::Observational,
        annotation: None,
        contract: Contract::None,
        objective: Objective::None,
    }
}

fn graph_with_strategy(weights: Vec<f64>, contract: Contract) -> NeuralGraph {
    let strat = GraphNode {
        id: 2,
        op: OpCode::Strategy,
        operands: vec![Operand::NodeRef(0), Operand::NodeRef(1)],
        weights,
        bias: 0.0,
        activation_count: 0,
        state_slot: None,
        weight_kind: WeightKind::Strategy,
        annotation: None,
        contract,
        objective: Objective::Speed,
    };
    let mut ng = NeuralGraph::new();
    ng.nodes.push(const_node(0, 1));
    ng.nodes.push(const_node(1, 1));
    ng.nodes.push(strat);
    ng.entry = 2;
    ng
}

#[test]
fn verifier_rejects_sameoutput_with_extra_weight() {
    // 2 operands, 3 weights, SameOutput — the exact shape that used to panic
    // the executor via `results.remove(best_idx)`.
    let ng = graph_with_strategy(vec![0.1, 0.2, 0.7], Contract::SameOutput);
    let err = syntra::verifier::verify(&ng).expect_err("verifier must reject weight/operand mismatch");
    let msg = err.to_string();
    assert!(msg.contains("weights count"), "unexpected error: {msg}");
}

#[test]
fn verifier_rejects_within_tolerance_without_epsilon_slot() {
    // 2 operands, 2 weights, WithinTolerance — missing the epsilon slot.
    let ng = graph_with_strategy(vec![0.5, 0.5], Contract::WithinTolerance);
    assert!(
        syntra::verifier::verify(&ng).is_err(),
        "WithinTolerance requires operands.len()+1 weights"
    );
}

#[test]
fn verifier_accepts_within_tolerance_with_epsilon_slot() {
    // 2 operands, 3 weights (n + epsilon), WithinTolerance — compiler output.
    let ng = graph_with_strategy(vec![0.5, 0.5, 1e-6], Contract::WithinTolerance);
    assert!(
        syntra::verifier::verify(&ng).is_ok(),
        "compiler-shaped WithinTolerance strategy must verify"
    );
}

#[test]
fn executor_does_not_panic_on_malformed_sameoutput() {
    // Defense in depth: even if a malformed graph reaches the executor
    // (verifier bypassed), it must not panic.
    let ng = graph_with_strategy(vec![0.1, 0.2, 0.7], Contract::SameOutput);
    let mut ex = GraphExecutor::new(ng);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ex.run()));
    assert!(r.is_ok(), "executor must not panic on malformed SameOutput strategy");
}
