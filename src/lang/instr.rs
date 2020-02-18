use std::cell::RefCell;
use std::fmt::{Debug, Error, Formatter};
use std::rc::Rc;
use std::str::FromStr;

use crate::lang::bb::BlockRef;
use crate::lang::ExtRc;
use crate::lang::val::{Func, SymbolRef, Value};

#[derive(Debug)]
pub enum Instr {
    /// Move (copy) data from one virtual register to another
    Mov { src: RefCell<Value>, dst: RefCell<SymbolRef> },
    /// Unary operations
    Un { op: UnOp, opd: RefCell<Value>, dst: RefCell<SymbolRef> },
    /// Binary operations
    Bin { op: BinOp, fst: RefCell<Value>, snd: RefCell<Value>, dst: RefCell<SymbolRef> },
    /// Jump to another basic block
    Jmp { tgt: RefCell<BlockRef> },
    /// Conditional branch to labels
    /// If `cond` evaluates to true, branch to `tr` block, otherwise to `fls` block
    Br { cond: RefCell<Value>, tr: RefCell<BlockRef>, fls: RefCell<BlockRef> },
    /// Procedure call
    Call { func: Rc<Func>, arg: Vec<RefCell<Value>>, dst: Option<RefCell<SymbolRef>> },
    /// Return computation results, or `None` if return type is `Void`.
    Ret { val: Option<RefCell<Value>> },
    /// Phi instructions in SSA
    /// A phi instruction hold a list of block-value pairs. The blocks are all predecessors of
    /// current block (where this instruction is defined). The values are different versions of
    /// of a certain variable.
    Phi { src: Vec<(Option<BlockRef>, RefCell<Value>)>, dst: RefCell<SymbolRef> },
}

pub type InstrRef = ExtRc<Instr>;

impl Debug for ExtRc<Instr> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> { self.0.fmt(f) }
}

impl Instr {
    /// Decide if this instruction is a control flow instruction
    /// Currently, only `Br`, `Call` and `Ret`are control flow instructions.
    pub fn is_ctrl(&self) -> bool {
        match self {
            Instr::Br { cond: _, tr: _, fls: _ } | Instr::Call { func: _, arg: _, dst: _ } |
            Instr::Ret { val: _ } => true,
            _ => false
        }
    }

    /// Possible return the symbol defined by this instruction.
    pub fn def(&self) -> Option<&RefCell<SymbolRef>> {
        match self {
            Instr::Mov { src: _, dst } => Some(dst),
            Instr::Un { op: _, opd: _, dst } => Some(dst),
            Instr::Bin { op: _, fst: _, snd: _, dst } => Some(dst),
            Instr::Call { func: _, arg: _, dst } => dst.as_ref(),
            Instr::Phi { src: _, dst } => Some(dst),
            _ => None
        }
    }

    /// Return list of all the operands used by this instruction.
    pub fn opd(&self) -> Vec<&RefCell<Value>> {
        match self {
            Instr::Mov { src, dst: _ } => vec![src],
            Instr::Un { op: _, opd, dst: _ } => vec![opd],
            Instr::Bin { op: _, fst: left, snd: right, dst: _ } => vec![left, right],
            Instr::Br { cond, tr: _, fls: _ } => vec![cond],
            Instr::Call { func: _, arg, dst: _ } => arg.iter().map(|a| a).collect(),
            Instr::Ret { val } => match val {
                Some(v) => vec![v],
                None => vec![]
            }
            Instr::Phi { src, dst: _ } => src.iter().map(|(_, v)| v).collect(),
            _ => vec![]
        }
    }
}

#[derive(Debug)]
pub enum UnOp {
    /// 2's complement of signed number
    Neg,
    /// Bitwise-NOT of bits
    Not,
}

impl FromStr for UnOp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "neg" => Ok(UnOp::Neg),
            "not" => Ok(UnOp::Not),
            _ => Err("not unary operation".to_string())
        }
    }
}

impl ToString for UnOp {
    fn to_string(&self) -> String {
        format!("{:?}", self).to_lowercase()
    }
}

#[derive(Debug)]
pub enum BinOp {
    /// Addition
    Add,
    /// Subtraction
    Sub,
    /// Multiplication
    Mul,
    /// Division
    Div,
    /// Modulo
    Mod,
    /// Bitwise-AND
    And,
    /// Bitwise-OR
    Or,
    /// Bitwise-XOR
    Xor,
    /// Left shift
    Shl,
    /// Right shift
    Shr,
    /// Equal
    Eq,
    /// Not equal
    Ne,
    /// Less than
    Lt,
    /// Less equal
    Le,
    /// Greater than
    Gt,
    /// Greater equal
    Ge,
}

impl FromStr for BinOp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "add" => Ok(BinOp::Add),
            "sub" => Ok(BinOp::Sub),
            "mul" => Ok(BinOp::Mul),
            "div" => Ok(BinOp::Div),
            "mod" => Ok(BinOp::Mod),
            "and" => Ok(BinOp::And),
            "or" => Ok(BinOp::Or),
            "xor" => Ok(BinOp::Xor),
            "shl" => Ok(BinOp::Shl),
            "shr" => Ok(BinOp::Shr),
            "eq" => Ok(BinOp::Eq),
            "ne" => Ok(BinOp::Ne),
            "lt" => Ok(BinOp::Lt),
            "le" => Ok(BinOp::Le),
            "gt" => Ok(BinOp::Gt),
            "ge" => Ok(BinOp::Ge),
            _ => Err("not binary operation".to_string())
        }
    }
}

impl ToString for BinOp {
    fn to_string(&self) -> String { format!("{:?}", self).to_lowercase() }
}
