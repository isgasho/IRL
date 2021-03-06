use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;
use std::str::FromStr;

use crate::irc::{CompileErr, Loc};
use crate::irc::syntax::{Term, Token};
use crate::lang::func::{BasicBlock, BlockRef, Fn, FnAttrib, FnRef};
use crate::lang::inst::{BinOp, Inst, PhiSrc, UnOp};
use crate::lang::Program;
use crate::lang::ssa::Verifier;
use crate::lang::util::ExtRc;
use crate::lang::value::{Const, GlobalVar, Scope, Symbol, SymbolRef, Type, Typed, Value};

pub struct Builder {
    root: Term,
}

struct Context {
    global: Rc<Scope>,
    func: FnRef,
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
            func: vec![],
            global: Rc::new(Scope::new()),
        };
        let bodies = self.build_top_level(&mut pro)?;

        // Build basic blocks in each function
        for (i, func) in pro.func.iter().enumerate() {
            let blocks = match bodies[i] {
                Term::FnBody { loc: _, bb } => bb,
                _ => unreachable!()
            };
            self.build_body(blocks, func.clone(), pro.global.clone())?;
        }

        Ok(pro)
    }

    fn build_top_level(&self, pro: &mut Program) -> Result<Vec<&Term>, CompileErr> {
        // Add type aliases to global scope
        let def = if let Term::Program { def } = &self.root { def } else { unreachable!() };
        for t in def {
            if let Term::AliasDef { loc, id: Token::GlobalId(_, id), ty: _ } = t {
                let name = self.trim_tag(id);
                let added = pro.global.insert(ExtRc::new(
                    Symbol::Type {
                        name: name.to_string(),
                        ty: RefCell::new(Type::Void),
                    }
                ));
                if !added {
                    return Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("type {} already defined", name),
                    });
                }
            }
        }

        // Build global variables and function signatures
        let mut bodies: Vec<&Term> = Vec::new();
        for t in def {
            match t {
                // Replace type alias symbol with real type
                Term::AliasDef { loc: _, id: Token::GlobalId(_, id), ty: term } => {
                    let name = self.trim_tag(id);
                    match pro.global.find(name).unwrap().deref() {
                        Symbol::Type { name: _, ty } => {
                            ty.replace(self.create_type(term.deref(), &pro.global)?);
                        }
                        _ => unreachable!()
                    }
                }
                // Create global variable, possibly with initial value
                Term::VarDef { loc, id, init, ty } => {
                    let var = ExtRc::new(self.build_global_var(id, ty, init, &pro.global)?);
                    pro.vars.push(var.clone());
                    let sym = ExtRc::new(Symbol::Global(var));
                    let added = pro.global.insert(sym.clone());
                    if !added {
                        return Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!("variable {} already defined", sym.name()),
                        });
                    }
                }
                // Create signature part for function, while its body are left empty for a later
                // pass.
                Term::FnDef { loc, attrib, sig, body } => {
                    let func = ExtRc::new(self.build_fn_sig(sig, attrib.as_ref(), &pro.global)?);
                    pro.func.push(func.clone());
                    let sym = ExtRc::new(Symbol::Func(func));
                    let added = pro.global.insert(sym.clone());
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
        Ok(bodies)
    }

    fn build_global_var(&self, id: &Token, ty: &Term, init: &Option<Token>, global: &Rc<Scope>)
                        -> Result<GlobalVar, CompileErr>
    {
        let ty = self.create_type(ty, global)?;
        if !ty.is_reg() {
            Err(CompileErr {
                loc: id.loc(),
                msg: format!("cannot create global variable of type {}", ty.to_string()),
            })?
        }
        let init = match init {
            Some(c) => Some(self.create_const(c, &ty)?),
            None => None
        };
        let name = if let Token::GlobalId(_, s) = id {
            self.trim_tag(s)
        } else { unreachable!() };
        Ok(GlobalVar { name: name.to_string(), ty, init })
    }

    fn build_fn_sig(&self, sig: &Term, attrib: Option<&Box<Term>>, global: &Rc<Scope>)
                    -> Result<Fn, CompileErr>
    {
        if let Term::FnSig { loc, id, param, ret } = sig {
            // Build function attributes
            let attrib = match attrib {
                Some(term) => if let Term::FnAttribList { loc: _, list } = term.as_ref() {
                    let mut attrib = vec![];
                    for a in list {
                        if let Token::Reserved(l, s) = a {
                            let a = FnAttrib::from_str(s.as_str()).map_err(|()| {
                                CompileErr {
                                    loc: l.clone(),
                                    msg: format!("invalid function attribute"),
                                }
                            })?;
                            if attrib.contains(&a) {
                                Err(CompileErr {
                                    loc: l.clone(),
                                    msg: format!("duplicated attribute {}", a.to_string()),
                                })?
                            }
                            attrib.push(a);
                        } else {
                            unreachable!()
                        };
                    }
                    attrib
                } else { unreachable!() }
                None => vec![]
            };

            // Extract function name
            let name = if let Token::GlobalId(_, s) = id {
                self.trim_tag(s) // trim global tag
            } else { unreachable!() };

            // Build parameter list, also add parameter to function scope
            let mut plist: Vec<RefCell<SymbolRef>> = Vec::new();
            let scope = Scope::new();
            if let Term::ParamList { loc: _, list } = param.as_ref() {
                // Process parameters
                for p in list {
                    if let Term::ParamDef { loc, id: Token::LocalId(_, s), ty } = p {
                        let sym = ExtRc::new(
                            self.create_local(s, self.create_type(ty, global)?)?
                        );
                        plist.push(RefCell::new(sym.clone()));
                        let added = scope.insert(sym.clone());
                        if !added {
                            return Err(CompileErr {
                                loc: loc.clone(),
                                msg: format!("parameter {} already defined", sym.name()),
                            });
                        }
                    } else { unreachable!() }
                }
            } else { unreachable!() }

            // Build return type
            let ret = match ret {
                Some(r) => if let Term::FnRet { loc: _, ty } = r.deref() {
                    self.create_type(ty, global)?
                } else { unreachable!() }
                None => Type::Void,
            };

            // Check special function
            self.check_special_fn(name, &plist, &ret, loc)?;

            // Return incomplete function object
            Ok(Fn::new(
                name.to_string(),
                scope,
                attrib,
                plist,
                ret,
                BasicBlock::default())
            )
        } else { unreachable!() }
    }

    fn check_special_fn(&self, name: &str, param: &Vec<RefCell<SymbolRef>>, ret: &Type, loc: &Loc)
                        -> Result<(), CompileErr>
    {
        match name {
            "main" => {
                if !param.is_empty() {
                    return Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("expect 0 parameter, got {}", param.len()),
                    });
                }
                if *ret != Type::Void {
                    return Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("expect void return type, got {}", ret.to_string()),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn build_body(&self, terms: &Vec<Term>, func: FnRef, global: Rc<Scope>)
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
        let mut may_ssa = func.has_attrib(FnAttrib::Ssa);
        for (b, loc, terms) in blocks {
            let mut in_phis = true;
            for t in terms {
                // Build instruction
                ctx.block.replace(b.clone());
                let instr = ExtRc::new(self.build_instr(t, &ctx)?);

                // Check SSA assumption
                if !may_ssa { may_ssa = self.assume_ssa(&instr) }

                // Check location of phi instruction
                match instr.as_ref() {
                    Inst::Phi { src: _, dst: _ } => if !in_phis {
                        return Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!(
                                "non-phi instruction found before phi's in block {}", b.name
                            ),
                        });
                    }
                    _ => in_phis = false
                };
                b.push_back(instr);
            }
            // Check if the block is ended with control flow instruction
            if !b.is_complete() {
                return Err(CompileErr {
                    loc: loc.clone(),
                    msg: format!("block {} is not complete", b.name),
                });
            }
        }

        // Build dominator tree of blocks
        func.build_dom();

        // Verify SSA property if it is assumed to be that
        if may_ssa {
            let mut ver = Verifier::new();
            func.walk_dom(&mut ver);
            if !ver.err.is_empty() {
                Err(CompileErr {
                    loc: Loc { line: 0, col: 0 },
                    msg: ver.err.first().unwrap().clone(),
                })?
            }
        }

        Ok(())
    }

    /// Make assumption about whether the instruction is in SSA form.
    /// Whether the function is really in SSA form remained to be verified.
    fn assume_ssa(&self, instr: &Inst) -> bool {
        // Criteria 1: Phi instruction
        if let Inst::Phi { src: _, dst: _ } = instr { return true; }
        false
    }

    fn build_instr(&self, term: &Term, ctx: &Context) -> Result<Inst, CompileErr> {
        match term {
            Term::AssignInstr { loc: _, id, rhs } => self.build_assign(id, rhs, ctx),
            Term::NonAssignInstr { loc: _, instr } => self.build_non_assign(instr, ctx),
            _ => unreachable!()
        }
    }

    fn build_assign(&self, dst: &Token, rhs: &Term, ctx: &Context) -> Result<Inst, CompileErr> {
        let dst_loc = dst.loc();
        match rhs {
            Term::CommonRhs { loc, name: Token::Reserved(_, op), ty, opd } => {
                let ty = self.create_type(ty, &ctx.global)?;
                self.build_op(dst, &ty, op, opd, ctx, loc)
            }
            Term::CallRhs { loc: _, ty, call } => {
                let ty = self.create_type(ty, &ctx.global)?;
                let dst = self.create_symbol(dst, &ty, ctx)?;
                self.build_fn_call(call, Some(dst), ctx)
            }
            Term::PhiRhs { loc, ty, list } => {
                let ty = self.create_type(ty, &ctx.global)?;
                let dst = self.create_symbol(dst, &ty, ctx)?;
                if let Term::PhiList { loc: _, list } = list.deref() {
                    if !dst.is_local_var() {
                        return Err(CompileErr {
                            loc: dst_loc.clone(),
                            msg: format!("destination {} is not local variable", dst.name()),
                        });
                    }
                    self.build_phi_instr(&ty, dst, list, ctx, loc)
                } else { unreachable!() }
            }
            Term::PtrRhs { loc, ty, opd, idx } => {
                let ty = self.create_type(ty, &ctx.global)?;
                let dst = self.create_symbol(dst, &ty, ctx)?;
                self.build_ptr(dst, opd.deref(), idx.as_ref().map(|idx| idx.deref().clone()),
                               ctx, loc)
            }
            Term::AllocRhs { loc: _, ty } => {
                let ty = self.create_type(ty, &ctx.global)?;
                let dst = self.create_symbol(dst, &Type::Ptr(Box::new(ty)), ctx)?;
                Ok(Inst::Alloc { dst: RefCell::new(dst) })
            }
            Term::NewRhs { loc: _, ty, len } => {
                let ty = self.create_type(ty, &ctx.global)?;
                let dst = self.create_symbol(dst, &Type::Ptr(Box::new(ty)), ctx)?;
                let len = match len.as_ref() {
                    Some(len) => Some(RefCell::new(Value::Var(self.find_symbol(len, ctx)?))),
                    None => None
                };
                Ok(Inst::New { dst: RefCell::new(dst), len })
            }
            _ => unreachable!()
        }
    }

    fn build_ptr(&self, dst: SymbolRef, opd: &Term, idx: Option<Term>, ctx: &Context, loc: &Loc)
        -> Result<Inst, CompileErr>
    {
        // Check operands
        let base: SymbolRef;
        let off: Option<Value>;
        if let Term::OpdList { loc, list } = opd {
            match list.len() {
                1 => {
                    base = self.find_symbol(list.get(0).unwrap(), ctx)?;
                    off = None;
                }
                2 => {
                    base = self.find_symbol(list.get(0).unwrap(), ctx)?;
                    off = Some(self.create_def_val(&Type::I(64), list.get(1).unwrap(), ctx)?);
                }
                n => return Err(CompileErr {
                    loc: loc.clone(),
                    msg: format!("expect 1 or 2 operands, got {}", n),
                })
            }
        } else { unreachable!() }

        // Check indices
        let mut elem_ty = match base.get_type().orig() {
            Type::Ptr(tgt) => tgt.deref().clone(),
            ty => return Err(CompileErr {
                loc: loc.clone(),
                msg: format!("expect pointer type, got {}", ty.to_string()),
            })
        };
        let idx = match idx {
            Some(Term::IndexList { loc: _, list }) => match list.deref() {
                Term::OpdList { loc: _, list } => {
                    let mut idx = vec![];
                    for tok in list {
                        let val = self.create_def_val(&Type::I(64), tok, ctx)?;
                        elem_ty = self.elem_idx(&elem_ty, &val, tok)?;
                        idx.push(val);
                    }
                    idx
                }
                _ => unreachable!()
            }
            Some(_) => unreachable!(),
            None => vec![]
        };

        // Check destination type
        let dst_ty = dst.get_type();
        let elem_ptr_ty = Type::Ptr(Box::new(elem_ty.clone()));
        if dst_ty != elem_ptr_ty {
            return Err(CompileErr {
                loc: loc.clone(),
                msg: format!("expect type {}, got {}", elem_ptr_ty.to_string(),
                             dst_ty.to_string()),
            });
        }

        Ok(Inst::Ptr {
            base: RefCell::new(Value::Var(base)),
            off: off.map(|off| RefCell::new(off)),
            ind: idx.into_iter().map(|i| RefCell::new(i)).collect(),
            dst: RefCell::new(dst),
        })
    }

    fn elem_idx(&self, ag_ty: &Type, val: &Value, tok: &Token) -> Result<Type, CompileErr> {
        match ag_ty.orig() {
            Type::Array { elem, len } => {
                if let Value::Const(Const::I64(c)) = val {
                    if *c as usize >= len {
                        return Err(CompileErr {
                            loc: tok.loc(),
                            msg: format!("index {} out of range {}", c, len),
                        });
                    }
                }
                Ok(elem.deref().clone())
            }
            Type::Struct { field } => {
                if let Value::Const(Const::I64(c)) = val {
                    if *c as usize >= field.len() {
                        return Err(CompileErr {
                            loc: tok.loc(),
                            msg: format!("index {} out of range {}", c, field.len()),
                        });
                    }
                    Ok(field.get(*c as usize).unwrap().clone())
                } else {
                    return Err(CompileErr {
                        loc: tok.loc(),
                        msg: format!("index into structure type is not constant"),
                    });
                }
            }
            ty => Err(CompileErr {
                loc: tok.loc(),
                msg: format!("type {} is not aggregate", ty.to_string()),
            })
        }
    }

    fn build_op(&self, dst: &Token, ty: &Type, op: &str, opd: &Term, ctx: &Context, loc: &Loc)
        -> Result<Inst, CompileErr>
    {
        match op {
            "mov" => {
                if !ty.is_reg() {
                    Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("cannot move value of type {}", ty.to_string()),
                    })?
                }
                let dst = self.create_symbol(dst, ty, ctx)?;
                let src = self.build_opd_list(vec![ty.clone()], opd, ctx)?[0].clone();
                Ok(Inst::Mov { src: RefCell::new(src), dst: RefCell::new(dst) })
            }
            "ld" => {
                if !ty.is_reg() {
                    Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("cannot load value of type {}", ty.to_string()),
                    })?
                }
                let dst = self.create_symbol(dst, ty, ctx)?;
                let src = self.build_opd_list(vec![Type::Ptr(Box::new(ty.clone()))], opd, ctx)?;
                Ok(Inst::Ld {
                    ptr: RefCell::new(src[0].clone()),
                    dst: RefCell::new(dst),
                })
            }
            op if UnOp::from_str(op).is_ok() => {
                let dst = self.create_symbol(dst, ty, ctx)?;
                let op = UnOp::from_str(op).unwrap();
                if !op.is_avail_for(ty) {
                    return Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("unary operation {} not supported for type {}",
                                     op.to_string(), ty.to_string()),
                    });
                }
                let opd = self.build_opd_list(vec![ty.clone()], opd, ctx)?;
                Ok(Inst::Un {
                    op,
                    opd: RefCell::new(opd[0].clone()),
                    dst: RefCell::new(dst),
                })
            }
            op if BinOp::from_str(op).is_ok() => {
                let op = BinOp::from_str(op).unwrap();
                if !op.is_avail_for(ty) {
                    return Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("binary operation {} not supported for type {}",
                                     op.to_string(), ty.to_string()),
                    });
                }
                let dst = if op.is_cmp() { // compare result is always `i1`
                    self.create_symbol(dst, &Type::I(1), ctx)?
                } else {
                    self.create_symbol(dst, ty, ctx)?
                };
                let opd = self.build_opd_list(vec![ty.clone(), ty.clone()], opd, ctx)?;
                Ok(Inst::Bin {
                    op,
                    fst: RefCell::new(opd[0].clone()),
                    snd: RefCell::new(opd[1].clone()),
                    dst: RefCell::new(dst),
                })
            }
            _ => Err(CompileErr {
                loc: loc.clone(),
                msg: format!("unknown operator {}", op),
            })
        }
    }

    fn build_fn_call(&self, call: &Term, dst: Option<SymbolRef>, ctx: &Context)
        -> Result<Inst, CompileErr>
    {
        if let Term::FnCall { loc, func: Token::GlobalId(_, id), arg } = call {
            // Find function definition from context
            let fn_name = self.trim_tag(id);
            if fn_name == "main" {
                Err(CompileErr {
                    loc: loc.clone(),
                    msg: format!("cannot call function @main"),
                })?
            }
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

            // Check argument type
            let param_ty = func.param.iter().map(|p| p.borrow().get_type()).collect();
            let arg = self.build_opd_list(param_ty, arg, ctx)?
                .into_iter().map(|a| RefCell::new(a)).collect();

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
            Ok(Inst::Call { func: func.clone(), arg, dst })
        } else { unreachable!() }
    }

    fn build_phi_instr(&self, ty: &Type, dst: SymbolRef, list: &Vec<Term>, ctx: &Context,
                       loc: &Loc) -> Result<Inst, CompileErr>
    {
        // Make sure destination is local variable
        if dst.is_global_var() {
            return Err(CompileErr {
                loc: loc.clone(),
                msg: format!(
                    "global variable cannot be used as destination of phi instruction",
                ),
            });
        }

        // Build phi source operands
        let mut pairs: Vec<PhiSrc> = Vec::new();
        for t in list {
            if let Term::PhiOpd { loc: _, lab, opd } = t {
                // Create operand value
                // the operand may not be defined when reading this instruction
                let val = self.create_value(ty, opd, ctx)?;
                if val.is_global_var() {
                    return Err(CompileErr {
                        loc: opd.loc(),
                        msg: format!(
                            "global variable cannot be used as source of phi instruction"
                        ),
                    });
                }

                // Create predecessor block
                let block = if let Token::Label(loc, s) = lab {
                    let s = self.trim_tag(s);
                    ctx.labels.get(self.trim_tag(s)).cloned().ok_or(
                        CompileErr {
                            loc: loc.clone(),
                            msg: format!("label {} not found", s),
                        }
                    )?
                } else { unreachable!() };
                pairs.push((RefCell::new(block), RefCell::new(val)));
            } else { unreachable!() }
        }
        pairs.sort_by_cached_key(|(blk, _)| blk.borrow().name.clone());
        Ok(Inst::Phi { src: pairs, dst: RefCell::new(dst) })
    }

    fn build_opd_list(&self, ty: Vec<Type>, opd: &Term, ctx: &Context)
                      -> Result<Vec<Value>, CompileErr>
    {
        if let Term::OpdList { loc, list } = opd {
            let mut v = Vec::new();
            if ty.len() != list.len() {
                return Err(CompileErr {
                    loc: loc.clone(),
                    msg: format!("expect {} operand(s), got {}", ty.len(), list.len()),
                });
            }
            for (ty, opd) in ty.iter().zip(list.iter()) {
                v.push(self.create_def_val(ty, opd, ctx)?);
            }
            Ok(v)
        } else { unreachable!() }
    }

    /// Create value from token.
    fn create_value(&self, ty: &Type, tok: &Token, ctx: &Context) -> Result<Value, CompileErr> {
        match tok {
            Token::GlobalId(_, _) | Token::LocalId(_, _) =>
                Ok(Value::Var(self.create_symbol(tok, ty, ctx)?)),
            Token::Integer(_, _) => Ok(Value::Const(self.create_const(tok, ty)?)),
            _ => unreachable!()
        }
    }

    /// Create defined value (defined variables and constants) from token.
    fn create_def_val(&self, ty: &Type, tok: &Token, ctx: &Context) -> Result<Value, CompileErr> {
        match tok {
            Token::GlobalId(loc, _) | Token::LocalId(loc, _) => {
                let sym = self.find_symbol(tok, ctx)?;
                self.check_type(&sym, ty, loc)?;
                Ok(Value::Var(sym))
            }
            Token::Integer(_, _) => Ok(Value::Const(self.create_const(tok, ty)?)),
            _ => unreachable!()
        }
    }

    fn build_non_assign(&self, term: &Term, ctx: &Context) -> Result<Inst, CompileErr> {
        match term {
            Term::RetInstr { loc, opd } => {
                ctx.func.exit.borrow_mut().push(ctx.block.borrow().clone());
                match &ctx.func.ret {
                    Type::Void => if opd.is_none() {
                        Ok(Inst::Ret { val: None })
                    } else {
                        Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!("expect void, got value"),
                        })
                    }
                    ty => if opd.is_some() {
                        let ret = self.create_def_val(ty, opd.as_ref().unwrap(), ctx)?;
                        Ok(Inst::Ret { val: Some(RefCell::new(ret)) })
                    } else {
                        Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!("expect value, got void"),
                        })
                    }
                }
            }
            Term::NoRetCall { loc: _, call } => self.build_fn_call(call, None, ctx),
            Term::JmpInstr { loc: _, tgt: Token::Label(loc, tgt) } => {
                let tgt = self.trim_tag(tgt);
                match ctx.labels.get(tgt) {
                    Some(tgt) => {
                        ctx.block.borrow().connect(tgt.clone());
                        if tgt == ctx.func.ent.borrow().deref() {
                            Err(CompileErr {
                                loc: loc.clone(),
                                msg: format!("cannot jump to function entry {:?}", tgt.name),
                            })?
                        }
                        Ok(Inst::Jmp { tgt: RefCell::new(tgt.clone()) })
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
                let cond = self.create_def_val(&Type::I(1), cond, ctx)?;
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
                Ok(Inst::Br {
                    cond: RefCell::new(cond),
                    tr: RefCell::new(tr.clone()),
                    fls: RefCell::new(fls.clone()),
                })
            }
            Term::StInstr { loc, ty, src, dst } => {
                let ty = self.create_type(ty.deref(), &ctx.global)?;
                if !ty.is_reg() {
                    Err(CompileErr {
                        loc: loc.clone(),
                        msg: format!("cannot store value of type {}", ty.to_string()),
                    })?
                }
                let src = self.create_def_val(&ty, src, ctx)?;
                let dst = self.create_value(&Type::Ptr(Box::new(ty.clone())), dst, ctx)?;
                Ok(Inst::St {
                    src: RefCell::new(src),
                    ptr: RefCell::new(dst),
                })
            }
            _ => unreachable!()
        }
    }

    /// This method use token `tok` to decide where to find symbol. If the symbol can be found,
    /// it checks whether it is of type `ty`. Otherwise, it create a new symbol in local scope of
    /// type `ty`.
    fn create_symbol(&self, tok: &Token, ty: &Type, ctx: &Context)
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
                    let _ = ctx.func.scope.insert(sym.clone());
                    Ok(sym)
                }
            }
            _ => unreachable!()
        }
    }

    /// This method find symbol with given token `tok`. If the symbol is not found, it returns an
    /// error.
    fn find_symbol(&self, tok: &Token, ctx: &Context) -> Result<SymbolRef, CompileErr> {
        match tok {
            Token::GlobalId(l, s) => ctx.global.find(self.trim_tag(s)).ok_or(
                CompileErr {
                    loc: l.clone(),
                    msg: format!("identifier {} not found in global scope", s),
                }
            ),
            Token::LocalId(l, s) => ctx.func.scope.find(self.trim_tag(s)).ok_or(
                CompileErr {
                    loc: l.clone(),
                    msg: format!("identifier {} not found in local scope", s),
                }
            ),
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
            Const::from_str(i, ty).ok_or_else(|| CompileErr {
                loc: l.clone(),
                msg: format!("cannot create constant {} of type {}", i, ty.to_string()),
            })
        } else { unreachable!() }
    }

    fn create_local(&self, s: &str, ty: Type) -> Result<Symbol, CompileErr> {
        let name = self.trim_tag(s); // trim local tag
        Ok(Symbol::Local { name: name.to_string(), ty })
    }

    fn create_type(&self, term: &Term, global: &Rc<Scope>) -> Result<Type, CompileErr> {
        if let Term::TypeDecl { loc: _, ty } = term {
            match ty.deref() {
                Term::PrimType { loc, ty: Token::Reserved(_, s) } =>
                    Type::from_str(s).map_err(|e| CompileErr { loc: loc.clone(), msg: e }),
                Term::AliasName { loc, id: Token::GlobalId(_, id) } => {
                    let name = self.trim_tag(id);
                    match global.find(name) {
                        Some(sym) => match sym.deref() {
                            Symbol::Type { name: _, ty: _ } => Ok(Type::Alias(sym.clone())),
                            _ => return Err(CompileErr {
                                loc: loc.clone(),
                                msg: format!("{} is not a type", name),
                            })
                        },
                        None => return Err(CompileErr {
                            loc: loc.clone(),
                            msg: format!("type {} not found", name),
                        })
                    }
                }
                Term::PtrType { loc: _, tgt } => Ok(Type::Ptr(Box::new(
                    self.create_type(tgt, global)?
                ))),
                Term::ArrayType { loc: _, len: Token::Integer(_, len), elem } =>
                    Ok(Type::Array {
                        elem: Box::new(self.create_type(elem.deref(), global)?),
                        len: usize::from_str(len).unwrap(),
                    }),
                Term::StructType { loc: _, field } => {
                    let mut v = vec![];
                    if let Term::TypeList { loc: _, list } = field.deref() {
                        for t in list.deref() {
                            v.push(self.create_type(t, global)?)
                        }
                    } else { unreachable!() }
                    Ok(Type::Struct { field: v })
                }
                _ => unreachable!()
            }
        } else { unreachable!() }
    }

    fn trim_tag<'a>(&self, s: &'a str) -> &'a str {
        match s.split_at(1).0 {
            "@" | "$" | "%" => s.split_at(1).1,
            _ => s
        }
    }
}
