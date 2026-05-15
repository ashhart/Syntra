use crate::ast::*;
use crate::environment::Env;
use crate::error::{LycanError, LycanResult};
use crate::value::{LycanFn, Value};

/// Signal for early return from functions.
enum Control {
    Value(Value),
    Return(Value),
}

impl Control {
    fn into_value(self) -> Value {
        match self {
            Control::Value(v) | Control::Return(v) => v,
        }
    }
}

pub struct Interpreter {
    env: Env,
    ctx: Option<crate::context::ExecutionContext>,
}

impl Interpreter {
    pub fn new() -> Self {
        Self { env: Env::new(), ctx: None }
    }

    #[allow(dead_code)]
    pub fn new_with_context(ctx: crate::context::ExecutionContext) -> Self {
        Self { env: Env::new(), ctx: Some(ctx) }
    }

    pub fn run(&mut self, program: &Program) -> LycanResult<Value> {
        let mut last = Value::Null;
        for node in &program.nodes {
            match self.exec(node)? {
                Control::Value(v) => last = v,
                Control::Return(v) => return Ok(v),
            }
        }
        Ok(last)
    }

    pub fn eval_node(&mut self, node: &Node) -> LycanResult<Value> {
        Ok(self.exec(node)?.into_value())
    }

    fn exec(&mut self, node: &Node) -> LycanResult<Control> {
        match node {
            Node::Int(n) => Ok(Control::Value(Value::Int(*n))),
            Node::Float(f) => Ok(Control::Value(Value::Float(*f))),
            Node::Str(s) => Ok(Control::Value(Value::Str(s.clone()))),
            Node::Bool(b) => Ok(Control::Value(Value::Bool(*b))),
            Node::Null => Ok(Control::Value(Value::Null)),

            Node::Ident(name) => {
                let val = self.env.get(name)?.clone();
                Ok(Control::Value(val))
            }

            Node::Bind { name, mutable, value, .. } => {
                let val = self.exec(value)?.into_value();
                self.env.define(name.clone(), val, *mutable);
                Ok(Control::Value(Value::Null))
            }

            Node::Assign { name, value } => {
                let val = self.exec(value)?.into_value();
                self.env.set(name, val)?;
                Ok(Control::Value(Value::Null))
            }

            Node::Fn { name, params, body, stateful, .. } => {
                let func = Value::Fn(LycanFn {
                    name: name.clone().unwrap_or_else(|| "lambda".to_string()),
                    params: params.clone(),
                    body: body.clone(),
                    stateful: *stateful,
                });
                if let Some(fn_name) = name {
                    self.env.define(fn_name.clone(), func.clone(), false);
                }
                Ok(Control::Value(func))
            }

            Node::Call { callee, args } => {
                let callee_val = self.exec(callee)?.into_value();
                let mut arg_vals = Vec::new();
                for arg in args {
                    arg_vals.push(self.exec(arg)?.into_value());
                }
                let result = self.call_fn(&callee_val, &arg_vals)?;
                Ok(Control::Value(result))
            }

            Node::If { cond, then_branch, else_branch } => {
                let cond_val = self.exec(cond)?.into_value();
                if cond_val.is_truthy() {
                    self.exec(then_branch)
                } else if let Some(else_b) = else_branch {
                    self.exec(else_b)
                } else {
                    Ok(Control::Value(Value::Null))
                }
            }

            Node::While { cond, body } => {
                let mut last = Value::Null;
                loop {
                    let cond_val = self.exec(cond)?.into_value();
                    if !cond_val.is_truthy() { break; }
                    for expr in body {
                        match self.exec(expr)? {
                            Control::Return(v) => return Ok(Control::Return(v)),
                            Control::Value(v) => last = v,
                        }
                    }
                }
                Ok(Control::Value(last))
            }

            Node::ForEach { var, iterable, body } => {
                let iter_val = self.exec(iterable)?.into_value();
                let items = match iter_val {
                    Value::Array(items) => items,
                    _ => return Err(LycanError::Runtime {
                        msg: format!("cannot iterate over {}", iter_val.type_name()),
                    }),
                };
                let mut last = Value::Null;
                self.env.push_scope();
                self.env.define(var.clone(), Value::Null, true);
                for item in items {
                    self.env.set(var, item)?;
                    for expr in body {
                        match self.exec(expr)? {
                            Control::Return(v) => {
                                self.env.pop_scope();
                                return Ok(Control::Return(v));
                            }
                            Control::Value(v) => last = v,
                        }
                    }
                }
                self.env.pop_scope();
                Ok(Control::Value(last))
            }

            Node::Repeat { count, body } => {
                let n = match self.exec(count)?.into_value() {
                    Value::Int(n) => n,
                    other => return Err(LycanError::Runtime {
                        msg: format!("repeat count must be int, got {}", other.type_name()),
                    }),
                };
                let mut last = Value::Null;
                for _ in 0..n {
                    for expr in body {
                        match self.exec(expr)? {
                            Control::Return(v) => return Ok(Control::Return(v)),
                            Control::Value(v) => last = v,
                        }
                    }
                }
                Ok(Control::Value(last))
            }

            Node::Return(val) => {
                let v = self.exec(val)?.into_value();
                Ok(Control::Return(v))
            }

            Node::Block(exprs) => {
                self.env.push_scope();
                let mut last = Value::Null;
                for expr in exprs {
                    match self.exec(expr)? {
                        Control::Return(v) => {
                            self.env.pop_scope();
                            return Ok(Control::Return(v));
                        }
                        Control::Value(v) => last = v,
                    }
                }
                self.env.pop_scope();
                Ok(Control::Value(last))
            }

            Node::Array(elems) => {
                let mut vals = Vec::new();
                for e in elems {
                    vals.push(self.exec(e)?.into_value());
                }
                Ok(Control::Value(Value::Array(vals)))
            }

            Node::Index { object, index } => {
                let obj = self.exec(object)?.into_value();
                let idx = self.exec(index)?.into_value();
                match (&obj, &idx) {
                    (Value::Array(arr), Value::Int(i)) => {
                        let i = *i as usize;
                        if i < arr.len() {
                            Ok(Control::Value(arr[i].clone()))
                        } else {
                            Err(LycanError::Runtime {
                                msg: format!("index {i} out of bounds (len {})", arr.len()),
                            })
                        }
                    }
                    _ => Err(LycanError::Runtime {
                        msg: format!("cannot index {} with {}", obj.type_name(), idx.type_name()),
                    }),
                }
            }

            Node::Range { start, end } => {
                let s = match self.exec(start)?.into_value() {
                    Value::Int(n) => n,
                    other => return Err(LycanError::Runtime {
                        msg: format!("range start must be int, got {}", other.type_name()),
                    }),
                };
                let e = match self.exec(end)?.into_value() {
                    Value::Int(n) => n,
                    other => return Err(LycanError::Runtime {
                        msg: format!("range end must be int, got {}", other.type_name()),
                    }),
                };
                let arr: Vec<Value> = (s..e).map(Value::Int).collect();
                Ok(Control::Value(Value::Array(arr)))
            }

            Node::Op { op, args } => {
                let result = self.eval_op(*op, args)?;
                Ok(Control::Value(result))
            }

            Node::Pipe { kind, data, func, init } => {
                let data_val = self.exec(data)?.into_value();
                let func_val = self.exec(func)?.into_value();

                match kind {
                    PipeKind::Pipe => {
                        self.call_fn(&func_val, &[data_val]).map(Control::Value)
                    }
                    PipeKind::Filter => {
                        let items = self.expect_array(data_val)?;
                        let mut result = Vec::new();
                        for item in items {
                            let keep = self.call_fn(&func_val, &[item.clone()])?;
                            if keep.is_truthy() {
                                result.push(item);
                            }
                        }
                        Ok(Control::Value(Value::Array(result)))
                    }
                    PipeKind::Map => {
                        let items = self.expect_array(data_val)?;
                        let mut result = Vec::new();
                        for item in items {
                            result.push(self.call_fn(&func_val, &[item])?);
                        }
                        Ok(Control::Value(Value::Array(result)))
                    }
                    PipeKind::Reduce => {
                        let items = self.expect_array(data_val)?;
                        let init_val = match init {
                            Some(init_expr) => self.exec(init_expr)?.into_value(),
                            None => Value::Null,
                        };
                        let mut acc = init_val;
                        for item in items {
                            acc = self.call_fn(&func_val, &[acc, item])?;
                        }
                        Ok(Control::Value(acc))
                    }
                }
            }

            Node::Adapt { target, body } => {
                let func = Value::Fn(LycanFn {
                    name: target.clone(),
                    params: match self.env.get(target)? {
                        Value::Fn(f) => f.params.clone(),
                        _ => Vec::new(),
                    },
                    body: body.clone(),
                    stateful: false,
                });
                self.env.redefine(target, func);
                Ok(Control::Value(Value::Null))
            }

            Node::Choice { options } => {
                // In tree-walker, just pick the first option (no weights)
                if let Some(opt) = options.first() {
                    self.exec(opt)
                } else {
                    Ok(Control::Value(Value::Null))
                }
            }

            Node::Guard { assumption, fast_path, fallback } => {
                let check = self.exec(assumption)?.into_value();
                if check.is_truthy() {
                    self.exec(fast_path)
                } else {
                    self.exec(fallback)
                }
            }

            Node::Strategy { options } => {
                // In tree-walker, just pick the first option
                if let Some(opt) = options.first() {
                    self.exec(opt)
                } else {
                    Ok(Control::Value(Value::Null))
                }
            }

            Node::Feedback { .. } => {
                // No-op in tree-walker — feedback only works in graph executor
                Ok(Control::Value(Value::Null))
            }

            Node::Builtin { name, args } => {
                let result = self.exec_builtin(name, args)?;
                Ok(Control::Value(result))
            }
        }
    }

    fn call_fn(&mut self, callee: &Value, args: &[Value]) -> LycanResult<Value> {
        let func = match callee {
            Value::Fn(f) => f.clone(),
            _ => return Err(LycanError::Runtime {
                msg: format!("cannot call {}", callee.type_name()),
            }),
        };

        self.env.push_scope();
        for (i, param) in func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::Null);
            self.env.define(param.name.clone(), val, false);
        }

        let mut result = Value::Null;
        for expr in &func.body {
            match self.exec(expr)? {
                Control::Return(v) => {
                    self.env.pop_scope();
                    return Ok(v);
                }
                Control::Value(v) => result = v,
            }
        }
        self.env.pop_scope();
        Ok(result)
    }

    fn eval_op(&mut self, op: OpKind, args: &[Node]) -> LycanResult<Value> {
        match op {
            OpKind::Not => {
                let a = self.exec(&args[0])?.into_value();
                Ok(Value::Bool(!a.is_truthy()))
            }
            OpKind::Neg => {
                let a = self.exec(&args[0])?.into_value();
                match a {
                    Value::Int(n) => Ok(Value::Int(-n)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    _ => Err(LycanError::Runtime { msg: format!("cannot negate {}", a.type_name()) }),
                }
            }
            _ => {
                let a = self.exec(&args[0])?.into_value();
                let b = self.exec(&args[1])?.into_value();
                match op {
                    OpKind::Add => self.add(a, b),
                    OpKind::Sub => self.arith(a, b, |x, y| x - y, |x, y| x - y),
                    OpKind::Mul => self.arith(a, b, |x, y| x * y, |x, y| x * y),
                    OpKind::Div => self.div(a, b),
                    OpKind::Mod => self.arith(a, b, |x, y| x % y, |x, y| x % y),
                    OpKind::Eq => Ok(Value::Bool(self.equal(&a, &b))),
                    OpKind::Neq => Ok(Value::Bool(!self.equal(&a, &b))),
                    OpKind::Lt => self.compare(a, b, |o| o.is_lt()),
                    OpKind::Gt => self.compare(a, b, |o| o.is_gt()),
                    OpKind::Lte => self.compare(a, b, |o| o.is_le()),
                    OpKind::Gte => self.compare(a, b, |o| o.is_ge()),
                    OpKind::And => Ok(Value::Bool(a.is_truthy() && b.is_truthy())),
                    OpKind::Or => Ok(Value::Bool(a.is_truthy() || b.is_truthy())),
                    OpKind::Not | OpKind::Neg => unreachable!(),
                }
            }
        }
    }

    fn add(&self, a: Value, b: Value) -> LycanResult<Value> {
        match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x + y)),
            (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x + y)),
            (Value::Int(x), Value::Float(y)) => Ok(Value::Float(*x as f64 + y)),
            (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x + *y as f64)),
            (Value::Str(x), Value::Str(y)) => Ok(Value::Str(format!("{x}{y}"))),
            (Value::Str(x), _) => Ok(Value::Str(format!("{x}{b}"))),
            (_, Value::Str(y)) => Ok(Value::Str(format!("{a}{y}"))),
            (Value::Array(x), Value::Array(y)) => {
                let mut result = x.clone();
                result.extend(y.iter().cloned());
                Ok(Value::Array(result))
            }
            _ => Err(LycanError::Runtime {
                msg: format!("cannot add {} and {}", a.type_name(), b.type_name()),
            }),
        }
    }

    fn div(&self, a: Value, b: Value) -> LycanResult<Value> {
        match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => {
                if *y == 0 { return Err(LycanError::Runtime { msg: "division by zero".into() }); }
                if x % y == 0 {
                    Ok(Value::Int(x / y))
                } else {
                    Ok(Value::Float(*x as f64 / *y as f64))
                }
            }
            (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x / y)),
            (Value::Int(x), Value::Float(y)) => Ok(Value::Float(*x as f64 / y)),
            (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x / *y as f64)),
            _ => Err(LycanError::Runtime {
                msg: format!("cannot divide {} by {}", a.type_name(), b.type_name()),
            }),
        }
    }

    fn arith(&self, a: Value, b: Value, int_op: fn(i64, i64) -> i64, float_op: fn(f64, f64) -> f64) -> LycanResult<Value> {
        match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => Ok(Value::Int(int_op(*x, *y))),
            (Value::Float(x), Value::Float(y)) => Ok(Value::Float(float_op(*x, *y))),
            (Value::Int(x), Value::Float(y)) => Ok(Value::Float(float_op(*x as f64, *y))),
            (Value::Float(x), Value::Int(y)) => Ok(Value::Float(float_op(*x, *y as f64))),
            _ => Err(LycanError::Runtime {
                msg: format!("cannot do arithmetic on {} and {}", a.type_name(), b.type_name()),
            }),
        }
    }

    fn compare(&self, a: Value, b: Value, cmp: fn(std::cmp::Ordering) -> bool) -> LycanResult<Value> {
        let ord = match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(std::cmp::Ordering::Equal),
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => return Err(LycanError::Runtime {
                msg: format!("cannot compare {} and {}", a.type_name(), b.type_name()),
            }),
        };
        Ok(Value::Bool(cmp(ord)))
    }

    fn equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Null, Value::Null) => true,
            _ => false,
        }
    }

    fn expect_array(&self, val: Value) -> LycanResult<Vec<Value>> {
        match val {
            Value::Array(items) => Ok(items),
            _ => Err(LycanError::Runtime {
                msg: format!("expected array, got {}", val.type_name()),
            }),
        }
    }

    fn exec_builtin(&mut self, name: &str, args: &[Node]) -> LycanResult<Value> {
        match name {
            "p" => {
                if let Some(ctx) = &self.ctx {
                    if let Some(pol) = &ctx.policy {
                        if !pol.allow_stdout {
                            return Err(LycanError::Runtime {
                                msg: "capability=print effect=stdout denied by policy".to_string(),
                            });
                        }
                    }
                }
                let mut vals = Vec::new();
                for arg in args {
                    vals.push(self.exec(arg)?.into_value());
                }
                let output: Vec<String> = vals.iter().map(|v| format!("{v}")).collect();
                println!("{}", output.join(" "));
                Ok(Value::Null)
            }
            "r" => {
                if let Some(ctx) = &self.ctx {
                    if let Some(pol) = &ctx.policy {
                        if !pol.allow_stdin {
                            return Err(LycanError::Runtime {
                                msg: "capability=readline effect=stdin denied by policy".to_string(),
                            });
                        }
                    }
                }
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).map_err(|e| LycanError::Runtime {
                    msg: format!("read error: {e}"),
                })?;
                Ok(Value::Str(input.trim_end().to_string()))
            }
            "len" => {
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Array(a) => Ok(Value::Int(a.len() as i64)),
                    Value::Str(s) => Ok(Value::Int(s.len() as i64)),
                    _ => Err(LycanError::Runtime {
                        msg: format!("cannot get length of {}", val.type_name()),
                    }),
                }
            }
            "str" => {
                let val = self.exec(&args[0])?.into_value();
                Ok(Value::Str(format!("{val}")))
            }
            "num" => {
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Int(n) => Ok(Value::Int(n)),
                    Value::Float(f) => Ok(Value::Float(f)),
                    Value::Str(s) => {
                        let s = s.trim();
                        if let Ok(n) = s.parse::<i64>() {
                            Ok(Value::Int(n))
                        } else if let Ok(f) = s.parse::<f64>() {
                            Ok(Value::Float(f))
                        } else {
                            Err(LycanError::Runtime {
                                msg: format!("cannot parse '{s}' as number"),
                            })
                        }
                    }
                    _ => Err(LycanError::Runtime {
                        msg: format!("cannot convert {} to number", val.type_name()),
                    }),
                }
            }
            "abs" => {
                if args.len() != 1 {
                    return Err(LycanError::Runtime { msg: "!abs expects 1 argument".to_string() });
                }
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Int(n) => n.checked_abs()
                        .map(Value::Int)
                        .ok_or_else(|| LycanError::Runtime { msg: "integer overflow in !abs".to_string() }),
                    Value::Float(f) => Ok(Value::Float(f.abs())),
                    _ => Err(LycanError::Runtime { msg: format!("cannot abs {}", val.type_name()) }),
                }
            }
            "sin" | "cos" => {
                if args.len() != 1 {
                    return Err(LycanError::Runtime { msg: format!("!{name} expects 1 argument") });
                }
                let val = self.exec(&args[0])?.into_value();
                let input = match val {
                    Value::Int(n) => n as f64,
                    Value::Float(f) => f,
                    _ => return Err(LycanError::Runtime {
                        msg: format!("!{name} requires number, got {}", val.type_name()),
                    }),
                };
                if !input.is_finite() {
                    return Err(LycanError::Runtime { msg: format!("!{name} requires finite input") });
                }
                let output = if name == "sin" { input.sin() } else { input.cos() };
                if !output.is_finite() {
                    return Err(LycanError::Runtime { msg: format!("!{name} produced non-finite output") });
                }
                Ok(Value::Float(output))
            }
            "round" => {
                if args.len() != 1 {
                    return Err(LycanError::Runtime { msg: "!round expects 1 argument".to_string() });
                }
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Int(n) => Ok(Value::Int(n)),
                    Value::Float(f) => {
                        if !f.is_finite() {
                            return Err(LycanError::Runtime { msg: "!round requires finite float".to_string() });
                        }
                        let rounded = f.round();
                        if rounded < i64::MIN as f64 || rounded > i64::MAX as f64 {
                            return Err(LycanError::Runtime { msg: "!round result out of i64 range".to_string() });
                        }
                        Ok(Value::Int(rounded as i64))
                    }
                    _ => Err(LycanError::Runtime { msg: format!("cannot round {}", val.type_name()) }),
                }
            }
            "sqrt" => {
                if args.len() != 1 {
                    return Err(LycanError::Runtime { msg: "!sqrt expects 1 argument".to_string() });
                }
                let val = self.exec(&args[0])?.into_value();
                let input = match val {
                    Value::Int(n) => n as f64,
                    Value::Float(f) => f,
                    _ => return Err(LycanError::Runtime {
                        msg: format!("!sqrt requires number, got {}", val.type_name()),
                    }),
                };
                if !input.is_finite() {
                    return Err(LycanError::Runtime { msg: "!sqrt requires finite input".to_string() });
                }
                if input < 0.0 {
                    return Err(LycanError::Runtime { msg: "!sqrt requires non-negative input".to_string() });
                }
                Ok(Value::Float(input.sqrt()))
            }
            "split" => {
                let val = self.exec(&args[0])?.into_value();
                let delim = if args.len() > 1 {
                    match self.exec(&args[1])?.into_value() {
                        Value::Str(s) => s,
                        _ => " ".to_string(),
                    }
                } else {
                    " ".to_string()
                };
                match val {
                    Value::Str(s) => {
                        let parts: Vec<Value> = s.split(&delim)
                            .filter(|p| !p.is_empty())
                            .map(|p| Value::Str(p.to_string()))
                            .collect();
                        Ok(Value::Array(parts))
                    }
                    _ => Err(LycanError::Runtime {
                        msg: format!("cannot split {}", val.type_name()),
                    }),
                }
            }
            "chars" => {
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Str(s) => {
                        let chars: Vec<Value> = s.chars()
                            .map(|c| Value::Str(c.to_string()))
                            .collect();
                        Ok(Value::Array(chars))
                    }
                    _ => Err(LycanError::Runtime {
                        msg: format!("cannot get chars of {}", val.type_name()),
                    }),
                }
            }
            "type" => {
                let val = self.exec(&args[0])?.into_value();
                Ok(Value::Str(val.type_name().to_string()))
            }
            "floor" => {
                if args.len() != 1 {
                    return Err(LycanError::Runtime { msg: "!floor expects 1 argument".to_string() });
                }
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Int(n) => Ok(Value::Int(n)),
                    Value::Float(f) if f.is_finite() => Ok(Value::Float(f.floor())),
                    Value::Float(_) => Err(LycanError::Runtime { msg: "!floor requires finite float".to_string() }),
                    _ => Err(LycanError::Runtime { msg: format!("cannot floor {}", val.type_name()) }),
                }
            }
            "ln" => {
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Float(f) if f > 0.0 => Ok(Value::Float(f.ln())),
                    Value::Int(n) if n > 0 => Ok(Value::Float((n as f64).ln())),
                    _ => Err(LycanError::Runtime { msg: format!("ln requires positive number") }),
                }
            }
            "exp" => {
                let val = self.exec(&args[0])?.into_value();
                match val {
                    Value::Float(f) => Ok(Value::Float(f.exp())),
                    Value::Int(n) => Ok(Value::Float((n as f64).exp())),
                    _ => Err(LycanError::Runtime { msg: format!("exp requires number") }),
                }
            }
            "lambert" => {
                // !lambert r1x r1y r1z r2x r2y r2z tof mu
                // Returns (A v1x v1y v1z v2x v2y v2z status)
                if args.len() < 8 {
                    return Err(LycanError::Runtime { msg: "!lambert needs 8 args: r1x r1y r1z r2x r2y r2z tof mu".into() });
                }
                let mut vals = Vec::new();
                for i in 0..8 {
                    vals.push(match self.exec(&args[i]).map(|c| c.into_value()) {
                        Ok(Value::Float(f)) => f,
                        Ok(Value::Int(n)) => n as f64,
                        _ => 0.0,
                    });
                }
                let get_f = |i: usize| -> f64 {
                    match vals.get(i) {
                        Some(v) => *v,
                        _ => 0.0,
                    }
                };
                let r1 = [get_f(0), get_f(1), get_f(2)];
                let r2 = [get_f(3), get_f(4), get_f(5)];
                let tof = get_f(6);
                let mu = get_f(7);
                let result = crate::lambert::solve(r1, r2, tof, mu, true);
                let status = if result.converged { 1.0 } else { 0.0 };
                Ok(Value::Array(vec![
                    Value::Float(result.v1[0]), Value::Float(result.v1[1]), Value::Float(result.v1[2]),
                    Value::Float(result.v2[0]), Value::Float(result.v2[1]), Value::Float(result.v2[2]),
                    Value::Float(status),
                ]))
            }
            "atan2" => {
                let y_val = self.exec(&args[0])?.into_value();
                let x_val = self.exec(&args[1])?.into_value();
                let y = match y_val { Value::Float(f) => f, Value::Int(n) => n as f64, _ => 0.0 };
                let x = match x_val { Value::Float(f) => f, Value::Int(n) => n as f64, _ => 0.0 };
                Ok(Value::Float(y.atan2(x)))
            }
            "cap" => {
                let name = match args.first() {
                    Some(node) => match self.exec(node)?.into_value() {
                        Value::Str(s) => s,
                        other => return Err(LycanError::Runtime {
                            msg: format!("!cap name must be str, got {}", other.type_name()),
                        }),
                    },
                    None => return Err(LycanError::Runtime {
                        msg: "!cap expects capability name".to_string(),
                    }),
                };
                let mut vals = Vec::new();
                for arg in &args[1..] {
                    vals.push(self.exec(arg)?.into_value());
                }
                self.exec_capability_value(&name, &vals)
            }
            _ => Err(LycanError::Runtime {
                msg: format!("unknown builtin '!{name}'"),
            }),
        }
    }

    fn exec_capability_value(&self, name: &str, args: &[Value]) -> LycanResult<Value> {
        let cap_args = args.iter()
            .map(value_to_cap_value)
            .collect::<LycanResult<Vec<_>>>()?;
        crate::capabilities::execute(name, &cap_args, self.ctx.as_ref())
            .map(cap_value_to_value)
            .map_err(|msg| LycanError::Runtime { msg })
    }
}

fn value_to_cap_value(value: &Value) -> LycanResult<crate::capabilities::CapValue> {
    Ok(match value {
        Value::Int(n) => crate::capabilities::CapValue::Int(*n),
        Value::Float(n) => crate::capabilities::CapValue::Float(*n),
        Value::Str(s) => crate::capabilities::CapValue::Str(s.clone()),
        Value::Bool(b) => crate::capabilities::CapValue::Bool(*b),
        Value::Null => crate::capabilities::CapValue::Null,
        Value::Array(items) => crate::capabilities::CapValue::Array(
            items.iter()
                .map(value_to_cap_value)
                .collect::<LycanResult<Vec<_>>>()?
        ),
        Value::Fn(_) => return Err(LycanError::Runtime {
            msg: "capability arguments cannot include functions".to_string(),
        }),
    })
}

fn cap_value_to_value(value: crate::capabilities::CapValue) -> Value {
    match value {
        crate::capabilities::CapValue::Int(n) => Value::Int(n),
        crate::capabilities::CapValue::Float(n) => Value::Float(n),
        crate::capabilities::CapValue::Str(s) => Value::Str(s),
        crate::capabilities::CapValue::Bool(b) => Value::Bool(b),
        crate::capabilities::CapValue::Null => Value::Null,
        crate::capabilities::CapValue::Array(items) => {
            Value::Array(items.into_iter().map(cap_value_to_value).collect())
        }
    }
}
