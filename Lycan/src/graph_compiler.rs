/// Compiles Lycan AST → NeuralGraph.
///
/// Transforms the S-expression source representation into a weighted
/// computation graph. All human-readable identifiers become numeric
/// IDs. Strings get XOR-scrambled. The output is pure machine state.

use std::collections::HashMap;
use crate::ast::*;
use crate::graph::{WeightKind, Contract};
use crate::graph::*;

pub struct GraphCompiler {
    graph: NeuralGraph,
    var_map: HashMap<String, u32>,    // name → variable slot
    fn_map: HashMap<String, u32>,     // name → entry node ID
    var_node: HashMap<String, u32>,   // name → graph node ID that produced the value
    next_var_slot: u32,
}

impl GraphCompiler {
    pub fn new() -> Self {
        Self {
            graph: NeuralGraph::new(),
            var_map: HashMap::new(),
            fn_map: HashMap::new(),
            var_node: HashMap::new(),
            next_var_slot: 0,
        }
    }

    pub fn compile(mut self, program: &Program) -> NeuralGraph {
        // Create entry sequence node
        let mut top_nodes = Vec::new();
        for node in &program.nodes {
            let id = self.compile_node(node);
            top_nodes.push(id);
        }

        // Create the program entry point as a Sequence node
        let entry = self.graph.add_node(
            OpCode::Sequence,
            top_nodes.iter().map(|id| Operand::NodeRef(*id)).collect(),
        );
        // Add halt after sequence
        let halt = self.graph.add_node(OpCode::Halt, vec![]);
        self.graph.add_edge(entry, halt, 1.0);

        self.graph.entry = entry;
        self.graph
    }

    fn compile_node(&mut self, node: &Node) -> u32 {
        match node {
            Node::Int(n) => {
                self.graph.add_node(OpCode::ConstInt, vec![Operand::Immediate(ImmValue::Int(*n))])
            }
            Node::Float(f) => {
                self.graph.add_node(OpCode::ConstFloat, vec![Operand::Immediate(ImmValue::Float(*f))])
            }
            Node::Str(s) => {
                let idx = self.graph.intern_string(s);
                self.graph.add_node(OpCode::ConstStr, vec![Operand::StringRef(idx)])
            }
            Node::Bool(b) => {
                self.graph.add_node(OpCode::ConstBool, vec![Operand::Immediate(ImmValue::Bool(*b))])
            }
            Node::Null => {
                self.graph.add_node(OpCode::ConstNull, vec![Operand::Immediate(ImmValue::Null)])
            }

            Node::Ident(name) => {
                let slot = self.get_or_create_var(name);
                self.graph.add_node(OpCode::LoadVar, vec![Operand::VarSlot(slot)])
            }

            Node::Bind { name, value, .. } => {
                let val_id = self.compile_node(value);
                let slot = self.get_or_create_var(name);
                // Track which graph node this variable points to (for Feedback resolution)
                self.var_node.insert(name.clone(), val_id);
                self.graph.add_node(OpCode::StoreVar, vec![
                    Operand::VarSlot(slot),
                    Operand::NodeRef(val_id),
                ])
            }

            Node::Assign { name, value } => {
                let val_id = self.compile_node(value);
                let slot = self.get_or_create_var(name);
                self.graph.add_node(OpCode::StoreVar, vec![
                    Operand::VarSlot(slot),
                    Operand::NodeRef(val_id),
                ])
            }

            Node::Op { op, args } => {
                let opcode = match op {
                    OpKind::Add => OpCode::Add, OpKind::Sub => OpCode::Sub,
                    OpKind::Mul => OpCode::Mul, OpKind::Div => OpCode::Div,
                    OpKind::Mod => OpCode::Mod, OpKind::Neg => OpCode::Neg,
                    OpKind::Eq => OpCode::Eq, OpKind::Neq => OpCode::Neq,
                    OpKind::Lt => OpCode::Lt, OpKind::Gt => OpCode::Gt,
                    OpKind::Lte => OpCode::Lte, OpKind::Gte => OpCode::Gte,
                    OpKind::And => OpCode::And, OpKind::Or => OpCode::Or,
                    OpKind::Not => OpCode::Not,
                };
                let operands: Vec<Operand> = args.iter()
                    .map(|a| Operand::NodeRef(self.compile_node(a)))
                    .collect();
                self.graph.add_node(opcode, operands)
            }

            Node::If { cond, then_branch, else_branch } => {
                let cond_id = self.compile_node(cond);
                let then_id = self.compile_node(then_branch);
                let else_id = match else_branch {
                    Some(e) => self.compile_node(e),
                    None => self.graph.add_node(OpCode::ConstNull, vec![Operand::Immediate(ImmValue::Null)]),
                };

                // Branch node with weighted paths
                let branch = self.graph.add_node(OpCode::Branch, vec![
                    Operand::NodeRef(cond_id),
                    Operand::NodeRef(then_id),
                    Operand::NodeRef(else_id),
                ]);

                // Add weighted edges — these can adapt over time
                self.graph.add_edge(branch, then_id, 0.5);
                self.graph.add_edge(branch, else_id, 0.5);

                // Set initial weights on the branch node
                self.graph.nodes[branch as usize].weights = vec![0.5, 0.5];

                branch
            }

            Node::While { cond, body } => {
                let cond_id = self.compile_node(cond);
                let body_ids: Vec<Operand> = body.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                let mut operands = vec![Operand::NodeRef(cond_id)];
                operands.extend(body_ids);
                self.graph.add_node(OpCode::Loop, operands)
            }

            Node::ForEach { var, iterable, body } => {
                let iter_id = self.compile_node(iterable);
                let var_slot = self.get_or_create_var(var);
                let body_ids: Vec<Operand> = body.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                let mut operands = vec![
                    Operand::NodeRef(iter_id),
                    Operand::VarSlot(var_slot),
                ];
                operands.extend(body_ids);
                self.graph.add_node(OpCode::ForEach, operands)
            }

            Node::Repeat { count, body } => {
                let count_id = self.compile_node(count);
                let body_ids: Vec<Operand> = body.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                let mut operands = vec![Operand::NodeRef(count_id)];
                operands.extend(body_ids);
                self.graph.add_node(OpCode::Repeat, operands)
            }

            Node::Fn { name, params, body, .. } => {
                // Compile body nodes
                let body_ids: Vec<Operand> = body.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();

                // Params become var slots
                let mut param_slots = Vec::new();
                for p in params {
                    let slot = self.get_or_create_var(&p.name);
                    param_slots.push(Operand::VarSlot(slot));
                }

                let mut operands = param_slots;
                operands.extend(body_ids);

                let fn_node = self.graph.add_node(OpCode::Define, operands);

                // Register named function — return the StoreVar so it gets executed
                if let Some(fn_name) = name {
                    self.fn_map.insert(fn_name.clone(), fn_node);
                    let slot = self.get_or_create_var(fn_name);
                    let store = self.graph.add_node(OpCode::StoreVar, vec![
                        Operand::VarSlot(slot),
                        Operand::NodeRef(fn_node),
                    ]);
                    return store;
                }
                fn_node
            }

            Node::Call { callee, args } => {
                let callee_id = self.compile_node(callee);
                let mut operands = vec![Operand::NodeRef(callee_id)];
                for arg in args {
                    operands.push(Operand::NodeRef(self.compile_node(arg)));
                }
                self.graph.add_node(OpCode::Call, operands)
            }

            Node::Return(val) => {
                let val_id = self.compile_node(val);
                self.graph.add_node(OpCode::Return, vec![Operand::NodeRef(val_id)])
            }

            Node::Block(exprs) => {
                let operands: Vec<Operand> = exprs.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                self.graph.add_node(OpCode::Sequence, operands)
            }

            Node::Array(elems) => {
                let operands: Vec<Operand> = elems.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                self.graph.add_node(OpCode::Array, operands)
            }

            Node::Index { object, index } => {
                let obj_id = self.compile_node(object);
                let idx_id = self.compile_node(index);
                self.graph.add_node(OpCode::Index, vec![
                    Operand::NodeRef(obj_id),
                    Operand::NodeRef(idx_id),
                ])
            }

            Node::Range { start, end } => {
                let s = self.compile_node(start);
                let e = self.compile_node(end);
                self.graph.add_node(OpCode::Range, vec![
                    Operand::NodeRef(s),
                    Operand::NodeRef(e),
                ])
            }

            Node::Pipe { kind, data, func, init } => {
                let opcode = match kind {
                    PipeKind::Pipe => OpCode::Pipe,
                    PipeKind::Filter => OpCode::Filter,
                    PipeKind::Map => OpCode::Map,
                    PipeKind::Reduce => OpCode::Reduce,
                };
                let data_id = self.compile_node(data);
                let func_id = self.compile_node(func);
                let mut operands = vec![
                    Operand::NodeRef(data_id),
                    Operand::NodeRef(func_id),
                ];
                if let Some(init_expr) = init {
                    operands.push(Operand::NodeRef(self.compile_node(init_expr)));
                }
                self.graph.add_node(opcode, operands)
            }

            Node::Adapt { target, body } => {
                let body_ids: Vec<Operand> = body.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                let target_slot = self.get_or_create_var(target);
                let mut operands = vec![Operand::VarSlot(target_slot)];
                operands.extend(body_ids);
                self.graph.add_node(OpCode::Adapt, operands)
            }

            Node::Choice { options } => {
                let operands: Vec<Operand> = options.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                let n = operands.len();
                let id = self.graph.add_node(OpCode::AdaptiveChoice, operands);
                // Initialize equal weights
                let w = 1.0 / n as f64;
                self.graph.nodes[id as usize].weights = vec![w; n];
                self.graph.nodes[id as usize].weight_kind = WeightKind::Adaptive;
                id
            }

            Node::Guard { assumption, fast_path, fallback } => {
                let a = self.compile_node(assumption);
                let f = self.compile_node(fast_path);
                let fb = self.compile_node(fallback);
                self.graph.add_node(OpCode::Guard, vec![
                    Operand::NodeRef(a),
                    Operand::NodeRef(f),
                    Operand::NodeRef(fb),
                ])
            }

            Node::Strategy { options } => {
                let operands: Vec<Operand> = options.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                let n = operands.len();
                let id = self.graph.add_node(OpCode::Strategy, operands);
                // Equal selection weights + epsilon tolerance at the end
                let mut weights = vec![1.0 / n as f64; n];
                weights.push(1e-6); // tolerance epsilon (last element)
                self.graph.nodes[id as usize].weights = weights;
                self.graph.nodes[id as usize].weight_kind = WeightKind::Strategy;
                self.graph.nodes[id as usize].contract = Contract::WithinTolerance;
                id
            }

            Node::Feedback { target, reward } => {
                // Resolve target: if it's a variable, find the original graph node
                let t = match target.as_ref() {
                    Node::Ident(name) => {
                        // Look up the graph node that was stored in this variable
                        self.var_node.get(name).copied()
                            .unwrap_or_else(|| self.compile_node(target))
                    }
                    _ => self.compile_node(target),
                };
                let r = self.compile_node(reward);
                self.graph.add_node(OpCode::Feedback, vec![
                    Operand::NodeRef(t),
                    Operand::NodeRef(r),
                ])
            }

            Node::Builtin { name, args } => {
                if name == "lambert" {
                    let cap_idx = self.graph.intern_string("astro.lambertSolve");
                    let cap_name = self.graph.add_node(
                        OpCode::ConstStr,
                        vec![Operand::StringRef(cap_idx)],
                    );
                    let mut operands = vec![Operand::NodeRef(cap_name)];
                    operands.extend(args.iter().map(|n| Operand::NodeRef(self.compile_node(n))));
                    return self.graph.add_node(OpCode::Capability, operands);
                }

                let opcode = match name.as_str() {
                    "p" => OpCode::Print,
                    "r" => OpCode::ReadLine,
                    "num" => OpCode::ParseNum,
                    "split" => OpCode::Split,
                    "str" => OpCode::ToString,
                    "len" => OpCode::Length,
                    "chars" => OpCode::Chars,
                    "abs" => OpCode::Abs,
                    "sin" => OpCode::Sin,
                    "cos" => OpCode::Cos,
                    "floor" => OpCode::Floor,
                    "round" => OpCode::Round,
                    "sqrt" => OpCode::Sqrt,
                    "ln" => OpCode::Ln,
                    "exp" => OpCode::Exp,
                    "atan2" => OpCode::Atan2,
                    "cap" => OpCode::Capability,
                    "type" => OpCode::ToString, // close enough for now
                    _ => OpCode::Noop,
                };
                let operands: Vec<Operand> = args.iter()
                    .map(|n| Operand::NodeRef(self.compile_node(n)))
                    .collect();
                self.graph.add_node(opcode, operands)
            }
        }
    }

    fn get_or_create_var(&mut self, name: &str) -> u32 {
        if let Some(&slot) = self.var_map.get(name) {
            slot
        } else {
            let slot = self.next_var_slot;
            self.next_var_slot += 1;
            self.var_map.insert(name.to_string(), slot);
            slot
        }
    }
}
