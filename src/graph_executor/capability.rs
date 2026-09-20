//! Native capability dispatch — GVal ↔ CapValue bridging and execution.

use super::GraphExecutor;
use super::rt_err;
use super::value::GVal;
use crate::error::LycanResult;

impl GraphExecutor {
    pub(super) fn exec_capability_gval(&self, name: &str, args: &[GVal]) -> LycanResult<GVal> {
        let cap_args = args
            .iter()
            .map(gval_to_cap_value)
            .collect::<LycanResult<Vec<_>>>()?;
        crate::capabilities::execute(name, &cap_args, self.ctx.as_ref())
            .map(cap_value_to_gval)
            .map_err(|msg| rt_err(&msg))
    }
}

fn gval_to_cap_value(value: &GVal) -> LycanResult<crate::capabilities::CapValue> {
    Ok(match value {
        GVal::Int(n) => crate::capabilities::CapValue::Int(*n),
        GVal::Float(n) => crate::capabilities::CapValue::Float(*n),
        GVal::Str(s) => crate::capabilities::CapValue::Str(s.clone()),
        GVal::Bool(b) => crate::capabilities::CapValue::Bool(*b),
        GVal::Null => crate::capabilities::CapValue::Null,
        GVal::Array(items) => crate::capabilities::CapValue::Array(
            items
                .iter()
                .map(gval_to_cap_value)
                .collect::<LycanResult<Vec<_>>>()?,
        ),
        GVal::GraphFn { .. } => {
            return Err(rt_err("capability arguments cannot include functions"));
        }
    })
}

fn cap_value_to_gval(value: crate::capabilities::CapValue) -> GVal {
    match value {
        crate::capabilities::CapValue::Int(n) => GVal::Int(n),
        crate::capabilities::CapValue::Float(n) => GVal::Float(n),
        crate::capabilities::CapValue::Str(s) => GVal::Str(s),
        crate::capabilities::CapValue::Bool(b) => GVal::Bool(b),
        crate::capabilities::CapValue::Null => GVal::Null,
        crate::capabilities::CapValue::Array(items) => {
            GVal::Array(items.into_iter().map(cap_value_to_gval).collect())
        }
    }
}
