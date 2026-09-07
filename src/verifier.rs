/// Strict .lyc graph verifier.
///
/// For AI-to-AI exchange, invalid capsules MUST fail closed.
/// This verifier checks structural integrity before any execution.

use crate::graph::*;

#[derive(Debug)]
pub struct VerifyError {
    pub errors: Vec<String>,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "verification failed ({} errors):", self.errors.len())?;
        for e in &self.errors {
            writeln!(f, "  - {e}")?;
        }
        Ok(())
    }
}

/// Verify a compiled graph is structurally sound.
/// Returns Ok(()) if valid, Err with all problems found if not.
pub fn verify(graph: &NeuralGraph) -> Result<(), VerifyError> {
    let mut errors = Vec::new();
    let node_count = graph.nodes.len() as u32;
    let string_count = graph.string_table.len() as u32;

    // 1. Entry node must exist
    if graph.entry >= node_count {
        errors.push(format!(
            "entry node #{} does not exist (graph has {} nodes)",
            graph.entry, node_count
        ));
    }

    // 2. Validate each node
    for node in &graph.nodes {
        // Node ID should match its position
        if node.id as usize >= graph.nodes.len() {
            errors.push(format!("node #{} has invalid id", node.id));
        }

        // Validate operand references
        for (i, op) in node.operands.iter().enumerate() {
            match op {
                Operand::NodeRef(id) => {
                    if *id >= node_count {
                        errors.push(format!(
                            "node #{} operand {}: references non-existent node #{}",
                            node.id, i, id
                        ));
                    }
                }
                Operand::StringRef(idx) => {
                    if *idx >= string_count {
                        errors.push(format!(
                            "node #{} operand {}: references non-existent string #{}",
                            node.id, i, idx
                        ));
                    }
                }
                _ => {}
            }
        }

        // Weights must be finite
        for (i, w) in node.weights.iter().enumerate() {
            if !w.is_finite() {
                errors.push(format!(
                    "node #{} weight {}: non-finite value {}",
                    node.id, i, w
                ));
            }
        }

        // Bias must be finite
        if !node.bias.is_finite() {
            errors.push(format!("node #{}: non-finite bias {}", node.id, node.bias));
        }

        // Annotation string ref must be valid
        if let Some(idx) = node.annotation {
            if idx >= string_count {
                errors.push(format!(
                    "node #{}: annotation references non-existent string #{}",
                    node.id, idx
                ));
            }
        }

        // Op-specific validation
        match node.op {
            OpCode::Branch => {
                if node.operands.len() < 3 {
                    errors.push(format!(
                        "node #{} Branch: needs 3 operands (cond, then, else), has {}",
                        node.id, node.operands.len()
                    ));
                }
            }
            OpCode::Guard => {
                if node.operands.len() < 3 {
                    errors.push(format!(
                        "node #{} Guard: needs 3 operands (assumption, fast, fallback), has {}",
                        node.id, node.operands.len()
                    ));
                }
            }
            OpCode::Call => {
                if node.operands.is_empty() {
                    errors.push(format!("node #{} Call: needs at least 1 operand (callee)", node.id));
                }
            }
            OpCode::StoreVar => {
                if node.operands.len() < 2 {
                    errors.push(format!(
                        "node #{} StoreVar: needs 2 operands (slot, value), has {}",
                        node.id, node.operands.len()
                    ));
                }
            }
            OpCode::Repeat => {
                if node.operands.is_empty() {
                    errors.push(format!("node #{} Repeat: needs at least 1 operand (count)", node.id));
                }
            }
            _ => {}
        }

        // SameOutput strategies must have pure operand subtrees
        if matches!(node.contract, crate::graph::Contract::SameOutput | crate::graph::Contract::WithinTolerance)
            && matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice)
        {
            for (i, op) in node.operands.iter().enumerate() {
                if let Operand::NodeRef(ref_id) = op {
                    if has_effects(graph, *ref_id, &mut Vec::new()) {
                        errors.push(format!(
                            "node #{} SameOutput strategy operand {} contains effectful nodes (Print/ReadLine) — \
                             SameOutput requires pure computation",
                            node.id, i
                        ));
                    }
                }
            }
        }
    }

    // 3. Validate edges
    for (i, edge) in graph.edges.iter().enumerate() {
        if edge.from >= node_count {
            errors.push(format!("edge {}: from node #{} does not exist", i, edge.from));
        }
        if edge.to >= node_count {
            errors.push(format!("edge {}: to node #{} does not exist", i, edge.to));
        }
        if !edge.weight.is_finite() {
            errors.push(format!("edge {}: non-finite weight {}", i, edge.weight));
        }
        if let Some(gate) = edge.gate {
            if gate >= node_count {
                errors.push(format!("edge {}: gate node #{} does not exist", i, gate));
            }
        }
    }

    // 4. Validate journal
    for (i, entry) in graph.journal.iter().enumerate() {
        if entry.node_id >= node_count {
            errors.push(format!(
                "journal entry {}: references non-existent node #{}",
                i, entry.node_id
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(VerifyError { errors })
    }
}

/// Check if a node subtree contains any effectful operations.
/// Effectful = Print, ReadLine, or any IO opcode.
fn has_effects(graph: &NeuralGraph, node_id: u32, visited: &mut Vec<u32>) -> bool {
    if visited.contains(&node_id) { return false; } // Cycle protection
    visited.push(node_id);

    let node = match graph.nodes.get(node_id as usize) {
        Some(n) => n,
        None => return false,
    };

    // These opcodes have side effects
    if matches!(node.op, OpCode::Print | OpCode::ReadLine | OpCode::Adapt | OpCode::Spawn | OpCode::Prune) {
        return true;
    }

    // Recurse into operand subtrees
    for op in &node.operands {
        if let Operand::NodeRef(ref_id) = op {
            if has_effects(graph, *ref_id, visited) {
                return true;
            }
        }
    }

    false
}
