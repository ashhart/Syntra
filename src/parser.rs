use crate::ast::*;
use crate::error::{LycanError, LycanResult};
use crate::token::{Spanned, Token};

/// Parses an S-expression token stream into an AST.
/// Grammar: `program = node*; node = atom | "(" tag node* ")"`.
pub struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Spanned>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse_program(&mut self) -> LycanResult<Program> {
        let mut nodes = Vec::new();
        while !self.at_end() {
            nodes.push(self.parse_node()?);
        }
        Ok(Program { nodes })
    }

    fn parse_node(&mut self) -> LycanResult<Node> {
        match self.current() {
            Token::LParen => self.parse_list(),
            Token::Int(n) => { let n = n; self.advance(); Ok(Node::Int(n)) }
            Token::Float(f) => { let f = f; self.advance(); Ok(Node::Float(f)) }
            Token::Str(s) => { let s = s; self.advance(); Ok(Node::Str(s)) }
            Token::Bool(b) => { let b = b; self.advance(); Ok(Node::Bool(b)) }
            Token::Null => { self.advance(); Ok(Node::Null) }
            Token::Ident(name) => { let name = name; self.advance(); Ok(Node::Ident(name)) }
            _ => Err(self.err(&format!("unexpected token {:?}", self.current()))),
        }
    }

    /// Parse a parenthesized list. The first element is the tag.
    fn parse_list(&mut self) -> LycanResult<Node> {
        self.expect_tok(&Token::LParen)?;

        if self.check(&Token::RParen) {
            self.advance();
            return Ok(Node::Null);
        }

        let head = self.current();

        match &head {
            // ($ name :type value) — immutable binding
            Token::Ident(s) if s == "$" => self.parse_bind(false),
            // ($! name :type value) — mutable binding
            Token::Ident(s) if s == "$!" => self.parse_bind(true),
            // (= name value) — assignment
            Token::Ident(s) if s == "=" => self.parse_assign(),
            // (F name (params) :ret body...) — function
            Token::Ident(s) if s == "F" => self.parse_fn(false),
            // (F! name (params) :ret body...) — stateful function
            Token::Ident(s) if s == "F!" => self.parse_fn(true),
            // (\ (params) :ret body...) — lambda
            Token::Ident(s) if s == "\\" => self.parse_lambda(),
            // (? cond then else) — if
            Token::Ident(s) if s == "?" => self.parse_if(),
            // (W cond body...) — while
            Token::Ident(s) if s == "W" => self.parse_while(),
            // (* var iter body...) — for-each
            // Note: we must distinguish from (* a b) multiplication
            // Convention: (* ident expr body...) is for-each if first arg is bare ident
            // For multiplication, always use (mul a b) or check arity
            Token::Ident(s) if s == "each" => self.parse_for_each(),
            // (# n body...) — repeat
            Token::Ident(s) if s == "#" => self.parse_repeat(),
            // (^ value) — return
            Token::Ident(s) if s == "^" => self.parse_return(),
            // (B expr...) — block
            Token::Ident(s) if s == "B" => self.parse_block(),
            // (A elem...) — array
            Token::Ident(s) if s == "A" => self.parse_array(),
            // (I obj idx) — index
            Token::Ident(s) if s == "I" => self.parse_index(),
            // (.. start end) — range
            Token::Ident(s) if s == ".." => self.parse_range(),
            // (~> name body...) — adapt
            Token::Ident(s) if s == "~>" => self.parse_adapt(),
            // (choice option1 option2 ...) — adaptive choice (weights decide)
            Token::Ident(s) if s == "choice" => self.parse_choice(),
            // (guard assumption fast fallback) — guarded fast path
            Token::Ident(s) if s == "guard" => self.parse_guard(),
            // (strategy opt1 opt2 ...) — algorithm selection by weight
            Token::Ident(s) if s == "strategy" => self.parse_strategy(),
            // (feedback target reward) — send reward signal to update weights
            Token::Ident(s) if s == "feedback" => self.parse_feedback(),
            // Operators: (+ a b), (<= a b), (&& a b), (!= a b), etc.
            // Must check operators BEFORE builtins since != starts with !
            Token::Ident(s) if is_operator(s) => self.parse_op(),
            // (!p expr), (!r) — builtins
            Token::Ident(s) if s.starts_with('!') => self.parse_builtin(),
            // Pipeline: (|> data fn), (|? data pred), (|* data fn), (|+ data fn init)
            Token::Ident(s) if is_pipe(s) => self.parse_pipe(),
            // Otherwise: function call (callee args...)
            _ => self.parse_call(),
        }
    }

    // ── Node parsers ──

    fn parse_bind(&mut self, mutable: bool) -> LycanResult<Node> {
        self.advance(); // skip $ or $!
        let name = self.expect_ident()?;
        let ty = self.try_type();
        let value = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Bind { name, mutable, ty, value: Box::new(value) })
    }

    fn parse_assign(&mut self) -> LycanResult<Node> {
        self.advance(); // skip =
        let name = self.expect_ident()?;
        let value = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Assign { name, value: Box::new(value) })
    }

    fn parse_fn(&mut self, stateful: bool) -> LycanResult<Node> {
        self.advance(); // skip F or F!
        let name = Some(self.expect_ident()?);
        // Params: (name :type name :type ...)
        self.expect_tok(&Token::LParen)?;
        let params = self.parse_params()?;
        self.expect_tok(&Token::RParen)?;
        let ret = self.try_type();
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Fn { name, params, ret, body, stateful })
    }

    fn parse_lambda(&mut self) -> LycanResult<Node> {
        self.advance(); // skip backslash
        self.expect_tok(&Token::LParen)?;
        let params = self.parse_params()?;
        self.expect_tok(&Token::RParen)?;
        let ret = self.try_type();
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Fn { name: None, params, ret, body, stateful: false })
    }

    fn parse_if(&mut self) -> LycanResult<Node> {
        self.advance(); // skip ?
        let cond = self.parse_node()?;
        let then_branch = self.parse_node()?;
        let else_branch = if !self.check(&Token::RParen) {
            Some(Box::new(self.parse_node()?))
        } else {
            None
        };
        self.expect_tok(&Token::RParen)?;
        Ok(Node::If { cond: Box::new(cond), then_branch: Box::new(then_branch), else_branch })
    }

    fn parse_while(&mut self) -> LycanResult<Node> {
        self.advance(); // skip W
        let cond = self.parse_node()?;
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::While { cond: Box::new(cond), body })
    }

    fn parse_for_each(&mut self) -> LycanResult<Node> {
        self.advance(); // skip each
        let var = self.expect_ident()?;
        let iterable = self.parse_node()?;
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::ForEach { var, iterable: Box::new(iterable), body })
    }

    fn parse_repeat(&mut self) -> LycanResult<Node> {
        self.advance(); // skip #
        let count = self.parse_node()?;
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Repeat { count: Box::new(count), body })
    }

    fn parse_return(&mut self) -> LycanResult<Node> {
        self.advance(); // skip ^
        let val = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Return(Box::new(val)))
    }

    fn parse_block(&mut self) -> LycanResult<Node> {
        self.advance(); // skip B
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Block(body))
    }

    fn parse_array(&mut self) -> LycanResult<Node> {
        self.advance(); // skip A
        let mut elems = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            elems.push(self.parse_node()?);
        }
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Array(elems))
    }

    fn parse_index(&mut self) -> LycanResult<Node> {
        self.advance(); // skip I
        let obj = self.parse_node()?;
        let idx = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Index { object: Box::new(obj), index: Box::new(idx) })
    }

    fn parse_range(&mut self) -> LycanResult<Node> {
        self.advance(); // skip ..
        let start = self.parse_node()?;
        let end = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Range { start: Box::new(start), end: Box::new(end) })
    }

    fn parse_adapt(&mut self) -> LycanResult<Node> {
        self.advance(); // skip ~>
        let target = self.expect_ident()?;
        let body = self.parse_body()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Adapt { target, body })
    }

    fn parse_choice(&mut self) -> LycanResult<Node> {
        self.advance(); // skip choice
        let mut options = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            options.push(self.parse_node()?);
        }
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Choice { options })
    }

    fn parse_guard(&mut self) -> LycanResult<Node> {
        self.advance(); // skip guard
        let assumption = self.parse_node()?;
        let fast_path = self.parse_node()?;
        let fallback = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Guard {
            assumption: Box::new(assumption),
            fast_path: Box::new(fast_path),
            fallback: Box::new(fallback),
        })
    }

    fn parse_strategy(&mut self) -> LycanResult<Node> {
        self.advance(); // skip strategy
        let mut options = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            options.push(self.parse_node()?);
        }
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Strategy { options })
    }

    fn parse_feedback(&mut self) -> LycanResult<Node> {
        self.advance(); // skip feedback
        let target = self.parse_node()?;
        let reward = self.parse_node()?;
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Feedback {
            target: Box::new(target),
            reward: Box::new(reward),
        })
    }

    fn parse_builtin(&mut self) -> LycanResult<Node> {
        let name = self.expect_ident()?;
        // Strip the leading !
        let name = name[1..].to_string();
        let mut args = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            args.push(self.parse_node()?);
        }
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Builtin { name, args })
    }

    fn parse_op(&mut self) -> LycanResult<Node> {
        let op_str = self.expect_ident()?;
        let op = match op_str.as_str() {
            "+" => OpKind::Add,
            "-" => OpKind::Sub,
            "*" => OpKind::Mul,
            "/" => OpKind::Div,
            "%" => OpKind::Mod,
            "==" => OpKind::Eq,
            "!=" => OpKind::Neq,
            "<" => OpKind::Lt,
            ">" => OpKind::Gt,
            "<=" => OpKind::Lte,
            ">=" => OpKind::Gte,
            "&&" => OpKind::And,
            "||" => OpKind::Or,
            "not" => OpKind::Not,
            "neg" => OpKind::Neg,
            _ => return Err(self.err(&format!("unknown operator '{op_str}'"))),
        };
        let mut args = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            args.push(self.parse_node()?);
        }
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Op { op, args })
    }

    fn parse_pipe(&mut self) -> LycanResult<Node> {
        let pipe_str = self.expect_ident()?;
        let kind = match pipe_str.as_str() {
            "|>" => PipeKind::Pipe,
            "|?" => PipeKind::Filter,
            "|*" => PipeKind::Map,
            "|+" => PipeKind::Reduce,
            _ => return Err(self.err(&format!("unknown pipe '{pipe_str}'"))),
        };
        let data = self.parse_node()?;
        let func = self.parse_node()?;
        let init = if matches!(kind, PipeKind::Reduce) && !self.check(&Token::RParen) {
            Some(Box::new(self.parse_node()?))
        } else {
            None
        };
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Pipe { kind, data: Box::new(data), func: Box::new(func), init })
    }

    fn parse_call(&mut self) -> LycanResult<Node> {
        let callee = self.parse_node()?;
        let mut args = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            args.push(self.parse_node()?);
        }
        self.expect_tok(&Token::RParen)?;
        Ok(Node::Call { callee: Box::new(callee), args })
    }

    // ── Helpers ──

    fn parse_params(&mut self) -> LycanResult<Vec<Param>> {
        let mut params = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            let name = self.expect_ident()?;
            let ty = self.try_type();
            params.push(Param { name, ty });
        }
        Ok(params)
    }

    fn parse_body(&mut self) -> LycanResult<Vec<Node>> {
        let mut nodes = Vec::new();
        while !self.check(&Token::RParen) && !self.at_end() {
            nodes.push(self.parse_node()?);
        }
        Ok(nodes)
    }

    fn try_type(&mut self) -> Option<Type> {
        match self.current() {
            Token::TypeInt => { self.advance(); Some(Type::Int) }
            Token::TypeFloat => { self.advance(); Some(Type::Float) }
            Token::TypeStr => { self.advance(); Some(Type::Str) }
            Token::TypeBool => { self.advance(); Some(Type::Bool) }
            Token::TypeNull => { self.advance(); Some(Type::Null) }
            _ => None,
        }
    }

    fn current(&self) -> Token {
        self.tokens.get(self.pos).map(|s| s.token.clone()).unwrap_or(Token::Eof)
    }

    fn check(&self, expected: &Token) -> bool {
        std::mem::discriminant(&self.current()) == std::mem::discriminant(expected)
    }

    fn advance(&mut self) -> Token {
        let tok = self.current();
        if self.pos < self.tokens.len() { self.pos += 1; }
        tok
    }

    fn expect_tok(&mut self, expected: &Token) -> LycanResult<()> {
        if self.check(expected) { self.advance(); Ok(()) }
        else { Err(self.err(&format!("expected {:?}, got {:?}", expected, self.current()))) }
    }

    fn expect_ident(&mut self) -> LycanResult<String> {
        if let Token::Ident(name) = self.current() { self.advance(); Ok(name) }
        else { Err(self.err(&format!("expected identifier, got {:?}", self.current()))) }
    }

    fn at_end(&self) -> bool { matches!(self.current(), Token::Eof) }

    fn err(&self, msg: &str) -> LycanError {
        let span = self.tokens.get(self.pos).unwrap_or(self.tokens.last().unwrap());
        LycanError::Parser { msg: msg.to_string(), line: span.line, col: span.col }
    }
}

fn is_operator(s: &str) -> bool {
    matches!(s, "+" | "-" | "*" | "/" | "%" | "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "not" | "neg")
}

fn is_pipe(s: &str) -> bool {
    matches!(s, "|>" | "|?" | "|*" | "|+")
}
