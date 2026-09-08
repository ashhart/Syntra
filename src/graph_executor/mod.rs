//! Neural Graph Executor — runs `.lyc` binaries. Weights update during
//! execution and can be persisted back.

mod capability;
mod exec;
mod state;
mod value;

pub use value::{GVal, OptionStats};

use std::collections::HashMap;
use crate::graph::*;
use crate::error::{LycanError, LycanResult};

/// Executes a NeuralGraph.
pub struct GraphExecutor {
    pub graph: NeuralGraph,
    vars: HashMap<u32, GVal>,
    weight_deltas: Vec<(u32, usize, f64)>,
    depth: u32,
    max_depth: u32,
    /// Per-option stats: node_id -> vec of OptionStats (one per option)
    pub strategy_stats: HashMap<u32, Vec<OptionStats>>,
    run_number: u64,
    ctx: Option<crate::context::ExecutionContext>,
    /// Wall-clock deadline derived from `policy.max_execution_ms`; checked
    /// at `exec_node` entry (cheap clock read — the Instant is only read on
    /// first construction, the comparison is one subtraction).
    deadline: Option<std::time::Instant>,
    budget_ms: Option<u64>,
    /// Node-evaluation counter; the deadline is checked every 64th node to
    /// keep the hot path free of per-node clock reads.
    steps: u64,
    /// Captured stdout from !p / Print nodes.
    pub stdout_buffer: Vec<String>,
}

/// Signal for early return.
enum Flow {
    Val(GVal),
    Return(GVal),
}

impl Flow {
    fn into_val(self) -> GVal {
        match self { Flow::Val(v) | Flow::Return(v) => v }
    }
}

impl GraphExecutor {
    pub fn new(graph: NeuralGraph) -> Self {
        // Derive run number from entry node activation count
        let run = graph.nodes.get(graph.entry as usize)
            .map(|n| n.activation_count).unwrap_or(0);
        Self {
            graph,
            vars: HashMap::new(),
            weight_deltas: Vec::new(),
            depth: 0,
            max_depth: 65536,
            strategy_stats: HashMap::new(),
            run_number: run,
            ctx: None,
            deadline: None,
            budget_ms: None,
            steps: 0,
            stdout_buffer: Vec::new(),
        }
    }

    pub fn new_with_context(graph: NeuralGraph, ctx: crate::context::ExecutionContext) -> Self {
        let run = graph.nodes.get(graph.entry as usize)
            .map(|n| n.activation_count).unwrap_or(0);
        let budget_ms = ctx.policy.as_ref().and_then(|p| p.max_execution_ms);
        let deadline = budget_ms
            .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
        Self {
            graph,
            vars: HashMap::new(),
            weight_deltas: Vec::new(),
            depth: 0,
            max_depth: 65536,
            strategy_stats: HashMap::new(),
            run_number: run,
            ctx: Some(ctx),
            deadline,
            budget_ms,
            steps: 0,
            stdout_buffer: Vec::new(),
        }
    }

    /// Execute the graph from the entry point.
    pub fn run(&mut self) -> LycanResult<GVal> {
        // Load persisted strategy stats from graph state vector
        self.load_strategy_stats();

        let entry = self.graph.entry;
        let result = self.exec_node(entry)?.into_val();

        // Apply weight adaptations
        self.apply_weight_deltas();

        // Persist strategy stats back to graph state
        self.save_strategy_stats();

        Ok(result)
    }

    /// Get the graph back (with updated weights and activation counts).
    pub fn into_graph(self) -> NeuralGraph {
        self.graph
    }
}

fn rt_err(msg: &str) -> LycanError {
    LycanError::Runtime { msg: msg.to_string() }
}
