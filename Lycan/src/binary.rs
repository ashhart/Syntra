/// Lycan binary format (.lyc)
///
/// The canonical program representation. This is what gets stored,
/// executed, and transmitted between AI systems. The text format
/// (.lycs) is just a generation interface — compiled away immediately.
///
/// Format:
///   Header: LYCAN\x00 + version:u8
///   Body:   sequence of encoded nodes
///   Each node: tag:u8 + payload (variable)

use crate::ast::*;
use crate::error::{LycanError, LycanResult};

const MAGIC: &[u8; 6] = b"LYCAN\x00";
const VERSION: u8 = 1;

// Node tags
const TAG_INT: u8 = 0x01;
const TAG_FLOAT: u8 = 0x02;
const TAG_STR: u8 = 0x03;
const TAG_BOOL: u8 = 0x04;
const TAG_NULL: u8 = 0x05;
const TAG_IDENT: u8 = 0x06;
const TAG_BIND: u8 = 0x10;
const TAG_ASSIGN: u8 = 0x11;
const TAG_FN: u8 = 0x20;
const TAG_CALL: u8 = 0x21;
const TAG_IF: u8 = 0x30;
const TAG_WHILE: u8 = 0x31;
const TAG_FOREACH: u8 = 0x32;
const TAG_REPEAT: u8 = 0x33;
const TAG_RETURN: u8 = 0x34;
const TAG_BLOCK: u8 = 0x35;
const TAG_ARRAY: u8 = 0x40;
const TAG_INDEX: u8 = 0x41;
const TAG_RANGE: u8 = 0x42;
const TAG_OP: u8 = 0x50;
const TAG_PIPE: u8 = 0x60;
const TAG_ADAPT: u8 = 0x70;
const TAG_BUILTIN: u8 = 0x80;

// Type tags
const TY_NONE: u8 = 0x00;
const TY_INT: u8 = 0x01;
const TY_FLOAT: u8 = 0x02;
const TY_STR: u8 = 0x03;
const TY_BOOL: u8 = 0x04;
const TY_NULL: u8 = 0x05;
const TY_ARRAY: u8 = 0x06;

/// Serialize a program to binary .lyc format.
#[allow(dead_code)]
pub fn encode(program: &Program) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    write_u32(&mut buf, program.nodes.len() as u32);
    for node in &program.nodes {
        encode_node(&mut buf, node);
    }
    buf
}

/// Deserialize a .lyc binary back to a Program.
pub fn decode(data: &[u8]) -> LycanResult<Program> {
    if data.len() < 7 || &data[0..6] != MAGIC {
        return Err(LycanError::Runtime { msg: "invalid .lyc file: bad magic".to_string() });
    }
    if data[6] != VERSION {
        return Err(LycanError::Runtime { msg: format!("unsupported .lyc version {}", data[6]) });
    }
    let mut pos = 7;
    let count = read_u32(data, &mut pos) as usize;
    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        nodes.push(decode_node(data, &mut pos)?);
    }
    Ok(Program { nodes })
}

// ── Encoding ──

#[allow(dead_code)]
fn encode_node(buf: &mut Vec<u8>, node: &Node) {
    match node {
        Node::Int(n) => { buf.push(TAG_INT); write_i64(buf, *n); }
        Node::Float(f) => { buf.push(TAG_FLOAT); write_f64(buf, *f); }
        Node::Str(s) => { buf.push(TAG_STR); write_str(buf, s); }
        Node::Bool(b) => { buf.push(TAG_BOOL); buf.push(if *b { 1 } else { 0 }); }
        Node::Null => { buf.push(TAG_NULL); }
        Node::Ident(name) => { buf.push(TAG_IDENT); write_str(buf, name); }

        Node::Bind { name, mutable, ty, value } => {
            buf.push(TAG_BIND);
            write_str(buf, name);
            buf.push(if *mutable { 1 } else { 0 });
            write_type(buf, ty);
            encode_node(buf, value);
        }
        Node::Assign { name, value } => {
            buf.push(TAG_ASSIGN);
            write_str(buf, name);
            encode_node(buf, value);
        }

        Node::Fn { name, params, ret, body, stateful } => {
            buf.push(TAG_FN);
            buf.push(if *stateful { 1 } else { 0 });
            match name {
                Some(n) => { buf.push(1); write_str(buf, n); }
                None => { buf.push(0); }
            }
            write_u32(buf, params.len() as u32);
            for p in params {
                write_str(buf, &p.name);
                write_type(buf, &p.ty);
            }
            write_type(buf, ret);
            write_u32(buf, body.len() as u32);
            for n in body { encode_node(buf, n); }
        }

        Node::Call { callee, args } => {
            buf.push(TAG_CALL);
            encode_node(buf, callee);
            write_u32(buf, args.len() as u32);
            for a in args { encode_node(buf, a); }
        }

        Node::If { cond, then_branch, else_branch } => {
            buf.push(TAG_IF);
            encode_node(buf, cond);
            encode_node(buf, then_branch);
            match else_branch {
                Some(e) => { buf.push(1); encode_node(buf, e); }
                None => { buf.push(0); }
            }
        }

        Node::While { cond, body } => {
            buf.push(TAG_WHILE);
            encode_node(buf, cond);
            write_u32(buf, body.len() as u32);
            for n in body { encode_node(buf, n); }
        }

        Node::ForEach { var, iterable, body } => {
            buf.push(TAG_FOREACH);
            write_str(buf, var);
            encode_node(buf, iterable);
            write_u32(buf, body.len() as u32);
            for n in body { encode_node(buf, n); }
        }

        Node::Repeat { count, body } => {
            buf.push(TAG_REPEAT);
            encode_node(buf, count);
            write_u32(buf, body.len() as u32);
            for n in body { encode_node(buf, n); }
        }

        Node::Return(val) => { buf.push(TAG_RETURN); encode_node(buf, val); }

        Node::Block(exprs) => {
            buf.push(TAG_BLOCK);
            write_u32(buf, exprs.len() as u32);
            for n in exprs { encode_node(buf, n); }
        }

        Node::Array(elems) => {
            buf.push(TAG_ARRAY);
            write_u32(buf, elems.len() as u32);
            for n in elems { encode_node(buf, n); }
        }

        Node::Index { object, index } => {
            buf.push(TAG_INDEX);
            encode_node(buf, object);
            encode_node(buf, index);
        }

        Node::Range { start, end } => {
            buf.push(TAG_RANGE);
            encode_node(buf, start);
            encode_node(buf, end);
        }

        Node::Op { op, args } => {
            buf.push(TAG_OP);
            buf.push(*op as u8);
            write_u32(buf, args.len() as u32);
            for a in args { encode_node(buf, a); }
        }

        Node::Pipe { kind, data, func, init } => {
            buf.push(TAG_PIPE);
            buf.push(*kind as u8);
            encode_node(buf, data);
            encode_node(buf, func);
            match init {
                Some(i) => { buf.push(1); encode_node(buf, i); }
                None => { buf.push(0); }
            }
        }

        Node::Adapt { target, body } => {
            buf.push(TAG_ADAPT);
            write_str(buf, target);
            write_u32(buf, body.len() as u32);
            for n in body { encode_node(buf, n); }
        }

        Node::Choice { options } => {
            buf.push(TAG_BUILTIN); // Reuse tag — legacy format
            write_str(buf, "choice");
            write_u32(buf, options.len() as u32);
            for o in options { encode_node(buf, o); }
        }
        Node::Guard { assumption, fast_path, fallback } => {
            buf.push(TAG_BUILTIN);
            write_str(buf, "guard");
            write_u32(buf, 3);
            encode_node(buf, assumption);
            encode_node(buf, fast_path);
            encode_node(buf, fallback);
        }
        Node::Strategy { options } => {
            buf.push(TAG_BUILTIN);
            write_str(buf, "strategy");
            write_u32(buf, options.len() as u32);
            for o in options { encode_node(buf, o); }
        }
        Node::Feedback { target, reward } => {
            buf.push(TAG_BUILTIN);
            write_str(buf, "feedback");
            write_u32(buf, 2);
            encode_node(buf, target);
            encode_node(buf, reward);
        }
        Node::Builtin { name, args } => {
            buf.push(TAG_BUILTIN);
            write_str(buf, name);
            write_u32(buf, args.len() as u32);
            for a in args { encode_node(buf, a); }
        }
    }
}

// ── Decoding ──

fn decode_node(data: &[u8], pos: &mut usize) -> LycanResult<Node> {
    let tag = read_u8(data, pos);
    match tag {
        TAG_INT => Ok(Node::Int(read_i64(data, pos))),
        TAG_FLOAT => Ok(Node::Float(read_f64(data, pos))),
        TAG_STR => Ok(Node::Str(read_str(data, pos))),
        TAG_BOOL => Ok(Node::Bool(read_u8(data, pos) != 0)),
        TAG_NULL => Ok(Node::Null),
        TAG_IDENT => Ok(Node::Ident(read_str(data, pos))),

        TAG_BIND => {
            let name = read_str(data, pos);
            let mutable = read_u8(data, pos) != 0;
            let ty = read_type(data, pos);
            let value = decode_node(data, pos)?;
            Ok(Node::Bind { name, mutable, ty, value: Box::new(value) })
        }
        TAG_ASSIGN => {
            let name = read_str(data, pos);
            let value = decode_node(data, pos)?;
            Ok(Node::Assign { name, value: Box::new(value) })
        }

        TAG_FN => {
            let stateful = read_u8(data, pos) != 0;
            let has_name = read_u8(data, pos) != 0;
            let name = if has_name { Some(read_str(data, pos)) } else { None };
            let param_count = read_u32(data, pos) as usize;
            let mut params = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                let pname = read_str(data, pos);
                let ty = read_type(data, pos);
                params.push(Param { name: pname, ty });
            }
            let ret = read_type(data, pos);
            let body_count = read_u32(data, pos) as usize;
            let mut body = Vec::with_capacity(body_count);
            for _ in 0..body_count { body.push(decode_node(data, pos)?); }
            Ok(Node::Fn { name, params, ret, body, stateful })
        }

        TAG_CALL => {
            let callee = decode_node(data, pos)?;
            let argc = read_u32(data, pos) as usize;
            let mut args = Vec::with_capacity(argc);
            for _ in 0..argc { args.push(decode_node(data, pos)?); }
            Ok(Node::Call { callee: Box::new(callee), args })
        }

        TAG_IF => {
            let cond = decode_node(data, pos)?;
            let then_branch = decode_node(data, pos)?;
            let has_else = read_u8(data, pos) != 0;
            let else_branch = if has_else { Some(Box::new(decode_node(data, pos)?)) } else { None };
            Ok(Node::If { cond: Box::new(cond), then_branch: Box::new(then_branch), else_branch })
        }

        TAG_WHILE => {
            let cond = decode_node(data, pos)?;
            let bc = read_u32(data, pos) as usize;
            let mut body = Vec::with_capacity(bc);
            for _ in 0..bc { body.push(decode_node(data, pos)?); }
            Ok(Node::While { cond: Box::new(cond), body })
        }

        TAG_FOREACH => {
            let var = read_str(data, pos);
            let iterable = decode_node(data, pos)?;
            let bc = read_u32(data, pos) as usize;
            let mut body = Vec::with_capacity(bc);
            for _ in 0..bc { body.push(decode_node(data, pos)?); }
            Ok(Node::ForEach { var, iterable: Box::new(iterable), body })
        }

        TAG_REPEAT => {
            let count = decode_node(data, pos)?;
            let bc = read_u32(data, pos) as usize;
            let mut body = Vec::with_capacity(bc);
            for _ in 0..bc { body.push(decode_node(data, pos)?); }
            Ok(Node::Repeat { count: Box::new(count), body })
        }

        TAG_RETURN => Ok(Node::Return(Box::new(decode_node(data, pos)?))),

        TAG_BLOCK => {
            let c = read_u32(data, pos) as usize;
            let mut exprs = Vec::with_capacity(c);
            for _ in 0..c { exprs.push(decode_node(data, pos)?); }
            Ok(Node::Block(exprs))
        }

        TAG_ARRAY => {
            let c = read_u32(data, pos) as usize;
            let mut elems = Vec::with_capacity(c);
            for _ in 0..c { elems.push(decode_node(data, pos)?); }
            Ok(Node::Array(elems))
        }

        TAG_INDEX => {
            let obj = decode_node(data, pos)?;
            let idx = decode_node(data, pos)?;
            Ok(Node::Index { object: Box::new(obj), index: Box::new(idx) })
        }

        TAG_RANGE => {
            let start = decode_node(data, pos)?;
            let end = decode_node(data, pos)?;
            Ok(Node::Range { start: Box::new(start), end: Box::new(end) })
        }

        TAG_OP => {
            let op_byte = read_u8(data, pos);
            let op = match op_byte {
                0 => OpKind::Add, 1 => OpKind::Sub, 2 => OpKind::Mul,
                3 => OpKind::Div, 4 => OpKind::Mod, 5 => OpKind::Eq,
                6 => OpKind::Neq, 7 => OpKind::Lt, 8 => OpKind::Gt,
                9 => OpKind::Lte, 10 => OpKind::Gte, 11 => OpKind::And,
                12 => OpKind::Or, 13 => OpKind::Not, 14 => OpKind::Neg,
                _ => return Err(LycanError::Runtime { msg: format!("unknown op byte {op_byte}") }),
            };
            let argc = read_u32(data, pos) as usize;
            let mut args = Vec::with_capacity(argc);
            for _ in 0..argc { args.push(decode_node(data, pos)?); }
            Ok(Node::Op { op, args })
        }

        TAG_PIPE => {
            let kind_byte = read_u8(data, pos);
            let kind = match kind_byte {
                0 => PipeKind::Pipe, 1 => PipeKind::Filter,
                2 => PipeKind::Map, 3 => PipeKind::Reduce,
                _ => return Err(LycanError::Runtime { msg: format!("unknown pipe byte {kind_byte}") }),
            };
            let data_node = decode_node(data, pos)?;
            let func = decode_node(data, pos)?;
            let has_init = read_u8(data, pos) != 0;
            let init = if has_init { Some(Box::new(decode_node(data, pos)?)) } else { None };
            Ok(Node::Pipe { kind, data: Box::new(data_node), func: Box::new(func), init })
        }

        TAG_ADAPT => {
            let target = read_str(data, pos);
            let bc = read_u32(data, pos) as usize;
            let mut body = Vec::with_capacity(bc);
            for _ in 0..bc { body.push(decode_node(data, pos)?); }
            Ok(Node::Adapt { target, body })
        }

        TAG_BUILTIN => {
            let name = read_str(data, pos);
            let argc = read_u32(data, pos) as usize;
            let mut args = Vec::with_capacity(argc);
            for _ in 0..argc { args.push(decode_node(data, pos)?); }
            Ok(Node::Builtin { name, args })
        }

        _ => Err(LycanError::Runtime { msg: format!("unknown node tag 0x{tag:02X}") }),
    }
}

// ── Primitive read/write ──

#[allow(dead_code)]
fn write_u8(buf: &mut Vec<u8>, v: u8) { buf.push(v); }
#[allow(dead_code)]
fn write_u32(buf: &mut Vec<u8>, v: u32) { buf.extend_from_slice(&v.to_le_bytes()); }
#[allow(dead_code)]
fn write_i64(buf: &mut Vec<u8>, v: i64) { buf.extend_from_slice(&v.to_le_bytes()); }
#[allow(dead_code)]
fn write_f64(buf: &mut Vec<u8>, v: f64) { buf.extend_from_slice(&v.to_le_bytes()); }
#[allow(dead_code)]
fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

#[allow(dead_code)]
fn write_type(buf: &mut Vec<u8>, ty: &Option<Type>) {
    match ty {
        None => buf.push(TY_NONE),
        Some(Type::Int) => buf.push(TY_INT),
        Some(Type::Float) => buf.push(TY_FLOAT),
        Some(Type::Str) => buf.push(TY_STR),
        Some(Type::Bool) => buf.push(TY_BOOL),
        Some(Type::Null) => buf.push(TY_NULL),
        Some(Type::Array(inner)) => {
            buf.push(TY_ARRAY);
            write_type(buf, &Some(*inner.clone()));
        }
    }
}

fn read_u8(data: &[u8], pos: &mut usize) -> u8 {
    let v = data[*pos];
    *pos += 1;
    v
}

fn read_u32(data: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    v
}

fn read_i64(data: &[u8], pos: &mut usize) -> i64 {
    let v = i64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    v
}

fn read_f64(data: &[u8], pos: &mut usize) -> f64 {
    let v = f64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    v
}

fn read_str(data: &[u8], pos: &mut usize) -> String {
    let len = read_u32(data, pos) as usize;
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
    *pos += len;
    s
}

fn read_type(data: &[u8], pos: &mut usize) -> Option<Type> {
    let tag = read_u8(data, pos);
    match tag {
        TY_NONE => None,
        TY_INT => Some(Type::Int),
        TY_FLOAT => Some(Type::Float),
        TY_STR => Some(Type::Str),
        TY_BOOL => Some(Type::Bool),
        TY_NULL => Some(Type::Null),
        TY_ARRAY => {
            let inner = read_type(data, pos).unwrap_or(Type::Int);
            Some(Type::Array(Box::new(inner)))
        }
        _ => None,
    }
}
