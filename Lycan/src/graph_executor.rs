/// Neural Graph Executor — runs `.lyc` binaries. Weights update during
/// execution and can be persisted back.

use std::collections::HashMap;
use std::io;
use crate::graph::*;
use crate::error::{LycanError, LycanResult};

/// Runtime value during graph execution.
#[derive(Debug, Clone)]
pub enum GVal {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Array(Vec<GVal>),
    GraphFn {
        param_slots: Vec<u32>,
        body_nodes: Vec<u32>,
    },
}

impl GVal {
    fn is_truthy(&self) -> bool {
        match self {
            GVal::Bool(b) => *b,
            GVal::Null => false,
            GVal::Int(0) => false,
            GVal::Str(s) if s.is_empty() => false,
            GVal::Array(a) if a.is_empty() => false,
            _ => true,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            GVal::Int(_) => "int", GVal::Float(_) => "float",
            GVal::Str(_) => "str", GVal::Bool(_) => "bool",
            GVal::Null => "null", GVal::Array(_) => "array",
            GVal::GraphFn { .. } => "fn",
        }
    }
}

impl std::fmt::Display for GVal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GVal::Int(n) => write!(f, "{n}"),
            GVal::Float(n) => write!(f, "{n}"),
            GVal::Str(s) => write!(f, "{s}"),
            GVal::Bool(b) => write!(f, "{}", if *b { "true" } else { "false" }),
            GVal::Null => write!(f, "null"),
            GVal::Array(a) => {
                write!(f, "(A")?;
                for v in a { write!(f, " {v}")?; }
                write!(f, ")")
            }
            GVal::GraphFn { .. } => write!(f, "(fn)"),
        }
    }
}

/// Per-option statistics for Strategy/AdaptiveChoice nodes.
#[derive(Debug, Clone, Default)]
pub struct OptionStats {
    pub tries: u64,
    pub total_ns: u128,
    pub correct: u64,
}

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
            stdout_buffer: Vec::new(),
        }
    }

    pub fn new_with_context(graph: NeuralGraph, ctx: crate::context::ExecutionContext) -> Self {
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
            ctx: Some(ctx),
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

    fn exec_node(&mut self, id: u32) -> LycanResult<Flow> {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(rt_err("max recursion depth exceeded"));
        }
        let result = self.exec_node_inner(id);
        self.depth -= 1;
        result
    }

    fn exec_node_inner(&mut self, id: u32) -> LycanResult<Flow> {
        // Track activation
        if id as usize >= self.graph.nodes.len() {
            return Ok(Flow::Val(GVal::Null));
        }
        if let Some(node) = self.graph.nodes.get_mut(id as usize) {
            node.activation_count += 1;
        }

        let node = self.graph.nodes[id as usize].clone();

        match node.op {
            // ── Values ──
            OpCode::ConstInt => {
                if let Some(Operand::Immediate(ImmValue::Int(n))) = node.operands.first() {
                    Ok(Flow::Val(GVal::Int(*n)))
                } else { Ok(Flow::Val(GVal::Null)) }
            }
            OpCode::ConstFloat => {
                if let Some(Operand::Immediate(ImmValue::Float(f))) = node.operands.first() {
                    Ok(Flow::Val(GVal::Float(*f)))
                } else { Ok(Flow::Val(GVal::Null)) }
            }
            OpCode::ConstStr => {
                if let Some(Operand::StringRef(idx)) = node.operands.first() {
                    Ok(Flow::Val(GVal::Str(self.graph.get_string(*idx))))
                } else { Ok(Flow::Val(GVal::Str(String::new()))) }
            }
            OpCode::ConstBool => {
                if let Some(Operand::Immediate(ImmValue::Bool(b))) = node.operands.first() {
                    Ok(Flow::Val(GVal::Bool(*b)))
                } else { Ok(Flow::Val(GVal::Bool(false))) }
            }
            OpCode::ConstNull => Ok(Flow::Val(GVal::Null)),

            // ── Variables ──
            OpCode::LoadVar => {
                let slot = self.get_var_slot(&node.operands[0]);
                let val = self.vars.get(&slot).cloned().unwrap_or(GVal::Null);
                Ok(Flow::Val(val))
            }
            OpCode::StoreVar => {
                let slot = self.get_var_slot(&node.operands[0]);
                let val = self.eval_operand(&node.operands[1])?;
                self.vars.insert(slot, val);
                Ok(Flow::Val(GVal::Null))
            }

            // ── Arithmetic ──
            OpCode::Add => self.binary_op(&node, |a, b| arith_add(a, b)),
            OpCode::Sub => self.binary_op(&node, |a, b| arith(a, b, |x,y| x-y, |x,y| x-y)),
            OpCode::Mul => self.binary_op(&node, |a, b| arith(a, b, |x,y| x*y, |x,y| x*y)),
            OpCode::Div => self.binary_op(&node, |a, b| arith_div(a, b)),
            OpCode::Mod => self.binary_op(&node, |a, b| arith(a, b, |x,y| x%y, |x,y| x%y)),
            OpCode::Neg => {
                let a = self.eval_operand(&node.operands[0])?;
                match a {
                    GVal::Int(n) => Ok(Flow::Val(GVal::Int(-n))),
                    GVal::Float(f) => Ok(Flow::Val(GVal::Float(-f))),
                    _ => Err(rt_err(&format!("cannot negate {}", a.type_name()))),
                }
            }
            OpCode::Abs => {
                let a = self.eval_operand(&node.operands[0])?;
                abs_val(a).map(Flow::Val)
            }
            OpCode::Floor => {
                let a = self.eval_operand(&node.operands[0])?;
                floor_val(a).map(Flow::Val)
            }
            OpCode::Sin => {
                let a = self.eval_operand(&node.operands[0])?;
                unary_float(a, "sin", f64::sin).map(Flow::Val)
            }
            OpCode::Cos => {
                let a = self.eval_operand(&node.operands[0])?;
                unary_float(a, "cos", f64::cos).map(Flow::Val)
            }
            OpCode::Round => {
                let a = self.eval_operand(&node.operands[0])?;
                round_val(a).map(Flow::Val)
            }
            OpCode::Sqrt => {
                let a = self.eval_operand(&node.operands[0])?;
                sqrt_val(a).map(Flow::Val)
            }
            OpCode::Ln => {
                let a = self.eval_operand(&node.operands[0])?;
                match a {
                    GVal::Float(f) if f > 0.0 => Ok(Flow::Val(GVal::Float(f.ln()))),
                    GVal::Int(n) if n > 0 => Ok(Flow::Val(GVal::Float((n as f64).ln()))),
                    _ => Err(rt_err("ln requires positive number")),
                }
            }
            OpCode::Exp => {
                let a = self.eval_operand(&node.operands[0])?;
                match a {
                    GVal::Float(f) => Ok(Flow::Val(GVal::Float(f.exp()))),
                    GVal::Int(n) => Ok(Flow::Val(GVal::Float((n as f64).exp()))),
                    _ => Err(rt_err("exp requires number")),
                }
            }
            OpCode::Atan2 => {
                let y = self.eval_operand(&node.operands[0])?;
                let x = self.eval_operand(&node.operands[1])?;
                let yf = match y { GVal::Float(f) => f, GVal::Int(n) => n as f64, _ => 0.0 };
                let xf = match x { GVal::Float(f) => f, GVal::Int(n) => n as f64, _ => 0.0 };
                Ok(Flow::Val(GVal::Float(yf.atan2(xf))))
            }

            // ── Comparison ──
            OpCode::Eq => self.binary_op(&node, |a, b| Ok(GVal::Bool(gval_eq(&a, &b)))),
            OpCode::Neq => self.binary_op(&node, |a, b| Ok(GVal::Bool(!gval_eq(&a, &b)))),
            OpCode::Lt => self.binary_op(&node, |a, b| gval_cmp(a, b, |o| o.is_lt())),
            OpCode::Gt => self.binary_op(&node, |a, b| gval_cmp(a, b, |o| o.is_gt())),
            OpCode::Lte => self.binary_op(&node, |a, b| gval_cmp(a, b, |o| o.is_le())),
            OpCode::Gte => self.binary_op(&node, |a, b| gval_cmp(a, b, |o| o.is_ge())),

            // ── Logic ──
            OpCode::And => self.binary_op(&node, |a, b| Ok(GVal::Bool(a.is_truthy() && b.is_truthy()))),
            OpCode::Or => self.binary_op(&node, |a, b| Ok(GVal::Bool(a.is_truthy() || b.is_truthy()))),
            OpCode::Not => {
                let a = self.eval_operand(&node.operands[0])?;
                Ok(Flow::Val(GVal::Bool(!a.is_truthy())))
            }

            // ── Control flow ──
            OpCode::Branch => {
                // operands: [cond, then_node, else_node]
                let cond = self.eval_operand(&node.operands[0])?;
                let taken = cond.is_truthy();

                // Adapt weights based on which branch was taken
                if node.weights.len() >= 2 {
                    let idx = if taken { 0 } else { 1 };
                    // Strengthen the taken path slightly
                    self.weight_deltas.push((id, idx, 0.01));
                    // Weaken the other
                    self.weight_deltas.push((id, 1 - idx, -0.01));
                }

                if taken {
                    self.exec_node(self.get_node_ref(&node.operands[1]))
                } else {
                    self.exec_node(self.get_node_ref(&node.operands[2]))
                }
            }

            OpCode::AdaptiveChoice => {
                if node.weights.is_empty() || node.operands.is_empty() {
                    return Ok(Flow::Val(GVal::Null));
                }
                // Tolerance-contract nodes carry an extra trailing weight slot
                // for the tolerance epsilon — exclude it from selection.
                let n_options = if node.contract == crate::graph::Contract::WithinTolerance
                    && node.weights.len() > 1
                {
                    node.weights.len() - 1
                } else {
                    node.weights.len()
                };
                let n_options = n_options.min(node.operands.len());
                if n_options == 0 {
                    return Ok(Flow::Val(GVal::Null));
                }

                let (mode, epsilon) = match self.ctx.as_ref() {
                    Some(c) => (c.selection_mode, c.selection_epsilon),
                    None => (crate::context::SelectionMode::Greedy, 0.0),
                };

                let best_idx = match mode {
                    crate::context::SelectionMode::Greedy => {
                        let mut bi = 0;
                        let mut bw = f64::NEG_INFINITY;
                        for i in 0..n_options {
                            let w = node.weights[i];
                            if w > bw { bw = w; bi = i; }
                        }
                        bi
                    }
                    crate::context::SelectionMode::Weighted => {
                        let sum: f64 = node.weights[..n_options].iter().sum();
                        if sum <= 0.0 {
                            0
                        } else {
                            let r = crate::learning::rand_f64() * sum;
                            let mut cum = 0.0;
                            let mut pick = n_options - 1;
                            for i in 0..n_options {
                                cum += node.weights[i];
                                if r < cum { pick = i; break; }
                            }
                            pick
                        }
                    }
                    crate::context::SelectionMode::EpsilonGreedy => {
                        if crate::learning::rand_f64() < epsilon {
                            (crate::learning::rand_f64() * n_options as f64) as usize
                        } else {
                            let mut bi = 0;
                            let mut bw = f64::NEG_INFINITY;
                            for i in 0..n_options {
                                let w = node.weights[i];
                                if w > bw { bw = w; bi = i; }
                            }
                            bi
                        }
                    }
                };
                let best_idx = best_idx.min(n_options - 1);

                self.graph.nodes[id as usize].bias = best_idx as f64;
                if best_idx < node.operands.len() {
                    self.exec_node(self.get_node_ref(&node.operands[best_idx]))
                } else {
                    self.exec_node(self.get_node_ref(&node.operands[0]))
                }
            }

            OpCode::Guard => {
                // Guard node: check assumption FIRST, then run appropriate path.
                // operands[0] = assumption (bool check)
                // operands[1] = fast_path (run if assumption holds)
                // operands[2] = fallback (run if assumption fails — deopt)
                if node.operands.len() < 3 {
                    return Ok(Flow::Val(GVal::Null));
                }
                let assumption = self.eval_operand(&node.operands[0])?;
                if assumption.is_truthy() {
                    // Guard passed — run fast path
                    self.exec_node(self.get_node_ref(&node.operands[1]))
                } else {
                    // Guard failed — deoptimize to fallback
                    self.exec_node(self.get_node_ref(&node.operands[2]))
                }
            }

            OpCode::Strategy => {
                // Strategy node with contracts, exploration, timing, auto-reward.
                if node.weights.is_empty() || node.operands.is_empty() {
                    return Ok(Flow::Val(GVal::Null));
                }
                let n_options = node.weights.len().min(node.operands.len());
                let node_id = id;
                let contract = node.contract;

                // Initialize stats if needed
                if !self.strategy_stats.contains_key(&node_id) {
                    self.strategy_stats.insert(node_id, vec![OptionStats::default(); n_options]);
                }

                // ── Contract: SameOutput ──
                // Run ALL options (PURE ONLY — no side effects allowed).
                // Use MAJORITY AGREEMENT to determine truth — not option 0.
                // If options disagree, NO learning occurs (safe default).
                if contract == Contract::SameOutput {
                    // Check purity: SameOutput strategies must not contain
                    // effectful nodes (Print, ReadLine, etc.) in their subtrees.
                    // For now we run all options but suppress side effects by
                    // only running pure computation. Full purity enforcement
                    // would require static analysis of the subtree.

                    let mut results = Vec::new();
                    let mut result_strs = Vec::new();
                    let mut times = Vec::new();
                    for i in 0..n_options {
                        let start = std::time::Instant::now();
                        let r = self.exec_node(self.get_node_ref(&node.operands[i]))?.into_val();
                        let elapsed = start.elapsed().as_nanos();
                        let r_str = format!("{r}");
                        result_strs.push(r_str);
                        results.push(r);
                        times.push(elapsed);
                    }

                    // MAJORITY AGREEMENT: find the most common result
                    let mut vote_counts: Vec<(String, usize)> = Vec::new();
                    for s in &result_strs {
                        if let Some(entry) = vote_counts.iter_mut().find(|(v, _)| v == s) {
                            entry.1 += 1;
                        } else {
                            vote_counts.push((s.clone(), 1));
                        }
                    }
                    vote_counts.sort_by(|a, b| b.1.cmp(&a.1));
                    let majority_result = &vote_counts[0].0;
                    let majority_count = vote_counts[0].1;

                    // Mark which options agree with majority
                    let correct_mask: Vec<bool> = result_strs.iter()
                        .map(|s| s == majority_result)
                        .collect();

                    // If NO majority (all different), skip learning — unsafe
                    let has_majority = majority_count > n_options / 2;

                    // Update stats
                    if let Some(stats) = self.strategy_stats.get_mut(&node_id) {
                        for i in 0..n_options {
                            if i < stats.len() {
                                stats[i].tries += 1;
                                stats[i].total_ns += times[i];
                                if correct_mask[i] { stats[i].correct += 1; }
                            }
                        }
                    }

                    // Only update weights if we have clear majority agreement
                    if has_majority {
                        let learning_rate = 0.08;
                        let n = self.graph.nodes[node_id as usize].weights.len();
                        let min_correct_time = times.iter().enumerate()
                            .filter(|(i, _)| correct_mask.get(*i).copied().unwrap_or(false))
                            .map(|(_, t)| *t)
                            .min().unwrap_or(0) as f64;
                        let max_time = *times.iter().max().unwrap_or(&1) as f64;
                        let range = max_time - min_correct_time;

                        for i in 0..n {
                            if !correct_mask.get(i).copied().unwrap_or(true) {
                                // PUNISH minority disagreement
                                self.graph.nodes[node_id as usize].weights[i] =
                                    (self.graph.nodes[node_id as usize].weights[i] - 0.2).clamp(0.01, 0.99);
                            } else if range > 0.0 {
                                let score = 1.0 - 2.0 * (times[i] as f64 - min_correct_time) / range;
                                let delta = score * learning_rate;
                                self.graph.nodes[node_id as usize].weights[i] =
                                    (self.graph.nodes[node_id as usize].weights[i] + delta).clamp(0.01, 0.99);
                            }
                        }
                        // Normalize
                        let sum: f64 = self.graph.nodes[node_id as usize].weights.iter().sum();
                        if sum > 0.0 {
                            for i in 0..n { self.graph.nodes[node_id as usize].weights[i] /= sum; }
                        }

                        // Journal the weight update
                        self.graph.journal.push(crate::graph::JournalEntry {
                            run_number: self.run_number,
                            node_id,
                            mutation: crate::graph::MutationKind::WeightUpdate,
                            reason: u32::MAX,
                        });
                    }

                    // Return the majority result using the highest-weight option
                    let mut best_idx = 0;
                    let mut best_w = f64::NEG_INFINITY;
                    for (i, w) in self.graph.nodes[node_id as usize].weights.iter().enumerate() {
                        if *w > best_w && correct_mask.get(i).copied().unwrap_or(false) {
                            best_w = *w; best_idx = i;
                        }
                    }
                    self.graph.nodes[node_id as usize].bias = best_idx as f64;
                    return Ok(Flow::Val(results.remove(best_idx)));
                }

                // ── Contract: WithinTolerance ──
                // Run ALL options. Compare numeric results within epsilon.
                // Tolerant of floating-point differences across solvers.
                if contract == Contract::WithinTolerance {
                    let mut results = Vec::new();
                    let mut times = Vec::new();
                    let mut numeric_vals: Vec<f64> = Vec::new();

                    // Epsilon is stored as the last element of the weights vector
                    let tol = *node.weights.last().unwrap_or(&1e-6);

                    for i in 0..n_options {
                        let start = std::time::Instant::now();
                        let r = self.exec_node(self.get_node_ref(&node.operands[i]))?.into_val();
                        let elapsed = start.elapsed().as_nanos();

                        // Extract numeric value for comparison
                        let num = match &r {
                            GVal::Float(f) => *f,
                            GVal::Int(n) => *n as f64,
                            GVal::Str(s) => {
                                // Try parse comma-separated values and sum them for comparison
                                s.split(',').filter_map(|p| p.trim().parse::<f64>().ok()).sum()
                            }
                            _ => 0.0,
                        };
                        numeric_vals.push(num);
                        results.push(r);
                        times.push(elapsed);
                    }

                    // Find median value as reference (robust to outliers)
                    let mut sorted_vals = numeric_vals.clone();
                    sorted_vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let median = sorted_vals[sorted_vals.len() / 2];

                    // Mark which options are within tolerance of median
                    let correct_mask: Vec<bool> = numeric_vals.iter()
                        .map(|v| (v - median).abs() <= tol)
                        .collect();
                    let correct_count = correct_mask.iter().filter(|&&c| c).count();
                    let has_consensus = correct_count > n_options / 2;

                    // Update stats
                    if let Some(stats) = self.strategy_stats.get_mut(&node_id) {
                        for i in 0..n_options {
                            if i < stats.len() {
                                stats[i].tries += 1;
                                stats[i].total_ns += times[i];
                                if correct_mask[i] { stats[i].correct += 1; }
                            }
                        }
                    }

                    // Update weights only if consensus exists
                    if has_consensus {
                        let learning_rate = 0.08;
                        let n = n_options.min(self.graph.nodes[node_id as usize].weights.len() - 1); // -1 for epsilon
                        let min_correct_time = times.iter().enumerate()
                            .filter(|(i, _)| correct_mask.get(*i).copied().unwrap_or(false))
                            .map(|(_, t)| *t)
                            .min().unwrap_or(0) as f64;
                        let max_time = *times.iter().max().unwrap_or(&1) as f64;
                        let range = max_time - min_correct_time;

                        for i in 0..n {
                            if !correct_mask.get(i).copied().unwrap_or(true) {
                                self.graph.nodes[node_id as usize].weights[i] =
                                    (self.graph.nodes[node_id as usize].weights[i] - 0.2).clamp(0.01, 0.99);
                            } else if range > 0.0 {
                                let score = 1.0 - 2.0 * (times[i] as f64 - min_correct_time) / range;
                                let delta = score * learning_rate;
                                self.graph.nodes[node_id as usize].weights[i] =
                                    (self.graph.nodes[node_id as usize].weights[i] + delta).clamp(0.01, 0.99);
                            }
                        }
                        // Normalize selection weights (not the epsilon at the end)
                        let sum: f64 = self.graph.nodes[node_id as usize].weights[..n].iter().sum();
                        if sum > 0.0 {
                            for i in 0..n { self.graph.nodes[node_id as usize].weights[i] /= sum; }
                        }

                        self.graph.journal.push(crate::graph::JournalEntry {
                            run_number: self.run_number,
                            node_id,
                            mutation: crate::graph::MutationKind::WeightUpdate,
                            reason: u32::MAX,
                        });
                    }

                    // Return result from highest-weight correct option
                    let mut best_idx = 0;
                    let mut best_w = f64::NEG_INFINITY;
                    let n = n_options.min(self.graph.nodes[node_id as usize].weights.len() - 1);
                    for i in 0..n {
                        let w = self.graph.nodes[node_id as usize].weights[i];
                        if w > best_w && correct_mask.get(i).copied().unwrap_or(false) {
                            best_w = w; best_idx = i;
                        }
                    }
                    self.graph.nodes[node_id as usize].bias = best_idx as f64;
                    return Ok(Flow::Val(results.remove(best_idx)));
                }

                // ── Default: exploration + timing (no contract) ──
                let total_tries: u64 = self.strategy_stats.get(&node_id)
                    .map(|s| s.iter().map(|o| o.tries).sum()).unwrap_or(0);
                let epsilon = (0.3 / (1.0 + total_tries as f64 / 5.0)).max(0.02);

                let exploring = {
                    let pseudo_random = (node.activation_count * 7 + 13) % 100;
                    (pseudo_random as f64 / 100.0) < epsilon
                };

                let chosen_idx = if exploring {
                    let stats = self.strategy_stats.get(&node_id).unwrap();
                    let mut min_tries = u64::MAX;
                    let mut candidates = Vec::new();
                    for (i, s) in stats.iter().enumerate() {
                        if s.tries < min_tries { min_tries = s.tries; candidates.clear(); candidates.push(i); }
                        else if s.tries == min_tries { candidates.push(i); }
                    }
                    let pick = node.activation_count as usize % candidates.len();
                    candidates[pick]
                } else {
                    let mut best_idx = 0;
                    let mut best_w = f64::NEG_INFINITY;
                    for (i, w) in node.weights.iter().enumerate() {
                        if i < n_options && *w > best_w { best_w = *w; best_idx = i; }
                    }
                    best_idx
                };

                self.graph.nodes[node_id as usize].bias = chosen_idx as f64;

                let start = std::time::Instant::now();
                let result = if chosen_idx < node.operands.len() {
                    self.exec_node(self.get_node_ref(&node.operands[chosen_idx]))?
                } else {
                    self.exec_node(self.get_node_ref(&node.operands[0]))?
                };
                let elapsed_ns = start.elapsed().as_nanos();

                if let Some(stats) = self.strategy_stats.get_mut(&node_id) {
                    if chosen_idx < stats.len() {
                        stats[chosen_idx].tries += 1;
                        stats[chosen_idx].total_ns += elapsed_ns;
                        stats[chosen_idx].correct += 1;
                    }
                }

                if let Some(stats) = self.strategy_stats.get(&node_id) {
                    let avg_times: Vec<f64> = stats.iter().map(|s| {
                        if s.tries > 0 { s.total_ns as f64 / s.tries as f64 } else { f64::MAX }
                    }).collect();

                    if stats.iter().all(|s| s.tries > 0) {
                        let min_time = avg_times.iter().cloned().fold(f64::MAX, f64::min);
                        let max_time = avg_times.iter().cloned().fold(0.0f64, f64::max);
                        let range = max_time - min_time;

                        if range > 0.0 {
                            let learning_rate = 0.08;
                            let n = self.graph.nodes[node_id as usize].weights.len();
                            for i in 0..n {
                                if i < avg_times.len() {
                                    let score = 1.0 - 2.0 * (avg_times[i] - min_time) / range;
                                    let delta = score * learning_rate;
                                    self.graph.nodes[node_id as usize].weights[i] =
                                        (self.graph.nodes[node_id as usize].weights[i] + delta)
                                        .clamp(0.01, 0.99);
                                }
                            }
                            let sum: f64 = self.graph.nodes[node_id as usize].weights.iter().sum();
                            if sum > 0.0 {
                                for i in 0..n { self.graph.nodes[node_id as usize].weights[i] /= sum; }
                            }
                            // Journal the weight update
                            self.graph.journal.push(crate::graph::JournalEntry {
                                run_number: self.run_number,
                                node_id,
                                mutation: crate::graph::MutationKind::WeightUpdate,
                                reason: u32::MAX,
                            });
                        }
                    }
                }

                Ok(result)
            }

            OpCode::Sequence => {
                let mut last = GVal::Null;
                for op in &node.operands {
                    let nid = self.get_node_ref(op);
                    match self.exec_node(nid)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Val(v) => last = v,
                    }
                }
                Ok(Flow::Val(last))
            }

            OpCode::Loop => {
                // While loop: operands[0]=cond, operands[1..]=body
                self.exec_while(&node)
            }

            OpCode::ForEach => {
                // For-each: operands[0]=iterable, operands[1]=VarSlot, operands[2..]=body
                self.exec_foreach(&node)
            }

            OpCode::Repeat => {
                // Repeat N times: operands[0]=count, operands[1..]=body
                let count_val = self.eval_operand(&node.operands[0])?;
                let n = match count_val {
                    GVal::Int(n) => n,
                    _ => return Err(rt_err(&format!("repeat count must be int, got {}", count_val.type_name()))),
                };
                let body_refs: Vec<u32> = node.operands[1..].iter()
                    .map(|op| self.get_node_ref(op))
                    .collect();
                let mut last = GVal::Null;
                for _ in 0..n {
                    for &nid in &body_refs {
                        match self.exec_node(nid)? {
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Val(v) => last = v,
                        }
                    }
                }
                Ok(Flow::Val(last))
            }

            // ── Functions ──
            OpCode::Define => {
                // Collect param slots and body node refs
                let mut param_slots = Vec::new();
                let mut body_nodes = Vec::new();
                let mut in_params = true;
                for op in &node.operands {
                    match op {
                        Operand::VarSlot(s) if in_params => param_slots.push(*s),
                        _ => {
                            in_params = false;
                            body_nodes.push(self.get_node_ref(op));
                        }
                    }
                }
                Ok(Flow::Val(GVal::GraphFn { param_slots, body_nodes }))
            }

            OpCode::Call => {
                let callee = self.eval_operand(&node.operands[0])?;
                let mut args = Vec::new();
                for op in &node.operands[1..] {
                    args.push(self.eval_operand(op)?);
                }
                let result = self.call_fn(&callee, &args)?;
                Ok(Flow::Val(result))
            }

            OpCode::Return => {
                let val = self.eval_operand(&node.operands[0])?;
                Ok(Flow::Return(val))
            }

            OpCode::Lambda => {
                // Same as Define but inline
                let mut param_slots = Vec::new();
                let mut body_nodes = Vec::new();
                let mut in_params = true;
                for op in &node.operands {
                    match op {
                        Operand::VarSlot(s) if in_params => param_slots.push(*s),
                        _ => {
                            in_params = false;
                            body_nodes.push(self.get_node_ref(op));
                        }
                    }
                }
                Ok(Flow::Val(GVal::GraphFn { param_slots, body_nodes }))
            }

            // ── Collections ──
            OpCode::Array => {
                let mut elems = Vec::new();
                for op in &node.operands {
                    elems.push(self.eval_operand(op)?);
                }
                Ok(Flow::Val(GVal::Array(elems)))
            }

            OpCode::Index => {
                let obj = self.eval_operand(&node.operands[0])?;
                let idx = self.eval_operand(&node.operands[1])?;
                match (&obj, &idx) {
                    (GVal::Array(arr), GVal::Int(i)) => {
                        let i = *i as usize;
                        if i < arr.len() {
                            Ok(Flow::Val(arr[i].clone()))
                        } else {
                            Err(rt_err(&format!("index {i} out of bounds (len {})", arr.len())))
                        }
                    }
                    _ => Err(rt_err(&format!("cannot index {} with {}", obj.type_name(), idx.type_name()))),
                }
            }

            OpCode::Range => {
                let s = self.eval_operand(&node.operands[0])?;
                let e = self.eval_operand(&node.operands[1])?;
                match (&s, &e) {
                    (GVal::Int(a), GVal::Int(b)) => {
                        let arr: Vec<GVal> = (*a..*b).map(GVal::Int).collect();
                        Ok(Flow::Val(GVal::Array(arr)))
                    }
                    _ => Err(rt_err("range requires integers")),
                }
            }

            OpCode::Chars => {
                let val = self.eval_operand(&node.operands[0])?;
                match val {
                    GVal::Str(s) => {
                        let chars: Vec<GVal> = s.chars()
                            .map(|c| GVal::Str(c.to_string()))
                            .collect();
                        Ok(Flow::Val(GVal::Array(chars)))
                    }
                    _ => Err(rt_err(&format!("cannot get chars of {}", val.type_name()))),
                }
            }

            OpCode::Length => {
                let val = self.eval_operand(&node.operands[0])?;
                match &val {
                    GVal::Array(a) => Ok(Flow::Val(GVal::Int(a.len() as i64))),
                    GVal::Str(s) => Ok(Flow::Val(GVal::Int(s.len() as i64))),
                    _ => Err(rt_err(&format!("cannot get length of {}", val.type_name()))),
                }
            }

            // ── IO ──
            OpCode::Print => {
                if let Some(ctx) = &self.ctx {
                    if let Some(pol) = &ctx.policy {
                        if !pol.allow_stdout {
                            return Err(rt_err("capability=print effect=stdout denied by policy"));
                        }
                    }
                }
                let mut parts = Vec::new();
                for op in &node.operands {
                    parts.push(format!("{}", self.eval_operand(op)?));
                }
                let line = parts.join(" ");
                println!("{}", line);
                self.stdout_buffer.push(line);
                Ok(Flow::Val(GVal::Null))
            }

            OpCode::ReadLine => {
                if let Some(ctx) = &self.ctx {
                    if let Some(pol) = &ctx.policy {
                        if !pol.allow_stdin {
                            return Err(rt_err("capability=readline effect=stdin denied by policy"));
                        }
                    }
                }
                let mut input = String::new();
                io::stdin().read_line(&mut input).map_err(|e| rt_err(&format!("read error: {e}")))?;
                Ok(Flow::Val(GVal::Str(input.trim_end().to_string())))
            }

            OpCode::ParseNum => {
                let val = self.eval_operand(&node.operands[0])?;
                match val {
                    GVal::Str(s) => {
                        let s = s.trim();
                        if let Ok(n) = s.parse::<i64>() {
                            Ok(Flow::Val(GVal::Int(n)))
                        } else if let Ok(f) = s.parse::<f64>() {
                            Ok(Flow::Val(GVal::Float(f)))
                        } else {
                            Err(rt_err(&format!("cannot parse '{s}' as number")))
                        }
                    }
                    GVal::Int(n) => Ok(Flow::Val(GVal::Int(n))),
                    GVal::Float(f) => Ok(Flow::Val(GVal::Float(f))),
                    _ => Err(rt_err(&format!("cannot convert {} to number", val.type_name()))),
                }
            }

            OpCode::Split => {
                let val = self.eval_operand(&node.operands[0])?;
                let delim = if node.operands.len() > 1 {
                    match self.eval_operand(&node.operands[1])? {
                        GVal::Str(s) => s,
                        _ => " ".to_string(),
                    }
                } else { " ".to_string() };
                match val {
                    GVal::Str(s) => {
                        let parts: Vec<GVal> = s.split(&delim)
                            .filter(|p| !p.is_empty())
                            .map(|p| GVal::Str(p.to_string()))
                            .collect();
                        Ok(Flow::Val(GVal::Array(parts)))
                    }
                    _ => Err(rt_err(&format!("cannot split {}", val.type_name()))),
                }
            }

            OpCode::ToString => {
                let val = self.eval_operand(&node.operands[0])?;
                Ok(Flow::Val(GVal::Str(format!("{val}"))))
            }

            // ── Adaptation ──
            OpCode::Adapt => {
                // operands[0] = VarSlot of target, rest = new body
                let slot = self.get_var_slot(&node.operands[0]);
                let body_nodes: Vec<u32> = node.operands[1..].iter()
                    .map(|op| self.get_node_ref(op))
                    .collect();
                // Get existing param slots if target is a function
                let param_slots = match self.vars.get(&slot) {
                    Some(GVal::GraphFn { param_slots, .. }) => param_slots.clone(),
                    _ => Vec::new(),
                };
                self.vars.insert(slot, GVal::GraphFn { param_slots, body_nodes });
                Ok(Flow::Val(GVal::Null))
            }

            OpCode::Spawn => {
                // Create a new node at runtime
                let op_val = self.eval_operand(&node.operands[0])?;
                let opcode = match op_val {
                    GVal::Int(n) => opcode_from_u8(n as u8),
                    _ => OpCode::Noop,
                };
                let new_id = self.graph.add_node(opcode, vec![]);
                Ok(Flow::Val(GVal::Int(new_id as i64)))
            }

            OpCode::Prune => {
                // Mark a node as Noop (effectively dead)
                let target = self.eval_operand(&node.operands[0])?;
                if let GVal::Int(nid) = target {
                    if let Some(n) = self.graph.nodes.get_mut(nid as usize) {
                        n.op = OpCode::Noop;
                    }
                }
                Ok(Flow::Val(GVal::Null))
            }

            OpCode::Feedback => {
                // Feedback: operands[0] = target node ref, operands[1] = reward value
                // Updates the weights on an AdaptiveChoice/Strategy node.
                // Positive reward strengthens the last-chosen path.
                // Negative reward weakens it.
                if node.operands.len() >= 2 {
                    let target_id = self.get_node_ref(&node.operands[0]);
                    let reward = self.eval_operand(&node.operands[1])?;
                    let reward_val = match reward {
                        GVal::Float(f) => f,
                        GVal::Int(n) => n as f64,
                        GVal::Bool(true) => 1.0,
                        GVal::Bool(false) => -1.0,
                        _ => 0.0,
                    };

                    // Find the target node and update its weights
                    if let Some(target) = self.graph.nodes.get_mut(target_id as usize) {
                        if matches!(target.op, OpCode::AdaptiveChoice | OpCode::Strategy)
                            && !target.weights.is_empty()
                        {
                            // Use the stored choice index from bias field
                            let best_idx = target.bias as usize;

                            // Apply reward: strengthen winner, weaken others
                            let learning_rate = 0.05;
                            let delta = reward_val * learning_rate;
                            let n = target.weights.len();
                            for i in 0..n {
                                if i == best_idx {
                                    target.weights[i] = (target.weights[i] + delta).clamp(0.01, 0.99);
                                } else {
                                    target.weights[i] = (target.weights[i] - delta / (n - 1) as f64).clamp(0.01, 0.99);
                                }
                            }

                            // Normalize
                            let sum: f64 = target.weights.iter().sum();
                            if sum > 0.0 {
                                for i in 0..n {
                                    target.weights[i] /= sum;
                                }
                            }
                            // Journal
                            let tid = target_id;
                            self.graph.journal.push(crate::graph::JournalEntry {
                                run_number: self.run_number,
                                node_id: tid,
                                mutation: crate::graph::MutationKind::WeightUpdate,
                                reason: u32::MAX,
                            });
                        }
                    }
                }
                Ok(Flow::Val(GVal::Null))
            }

            OpCode::Weight | OpCode::Predict => {
                Ok(Flow::Val(GVal::Null))
            }

            // ── Pipeline ──
            OpCode::Pipe => {
                let data = self.eval_operand(&node.operands[0])?;
                let func = self.eval_operand(&node.operands[1])?;
                self.call_fn(&func, &[data]).map(Flow::Val)
            }

            OpCode::Filter => {
                let data = self.eval_operand(&node.operands[0])?;
                let func = self.eval_operand(&node.operands[1])?;
                let items = expect_array(data)?;
                let mut result = Vec::new();
                for item in items {
                    if self.call_fn(&func, &[item.clone()])?.is_truthy() {
                        result.push(item);
                    }
                }
                Ok(Flow::Val(GVal::Array(result)))
            }

            OpCode::Map => {
                let data = self.eval_operand(&node.operands[0])?;
                let func = self.eval_operand(&node.operands[1])?;
                let items = expect_array(data)?;
                let mut result = Vec::new();
                for item in items {
                    result.push(self.call_fn(&func, &[item])?);
                }
                Ok(Flow::Val(GVal::Array(result)))
            }

            OpCode::Reduce => {
                let data = self.eval_operand(&node.operands[0])?;
                let func = self.eval_operand(&node.operands[1])?;
                let init = if node.operands.len() > 2 {
                    self.eval_operand(&node.operands[2])?
                } else { GVal::Null };
                let items = expect_array(data)?;
                let mut acc = init;
                for item in items {
                    acc = self.call_fn(&func, &[acc, item])?;
                }
                Ok(Flow::Val(acc))
            }

            // ── Native capabilities ──
            OpCode::Capability => {
                let name = match node.operands.first() {
                    Some(op) => match self.eval_operand(op)? {
                        GVal::Str(s) => s,
                        other => return Err(rt_err(&format!(
                            "capability name must be str, got {}",
                            other.type_name()
                        ))),
                    },
                    None => return Err(rt_err("capability node expects a name")),
                };
                let mut args = Vec::new();
                for op in &node.operands[1..] {
                    args.push(self.eval_operand(op)?);
                }
                self.exec_capability_gval(&name, &args).map(Flow::Val)
            }

            OpCode::Merge | OpCode::Noop => Ok(Flow::Val(GVal::Null)),
            OpCode::Halt => Ok(Flow::Val(GVal::Null)),
        }
    }

    fn exec_while(&mut self, node: &GraphNode) -> LycanResult<Flow> {
        let cond_id = self.get_node_ref(&node.operands[0]);
        let body_refs: Vec<u32> = node.operands[1..].iter()
            .map(|op| self.get_node_ref(op))
            .collect();

        let mut last = GVal::Null;
        loop {
            let cond = self.exec_node(cond_id)?.into_val();
            if !cond.is_truthy() { break; }
            for &nid in &body_refs {
                match self.exec_node(nid)? {
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    Flow::Val(v) => last = v,
                }
            }
        }
        Ok(Flow::Val(last))
    }

    fn exec_foreach(&mut self, node: &GraphNode) -> LycanResult<Flow> {
        let iter_id = self.get_node_ref(&node.operands[0]);
        let var_slot = self.get_var_slot(&node.operands[1]);
        let body_refs: Vec<u32> = node.operands[2..].iter()
            .map(|op| self.get_node_ref(op))
            .collect();

        let iterable = self.exec_node(iter_id)?.into_val();
        let items = expect_array(iterable)?;

        let mut last = GVal::Null;
        for item in items {
            self.vars.insert(var_slot, item);
            for &nid in &body_refs {
                match self.exec_node(nid)? {
                    Flow::Return(v) => return Ok(Flow::Return(v)),
                    Flow::Val(v) => last = v,
                }
            }
        }
        Ok(Flow::Val(last))
    }

    fn call_fn(&mut self, callee: &GVal, args: &[GVal]) -> LycanResult<GVal> {
        match callee {
            GVal::GraphFn { param_slots, body_nodes } => {
                let param_slots = param_slots.clone();
                let body_nodes = body_nodes.clone();
                // Bind args to param slots
                let mut old_vals = Vec::new();
                for (i, &slot) in param_slots.iter().enumerate() {
                    old_vals.push((slot, self.vars.get(&slot).cloned()));
                    self.vars.insert(slot, args.get(i).cloned().unwrap_or(GVal::Null));
                }
                // Execute body
                let mut result = GVal::Null;
                for &nid in &body_nodes {
                    match self.exec_node(nid)? {
                        Flow::Return(v) => {
                            self.restore_vars(&old_vals);
                            return Ok(v);
                        }
                        Flow::Val(v) => result = v,
                    }
                }
                self.restore_vars(&old_vals);
                Ok(result)
            }
            _ => Err(rt_err(&format!("cannot call {}", callee.type_name()))),
        }
    }

    fn restore_vars(&mut self, old_vals: &[(u32, Option<GVal>)]) {
        for (slot, val) in old_vals {
            match val {
                Some(v) => { self.vars.insert(*slot, v.clone()); }
                None => { self.vars.remove(slot); }
            }
        }
    }

    fn eval_operand(&mut self, op: &Operand) -> LycanResult<GVal> {
        match op {
            Operand::NodeRef(id) => Ok(self.exec_node(*id)?.into_val()),
            Operand::Immediate(ImmValue::Int(n)) => Ok(GVal::Int(*n)),
            Operand::Immediate(ImmValue::Float(f)) => Ok(GVal::Float(*f)),
            Operand::Immediate(ImmValue::Bool(b)) => Ok(GVal::Bool(*b)),
            Operand::Immediate(ImmValue::Null) => Ok(GVal::Null),
            Operand::StateRef(idx) => {
                Ok(GVal::Float(self.graph.state.get(*idx as usize).copied().unwrap_or(0.0)))
            }
            Operand::StringRef(idx) => Ok(GVal::Str(self.graph.get_string(*idx))),
            Operand::VarSlot(slot) => Ok(self.vars.get(slot).cloned().unwrap_or(GVal::Null)),
        }
    }

    fn get_node_ref(&self, op: &Operand) -> u32 {
        match op {
            Operand::NodeRef(id) => *id,
            _ => 0,
        }
    }

    fn get_var_slot(&self, op: &Operand) -> u32 {
        match op {
            Operand::VarSlot(s) => *s,
            _ => 0,
        }
    }

    fn binary_op<F>(&mut self, node: &GraphNode, f: F) -> LycanResult<Flow>
    where F: FnOnce(GVal, GVal) -> LycanResult<GVal>
    {
        let a = self.eval_operand(&node.operands[0])?;
        let b = self.eval_operand(&node.operands[1])?;
        f(a, b).map(Flow::Val)
    }

    /// Apply accumulated weight changes after execution.
    fn apply_weight_deltas(&mut self) {
        for (node_id, weight_idx, delta) in &self.weight_deltas {
            if let Some(node) = self.graph.nodes.get_mut(*node_id as usize) {
                if let Some(w) = node.weights.get_mut(*weight_idx) {
                    *w = (*w + delta).clamp(0.01, 0.99);
                }
            }
        }
        // Normalize weights per node
        let node_ids: Vec<u32> = self.weight_deltas.iter().map(|(id, _, _)| *id).collect();
        for id in node_ids {
            if let Some(node) = self.graph.nodes.get_mut(id as usize) {
                let sum: f64 = node.weights.iter().sum();
                if sum > 0.0 {
                    for w in &mut node.weights {
                        *w /= sum;
                    }
                }
            }
        }
        self.weight_deltas.clear();
    }

    /// Load strategy stats from the graph's state vector.
    /// Layout per strategy node: [tries_0, time_0, correct_0, tries_1, time_1, correct_1, ...]
    fn load_strategy_stats(&mut self) {
        for node in &self.graph.nodes {
            if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) { continue; }
            if let Some(slot) = node.state_slot {
                let n = node.weights.len();
                let mut stats = vec![OptionStats::default(); n];
                for i in 0..n {
                    let base = slot as usize + i * 3;
                    if base + 2 < self.graph.state.len() {
                        stats[i].tries = self.graph.state[base] as u64;
                        stats[i].total_ns = self.graph.state[base + 1] as u128;
                        stats[i].correct = self.graph.state[base + 2] as u64;
                    }
                }
                self.strategy_stats.insert(node.id, stats);
            }
        }
    }

    /// Save strategy stats back to the graph's state vector.
    fn save_strategy_stats(&mut self) {
        for (&node_id, stats) in &self.strategy_stats {
            let node = &mut self.graph.nodes[node_id as usize];
            let n = stats.len();
            let slots_needed = n * 3;

            // Allocate state slots if not yet assigned
            if node.state_slot.is_none() {
                let base = self.graph.state.len();
                self.graph.state.resize(base + slots_needed, 0.0);
                node.state_slot = Some(base as u32);
            }

            if let Some(slot) = node.state_slot {
                // Ensure state vector is large enough
                let end = slot as usize + slots_needed;
                if end > self.graph.state.len() {
                    self.graph.state.resize(end, 0.0);
                }
                for (i, s) in stats.iter().enumerate() {
                    let base = slot as usize + i * 3;
                    self.graph.state[base] = s.tries as f64;
                    self.graph.state[base + 1] = s.total_ns as f64;
                    self.graph.state[base + 2] = s.correct as f64;
                }
            }
        }
    }

    fn exec_capability_gval(&self, name: &str, args: &[GVal]) -> LycanResult<GVal> {
        let cap_args = args.iter()
            .map(gval_to_cap_value)
            .collect::<LycanResult<Vec<_>>>()?;
        crate::capabilities::execute(name, &cap_args, self.ctx.as_ref())
            .map(cap_value_to_gval)
            .map_err(|msg| rt_err(&msg))
    }
}

// ── Helpers ──

fn rt_err(msg: &str) -> LycanError {
    LycanError::Runtime { msg: msg.to_string() }
}

fn arith_add(a: GVal, b: GVal) -> LycanResult<GVal> {
    match (&a, &b) {
        (GVal::Int(x), GVal::Int(y)) => Ok(GVal::Int(x + y)),
        (GVal::Float(x), GVal::Float(y)) => Ok(GVal::Float(x + y)),
        (GVal::Int(x), GVal::Float(y)) => Ok(GVal::Float(*x as f64 + y)),
        (GVal::Float(x), GVal::Int(y)) => Ok(GVal::Float(x + *y as f64)),
        (GVal::Str(x), GVal::Str(y)) => Ok(GVal::Str(format!("{x}{y}"))),
        (GVal::Str(x), _) => Ok(GVal::Str(format!("{x}{b}"))),
        (_, GVal::Str(y)) => Ok(GVal::Str(format!("{a}{y}"))),
        (GVal::Array(x), GVal::Array(y)) => {
            let mut r = x.clone(); r.extend(y.iter().cloned()); Ok(GVal::Array(r))
        }
        _ => Err(rt_err(&format!("cannot add {} and {}", a.type_name(), b.type_name()))),
    }
}

fn arith_div(a: GVal, b: GVal) -> LycanResult<GVal> {
    match (&a, &b) {
        (GVal::Int(x), GVal::Int(y)) => {
            if *y == 0 { return Err(rt_err("division by zero")); }
            if x % y == 0 { Ok(GVal::Int(x / y)) }
            else { Ok(GVal::Float(*x as f64 / *y as f64)) }
        }
        (GVal::Float(x), GVal::Float(y)) => Ok(GVal::Float(x / y)),
        (GVal::Int(x), GVal::Float(y)) => Ok(GVal::Float(*x as f64 / y)),
        (GVal::Float(x), GVal::Int(y)) => Ok(GVal::Float(x / *y as f64)),
        _ => Err(rt_err(&format!("cannot divide {} by {}", a.type_name(), b.type_name()))),
    }
}

fn arith(a: GVal, b: GVal, int_op: fn(i64,i64)->i64, float_op: fn(f64,f64)->f64) -> LycanResult<GVal> {
    match (&a, &b) {
        (GVal::Int(x), GVal::Int(y)) => Ok(GVal::Int(int_op(*x, *y))),
        (GVal::Float(x), GVal::Float(y)) => Ok(GVal::Float(float_op(*x, *y))),
        (GVal::Int(x), GVal::Float(y)) => Ok(GVal::Float(float_op(*x as f64, *y))),
        (GVal::Float(x), GVal::Int(y)) => Ok(GVal::Float(float_op(*x, *y as f64))),
        _ => Err(rt_err(&format!("cannot do arithmetic on {} and {}", a.type_name(), b.type_name()))),
    }
}

fn abs_val(a: GVal) -> LycanResult<GVal> {
    match a {
        GVal::Int(n) => n.checked_abs()
            .map(GVal::Int)
            .ok_or_else(|| rt_err("integer overflow in abs")),
        GVal::Float(f) if f.is_finite() => Ok(GVal::Float(f.abs())),
        GVal::Float(_) => Err(rt_err("abs requires finite float")),
        _ => Err(rt_err(&format!("cannot abs {}", a.type_name()))),
    }
}

fn unary_float(a: GVal, name: &str, op: fn(f64) -> f64) -> LycanResult<GVal> {
    let input = match a {
        GVal::Int(n) => n as f64,
        GVal::Float(f) => f,
        _ => return Err(rt_err(&format!("{name} requires number, got {}", a.type_name()))),
    };
    if !input.is_finite() {
        return Err(rt_err(&format!("{name} requires finite input")));
    }
    let output = op(input);
    if !output.is_finite() {
        return Err(rt_err(&format!("{name} produced non-finite output")));
    }
    Ok(GVal::Float(output))
}

fn round_val(a: GVal) -> LycanResult<GVal> {
    match a {
        GVal::Int(n) => Ok(GVal::Int(n)),
        GVal::Float(f) => {
            if !f.is_finite() {
                return Err(rt_err("round requires finite float"));
            }
            let rounded = f.round();
            if rounded < i64::MIN as f64 || rounded > i64::MAX as f64 {
                return Err(rt_err("round result out of i64 range"));
            }
            Ok(GVal::Int(rounded as i64))
        }
        _ => Err(rt_err(&format!("cannot round {}", a.type_name()))),
    }
}

fn floor_val(a: GVal) -> LycanResult<GVal> {
    match a {
        GVal::Int(n) => Ok(GVal::Int(n)),
        GVal::Float(f) => {
            if !f.is_finite() {
                return Err(rt_err("floor requires finite float"));
            }
            Ok(GVal::Float(f.floor()))
        }
        _ => Err(rt_err(&format!("cannot floor {}", a.type_name()))),
    }
}

fn sqrt_val(a: GVal) -> LycanResult<GVal> {
    let input = match a {
        GVal::Int(n) => n as f64,
        GVal::Float(f) => f,
        _ => return Err(rt_err(&format!("cannot sqrt {}", a.type_name()))),
    };
    if !input.is_finite() {
        return Err(rt_err("sqrt requires finite input"));
    }
    if input < 0.0 {
        return Err(rt_err("sqrt requires non-negative input"));
    }
    Ok(GVal::Float(input.sqrt()))
}

fn gval_eq(a: &GVal, b: &GVal) -> bool {
    match (a, b) {
        (GVal::Int(x), GVal::Int(y)) => x == y,
        (GVal::Float(x), GVal::Float(y)) => x == y,
        (GVal::Str(x), GVal::Str(y)) => x == y,
        (GVal::Bool(x), GVal::Bool(y)) => x == y,
        (GVal::Null, GVal::Null) => true,
        _ => false,
    }
}

fn gval_cmp(a: GVal, b: GVal, f: fn(std::cmp::Ordering) -> bool) -> LycanResult<GVal> {
    let ord = match (&a, &b) {
        (GVal::Int(x), GVal::Int(y)) => x.cmp(y),
        (GVal::Float(x), GVal::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (GVal::Int(x), GVal::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (GVal::Float(x), GVal::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(std::cmp::Ordering::Equal),
        _ => return Err(rt_err(&format!("cannot compare {} and {}", a.type_name(), b.type_name()))),
    };
    Ok(GVal::Bool(f(ord)))
}

fn expect_array(val: GVal) -> LycanResult<Vec<GVal>> {
    match val {
        GVal::Array(items) => Ok(items),
        _ => Err(rt_err(&format!("expected array, got {}", val.type_name()))),
    }
}

fn gval_to_cap_value(value: &GVal) -> LycanResult<crate::capabilities::CapValue> {
    Ok(match value {
        GVal::Int(n) => crate::capabilities::CapValue::Int(*n),
        GVal::Float(n) => crate::capabilities::CapValue::Float(*n),
        GVal::Str(s) => crate::capabilities::CapValue::Str(s.clone()),
        GVal::Bool(b) => crate::capabilities::CapValue::Bool(*b),
        GVal::Null => crate::capabilities::CapValue::Null,
        GVal::Array(items) => crate::capabilities::CapValue::Array(
            items.iter()
                .map(gval_to_cap_value)
                .collect::<LycanResult<Vec<_>>>()?
        ),
        GVal::GraphFn { .. } => return Err(rt_err("capability arguments cannot include functions")),
    })
}

fn cap_value_to_gval(value: crate::capabilities::CapValue) -> GVal {
    match value {
        crate::capabilities::CapValue::Int(n) => GVal::Int(n),
        crate::capabilities::CapValue::Float(n) => GVal::Float(n),
        crate::capabilities::CapValue::Str(s) => GVal::Str(s),
        crate::capabilities::CapValue::Bool(b) => GVal::Bool(b),
        crate::capabilities::CapValue::Null => GVal::Null,
        crate::capabilities::CapValue::Array(items) => {
            GVal::Array(items.into_iter().map(cap_value_to_gval).collect())
        }
    }
}

fn opcode_from_u8(b: u8) -> OpCode {
    // For runtime node spawning
    match b {
        0x10 => OpCode::Add, 0x11 => OpCode::Sub,
        0x12 => OpCode::Mul, 0x13 => OpCode::Div,
        0x75 => OpCode::Sin, 0x76 => OpCode::Cos,
        0x77 => OpCode::Abs, 0x78 => OpCode::Floor,
        0x79 => OpCode::Round, 0x7A => OpCode::Sqrt,
        0x70 => OpCode::Print,
        _ => OpCode::Noop,
    }
}
