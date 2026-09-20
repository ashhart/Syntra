//! Runtime values produced and consumed during graph execution.

use std::rc::Rc;
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
        /// Shared read-only param slots; `Rc` keeps function-value clones
        /// (var loads, call frames) allocation-free in loop-heavy graphs.
        param_slots: Rc<Vec<u32>>,
        /// Shared read-only body node refs; same allocation-free clone rationale.
        body_nodes: Rc<Vec<u32>>,
    },
}

impl GVal {
    pub(super) fn is_truthy(&self) -> bool {
        match self {
            GVal::Bool(b) => *b,
            GVal::Null => false,
            GVal::Int(0) => false,
            GVal::Str(s) if s.is_empty() => false,
            GVal::Array(a) if a.is_empty() => false,
            _ => true,
        }
    }

    pub(super) fn type_name(&self) -> &'static str {
        match self {
            GVal::Int(_) => "int",
            GVal::Float(_) => "float",
            GVal::Str(_) => "str",
            GVal::Bool(_) => "bool",
            GVal::Null => "null",
            GVal::Array(_) => "array",
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
                for v in a {
                    write!(f, " {v}")?;
                }
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
