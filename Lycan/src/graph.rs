/// Lycan Neural Graph — the program representation.
/// Two layers: immutable semantic graph (nodes/edges/types) plus a mutable
/// adaptive layer (weights, activations, journal, guards).
#[derive(Debug, Clone)]
pub struct NeuralGraph {
    pub header: GraphHeader,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<Edge>,
    pub string_table: Vec<Vec<u8>>,
    pub state: Vec<f64>,
    pub entry: u32,
    pub journal: Vec<JournalEntry>,  // Evolution history
}

#[derive(Debug, Clone)]
pub struct GraphHeader {
    pub version: u8,
    pub node_count: u32,
    pub edge_count: u32,
    #[allow(dead_code)]
    pub state_size: u32,
    pub string_count: u32,
    pub flags: u32,
}

/// A computation node — like a neuron.
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: u32,
    pub op: OpCode,
    pub operands: Vec<Operand>,
    pub weights: Vec<f64>,           // Learnable parameters
    pub bias: f64,                   // Type specialization hint (0=generic, 1=int, 2=float)
    pub activation_count: u64,       // How many times this node has fired
    pub state_slot: Option<u32>,
    pub weight_kind: WeightKind,     // What the weights mean for this node
    pub annotation: Option<u32>,     // String table index — intent/meaning annotation
    pub contract: Contract,          // Correctness contract for strategy nodes
    pub objective: Objective,         // What the weights optimize for
}

/// Correctness contract for Strategy/AdaptiveChoice nodes.
/// Defines what "correct" means for this strategy.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum Contract {
    /// No contract — any output is accepted (default)
    None = 0,
    /// All options must produce the exact same output (string equality).
    SameOutput = 1,
    /// Output must satisfy a validator node (bool check)
    Validated = 2,
    /// All options must produce numeric results within tolerance.
    /// Stores epsilon in the node's weights[weights.len()-1] slot.
    WithinTolerance = 3,
}

/// What a node's weights represent.
/// This is THE key architectural distinction in Lycan.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum WeightKind {
    /// Weights are observational. They track hot paths but
    /// the condition always decides truth. Safe to ignore.
    Observational = 0,
    /// Weights are semantic. The program is explicitly allowed
    /// to choose behavior based on learned preference.
    Adaptive = 1,
    /// Weights record likely runtime type for dispatch optimization.
    TypeHint = 2,
    /// Weights choose between strategy alternatives (e.g. algorithms).
    Strategy = 3,
    /// Weights represent decision confidence based on real-world outcomes.
    #[allow(dead_code)]
    Decision = 4,
}

/// What a decision/strategy node is optimizing for.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum Objective {
    None = 0,
    Speed = 1,
    Accuracy = 2,
    Reliability = 3,
    Cost = 4,
    Risk = 5,
    Confidence = 6,
    Reward = 7,
    MultiObjective = 8,
}

/// Evolution journal entry — records what changed and why.
#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub run_number: u64,
    pub node_id: u32,
    pub mutation: MutationKind,
    pub reason: u32,         // String table index
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum MutationKind {
    WeightUpdate = 0,
    TypeSpecialized = 1,
    ConstantFolded = 2,
    PathPruned = 3,
    GuardInserted = 4,
    NodeSpawned = 5,
    FeedbackReceived = 6,
    EvolutionStarted = 7,
    ProposalAccepted = 8,
    ProposalRejected = 9,
    EvolutionCompleted = 10,
}

/// Weighted connection between nodes.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub weight: f64,
    pub gate: Option<u32>,           // Optional gating node (if gate output is falsy, edge is inactive)
}

/// What a node computes.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum OpCode {
    // ── Values ──
    ConstInt = 0x01,
    ConstFloat = 0x02,
    ConstStr = 0x03,     // Index into string table
    ConstBool = 0x04,
    ConstNull = 0x05,
    LoadVar = 0x06,      // Load from variable slot
    StoreVar = 0x07,     // Store to variable slot

    // ── Arithmetic ──
    Add = 0x10,
    Sub = 0x11,
    Mul = 0x12,
    Div = 0x13,
    Mod = 0x14,
    Neg = 0x15,

    // ── Comparison ──
    Eq = 0x20,
    Neq = 0x21,
    Lt = 0x22,
    Gt = 0x23,
    Lte = 0x24,
    Gte = 0x25,

    // ── Logic ──
    And = 0x30,
    Or = 0x31,
    Not = 0x32,

    // ── Control flow ──
    Branch = 0x40,       // Deterministic branch — condition decides, weights OBSERVE only
    Merge = 0x41,        // Merge multiple paths
    Loop = 0x42,         // While loop: operands[0]=cond, rest=body
    Sequence = 0x43,     // Execute children in order
    ForEach = 0x46,      // For-each: operands[0]=iterable, operands[1]=VarSlot, rest=body
    Repeat = 0x47,       // Repeat N times: operands[0]=count, rest=body
    AdaptiveChoice = 0x44, // Adaptive branch — weights DECIDE (explicitly declared)
    Guard = 0x45,        // Guard node: fast path + assumption check + deopt fallback

    // ── Functions ──
    Define = 0x50,       // Define a callable subgraph
    Call = 0x51,         // Call a subgraph
    Return = 0x52,       // Return from subgraph
    Lambda = 0x53,       // Inline callable

    // ── Collections ──
    Array = 0x60,        // Create array
    Index = 0x61,        // Array access
    Range = 0x62,        // Generate range array
    Length = 0x63,       // Array/string length
    Chars = 0x64,        // String to char array

    // ── IO ──
    Print = 0x70,
    ReadLine = 0x71,
    ParseNum = 0x72,
    Split = 0x73,
    ToString = 0x74,
    Sin = 0x75,
    Cos = 0x76,
    Abs = 0x77,
    Floor = 0x78,
    Round = 0x79,
    Sqrt = 0x7A,
    Ln = 0x7B,
    Exp = 0x7C,
    Atan2 = 0x7D,

    // ── Neural / Adaptive ──
    Adapt = 0x80,        // Rewrite a node/subgraph
    Weight = 0x81,       // Adjust edge weights
    Predict = 0x82,      // Predict next node (prefetch)
    Feedback = 0x83,     // Send result feedback to adjust weights
    Spawn = 0x84,        // Create new node at runtime
    Prune = 0x85,        // Remove underperforming path
    Strategy = 0x86,     // Choose algorithm based on weights (e.g. recursive vs memoized)

    // ── Pipeline ──
    Pipe = 0x90,
    Filter = 0x91,
    Map = 0x92,
    Reduce = 0x93,

    // ── Native capabilities ──
    Capability = 0xA0,  // Runtime-provided capability, addressed by string name

    // ── Meta ──
    Noop = 0xFE,
    Halt = 0xFF,
}

/// Operand — what a node takes as input.
#[derive(Debug, Clone)]
pub enum Operand {
    NodeRef(u32),        // Reference to another node's output
    Immediate(ImmValue), // Inline constant
    StateRef(u32),       // Reference to persistent state slot
    StringRef(u32),      // Index into string table
    VarSlot(u32),        // Variable slot number
}

/// Inline constant value.
#[derive(Debug, Clone)]
pub enum ImmValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

// ── String storage ──
// Strings are stored as raw bytes. No scrambling, no obfuscation.
// The binary is unreadable because of its STRUCTURE (83 nodes with
// cross-referenced operands and weights), not because we hid anything.
// Same reason you can't read a neural network by looking at its weights.

// ── Binary serialization ──

const MAGIC: [u8; 4] = [0x4C, 0x59, 0x43, 0x4E]; // "LYCN" in hex, not readable as "LYCAN"
const FORMAT_VERSION: u8 = 5;

impl NeuralGraph {
    pub fn new() -> Self {
        Self {
            header: GraphHeader {
                version: FORMAT_VERSION,
                node_count: 0,
                edge_count: 0,
                state_size: 0,
                string_count: 0,
                flags: 0,
            },
            nodes: Vec::new(),
            edges: Vec::new(),
            string_table: Vec::new(),
            state: Vec::new(),
            entry: 0,
            journal: Vec::new(),
        }
    }

    /// Add a string to the table, return its index.
    pub fn intern_string(&mut self, s: &str) -> u32 {
        let idx = self.string_table.len() as u32;
        self.string_table.push(s.as_bytes().to_vec());
        self.header.string_count = self.string_table.len() as u32;
        idx
    }

    /// Add a node, return its ID.
    pub fn add_node(&mut self, op: OpCode, operands: Vec<Operand>) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(GraphNode {
            id,
            op,
            operands,
            weights: Vec::new(),
            bias: 0.0,
            activation_count: 0,
            state_slot: None,
            weight_kind: WeightKind::Observational,
            annotation: None,
            contract: Contract::None,
            objective: Objective::None,
        });
        self.header.node_count = self.nodes.len() as u32;
        id
    }

    /// Add a weighted edge.
    pub fn add_edge(&mut self, from: u32, to: u32, weight: f64) {
        self.edges.push(Edge { from, to, weight, gate: None });
        self.header.edge_count = self.edges.len() as u32;
    }

    /// Serialize the entire graph to binary.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Magic + version
        buf.extend_from_slice(&MAGIC);
        buf.push(self.header.version);

        // Header fields — use actual lengths, not cached header values
        write_u32(&mut buf, self.nodes.len() as u32);
        write_u32(&mut buf, self.edges.len() as u32);
        write_u32(&mut buf, self.state.len() as u32);
        write_u32(&mut buf, self.string_table.len() as u32);
        write_u32(&mut buf, self.header.flags);
        write_u32(&mut buf, self.entry);

        // String table
        for s in &self.string_table {
            write_u32(&mut buf, s.len() as u32);
            buf.extend_from_slice(s);
        }

        // Nodes
        for node in &self.nodes {
            write_u32(&mut buf, node.id);
            buf.push(node.op as u8);
            write_u32(&mut buf, node.operands.len() as u32);
            for op in &node.operands {
                encode_operand(&mut buf, op);
            }
            write_u32(&mut buf, node.weights.len() as u32);
            for w in &node.weights {
                write_f64(&mut buf, *w);
            }
            write_f64(&mut buf, node.bias);
            write_u64(&mut buf, node.activation_count);
            match node.state_slot {
                Some(slot) => { buf.push(1); write_u32(&mut buf, slot); }
                None => buf.push(0),
            }
            buf.push(node.weight_kind as u8);
            match node.annotation {
                Some(idx) => { buf.push(1); write_u32(&mut buf, idx); }
                None => buf.push(0),
            }
            buf.push(node.contract as u8);
            buf.push(node.objective as u8);
        }

        // Edges
        for edge in &self.edges {
            write_u32(&mut buf, edge.from);
            write_u32(&mut buf, edge.to);
            write_f64(&mut buf, edge.weight);
            match edge.gate {
                Some(g) => { buf.push(1); write_u32(&mut buf, g); }
                None => buf.push(0),
            }
        }

        // State
        write_u32(&mut buf, self.state.len() as u32);
        for s in &self.state {
            write_f64(&mut buf, *s);
        }

        // Journal
        write_u32(&mut buf, self.journal.len() as u32);
        for entry in &self.journal {
            write_u64(&mut buf, entry.run_number);
            write_u32(&mut buf, entry.node_id);
            buf.push(entry.mutation as u8);
            write_u32(&mut buf, entry.reason);
        }

        buf
    }

    /// Deserialize from binary.
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < 5 || data[0..4] != MAGIC {
            return Err("invalid .lyc file: bad magic".into());
        }
        let mut pos = 4;
        let version = read_u8(data, &mut pos);
        if version != FORMAT_VERSION {
            return Err(format!("unsupported version {version}"));
        }

        let node_count = read_u32(data, &mut pos);
        let edge_count = read_u32(data, &mut pos);
        let state_size = read_u32(data, &mut pos);
        let string_count = read_u32(data, &mut pos);
        let flags = read_u32(data, &mut pos);
        let entry = read_u32(data, &mut pos);

        // String table
        let mut string_table = Vec::with_capacity(string_count as usize);
        for _ in 0..string_count {
            let len = read_u32(data, &mut pos) as usize;
            if pos + len > data.len() {
                return Err(format!("string table overflow at pos {pos}, len {len}"));
            }
            let s = data[pos..pos + len].to_vec();
            pos += len;
            string_table.push(s);
        }

        // Nodes
        let mut nodes = Vec::with_capacity(node_count as usize);
        for _ in 0..node_count {
            let id = read_u32(data, &mut pos);
            let op_byte = read_u8(data, &mut pos);
            let op = opcode_from_byte(op_byte)?;
            let operand_count = read_u32(data, &mut pos) as usize;
            let mut operands = Vec::with_capacity(operand_count);
            for _ in 0..operand_count {
                operands.push(decode_operand(data, &mut pos)?);
            }
            let weight_count = read_u32(data, &mut pos) as usize;
            let mut weights = Vec::with_capacity(weight_count);
            for _ in 0..weight_count {
                weights.push(read_f64(data, &mut pos));
            }
            let bias = read_f64(data, &mut pos);
            let activation_count = read_u64(data, &mut pos);
            let has_state = read_u8(data, &mut pos) != 0;
            let state_slot = if has_state { Some(read_u32(data, &mut pos)) } else { None };
            let wk_byte = read_u8(data, &mut pos);
            let weight_kind = match wk_byte {
                1 => WeightKind::Adaptive,
                2 => WeightKind::TypeHint,
                3 => WeightKind::Strategy,
                _ => WeightKind::Observational,
            };
            let has_annotation = read_u8(data, &mut pos) != 0;
            let annotation = if has_annotation { Some(read_u32(data, &mut pos)) } else { None };
            let contract_byte = read_u8(data, &mut pos);
            let contract = match contract_byte {
                1 => Contract::SameOutput,
                2 => Contract::Validated,
                3 => Contract::WithinTolerance,
                _ => Contract::None,
            };
            let objective_byte = read_u8(data, &mut pos);
            let objective = match objective_byte {
                1 => Objective::Speed, 2 => Objective::Accuracy,
                3 => Objective::Reliability, 4 => Objective::Cost,
                5 => Objective::Risk, 6 => Objective::Confidence,
                7 => Objective::Reward, 8 => Objective::MultiObjective,
                _ => Objective::None,
            };
            nodes.push(GraphNode { id, op, operands, weights, bias, activation_count, state_slot, weight_kind, annotation, contract, objective });
        }

        // Edges
        let mut edges = Vec::with_capacity(edge_count as usize);
        for _ in 0..edge_count {
            let from = read_u32(data, &mut pos);
            let to = read_u32(data, &mut pos);
            let weight = read_f64(data, &mut pos);
            let has_gate = read_u8(data, &mut pos) != 0;
            let gate = if has_gate { Some(read_u32(data, &mut pos)) } else { None };
            edges.push(Edge { from, to, weight, gate });
        }

        // State
        let state_len = if pos < data.len() { read_u32(data, &mut pos) as usize } else { 0 };
        let mut state = Vec::with_capacity(state_len);
        for _ in 0..state_len {
            state.push(read_f64(data, &mut pos));
        }

        // Journal
        let journal_len = if pos < data.len() { read_u32(data, &mut pos) as usize } else { 0 };
        let mut journal = Vec::with_capacity(journal_len);
        for _ in 0..journal_len {
            if pos >= data.len() { break; }
            let run_number = read_u64(data, &mut pos);
            let node_id = read_u32(data, &mut pos);
            let mutation_byte = read_u8(data, &mut pos);
            let mutation = match mutation_byte {
                1 => MutationKind::TypeSpecialized,
                2 => MutationKind::ConstantFolded,
                3 => MutationKind::PathPruned,
                4 => MutationKind::GuardInserted,
                5 => MutationKind::NodeSpawned,
                6 => MutationKind::FeedbackReceived,
                7 => MutationKind::EvolutionStarted,
                8 => MutationKind::ProposalAccepted,
                9 => MutationKind::ProposalRejected,
                10 => MutationKind::EvolutionCompleted,
                _ => MutationKind::WeightUpdate,
            };
            let reason = read_u32(data, &mut pos);
            journal.push(JournalEntry { run_number, node_id, mutation, reason });
        }

        Ok(NeuralGraph {
            header: GraphHeader { version, node_count, edge_count, state_size, string_count, flags },
            nodes,
            edges,
            string_table,
            state,
            entry,
            journal,
        })
    }

    /// Look up a string from the table.
    pub fn get_string(&self, idx: u32) -> String {
        if let Some(data) = self.string_table.get(idx as usize) {
            String::from_utf8_lossy(data).to_string()
        } else {
            String::new()
        }
    }
}

// ── Encoding helpers ──

fn encode_operand(buf: &mut Vec<u8>, op: &Operand) {
    match op {
        Operand::NodeRef(id) => { buf.push(0x01); write_u32(buf, *id); }
        Operand::Immediate(ImmValue::Int(n)) => { buf.push(0x10); write_i64(buf, *n); }
        Operand::Immediate(ImmValue::Float(f)) => { buf.push(0x11); write_f64(buf, *f); }
        Operand::Immediate(ImmValue::Bool(b)) => { buf.push(0x12); buf.push(if *b { 1 } else { 0 }); }
        Operand::Immediate(ImmValue::Null) => { buf.push(0x13); }
        Operand::StateRef(idx) => { buf.push(0x20); write_u32(buf, *idx); }
        Operand::StringRef(idx) => { buf.push(0x30); write_u32(buf, *idx); }
        Operand::VarSlot(slot) => { buf.push(0x40); write_u32(buf, *slot); }
    }
}

fn decode_operand(data: &[u8], pos: &mut usize) -> Result<Operand, String> {
    let tag = read_u8(data, pos);
    match tag {
        0x01 => Ok(Operand::NodeRef(read_u32(data, pos))),
        0x10 => Ok(Operand::Immediate(ImmValue::Int(read_i64(data, pos)))),
        0x11 => Ok(Operand::Immediate(ImmValue::Float(read_f64(data, pos)))),
        0x12 => Ok(Operand::Immediate(ImmValue::Bool(read_u8(data, pos) != 0))),
        0x13 => Ok(Operand::Immediate(ImmValue::Null)),
        0x20 => Ok(Operand::StateRef(read_u32(data, pos))),
        0x30 => Ok(Operand::StringRef(read_u32(data, pos))),
        0x40 => Ok(Operand::VarSlot(read_u32(data, pos))),
        _ => Err(format!("unknown operand tag 0x{tag:02X}")),
    }
}

fn opcode_from_byte(b: u8) -> Result<OpCode, String> {
    match b {
        0x01 => Ok(OpCode::ConstInt), 0x02 => Ok(OpCode::ConstFloat),
        0x03 => Ok(OpCode::ConstStr), 0x04 => Ok(OpCode::ConstBool),
        0x05 => Ok(OpCode::ConstNull), 0x06 => Ok(OpCode::LoadVar),
        0x07 => Ok(OpCode::StoreVar),
        0x10 => Ok(OpCode::Add), 0x11 => Ok(OpCode::Sub),
        0x12 => Ok(OpCode::Mul), 0x13 => Ok(OpCode::Div),
        0x14 => Ok(OpCode::Mod), 0x15 => Ok(OpCode::Neg),
        0x20 => Ok(OpCode::Eq), 0x21 => Ok(OpCode::Neq),
        0x22 => Ok(OpCode::Lt), 0x23 => Ok(OpCode::Gt),
        0x24 => Ok(OpCode::Lte), 0x25 => Ok(OpCode::Gte),
        0x30 => Ok(OpCode::And), 0x31 => Ok(OpCode::Or),
        0x32 => Ok(OpCode::Not),
        0x40 => Ok(OpCode::Branch), 0x41 => Ok(OpCode::Merge),
        0x42 => Ok(OpCode::Loop), 0x43 => Ok(OpCode::Sequence),
        0x44 => Ok(OpCode::AdaptiveChoice), 0x45 => Ok(OpCode::Guard),
        0x46 => Ok(OpCode::ForEach), 0x47 => Ok(OpCode::Repeat),
        0x50 => Ok(OpCode::Define), 0x51 => Ok(OpCode::Call),
        0x52 => Ok(OpCode::Return), 0x53 => Ok(OpCode::Lambda),
        0x60 => Ok(OpCode::Array), 0x61 => Ok(OpCode::Index),
        0x62 => Ok(OpCode::Range), 0x63 => Ok(OpCode::Length),
        0x64 => Ok(OpCode::Chars),
        0x75 => Ok(OpCode::Sin), 0x76 => Ok(OpCode::Cos),
        0x77 => Ok(OpCode::Abs), 0x78 => Ok(OpCode::Floor),
        0x79 => Ok(OpCode::Round), 0x7A => Ok(OpCode::Sqrt),
        0x7B => Ok(OpCode::Ln), 0x7C => Ok(OpCode::Exp),
        0x7D => Ok(OpCode::Atan2),
        0x70 => Ok(OpCode::Print), 0x71 => Ok(OpCode::ReadLine),
        0x72 => Ok(OpCode::ParseNum), 0x73 => Ok(OpCode::Split),
        0x74 => Ok(OpCode::ToString),
        0x80 => Ok(OpCode::Adapt), 0x81 => Ok(OpCode::Weight),
        0x82 => Ok(OpCode::Predict), 0x83 => Ok(OpCode::Feedback),
        0x84 => Ok(OpCode::Spawn), 0x85 => Ok(OpCode::Prune),
        0x86 => Ok(OpCode::Strategy),
        0x90 => Ok(OpCode::Pipe), 0x91 => Ok(OpCode::Filter),
        0x92 => Ok(OpCode::Map), 0x93 => Ok(OpCode::Reduce),
        0xA0 => Ok(OpCode::Capability),
        0xFE => Ok(OpCode::Noop), 0xFF => Ok(OpCode::Halt),
        _ => Err(format!("unknown opcode 0x{b:02X}")),
    }
}

// ── Primitive I/O ──

fn write_u32(buf: &mut Vec<u8>, v: u32) { buf.extend_from_slice(&v.to_le_bytes()); }
fn write_i64(buf: &mut Vec<u8>, v: i64) { buf.extend_from_slice(&v.to_le_bytes()); }
fn write_f64(buf: &mut Vec<u8>, v: f64) { buf.extend_from_slice(&v.to_le_bytes()); }
fn write_u64(buf: &mut Vec<u8>, v: u64) { buf.extend_from_slice(&v.to_le_bytes()); }

fn read_u8(data: &[u8], pos: &mut usize) -> u8 {
    if *pos >= data.len() { return 0; }
    let v = data[*pos]; *pos += 1; v
}
fn read_u32(data: &[u8], pos: &mut usize) -> u32 {
    if *pos + 4 > data.len() { *pos = data.len(); return 0; }
    let v = u32::from_le_bytes(data[*pos..*pos+4].try_into().unwrap()); *pos += 4; v
}
fn read_i64(data: &[u8], pos: &mut usize) -> i64 {
    if *pos + 8 > data.len() { *pos = data.len(); return 0; }
    let v = i64::from_le_bytes(data[*pos..*pos+8].try_into().unwrap()); *pos += 8; v
}
fn read_f64(data: &[u8], pos: &mut usize) -> f64 {
    if *pos + 8 > data.len() { *pos = data.len(); return 0.0; }
    let v = f64::from_le_bytes(data[*pos..*pos+8].try_into().unwrap()); *pos += 8; v
}
fn read_u64(data: &[u8], pos: &mut usize) -> u64 {
    if *pos + 8 > data.len() { *pos = data.len(); return 0; }
    let v = u64::from_le_bytes(data[*pos..*pos+8].try_into().unwrap()); *pos += 8; v
}
