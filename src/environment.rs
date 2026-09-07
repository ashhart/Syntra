use std::collections::HashMap;
use crate::error::{LycanError, LycanResult};
use crate::value::Value;

/// Lexically scoped environment — chain of frames.
#[derive(Debug, Clone)]
pub struct Env {
    frames: Vec<Frame>,
}

#[derive(Debug, Clone)]
struct Frame {
    vars: HashMap<String, Slot>,
}

#[derive(Debug, Clone)]
struct Slot {
    value: Value,
    mutable: bool,
}

impl Env {
    pub fn new() -> Self {
        Self {
            frames: vec![Frame { vars: HashMap::new() }],
        }
    }

    pub fn push_scope(&mut self) {
        self.frames.push(Frame { vars: HashMap::new() });
    }

    pub fn pop_scope(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    pub fn define(&mut self, name: String, value: Value, mutable: bool) {
        let frame = self.frames.last_mut().unwrap();
        frame.vars.insert(name, Slot { value, mutable });
    }

    pub fn get(&self, name: &str) -> LycanResult<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(slot) = frame.vars.get(name) {
                return Ok(&slot.value);
            }
        }
        Err(LycanError::Runtime {
            msg: format!("undefined '{name}'"),
        })
    }

    pub fn set(&mut self, name: &str, value: Value) -> LycanResult<()> {
        for frame in self.frames.iter_mut().rev() {
            if let Some(slot) = frame.vars.get_mut(name) {
                if !slot.mutable {
                    return Err(LycanError::Runtime {
                        msg: format!("'{name}' is immutable"),
                    });
                }
                slot.value = value;
                return Ok(());
            }
        }
        Err(LycanError::Runtime {
            msg: format!("undefined '{name}'"),
        })
    }

    /// Force-set a value (used by adapt/~> to redefine functions).
    pub fn redefine(&mut self, name: &str, value: Value) {
        for frame in self.frames.iter_mut().rev() {
            if let Some(slot) = frame.vars.get_mut(name) {
                slot.value = value;
                return;
            }
        }
        // If not found, define in global scope
        self.frames[0].vars.insert(name.to_string(), Slot { value, mutable: true });
    }
}
