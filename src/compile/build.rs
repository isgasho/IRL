use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;
use std::str::FromStr;

use crate::compile::{CompileErr, Loc};
use crate::compile::syntax::{Term, Token};
use crate::lang::{ExtRc, Program};
use crate::lang::func::{BasicBlock, BlockRef, Func};
use crate::lang::instr::{BinOp, Instr, UnOp};
use crate::lang::val::{Const, GlobalVar, Scope, Symbol, SymbolRef, Type, Typed, Value};

pub struct Builder {
    root: Term,
}

struct Context {
    global: Rc<Scope>,
    func: Rc<Func>,
    labels: HashMap<String, BlockRef>,
    block: RefCell<BlockRef>,
}

impl Builder {
    pub fn new(root: Term) -> Builder { Builder { root } }

    /// Build program from passed syntax tree. Semantic analysis is also performed.
    pub fn build(self) -> Result<Program, CompileErr> {
        // Build top level scope
        let mut pro = Program {
            vars: vec![],
            funcs: vec![],
            global: Rc::new(Scope::new()),
        };
        let mut bodies: Vec<&Term> = Vec::new();
        if let Term::Program { def } = &self.root {
            for t in def {
                match t {
                    // Create global variable, possibly with initial value
                    Term::VarDef { loc, id, init, ty } => {
                        let var = Rc::new(self.build_global_var(id, ty, init)?);
                        pro.vars.push(var.clone());
                        let sym = ExtRc::new(Symbol::Global(var));
                        let added = pro.global.add(sym.clone());
                        if !added {
                            return Err(CompileErr {
                                loc: loc.clone(),
                                msg: format!("variable {} already in global scope", sym.name()),
                            });
                        }
                    }
                    // Create signature part for function, while its body are left empty for a
                    // later pass.
                    Term::FnDef { loc, sig, body } => {
                        let func = Rc::new(self.build_fn_sig(sig)?);
                        pro.funcs.push(func.clone());
                        let sym = ExtRc::new(Symbol::Func(func));
                        let added = pro.global.add(sym.clone());
                        if !added {
                            return Err(CompileErr {
                                loc: loc.clone(),
                                msg: format!("function {} already defined", sym.name()),
                            });
                        }
                        bodies.push(body.deref())
                    }
                    _ => unreachable!()
                }
            }
        } else { unreachable!() }

        // Build basic blocks in each function
        for (i, func) in pro.funcs.iter().enumerate() {
            let blocks = match bodies[i] {
                Term::FnBody { loc: _, bb } => bb,
                _ => unreachable!()
            };
            self.build_body(blocks, func.clone(), pro.global.clone())?;
        }

        Ok(pro)
    }

    fn build_global_var(&self, id: &Token, ty: &Term, init: &Option<Token>)
                        -> Result<GlobalVar, CompileErr>
    {
        let ty = self.create_type(ty)?;
        let init = match init {
            Some(c) => Some(self.create_const(c, &ty)?),
            None => None
        };
        let name = if let Token::GlobalId(_, s) = id { self.trim_tag(s) } else { unreachable!() };
        Ok(GlobalVar { name: name.to_string(), ty, init })
    }

    fn build_fn_sig(&self, term: &Term) -> Result<Func, CompileErr> {
        if let Term::FnSig { loc: _, id, param, ret } = term {
            // Extract function name
            let name = if let Token::GlobalId(_, s) = id {
                self.trim_tag(s) // trim global tag
            } else { unreachable!() };

            // Build parameter list, also add parameter to function scope
            let mut plist: Vec<SymbolRef> = Vec::new();
            let scope = Scope::new();
            if let Term::ParamList { loc: _, list } = param.as_ref() {
                for p in list {
                    if let Term::ParamDef { loc, id: Token::LocalId(_, s), ty } = p {
                        let sym = ExtRc::new(self.create_local(s, self.create_type(ty)?)?);
                        plist.push(sym.clone());
                        let added = scope.add(sym.clone());
                        if !added {
                            return Err(CompileErr {
                                loc: loc.clone(),
                                msg: format!("parameter {} already defined", sym.id()),
                            });
                        }
                    } else { unreachable!() }
                }
            } else { unreachable!() }

            // Build return type
            let ret = match ret {
                Some(r) => if let Term::FnRet { loc: _, ty } = r.deref() {
                    self.create_type(ty)?
                } else { unreachable!() }
                None => Type::Void,
            };

            // Return incomplete function object
            Ok(Func::new(name.to_string(), scope, plist, ret, BasicBlock::default()))
        } else { unreachable!() }
    }

    fn build_body(&self, terms: &Vec<Term>, func: Rc<Func>, global: Rc<Scope>)
                  -> Result<(), CompileErr>
    {
        // Build block labels
        let mut labels: HashMap<String, BlockRef> = HashMap::new();
        let mut blocks: Vec<(BlockRef, &Loc, &Vec<Term>)> = vec![];
        for i in 0..terms.len() {
            if let Term::BlockDef { loc, id, instr } = &terms[i] {
                let name = if let Token::Label(_, s) = id {
                    self.trim_tag(s).to_string()
                } else { unreachable!() };
                let block = ExtRc::new(BasicBlock::new(name.clone()));
                labels.insert(name, block.clone());
                blocks.push((block.clone(), loc, instr));
                if i == 0 { func.ent.replace(block); } // replace dummy entrance with real one
            } else { unreachable!() };
        }

        // Build instructions inside each block
        let ctx = Context {
            global,
            func: func.clone(),
            labels,
            block: RefCell::new(func.ent.borrow().clone()),
        };
        let mut asm_ssa = false; // whether this function is assumed to be in SSA form
        for (b, loc, terms) in blocks {
            // Build instructions
            for t in terms {
                ctx.block.replace(b.clone());
                let instr = self.build_instr(t, &ctx)?;
                if !asm_ssa { asm_ssa = self.assume_ssa(&instr) }
                b.push_back(instr);
            }
            // Check if the block is ended with control flow instructions
            if !b.is_complete() {
                return Err(CompileErr {
                    loc: loc.clone(),
                    msg: format!("block {} is not complete", b.name),
                });
            }
        }

        // Compute dominators of blocks
        func.build_dom();

        Ok(())
    }

    /// Make assumption about whether the instruction is in SSA form.
    /// Whether the function is really in SSA form remained to be verified.
    fn assume_ssa(&self, instr: &Instr) -> bool {
        // Criteria 1: Phi instruction
        if let Instr::Phi { src: _, dst: _ } = instr { return true; }
        // Criteria 2: Contain versioned symbol
        if let Some(sym) = instr.def() {
            if let Symbol::Local { name: _, ty: _, ver: Some(_) } = sym.borrow().as_ref() {
                return true;
            }
        }
        for u in instr.opd() {
            if let Value::Var(sym) = u.borrow().deref() {
                if let Symbol::Local { name: _, ty: _, ver: Some(_) } = sym.deref() {
                    return true;
                }
            }
        }
        false
    }

    fn build_instr(&self, term: &Term, ctx: &Context) -> Result<Instr, CompileErr> {
        match term {
            Term::AssignInstr { loc: _, id, rhs } => self.build_assign(id, rhs, ctx),
            Term::CtrlInstr { loc: _, instr } => self.build_ctrl(instr, ctx),
            _ => unreachable!()
        }
    }

    fn build_assign(&self, dst: &Token, rhs: &Term, ctx: &Context) -> Result<Instr, CompileErr> {
        if let Term::AssignRhs { loc: _, name: Token::Reserved(_, op), ty, opd } = rhs {
            // Create symbols for destination
            let ref ty = self.create_type(ty)?;

            // Deal with operands
            match opd.deref() {
                Term::OpdList { loc, list } =>
                    self.build_op(&ty, dst, op.as_str(), list, ctx, loc),
                Term::FnCall { loc, func: Token::GlobalId(_, func), arg } => {
                    let dst = self.build_symbol(dst, &ty, ctx)?;
                    self.build_fn_call(func, arg.deref(), Some(dst), ctx, loc)
                }
                Term::PhiList { loc: _, list } => {
                    let dst = self.build_symbol(dst, &ty, ctx)?;
                    self.build_phi_instr(list, ty, dst, ctx)
                }
                _ => unreachable!()
            }
        } else { unreachable!() }
    }

    fn build_op(&self, ty: &Type, dst: &Token, op: &str, opd: &Vec<Token>, ctx: &Context,
                loc: &Loc) -> Result<Instr, CompileErr>
    {
        match op {
            "mov" => {
                let dst = self.build_symbol(dst, ty, ctx)?;
                if opd.len() == 1 {
                    let src = self.build_opd_list(ty, opd, ctx)?[0].clone();
                    Ok(Instr::Mov {
                        src: RefCell::new(src),
                        dst: RefCell::new(dst),
                    })
                } else {
                    Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("expect 1 operand, got {}", opd.len()),
                    })
                }
            }
            op if UnOp::from_str(op).is_ok() => {
                let dst = self.build_symbol(dst, ty, ctx)?;
                let op = UnOp::from_str(op).unwrap();
                if opd.len() == 1 {
                    let opd = self.build_opd_list(ty, opd, ctx)?;
                    Ok(Instr::Un {
                        op,
                        opd: RefCell::new(opd[0].clone()),
                        dst: RefCell::new(dst),
                    })
                } else {
                    Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("expect 1 operand, got {}", opd.len()),
                    })
                }
            }
            op if BinOp::from_str(op).is_ok() => {
                let op = BinOp::from_str(op).unwrap();
                let dst = if op.is_cmp() { // compare result is always `i1`
                    self.build_symbol(dst, &Type::I1, ctx)?
                } else {
                    self.build_symbol(dst, ty, ctx)?
                };
                if opd.len() == 2 {
                    let opd = self.build_opd_list(ty, opd, ctx)?;
                    Ok(Instr::Bin {
                        op,
                        fst: RefCell::new(opd[0].clone()),
                        snd: RefCell::new(opd[1].clone()),
                        dst: RefCell::new(dst),
                    })
                } else {
                    Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("expect 2 operands, got {}", opd.len()),
                    })
                }
            }
            _ => Err(CompileErr {
                loc: loc.clone(),
                msg: format!("unknown operator {}", op),
            })
        }
    }

    fn build_fn_call(&self, func: &str, arg: &Term, dst: Option<SymbolRef>,
                     ctx: &Context, loc: &Loc) -> Result<Instr, CompileErr>
    {
        // Find function definition from context
        let fn_name = self.trim_tag(func);
        let fn_sym = ctx.global.find(fn_name).ok_or(
            CompileErr {
                loc: loc.clone(),
                msg: format!("function {} not found", fn_name),
            }
        )?;
        let func = if let Symbol::Func(func) = fn_sym.deref() { func } else {
            return Err(CompileErr {
                loc: loc.clone(),
                msg: format!("symbol {} is not a function", fn_sym.name()),
            });
        };

        // Check argument number
        let arg = if let Term::OpdList { loc: _, list } = arg.deref() {
            list
        } else { unreachable!() };
        if func.param.len() != arg.len() {
            return Err(CompileErr {
                loc: loc.clone(),
                msg: format!("expect {} arguments, got {}", func.param.len(), arg.len()),
            });
        }

        // Check argument type
        let mut arg_list = Vec::new();
        for (p, a) in func.param.iter().zip(arg.iter()) {
            let a = self.build_value(&p.get_type(), a, ctx)?;
            arg_list.push(RefCell::new(a))
        }

        // Check return type, if necessary
        let dst = match dst {
            Some(sym) => {
                let tgt_ty = sym.get_type();
                if tgt_ty != func.ret {
                    return Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("expect type {}, got {}", tgt_ty.to_string(),
                                     func.ret.to_string()),
                    });
                }
                Some(RefCell::new(sym))
            }
            None => None // Don't care its type, if returned value is not assigned.
        };

        // Build instruction
        Ok(Instr::Call { func: func.clone(), arg: arg_list, dst })
    }

    fn build_phi_instr(&self, list: &Vec<Term>, ty: &Type, dst: SymbolRef, ctx: &Context)
                       -> Result<Instr, CompileErr>
    {
        let mut pairs: Vec<(Option<BlockRef>, RefCell<Value>)> = Vec::new();
        for t in list {
            if let Term::PhiOpd { loc, bb, opd } = t {
                let val = self.build_value(ty, opd, ctx)?;
                let block = match bb {
                    Some(Token::Label(loc, s)) => {
                        let s = self.trim_tag(s);
                        Some(ctx.labels.get(self.trim_tag(s)).cloned().ok_or(
                            CompileErr {
                                loc: loc.clone(),
                                msg: format!("label {} not found", s),
                            }
                        )?)
                    }
                    None => match &val { // ensure this operand is from parameter
                        Value::Var(sym) => match sym.deref() {
                            Symbol::Local { name: _, ty: _, ver: _ } =>
                                if ctx.func.param.iter().find(|s| *s == sym).is_some() {
                                    None
                                } else {
                                    return Err(CompileErr {
                                        loc: loc.clone(),
                                        msg: format!("operand {} is not in parameter list",
                                                     sym.name()),
                                    });
                                }
                            _ => unreachable!()
                        }
                        Value::Const(_) => return Err(CompileErr {
                            loc: loc.clone(),
                            msg: "parameter is not constant".to_string(),
                        })
                    },
                    _ => { unreachable!() }
                };
                pairs.push((block, RefCell::new(val)));
            } else { unreachable!() }
        }
        Ok(Instr::Phi { src: pairs, dst: RefCell::new(dst) })
    }

    fn build_opd_list(&self, ty: &Type, opd: &Vec<Token>, ctx: &Context)
                      -> Result<Vec<Value>, CompileErr>
    {
        let mut list = Vec::new();
        for v in opd {
            list.push(self.build_value(ty, v, ctx)?);
        }
        Ok(list)
    }

    fn build_value(&self, ty: &Type, tok: &Token, ctx: &Context) -> Result<Value, CompileErr> {
        match tok {
            Token::GlobalId(_, _) | Token::LocalId(_, _) =>
                Ok(Value::Var(self.build_symbol(tok, ty, ctx)?)),
            Token::Integer(_, _) => Ok(Value::Const(self.create_const(tok, ty)?)),
            _ => unreachable!()
        }
    }

    fn build_ctrl(&self, term: &Term, ctx: &Context) -> Result<Instr, CompileErr> {
        match term {
            Term::RetInstr { loc, opd } => {
                match &ctx.func.ret {
                    Type::Void => if opd.is_none() {
                        Ok(Instr::Ret { val: None })
                    } else {
                        Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!("expect void, got value"),
                        })
                    }
                    ty => if opd.is_some() {
                        let ret = self.build_value(ty, opd.as_ref().unwrap(), ctx)?;
                        ctx.func.exit.borrow_mut().insert(ctx.block.borrow().clone());
                        Ok(Instr::Ret { val: Some(RefCell::new(ret)) })
                    } else {
                        Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!("expect value, got void"),
                        })
                    }
                }
            }
            Term::FnCall { loc, func: Token::GlobalId(_, func), arg } =>
                self.build_fn_call(func, arg.deref(), None, ctx, loc),
            Term::JmpInstr { loc: _, tgt: Token::Label(loc, tgt) } => {
                let tgt = self.trim_tag(tgt);
                match ctx.labels.get(tgt) {
                    Some(tgt) => {
                        ctx.block.borrow().connect(tgt.clone());
                        Ok(Instr::Jmp { tgt: RefCell::new(tgt.clone()) })
                    }
                    None => Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("label {} not found", tgt),
                    })
                }
            }
            Term::BrInstr {
                loc: _, cond, tr: Token::Label(t_loc, t_lab),
                fls: Token::Label(f_loc, f_lab)
            } => {
                let cond = self.build_value(&Type::I1, cond, ctx)?;
                let t_lab = self.trim_tag(t_lab);
                let tr = ctx.labels.get(t_lab).ok_or(
                    CompileErr {
                        loc: t_loc.clone(),
                        msg: format!("label {} not found", t_lab),
                    }
                )?;
                let f_lab = self.trim_tag(f_lab);
                let fls = ctx.labels.get(f_lab).ok_or(
                    CompileErr {
                        loc: f_loc.clone(),
                        msg: format!("label {} not found", f_lab),
                    }
                )?;
                ctx.block.borrow().connect(tr.clone());
                ctx.block.borrow().connect(fls.clone());
                Ok(Instr::Br {
                    cond: RefCell::new(cond),
                    tr: RefCell::new(tr.clone()),
                    fls: RefCell::new(fls.clone()),
                })
            }
            _ => unreachable!()
        }
    }

    fn build_symbol(&self, tok: &Token, ty: &Type, ctx: &Context)
                    -> Result<SymbolRef, CompileErr>
    {
        match tok {
            Token::GlobalId(l, s) => match ctx.global.find(self.trim_tag(s)) {
                Some(sym) => {
                    self.check_type(sym.deref(), ty, l)?;
                    Ok(sym)
                }
                None => Err(CompileErr {
                    loc: l.clone(),
                    msg: format!("identifier {} not found in global scope", s),
                })
            }
            Token::LocalId(l, s) => match ctx.func.scope.find(self.trim_tag(s)) {
                Some(sym) => {
                    self.check_type(sym.deref(), ty, l)?;
                    Ok(sym)
                }
                None => {
                    let sym = ExtRc::new(self.create_local(s, ty.clone())?);
                    let _ = ctx.func.scope.add(sym.clone());
                    Ok(sym)
                }
            }
            _ => unreachable!()
        }
    }

    fn check_type(&self, sym: &Symbol, ty: &Type, loc: &Loc) -> Result<(), CompileErr> {
        let sym_ty = sym.get_type();
        if ty != &sym_ty {
            Err(CompileErr {
                loc: loc.clone(),
                msg: format!("expect symbol of type {}, found {}", ty.to_string(),
                             sym_ty.to_string()),
            })
        } else { Ok(()) }
    }

    fn create_const(&self, tok: &Token, ty: &Type) -> Result<Const, CompileErr> {
        if let Token::Integer(l, i) = tok {
            match ty {
                Type::I1 => match i.as_str() {
                    "0" => Ok(Const::I1(false)),
                    "1" => Ok(Const::I1(true)),
                    _ => Err(CompileErr {
                        loc: l.clone(),
                        msg: format!("cannot create constant {} of type i1", i),
                    })
                }
                Type::I64 => Ok(Const::I64(i.parse().unwrap())),
                _ => unreachable!()
            }
        } else { unreachable!() }
    }

    fn create_local(&self, s: &str, ty: Type) -> Result<Symbol, CompileErr> {
        let mut name = self.trim_tag(s); // trim local tag
        let ver = match name.find(".") {
            Some(_) => {
                let split: Vec<&str> = name.split('.').collect();
                name = split[0];
                Some(usize::from_str(split[1]).unwrap())
            }
            None => None
        };
        Ok(Symbol::Local { name: name.to_string(), ty, ver })
    }

    fn create_type(&self, term: &Term) -> Result<Type, CompileErr> {
        if let Term::TypeDecl { loc, ty: Token::Reserved(_, s) } = term {
            Type::from_str(s).map_err(|e| CompileErr { loc: loc.clone(), msg: e })
        } else { unreachable!() }
    }

    fn trim_tag<'a>(&self, s: &'a str) -> &'a str {
        match s.split_at(1).0 {
            "@" | "$" | "%" => s.split_at(1).1,
            _ => s
        }
    }
}

#[test]
fn test_build() {
    use crate::compile::lex::Lexer;
    use crate::compile::parse::Parser;
    use std::fs::File;
    use std::convert::TryFrom;
    use std::io::Read;

    let mut file = File::open("test/example.ir").unwrap();
    let lexer = Lexer::try_from(&mut file as &mut dyn Read).unwrap();
    let parser = Parser::new(lexer);
    let tree = parser.parse().unwrap();
    let builder = Builder::new(tree);
    println!("{:?}", builder.build());
}
