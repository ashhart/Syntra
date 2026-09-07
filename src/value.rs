use std::fmt;
use crate::ast::{Node, Param};

/// Runtime values in the Lycan VM.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Array(Vec<Value>),
    Fn(LycanFn),
}

#[derive(Debug, Clone)]
pub struct LycanFn {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Node>,
    #[allow(dead_code)]
    pub stateful: bool,
}

impl Value {
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Int(0) => false,
            Value::Str(s) if s.is_empty() => false,
            Value::Array(a) if a.is_empty() => false,
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Array(_) => "array",
            Value::Fn(_) => "fn",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{}", if *b { "true" } else { "false" }),
            Value::Null => write!(f, "null"),
            Value::Array(elems) => {
                write!(f, "(A")?;
                for e in elems {
                    write!(f, " {e}")?;
                }
                write!(f, ")")
            }
            Value::Fn(func) => write!(f, "(F {})", func.name),
        }
    }
}
