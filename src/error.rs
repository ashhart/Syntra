use std::fmt;

#[derive(Debug)]
pub enum LycanError {
    Lexer { msg: String, line: usize, col: usize },
    Parser { msg: String, line: usize, col: usize },
    Runtime { msg: String },
}

impl fmt::Display for LycanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LycanError::Lexer { msg, line, col } => {
                write!(f, "[lex {line}:{col}] {msg}")
            }
            LycanError::Parser { msg, line, col } => {
                write!(f, "[parse {line}:{col}] {msg}")
            }
            LycanError::Runtime { msg } => {
                write!(f, "[runtime] {msg}")
            }
        }
    }
}

impl std::error::Error for LycanError {}

pub type LycanResult<T> = Result<T, LycanError>;
