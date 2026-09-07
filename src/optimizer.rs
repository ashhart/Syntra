/// Lycan self-optimization engine: dead-path pruning, node specialization,
/// hot-path caching.
///
/// CRITICAL INVARIANT: every optimization must be semantics-preserving.
/// Weight-based pruning of reachable paths is FORBIDDEN.

use crate::graph::*;

#[derive(Debug, Default)]
pub struct OptStats {
    pub paths_pruned: u32,
    pub nodes_specialized: u32,
    pub nodes_cached: u32,
    pub nodes_before: u32,
    pub nodes_after: u32,
    pub edges_before: u32,
    pub edges_after: u32,
}

/// Safe optimization — never removes reachable code.
pub fn optimize(graph: &mut NeuralGraph) -> OptStats {
    // Derive run number from max activation of entry node
    let run = graph.nodes.get(graph.entry as usize)
        .map(|n| n.activation_count).unwrap_or(0);
    optimize_inner(graph, false, run)
}

fn optimize_inner(graph: &mut NeuralGraph, allow_pruning: bool, run: u64) -> OptStats {
    let mut stats = OptStats::default();
    stats.nodes_before = graph.nodes.len() as u32;
    stats.edges_before = graph.edges.len() as u32;

    // Pass 1: Dead path pruning (ONLY if explicitly allowed)
    if allow_pruning {
        stats.paths_pruned = prune_dead_paths(graph);
    }

    // Pass 2: Node specialization (metadata only — safe for all programs)
    stats.nodes_specialized = specialize_hot_nodes(graph, run);

    // Pass 3: Constant folding (provably correct — safe for all programs)
    stats.nodes_cached = fold_constants(graph, run);

    // Pass 4: Clean up edges to dead nodes
    if allow_pruning {
        cleanup_dead_edges(graph);
    }

    stats.nodes_after = graph.nodes.iter()
        .filter(|n| n.op != OpCode::Noop)
        .count() as u32;
    stats.edges_after = graph.edges.len() as u32;

    stats
}

/// Pass 1 — dead-path pruning. Only prunes branches where one target's
/// `activation_count == 0` (never taken, provably dead). Never uses
/// weight thresholds.
const MIN_BRANCH_ACTIVATIONS: u64 = 50;

fn prune_dead_paths(graph: &mut NeuralGraph) -> u32 {
    let mut pruned = 0;

    // Collect branches where one child was never activated
    let candidates: Vec<(u32, usize)> = graph.nodes.iter()
        .filter(|n| n.op == OpCode::Branch)
        .filter(|n| n.activation_count >= MIN_BRANCH_ACTIVATIONS)
        .filter(|n| n.operands.len() >= 3)
        .filter_map(|n| {
            // operands[0]=cond, operands[1]=then, operands[2]=else
            let then_id = get_node_ref(&n.operands[1]);
            let else_id = get_node_ref(&n.operands[2]);

            let then_fired = graph.nodes.get(then_id as usize)
                .map(|n| n.activation_count).unwrap_or(0);
            let else_fired = graph.nodes.get(else_id as usize)
                .map(|n| n.activation_count).unwrap_or(0);

            if then_fired == 0 && else_fired > 0 {
                // Then path was NEVER taken — safe to prune
                Some((n.id, 2)) // Keep else (operand index 2)
            } else if else_fired == 0 && then_fired > 0 {
                // Else path was NEVER taken — safe to prune
                Some((n.id, 1)) // Keep then (operand index 1)
            } else {
                None // Both paths were taken — DO NOT prune
            }
        })
        .collect();

    for (branch_id, keep_idx) in candidates {
        let node = &graph.nodes[branch_id as usize];
        let surviving = node.operands[keep_idx].clone();
        let dead_idx = if keep_idx == 1 { 2 } else { 1 };

        if let Operand::NodeRef(dead_id) = &node.operands[dead_idx] {
            let dead_id = *dead_id;
            // Only mark dead if the node truly never fired
            if graph.nodes.get(dead_id as usize)
                .map(|n| n.activation_count == 0).unwrap_or(false)
            {
                graph.nodes[dead_id as usize].op = OpCode::Noop;
                graph.nodes[dead_id as usize].operands.clear();
                graph.nodes[dead_id as usize].weights.clear();
            }
        }

        // Replace Branch with Sequence to surviving path
        let node = &mut graph.nodes[branch_id as usize];
        node.op = OpCode::Sequence;
        node.operands = vec![surviving];
        node.weights.clear();

        pruned += 1;
    }

    pruned
}

/// Pass 2 — node specialization. Sets `bias` on hot arithmetic nodes as
/// a type hint; advisory only.
const SPECIALIZE_THRESHOLD: u64 = 100;

fn specialize_hot_nodes(graph: &mut NeuralGraph, run: u64) -> u32 {
    let mut specialized = 0;
    let mut journal_entries = Vec::new();

    for node in &mut graph.nodes {
        if node.activation_count < SPECIALIZE_THRESHOLD { continue; }
        if node.bias != 0.0 { continue; }

        match node.op {
            OpCode::Add | OpCode::Sub | OpCode::Mul | OpCode::Div | OpCode::Mod |
            OpCode::Eq | OpCode::Neq | OpCode::Lt | OpCode::Gt | OpCode::Lte | OpCode::Gte => {
                node.bias = 1.0;
                node.weight_kind = WeightKind::TypeHint;
                journal_entries.push(JournalEntry {
                    run_number: run,
                    node_id: node.id,
                    mutation: MutationKind::TypeSpecialized,
                    reason: u32::MAX, // No string — sentinel for "none"
                });
                specialized += 1;
            }
            _ => {}
        }
    }

    graph.journal.extend(journal_entries);
    specialized
}

/// Pass 3 — constant folding. Replaces arithmetic on all-constant
/// operands with the precomputed result.
const FOLD_THRESHOLD: u64 = 10;

fn fold_constants(graph: &mut NeuralGraph, run: u64) -> u32 {
    let mut folded = 0;

    let candidates: Vec<(u32, OpCode, Vec<Operand>)> = graph.nodes.iter()
        .filter(|n| n.activation_count >= FOLD_THRESHOLD)
        .filter(|n| matches!(n.op, OpCode::Add | OpCode::Sub | OpCode::Mul | OpCode::Div | OpCode::Mod))
        .filter(|n| n.operands.len() == 2)
        .filter(|n| {
            n.operands.iter().all(|op| match op {
                Operand::Immediate(_) => true,
                Operand::NodeRef(id) => {
                    graph.nodes.get(*id as usize)
                        .map(|n| matches!(n.op, OpCode::ConstInt | OpCode::ConstFloat)
                              && n.operands.len() == 1
                              && matches!(n.operands[0], Operand::Immediate(_)))
                        .unwrap_or(false)
                }
                _ => false,
            })
        })
        .map(|n| (n.id, n.op, n.operands.clone()))
        .collect();

    for (node_id, op, operands) in candidates {
        let vals: Vec<Option<f64>> = operands.iter().map(|o| {
            match o {
                Operand::Immediate(ImmValue::Int(n)) => Some(*n as f64),
                Operand::Immediate(ImmValue::Float(f)) => Some(*f),
                Operand::NodeRef(id) => {
                    graph.nodes.get(*id as usize).and_then(|n| {
                        match n.operands.first() {
                            Some(Operand::Immediate(ImmValue::Int(n))) => Some(*n as f64),
                            Some(Operand::Immediate(ImmValue::Float(f))) => Some(*f),
                            _ => None,
                        }
                    })
                }
                _ => None,
            }
        }).collect();

        if let (Some(Some(a)), Some(Some(b))) = (vals.first(), vals.get(1)) {
            let result = match op {
                OpCode::Add => Some(a + b),
                OpCode::Sub => Some(a - b),
                OpCode::Mul => Some(a * b),
                OpCode::Div if *b != 0.0 => Some(a / b),
                OpCode::Mod if *b != 0.0 => Some(a % b),
                _ => None,
            };

            if let Some(val) = result {
                let node = &mut graph.nodes[node_id as usize];
                if val == val.floor() && val.abs() < i64::MAX as f64 {
                    node.op = OpCode::ConstInt;
                    node.operands = vec![Operand::Immediate(ImmValue::Int(val as i64))];
                } else {
                    node.op = OpCode::ConstFloat;
                    node.operands = vec![Operand::Immediate(ImmValue::Float(val))];
                }
                graph.journal.push(JournalEntry {
                    run_number: run,
                    node_id,
                    mutation: MutationKind::ConstantFolded,
                    reason: 0,
                });
                folded += 1;
            }
        }
    }

    folded
}

fn cleanup_dead_edges(graph: &mut NeuralGraph) {
    graph.edges.retain(|edge| {
        let from_alive = graph.nodes.get(edge.from as usize)
            .map(|n| n.op != OpCode::Noop).unwrap_or(false);
        let to_alive = graph.nodes.get(edge.to as usize)
            .map(|n| n.op != OpCode::Noop).unwrap_or(false);
        from_alive && to_alive
    });
}

pub fn print_stats(stats: &OptStats) {
    eprintln!("--- optimization pass ---");
    if stats.paths_pruned > 0 {
        eprintln!("  dead paths pruned: {}", stats.paths_pruned);
    }
    if stats.nodes_specialized > 0 {
        eprintln!("  nodes specialized: {}", stats.nodes_specialized);
    }
    if stats.nodes_cached > 0 {
        eprintln!("  constants folded:  {}", stats.nodes_cached);
    }
    if stats.nodes_after < stats.nodes_before {
        eprintln!("  nodes: {} -> {} (-{})", stats.nodes_before, stats.nodes_after,
            stats.nodes_before - stats.nodes_after);
    }
}

fn get_node_ref(op: &Operand) -> u32 {
    match op { Operand::NodeRef(id) => *id, _ => 0 }
}
