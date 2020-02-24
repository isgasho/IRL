use std::cell::RefCell;
use std::fmt::{Debug, Error, Formatter};
use std::rc::Rc;
use std::str::FromStr;

use crate::lang::func::{BlockRef, Func};
use crate::lang::util::ExtRc;
use crate::lang::val::{Symbol, SymbolRef, Value};

#[derive(Clone, Debug)]
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
    Phi { src: Vec<PhiSrc>, dst: RefCell<SymbolRef> },
    /// Allocate memory on stack, and return pointer to the beginning of that location.
    Alloc { dst: RefCell<SymbolRef> },
    /// Get pointer to a element of a pointer to value.
    /// `base` is a pointer value. If `off` is not none, the instruction offset pointer by `off`
    /// multiplied by the size of target type of `base`. If `ind` is none, the pointer is
    /// returned. Otherwise, the instruction dereference the pointer and progresses by indexing
    /// into the aggregate. If `i`th level of the aggregate is a structure type, then `i`th `ind`
    /// must be an `i64` constant. Otherwise, it can be any `i64` value. Finally, the indexed
    /// element is referenced again, returning a pointer to that element.
    Ptr {
        base: RefCell<Value>,
        off: Option<RefCell<Value>>,
        ind: Option<Vec<RefCell<Value>>>,
        dst: RefCell<SymbolRef>,
    },
    /// Load data from a pointer
    Ld { ptr: RefCell<Value>, dst: RefCell<SymbolRef> },
    /// Store data to a pointer
    St { src: RefCell<Value>, ptr: RefCell<Value> },
}

pub type PhiSrc = (Option<BlockRef>, RefCell<Value>);

pub type InstrRef = ExtRc<Instr>;

impl Debug for InstrRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        write!(f, "{}", self.0.name())
    }
}

impl Instr {
    /// Get instruction name
    pub fn name(&self) -> String {
        match self {
            Instr::Mov { src: _, dst: _ } => "mov".to_string(),
            Instr::Un { op, opd: _, dst: _ } => op.to_string(),
            Instr::Bin { op, fst: _, snd: _, dst: _ } => op.to_string(),
            Instr::Jmp { tgt: _ } => "jmp".to_string(),
            Instr::Br { cond: _, tr: _, fls: _ } => "br".to_string(),
            Instr::Call { func: _, arg: _, dst: _ } => "call".to_string(),
            Instr::Ret { val: _ } => "ret".to_string(),
            Instr::Phi { src: _, dst: _ } => "phi".to_string(),
            Instr::Alloc { dst: _ } => "alloc".to_string(),
            Instr::Ptr { base: _, off: _, ind: _, dst: _ } => "ptr".to_string(),
            Instr::Ld { ptr: _, dst: _ } => "ld".to_string(),
            Instr::St { src: _, ptr: _ } => "st".to_string(),
        }
    }

    /// Decide if this instruction is a control flow instruction.
    /// A control flow instruction correspond to a directed edge in the CFG.
    /// Currently, only `jmp`, `br` and `ret`are control flow instructions.
    pub fn is_ctrl(&self) -> bool {
        match self {
            Instr::Jmp { tgt: _ } | Instr::Br { cond: _, tr: _, fls: _ }
            | Instr::Ret { val: _ } => true,
            _ => false
        }
    }

    /// Possible return the destination symbol of this instruction. This symbol is defined by
    /// this instruction.
    pub fn dst(&self) -> Option<&RefCell<SymbolRef>> {
        match self {
            Instr::Mov { src: _, dst } => Some(dst),
            Instr::Un { op: _, opd: _, dst } => Some(dst),
            Instr::Bin { op: _, fst: _, snd: _, dst } => Some(dst),
            Instr::Call { func: _, arg: _, dst } => dst.as_ref(),
            Instr::Phi { src: _, dst } => Some(dst),
            Instr::Jmp { tgt: _ } => None,
            Instr::Br { cond: _, tr: _, fls: _ } => None,
            Instr::Ret { val: _ } => None,
            Instr::Alloc { dst } => Some(dst),
            Instr::Ptr { base: _, off: _, ind: _, dst } => Some(dst),
            Instr::Ld { ptr: _, dst } => Some(dst),
            Instr::St { src: _, ptr: _ } => None,
        }
    }

    /// Return list of all the source operands used by this instruction.
    pub fn src(&self) -> Vec<&RefCell<Value>> {
        match self {
            Instr::Mov { src, dst: _ } => vec![src],
            Instr::Un { op: _, opd, dst: _ } => vec![opd],
            Instr::Bin { op: _, fst, snd, dst: _ } => vec![fst, snd],
            Instr::Call { func: _, arg, dst: _ } => arg.iter().map(|a| a).collect(),
            Instr::Phi { src, dst: _ } => src.iter().map(|(_, v)| v).collect(),
            Instr::Ret { val } => match val {
                Some(v) => vec![v],
                None => vec![]
            }
            Instr::Jmp { tgt: _ } => vec![],
            Instr::Br { cond, tr: _, fls: _ } => vec![cond],
            Instr::Alloc { dst: _ } => vec![],
            Instr::Ptr { base, off, ind, dst: _ } => {
                let mut v = vec![base];
                off.as_ref().map(|off| v.push(off));
                ind.as_ref().map(|ind| { for i in ind { v.push(i) } });
                v
            }
            Instr::Ld { ptr, dst: _ } => vec![ptr],
            Instr::St { src, ptr } => vec![src, ptr]
        }
    }

    /// Decide whether this instruction has side effects
    pub fn has_side_effect(&self) -> bool {
        match self {
            // Called function may or may not have side effect, here we assume it has
            Instr::Call { func: _, arg: _, dst: _ } => true,
            // Store instruction modifies memory
            Instr::St { src: _, ptr: _ } => true,
            // For other instructions, check if it assigns to global variable
            instr if instr.dst().is_some() => {
                let sym = instr.dst().unwrap();
                match sym.borrow().as_ref() {
                    Symbol::Global(_) => true,
                    _ => false
                }
            }
            _ => false
        }
    }
}

#[derive(Clone, Copy, Debug)]
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

#[derive(Clone, Copy, Debug)]
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

impl BinOp {
    pub fn is_arith(&self) -> bool { !self.is_cmp() }

    pub fn is_cmp(&self) -> bool {
        match self {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => true,
            _ => false
        }
    }
}
