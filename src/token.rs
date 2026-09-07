/// Lycan tokens — dramatically simple because S-expression structure
/// eliminates the need for operator precedence, statement terminators,
/// or complex delimiter rules.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    LParen,
    RParen,
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Ident(String),
    // Type annotations
    TypeInt,    // :i
    TypeFloat,  // :f
    TypeStr,    // :s
    TypeBool,   // :b
    TypeNull,   // :n
    Eof,
}

#[derive(Debug, Clone)]
pub struct Spanned {
    pub token: Token,
    pub line: usize,
    pub col: usize,
}
