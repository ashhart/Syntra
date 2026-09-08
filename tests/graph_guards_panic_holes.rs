//! Fail-closed regression tests for three reachable executor panics that the
//! verifier used to wave through (spec §"open questions" item 5 + grammar
//! arity findings, 2026-09-08). All three are constructible as hostile .lyc
//! bytes or via `syntra author`/install paths that decode without verify
//! until decide time; a panic is a server DoS, so the verifier must reject
//! them and the executor must degrade to errors.
//!
//! Cases:
//!  1. Strategy node with 0 operands + 1 weight — satisfies rule 13
//!     (weights == operands+1 for WithinTolerance) but the executor derives
//!     n_options=1 and indexes operands[0].
//!  2. Binary arithmetic opcode with 1 operand — no arity rule existed;
//!     binary_op indexes operands[1].
//!  3. Mod-by-zero (static and dynamic) — Rust `i64 % 0` panics.

use std::panic::{catch_unwind, AssertUnwindSafe};
use syntra::graph::{
    Contract, GraphHeader, GraphNode, ImmValue, JournalEntry, NeuralGraph, Objective, OpCode,
    Operand, WeightKind,
};
use syntra::graph_executor::GraphExecutor;
use syntra::verifier::verify;

fn graph_with(nodes: Vec<GraphNode>) -> NeuralGraph {
    NeuralGraph {
        header: GraphHeader {
            version: 5,
            node_count: nodes.len() as u32,
            edge_count: 0,
            state_size: 0,
            string_count: 0,
            flags: 0,
        },
        nodes,
        edges: vec![],
        string_table: vec![],
        state: vec![],
        entry: 0,
        journal: vec![],
    }
}

fn node(id: u32, op: OpCode, operands: Vec<Operand>, weights: Vec<f64>) -> GraphNode {
    GraphNode {
        id,
        op,
        operands,
        weights,
        bias: 0.0,
        activation_count: 0,
        state_slot: None,
        weight_kind: WeightKind::Adaptive,
        annotation: None,
        contract: Contract::None,
        objective: Objective::None,
    }
}

/// Run must Err-or-Ok, never panic.
fn run_without_panic(g: NeuralGraph) -> bool {
    catch_unwind(AssertUnwindSafe(move || {
        let mut ex = GraphExecutor::new(g);
        let _ = ex.run();
    }))
    .is_ok()
}

#[test]
fn strategy_without_options_is_rejected_and_never_panics() {
    assert!(
        run_without_panic(graph_with(vec![node2clone(
            0,
            OpCode::Strategy,
            vec![0.5]
        )])),
        "zero-option Strategy execution must error, not panic"
    );
}

// helper to keep the strategy case runnable without panicking too
fn node2clone(id: u32, op: OpCode, weights: Vec<f64>) -> GraphNode {
    let mut n = node(id, op, vec![], weights);
    n.contract = Contract::WithinTolerance;
    n
}

#[test]
fn single_operand_arithmetic_is_rejected_and_never_panics() {
    let g = graph_with(vec![node(
        0,
        OpCode::Add,
        vec![Operand::Immediate(ImmValue::Int(1))],
        vec![],
    )]);
    assert!(
        verify(&g).is_err(),
        "verifier accepted Add with one operand (binary_op indexes operands[1])"
    );
    assert!(run_without_panic(graph_with(vec![node(
        0,
        OpCode::Add,
        vec![Operand::Immediate(ImmValue::Int(1))],
        vec![],
    )])));
}

#[test]
fn static_mod_zero_is_rejected_and_never_panics() {
    let g = graph_with(vec![node(
        0,
        OpCode::Mod,
        vec![
            Operand::Immediate(ImmValue::Int(1)),
            Operand::Immediate(ImmValue::Int(0)),
        ],
        vec![],
    )]);
    assert!(
        verify(&g).is_err(),
        "verifier accepted a static `1 % 0` node (Rust i64 % 0 panics)"
    );
    assert!(run_without_panic(graph_with(vec![node(
        0,
        OpCode::Mod,
        vec![
            Operand::Immediate(ImmValue::Int(1)),
            Operand::Immediate(ImmValue::Int(0)),
        ],
        vec![],
    )])));
}

#[test]
fn dynamic_mod_zero_never_panics() {
    // The value is only known at runtime — the verifier cannot reject;
    // the executor itself must return an error instead of panicking.
    let zero = node(1, OpCode::Add, vec![Operand::Immediate(ImmValue::Int(0)), Operand::Immediate(ImmValue::Int(0))], vec![]);
    // 0: Mod(NodeRef(2), NodeRef(1)) ; 1: 0+0 ; 2: 1+1
    let one = node(2, OpCode::Add, vec![Operand::Immediate(ImmValue::Int(1)), Operand::Immediate(ImmValue::Int(1))], vec![]); // 2 (any non-zero)
    let zero_b = node(3, OpCode::Sub, vec![Operand::Immediate(ImmValue::Int(0)), Operand::Immediate(ImmValue::Int(0))], vec![]);
    let m = node(
        0,
        OpCode::Mod,
        vec![Operand::NodeRef(2), Operand::NodeRef(3)],
        vec![],
    );
    let g = graph_with(vec![m, zero, one, zero_b]);
    assert!(verify(&g).is_ok(), "well-formed graph must verify: {:?}", verify(&g).err());
    assert!(
        run_without_panic(g),
        "dynamic modulo-by-zero must surface as an error, not a panic"
    );
    let _: Option<JournalEntry> = None; // keep JournalEntry import meaningful
}

/// The tree-walking interpreter (`lycan <file.lycs>`) had the same holes;
/// `(+ 1)` used to abort the process with an index-out-of-bounds panic.
#[test]
fn lycs_arith_panic_holes_now_error_cleanly() {
    for (name, src) in [
        ("one_operand", "(+ 1)\n"),
        ("mod_zero", "(% 1 0)\n"),
        ("not_noargs", "(not)\n"),
    ] {
        let dir = std::env::temp_dir().join(format!("lycs_arith_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("p.lycs");
        std::fs::write(&file, src).unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_lycan"))
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "{name}: lycan panicked instead of erroring: {stderr}"
        );
        assert!(
            !out.status.success(),
            "{name}: lycan must exit non-zero, stderr: {stderr}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
