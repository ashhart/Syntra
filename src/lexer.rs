use crate::error::{LycanError, LycanResult};
use crate::token::{Spanned, Token};

pub struct Lexer {
    src: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(src: &str) -> Self {
        Self {
            src: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    pub fn tokenize(&mut self) -> LycanResult<Vec<Spanned>> {
        let mut tokens = Vec::new();
        while !self.at_end() {
            self.skip_whitespace();
            if self.at_end() {
                break;
            }
            // Comments: ;; to end of line
            if self.peek() == ';' && self.peek_at(1) == Some(';') {
                while !self.at_end() && self.peek() != '\n' {
                    self.advance();
                }
                continue;
            }
            let line = self.line;
            let col = self.col;
            let tok = self.next_token()?;
            tokens.push(Spanned { token: tok, line, col });
        }
        tokens.push(Spanned { token: Token::Eof, line: self.line, col: self.col });
        Ok(tokens)
    }

    fn next_token(&mut self) -> LycanResult<Token> {
        let c = self.peek();
        match c {
            '(' => { self.advance(); Ok(Token::LParen) }
            ')' => { self.advance(); Ok(Token::RParen) }
            '"' => self.read_string(),
            ':' => self.read_type_or_ident(),
            _ if c.is_ascii_digit() => self.read_number(),
            '-' if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => self.read_number(),
            _ if is_atom_char(c) => self.read_atom(),
            _ => {
                let ch = self.advance();
                Err(self.err(&format!("unexpected character '{ch}'")))
            }
        }
    }

    fn read_string(&mut self) -> LycanResult<Token> {
        self.advance(); // skip "
        let mut s = String::new();
        while !self.at_end() && self.peek() != '"' {
            let c = self.advance();
            if c == '\\' {
                if self.at_end() {
                    return Err(self.err("unterminated escape"));
                }
                match self.advance() {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    '\\' => s.push('\\'),
                    '"' => s.push('"'),
                    other => { s.push('\\'); s.push(other); }
                }
            } else {
                s.push(c);
            }
        }
        if self.at_end() {
            return Err(self.err("unterminated string"));
        }
        self.advance(); // skip closing "
        Ok(Token::Str(s))
    }

    fn read_number(&mut self) -> LycanResult<Token> {
        let mut num = String::new();
        let mut is_float = false;
        // Handle negative
        if self.peek() == '-' {
            num.push(self.advance());
        }
        while !self.at_end() && (self.peek().is_ascii_digit() || self.peek() == '.') {
            if self.peek() == '.' {
                if is_float { break; }
                if self.peek_at(1) == Some('.') { break; }
                is_float = true;
            }
            num.push(self.advance());
        }
        if is_float {
            let val: f64 = num.parse().map_err(|_| self.err(&format!("invalid float '{num}'")))?;
            Ok(Token::Float(val))
        } else {
            let val: i64 = num.parse().map_err(|_| self.err(&format!("invalid int '{num}'")))?;
            Ok(Token::Int(val))
        }
    }

    fn read_type_or_ident(&mut self) -> LycanResult<Token> {
        self.advance(); // skip :
        if self.at_end() || !self.peek().is_alphanumeric() {
            // standalone colon — treat as part of an atom
            return Ok(Token::Ident(":".to_string()));
        }
        match self.peek() {
            'i' if !self.peek_at(1).is_some_and(|c| c.is_alphanumeric() || c == '_') => {
                self.advance();
                Ok(Token::TypeInt)
            }
            'f' if !self.peek_at(1).is_some_and(|c| c.is_alphanumeric() || c == '_') => {
                self.advance();
                Ok(Token::TypeFloat)
            }
            's' if !self.peek_at(1).is_some_and(|c| c.is_alphanumeric() || c == '_') => {
                self.advance();
                Ok(Token::TypeStr)
            }
            'b' if !self.peek_at(1).is_some_and(|c| c.is_alphanumeric() || c == '_') => {
                self.advance();
                Ok(Token::TypeBool)
            }
            'n' if !self.peek_at(1).is_some_and(|c| c.is_alphanumeric() || c == '_') => {
                self.advance();
                Ok(Token::TypeNull)
            }
            _ => {
                // It's a keyword-style atom like :keyword
                let mut name = ":".to_string();
                while !self.at_end() && is_atom_char(self.peek()) {
                    name.push(self.advance());
                }
                Ok(Token::Ident(name))
            }
        }
    }

    fn read_atom(&mut self) -> LycanResult<Token> {
        let mut name = String::new();
        while !self.at_end() && is_atom_char(self.peek()) {
            name.push(self.advance());
        }
        match name.as_str() {
            "true" => Ok(Token::Bool(true)),
            "false" => Ok(Token::Bool(false)),
            "null" => Ok(Token::Null),
            _ => Ok(Token::Ident(name)),
        }
    }

    fn peek(&self) -> char {
        self.src.get(self.pos).copied().unwrap_or('\0')
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.src.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> char {
        let c = self.src[self.pos];
        self.pos += 1;
        if c == '\n' { self.line += 1; self.col = 1; } else { self.col += 1; }
        c
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn skip_whitespace(&mut self) {
        while !self.at_end() && self.peek().is_whitespace() {
            self.advance();
        }
    }

    fn err(&self, msg: &str) -> LycanError {
        LycanError::Lexer { msg: msg.to_string(), line: self.line, col: self.col }
    }
}

/// Characters valid inside an atom (identifiers, operators, sigils).
fn is_atom_char(c: char) -> bool {
    !c.is_whitespace() && c != '(' && c != ')' && c != '"' && c != ';'
}
