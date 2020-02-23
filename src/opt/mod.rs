use std::ops::Deref;

use crate::lang::func::Func;
use crate::lang::Program;

pub mod simple;
pub mod graph;

/// Program optimization pass trait
pub trait Pass {
    fn opt(&mut self, pro: &mut Program);
}

/// Global (function-level) optimizer trait
pub trait FuncPass: Pass {
    fn opt(&mut self, pro: &mut Program) {
        for func in &pro.funcs {
            self.opt_fn(func.deref())
        }
    }

    fn opt_fn(&mut self, func: &Func);
}