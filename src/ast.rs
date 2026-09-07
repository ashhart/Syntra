/// Lycan computation graph nodes.
///
/// Every node is a tagged, self-describing unit of computation.
/// The structure mirrors what an LLM naturally generates:
/// tagged prefix notation with explicit types.

#[derive(Debug, Clone)]
pub struct Program {
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone)]
pub enum Node {
    // ── Literals ──
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,

    // ── Reference ──
    Ident(String),

    // ── Binding: ($ name :type value) or ($! name :type value) ──
    Bind {
        name: String,
        mutable: bool,
        ty: Option<Type>,
        value: Box<Node>,
    },

    // ── Assignment: (= name value) ──
    Assign {
        name: String,
        value: Box<Node>,
    },

    // ── Function: (F name (params) :ret body...) ──
    //    Lambda:   (\ (params) :ret body...)
    Fn {
        name: Option<String>,
        params: Vec<Param>,
        ret: Option<Type>,
        body: Vec<Node>,
        stateful: bool,
    },

    // ── Function call: (name args...) ──
    Call {
        callee: Box<Node>,
        args: Vec<Node>,
    },

    // ── If: (? cond then else) ──
    If {
        cond: Box<Node>,
        then_branch: Box<Node>,
        else_branch: Option<Box<Node>>,
    },

    // ── While: (W cond body...) ──
    While {
        cond: Box<Node>,
        body: Vec<Node>,
    },

    // ── For-each: (* var iterable body...) ──
    ForEach {
        var: String,
        iterable: Box<Node>,
        body: Vec<Node>,
    },

    // ── Repeat: (# n body...) ──
    Repeat {
        count: Box<Node>,
        body: Vec<Node>,
    },

    // ── Return: (^ value) ──
    Return(Box<Node>),

    // ── Block: (B expr...) ──
    Block(Vec<Node>),

    // ── Array: (A elem...) ──
    Array(Vec<Node>),

    // ── Index: (I obj idx) ──
    Index {
        object: Box<Node>,
        index: Box<Node>,
    },

    // ── Range: (.. start end) ──
    Range {
        start: Box<Node>,
        end: Box<Node>,
    },

    // ── Arithmetic: (+ a b), (- a b), (* a b), (/ a b), (% a b) ──
    // ── Comparison: (== a b), (!= a b), (< a b), (> a b), (<= a b), (>= a b) ──
    // ── Logic: (&& a b), (|| a b), (! a) ──
    Op {
        op: OpKind,
        args: Vec<Node>,
    },

    // ── Pipeline: (|> data fn), (|? data pred), (|* data fn), (|+ data fn init) ──
    Pipe {
        kind: PipeKind,
        data: Box<Node>,
        func: Box<Node>,
        init: Option<Box<Node>>,
    },

    // ── Adapt: (~> name new-body...) — runtime mutation ──
    Adapt {
        target: String,
        body: Vec<Node>,
    },

    // ── Adaptive: (choice opt1 opt2 ...) — weights decide ──
    Choice {
        options: Vec<Node>,
    },

    // ── Guard: (guard assumption fast fallback) ──
    Guard {
        assumption: Box<Node>,
        fast_path: Box<Node>,
        fallback: Box<Node>,
    },

    // ── Strategy: (strategy opt1 opt2 ...) — algorithm selection ──
    Strategy {
        options: Vec<Node>,
    },

    // ── Feedback: (feedback target reward) — reward signal ──
    Feedback {
        target: Box<Node>,
        reward: Box<Node>,
    },

    // ── Builtins: (!p expr), (!r) ──
    Builtin {
        name: String,
        args: Vec<Node>,
    },
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
}

#[derive(Debug, Clone)]
pub enum Type {
    Int,
    Float,
    Str,
    Bool,
    Null,
    Array(Box<Type>),
}

#[derive(Debug, Clone, Copy)]
pub enum OpKind {
    Add, Sub, Mul, Div, Mod,
    Eq, Neq, Lt, Gt, Lte, Gte,
    And, Or, Not, Neg,
}

#[derive(Debug, Clone, Copy)]
pub enum PipeKind {
    Pipe,    // |>
    Filter,  // |?
    Map,     // |*
    Reduce,  // |+
}
