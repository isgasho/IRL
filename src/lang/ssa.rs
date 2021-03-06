use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::ops::Deref;
use std::rc::Rc;

use crate::lang::func::{BlockRef, DomTreeListener, Fn};
use crate::lang::inst::{Inst, InstRef, PhiSrc};
use crate::lang::util::{ExtRc, WorkList};
use crate::lang::value::{Scope, Symbol, SymbolRef, Typed, Value};

/// Wrapper of SSA flag to make it only modifiable in this module.
#[derive(Debug)]
pub struct SsaFlag(Cell<bool>);

impl SsaFlag {
    pub fn new() -> SsaFlag { SsaFlag(Cell::new(false)) }

    pub fn get(&self) -> bool { self.0.get() }

    fn set(&self, val: bool) { self.0.set(val) }
}

impl Fn {
    /// Assert that current function is in SSA form
    pub fn assert_ssa(&self) {
        if !self.ssa.get() {
            panic!("fn @{} is not in SSA form", self.name)
        }
    }
}

/// Visitor of instructions in SSA program.
pub trait InstListener: DomTreeListener {
    fn on_begin(&mut self, _func: &Fn) {}

    fn on_enter(&mut self, block: BlockRef) {
        // Visit instructions
        for instr in block.inst.borrow().iter().cloned() {
            self.on_instr(instr);
        }

        // Visit phi instructions in successors
        for succ in block.succ.borrow().clone().into_iter() {
            for instr in succ.inst.borrow().clone().into_iter() {
                match instr.deref() {
                    Inst::Phi { src: _, dst: _ } =>
                        self.on_succ_phi(block.clone(), instr),
                    _ => break // phi instructions must be at front of each block
                }
            }
        }
    }

    /// Called when visiting each instruction.
    fn on_instr(&mut self, instr: InstRef);

    /// Called when visiting phi instructions in successor blocks.
    fn on_succ_phi(&mut self, this: BlockRef, instr: InstRef);
}

/// Visitor of variables in SSA program.
pub trait ValueListener: InstListener {
    fn on_instr(&mut self, instr: InstRef) {
        match instr.deref() {
            Inst::Phi { src: _, dst: _ } => if let Some(dst) = instr.dst() {
                self.on_def(instr.clone(), dst);
            }
            _ => {
                for opd in instr.src() {
                    self.on_use(instr.clone(), opd);
                }
                if let Some(dst) = instr.dst() {
                    self.on_def(instr.clone(), dst);
                }
            }
        }
    }

    fn on_succ_phi(&mut self, this: BlockRef, instr: InstRef) {
        if let Inst::Phi { src, dst: _ } = instr.deref() {
            src.iter().filter(|(pred, _)| pred.borrow().deref() == &this)
                .for_each(|(_, opd)| self.on_use(instr.clone(), opd))
        }
    }

    /// Call on operands (uses) of the instruction.
    fn on_use(&mut self, instr: InstRef, opd: &RefCell<Value>);

    /// Call on possible definition of the instruction.
    fn on_def(&mut self, instr: InstRef, def: &RefCell<SymbolRef>);
}

pub struct Verifier {
    // Whether a variable is found to be statically defined.
    def: HashSet<SymbolRef>,
    // Whether variables are available when reaching this block.
    // Organized as stack of frames, representing nodes on the path from root to current block
    avail: Vec<Vec<SymbolRef>>,
    // Error information
    pub err: Vec<String>,
}

impl DomTreeListener for Verifier {
    fn on_begin(&mut self, func: &Fn) {
        // Add parameters as the first frame
        func.param.iter().for_each(|p| { self.def.insert(p.borrow().clone()); });
        self.avail.push(func.param.iter().map(|p| p.borrow().clone()).collect());

        // Check phi operands in entrance block
        InstListener::on_begin(self, func);
    }

    fn on_end(&mut self, func: &Fn) {
        func.ssa.set(true);
        self.def.clear();
        self.avail.clear();
    }

    fn on_enter(&mut self, block: BlockRef) {
        // Push current frame to stack
        self.avail.push(vec![]);

        // Build predecessor list
        let req_pred: Vec<_> = block.pred.borrow().clone().into_iter()
            .map(|b| RefCell::new(b)).collect();

        // Check correspondence of phi operands to predecessors
        for instr in block.inst.borrow().iter() {
            match instr.deref() {
                Inst::Phi { src, dst: _ } => {
                    let phi_pred: Vec<_> = src.clone().into_iter().map(|(pred, _)| pred).collect();
                    for pred in &req_pred {
                        if !phi_pred.contains(pred) {
                            self.err.push(format!(
                                "phi operand not found for {}", pred.borrow().name
                            ));
                        }
                    }
                }
                _ => break
            }
        }

        InstListener::on_enter(self, block)
    }

    fn on_exit(&mut self, _: BlockRef) {
        self.avail.pop();
    }

    fn on_enter_child(&mut self, _: BlockRef, _: BlockRef) {}

    fn on_exit_child(&mut self, _: BlockRef, _: BlockRef) {}
}

impl InstListener for Verifier {
    fn on_instr(&mut self, instr: InstRef) {
        ValueListener::on_instr(self, instr)
    }

    fn on_succ_phi(&mut self, this: BlockRef, instr: InstRef) {
        ValueListener::on_succ_phi(self, this, instr)
    }
}

impl ValueListener for Verifier {
    fn on_use(&mut self, _: InstRef, opd: &RefCell<Value>) {
        match opd.borrow().deref() {
            Value::Var(sym) if sym.is_local_var() && !self.is_avail(sym) => {
                self.err.push(format!(
                    "variable {} is used before defined", sym.name()
                ));
            }
            _ => ()
        }
    }

    fn on_def(&mut self, _: InstRef, def: &RefCell<SymbolRef>) {
        if def.borrow().is_local_var() {
            let sym = def.borrow().clone();
            if self.def.contains(&sym) { // already statically defined
                self.err.push(format!("variable {} already defined", sym.name()));
            } else {
                self.def.insert(sym.clone()); // mark this static definition
                // add to current frame of availability stack
                self.avail.last_mut().unwrap().push(sym)
            }
        }
    }
}

impl Verifier {
    pub fn new() -> Verifier {
        Verifier {
            def: HashSet::new(),
            avail: vec![],
            err: vec![],
        }
    }

    fn is_avail(&self, sym: &SymbolRef) -> bool {
        self.avail.iter().any(|frame| frame.contains(sym))
    }
}

impl Fn {
    pub fn to_ssa(&self) {
        if self.ssa.get() { return; } // already in SSA form
        let df = self.compute_df();
        self.insert_phi(&df);
        self.rename();
        self.elim_dead_code();
        self.ssa.set(true);
    }

    fn insert_phi(&self, df: &HashMap<BlockRef, Vec<BlockRef>>) {
        // Keep records for blocks and symbols
        // set of symbols the phi's of whom are inserted
        let mut ins_phi: HashMap<BlockRef, HashSet<SymbolRef>> = HashMap::new();
        // set of symbols defined in a block
        let mut orig: HashMap<BlockRef, HashSet<SymbolRef>> = HashMap::new();
        // set of block where a symbol is defined
        let mut def_site: HashMap<SymbolRef, HashSet<BlockRef>> = HashMap::new();

        // Build these records
        self.scope.for_each(|sym| { def_site.insert(sym, HashSet::new()); });
        self.dfs().for_each(|block| {
            ins_phi.insert(block.clone(), HashSet::new());
            let def = self.defined_sym(&block);
            def.iter().for_each(|sym| {
                def_site.get_mut(sym).unwrap().insert(block.clone());
            });
            orig.insert(block, def);
        });

        // Insert phi instructions using worklist algorithm
        self.scope.for_each(|sym| {
            let mut work: WorkList<BlockRef> = def_site.get(&sym).unwrap().iter()
                .cloned().collect();
            while !work.is_empty() {
                let block = work.pick().unwrap();
                for tgt in df.get(&block).unwrap() {
                    // Insert phi instruction for this symbol
                    if ins_phi.get(tgt).unwrap().contains(&sym) { continue; }
                    let src: Vec<PhiSrc> = tgt.pred.borrow().clone().into_iter().map(|pred| {
                        (RefCell::new(pred), RefCell::new(Value::Var(sym.clone())))
                    }).collect();
                    tgt.push_front(ExtRc::new(Inst::Phi {
                        src,
                        dst: RefCell::new(sym.clone()),
                    }));

                    // Update records
                    ins_phi.get_mut(tgt).unwrap().insert(sym.clone());
                    if !orig.get(&tgt).unwrap().contains(&sym) {
                        work.insert(tgt.clone());
                    }
                }
            }
        })
    }

    fn rename(&self) {
        let mut listener = Renamer {
            sym: HashMap::new(),
            def: vec![],
            scope: None,
        };
        self.walk_dom(&mut listener);
    }

    fn defined_sym(&self, block: &BlockRef) -> HashSet<SymbolRef> {
        let mut def: HashSet<SymbolRef> = HashSet::new();
        for instr in block.inst.borrow().iter() {
            for sym in instr.dst() {
                match sym.borrow().as_ref() {
                    Symbol::Local { name: _, ty: _ } => {
                        def.insert(sym.borrow().clone());
                    }
                    _ => continue
                }
            }
        }
        def
    }
}

struct RenamedSym {
    /// Original name of this symbol
    name: String,
    /// How many versions are defined now
    count: usize,
    /// Stack of versioned variables
    stack: Vec<SymbolRef>,
}

impl RenamedSym {
    fn latest(&self) -> SymbolRef { self.stack.last().unwrap().clone() }

    fn pop(&mut self) { self.stack.pop(); }

    fn rename(&mut self) -> SymbolRef {
        self.count += 1;
        let new_sym =
            if let Symbol::Local { name: _, ty } = self.latest().deref() {
                ExtRc::new(Symbol::Local {
                    name: format!("{}.{}", self.name, self.count),
                    ty: ty.clone(),
                })
            } else { unreachable!() };
        self.stack.push(new_sym.clone());
        new_sym
    }
}

struct Renamer {
    /// Map symbol name to its renaming status
    sym: HashMap<String, RenamedSym>,
    /// Stack of frames for defined symbols in each block
    def: Vec<Vec<String>>,
    /// The scope we are interested
    scope: Option<Rc<Scope>>,
}

impl DomTreeListener for Renamer {
    fn on_begin(&mut self, func: &Fn) {
        // Initialize renaming stack
        let mut added = vec![];
        func.scope.for_each(|sym| {
            let new_sym = ExtRc::new(Symbol::Local {
                name: sym.name().to_string(),
                ty: sym.get_type(),
            });
            added.push(new_sym.clone());
            self.sym.insert(sym.name().to_string(), RenamedSym {
                name: sym.name().to_string(),
                count: 0,
                stack: vec![new_sym],
            });
        });

        // Reset scope
        func.scope.clear();
        func.scope.append(added.into_iter());

        // Replace function parameters
        func.param.iter().for_each(|param| {
            let new_sym = self.sym.get(param.borrow().name()).unwrap()
                .stack.last().unwrap().clone();
            param.replace(new_sym);
        });

        self.scope = Some(func.scope.clone());
        InstListener::on_begin(self, func)
    }

    fn on_end(&mut self, _: &Fn) {
        self.sym.clear();
        self.def.clear();
        self.scope = None;
    }

    fn on_enter(&mut self, block: BlockRef) {
        self.def.push(vec![]);
        InstListener::on_enter(self, block)
    }

    fn on_exit(&mut self, _: BlockRef) {
        for name in self.def.last().unwrap() {
            self.sym.get_mut(name).unwrap().pop();
        }
        self.def.pop();
    }

    fn on_enter_child(&mut self, _: BlockRef, _: BlockRef) {}

    fn on_exit_child(&mut self, _: BlockRef, _: BlockRef) {}
}

impl InstListener for Renamer {
    fn on_instr(&mut self, instr: InstRef) {
        ValueListener::on_instr(self, instr)
    }

    fn on_succ_phi(&mut self, this: BlockRef, instr: InstRef) {
        ValueListener::on_succ_phi(self, this, instr)
    }
}

impl ValueListener for Renamer {
    fn on_use(&mut self, _: InstRef, opd: &RefCell<Value>) {
        opd.replace_with(|opd| {
            match opd.deref() {
                Value::Var(sym) => match sym.deref() {
                    Symbol::Local { name: _, ty: _ } => {
                        let latest = self.sym.get(sym.name()).unwrap().latest();
                        Value::Var(latest)
                    }
                    _ => opd.clone()
                }
                _ => opd.clone()
            }
        });
    }

    fn on_def(&mut self, _: InstRef, def: &RefCell<SymbolRef>) {
        def.replace_with(|sym| {
            match sym.as_ref() {
                Symbol::Local { name: _, ty: _ } => {
                    let rename_sym = self.sym.get_mut(sym.name()).unwrap();
                    let name = rename_sym.name.clone();
                    let new_sym = rename_sym.rename();
                    self.def.last_mut().unwrap().push(name);
                    self.scope.as_deref().unwrap().insert(new_sym.clone());
                    new_sym
                }
                _ => sym.clone()
            }
        });
    }
}

/// Carry definition point and use points of a certain symbol
#[derive(Debug)]
pub struct DefUse {
    pub def: DefPos,
    pub uses: Vec<InstRef>,
}

/// Specify the definition position
#[derive(Clone, Debug)]
pub enum DefPos {
    /// Defined in parameter list
    Param,
    /// Defined in instruction
    Inst(BlockRef, InstRef),
    /// Serve as placeholder for the symbol whose definition point has not yet been determined
    None,
}

struct DefUseBuilder {
    info: HashMap<SymbolRef, DefUse>,
    blk: Vec<BlockRef>,
}

impl DomTreeListener for DefUseBuilder {
    fn on_begin(&mut self, func: &Fn) {
        // Build parameter definition
        func.param.iter().for_each(|param| {
            self.info.insert(param.borrow().clone(), DefUse {
                def: DefPos::Param,
                uses: vec![],
            });
        });
        InstListener::on_begin(self, func)
    }

    fn on_end(&mut self, _: &Fn) {}

    fn on_enter(&mut self, block: BlockRef) {
        self.blk.push(block.clone());
        InstListener::on_enter(self, block);
    }

    fn on_exit(&mut self, _: BlockRef) { self.blk.pop(); }

    fn on_enter_child(&mut self, _: BlockRef, _: BlockRef) {}

    fn on_exit_child(&mut self, _: BlockRef, _: BlockRef) {}
}

impl InstListener for DefUseBuilder {
    fn on_instr(&mut self, instr: InstRef) {
        ValueListener::on_instr(self, instr)
    }

    fn on_succ_phi(&mut self, this: BlockRef, instr: InstRef) {
        ValueListener::on_succ_phi(self, this, instr)
    }
}

impl ValueListener for DefUseBuilder {
    fn on_use(&mut self, instr: InstRef, opd: &RefCell<Value>) {
        match opd.borrow().deref() {
            Value::Var(sym) if sym.is_local_var() => match self.info.get_mut(sym) {
                Some(info) => info.uses.push(instr),
                None => { // some symbols may be undefined in transformed SSA
                    self.info.insert(sym.clone(), DefUse {
                        def: DefPos::None,
                        uses: vec![instr.clone()],
                    });
                }
            }
            _ => {}
        }
    }

    fn on_def(&mut self, instr: InstRef, def: &RefCell<SymbolRef>) {
        let def = def.borrow().clone();
        if def.is_local_var() {
            self.info.insert(def.clone(), DefUse {
                def: DefPos::Inst(self.blk.last().unwrap().clone(), instr),
                uses: vec![],
            });
        }
    }
}

impl Fn {
    /// Rebuild scope for SSA form function.
    pub fn rebuild_ssa_scope(&self) {
        self.assert_ssa();
        self.scope.clear();
        let mut sym: Vec<SymbolRef> = vec![];
        self.param.iter().for_each(|p| sym.push(p.borrow().clone()));
        self.dfs().for_each(|block| {
            block.inst.borrow().iter().for_each(|instr| {
                match instr.dst() {
                    Some(dst) if dst.borrow().is_local_var() => sym.push(dst.borrow().clone()),
                    _ => {}
                }
            })
        });
        self.scope.append(sym.into_iter());
    }
}

pub type DefUseMap = HashMap<SymbolRef, DefUse>;

impl Fn {
    /// Compute define-use information for symbols
    pub fn def_use(&self) -> DefUseMap {
        self.assert_ssa();
        let mut listener = DefUseBuilder {
            info: HashMap::new(),
            blk: vec![],
        };
        self.walk_dom(&mut listener);
        listener.info
    }

    /// Dead code elimination
    /// This is placed here, not in `pass` module, because SSA transformation need this procedure.
    pub fn elim_dead_code(&self) {
        // DCE should be performed on SSA form
        self.assert_ssa();

        // Compute define-use information
        let mut def_use = self.def_use();

        // Use work list algorithm to create target set
        let mut marked = HashSet::new();
        let mut work: WorkList<SymbolRef> = WorkList::from_iter(def_use.keys().cloned());

        while !work.is_empty() {
            // Search for instruction that can be removed
            let ref sym = work.pick().unwrap();
            let mut remove = vec![];
            match def_use.get(sym).unwrap().def.clone() {
                // Remove circular reference
                // A circular reference is a pair of symbols that for each symbol, its only use
                // point is the definition of the other symbol, and the only use point of that
                // symbol is the one of this symbol.
                DefPos::Inst(_, instr) if instr.is_phi() && def_use[sym].uses.len() == 1 => {
                    let phi_dst = sym;
                    let other_instr = def_use[phi_dst].uses[0].clone();
                    match other_instr.dst() {
                        Some(dst) if dst.borrow().is_local_var() => {
                            let ref other_dst = dst.borrow().clone();
                            if def_use[other_dst].uses.len() == 1
                                && def_use[other_dst].uses[0] == instr {
                                remove.push(instr.clone());
                                remove.push(other_instr.clone());
                            }
                        }
                        _ => {}
                    }
                }
                // For any other instruction, remove if it has no uses and it has no side effects.
                DefPos::Inst(_, instr) if def_use[sym].uses.is_empty()
                    && !instr.has_side_effect() => remove.push(instr),
                _ => {}
            }

            // Mark the instructions that can be removed
            remove.into_iter().for_each(|instr| {
                marked.insert(instr.clone());
                for opd in instr.src() {
                    match opd.borrow().deref() {
                        // Also remove this instruction from the use list of the symbols it
                        // uses.
                        Value::Var(opd) if opd.is_local_var() => {
                            def_use[opd].uses.iter().position(|elem| *elem == instr)
                                .map(|pos| {
                                    def_use.get_mut(opd).unwrap().uses.remove(pos);
                                    work.insert(opd.clone());
                                });
                        }
                        _ => {}
                    }
                }
            })
        }

        // Remove instruction if it is not marked before
        self.iter_dom().for_each(|block| {
            block.inst.borrow_mut().retain(|instr| {
                if marked.contains(instr) {
                    instr.dst().map(|dst| self.scope.remove(&dst.borrow().name()));
                    false
                } else { true }
            })
        })
    }
}

#[test]
fn test_ssa() {
    use crate::irc::lex::Lexer;
    use crate::irc::parse::Parser;
    use crate::irc::build::Builder;
    use crate::lang::print::Printer;
    use std::io::stdout;
    use std::fs::File;
    use std::convert::TryFrom;
    use std::io::Read;
    use std::borrow::BorrowMut;

    let mut file = File::open("test/ssa.ir").unwrap();
    let lexer = Lexer::try_from(&mut file as &mut dyn Read).unwrap();
    let parser = Parser::new(lexer);
    let tree = parser.parse().unwrap();
    let builder = Builder::new(tree);
    let pro = builder.build().unwrap();
    for func in &pro.func {
        func.to_ssa();
    }
    let mut out = stdout();
    let mut printer = Printer::new(out.borrow_mut());
    printer.print(&pro).unwrap();
}
