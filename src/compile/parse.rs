use std::collections::VecDeque;

use crate::compile::{CompileErr, Loc};
use crate::compile::lex::Lexer;
use crate::compile::syntax::{Term, Token};

pub struct Parser {
    lexer: Lexer,
    buf: VecDeque<Token>,
    loc: Loc,
}

type ParseResult = Result<Term, CompileErr>;

macro_rules! check_op {
    ($parser:ident, $tok:ident, $tgt: expr) => {
        if $tok.to_string().as_str() != $tgt {
            return $parser.err(vec![$tgt], $tok)
        }
    };
}

impl Parser {
    /// Construct parser from lexer object
    pub fn new(lexer: Lexer) -> Parser {
        Parser {
            lexer,
            buf: VecDeque::new(),
            loc: Loc { line: 0, col: 0 },
        }
    }

    /// Parse the source file from token stream.
    /// `Ok(t)` if the source is successfully parsed, or `Err(e)` if some syntax error is found.
    pub fn parse(mut self) -> Result<Term, CompileErr> {
        let mut def = Vec::new();
        loop {
            let term = match self.peek(0)? {
                Token::GlobalId(_, _) => self.var_def()?,
                Token::Reserved(_, k) if &k == "fn" => self.fn_def()?,
                Token::Eof(_) => break,
                tok => self.err(vec!["{GlobalId}", "{Reserved}", "Eof"], tok)?
            };
            def.push(term);
        }
        Ok(Term::Program { def })
    }

    fn var_def(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let id = self.consume()?; // GlobalId
        if let Token::GlobalId(_, _) = id {} else {
            return self.err(vec!["{GlobalId}"], id);
        }
        let init = match self.peek(0)? {
            Token::LeftArrow(_) => { // VarInit
                self.consume()?; // `<-`
                let val = self.consume()?; // Integer
                if let Token::Integer(_, _) = val {} else {
                    return self.err(vec!["Integer"], val);
                }
                Some(val)
            }
            Token::Colon(_) => None,
            tok => return self.err(vec!["<-", ":"], tok)
        };
        let col = self.consume()?;
        check_op!(self, col, ":");
        let ty = self.type_decl()?; // TypeDecl
        let semi = self.consume()?;
        check_op!(self, semi, ";");
        Ok(Term::VarDef { loc, id, init, ty: Box::new(ty) })
    }

    fn fn_def(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        match self.consume()? {
            Token::Reserved(_, k) if &k == "fn" => (),
            kw => return self.err(vec!["fn"], kw)
        }
        let sig = self.fn_sig()?; // FnSig
        let body = self.fn_body()?; // FnBody
        Ok(Term::FnDef { loc, sig: Box::new(sig), body: Box::new(body) })
    }

    fn fn_sig(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let id = self.consume()?; // GlobalId
        if let Token::GlobalId(_, _) = id {} else {
            return self.err(vec!["{GlobalId}"], id);
        }
        let left = self.consume()?;
        check_op!(self, left, "(");
        let param = self.param_list()?; // ParamList
        let right = self.consume()?;
        check_op!(self, right, ")");
        let ret: Option<Term>;
        match self.peek(0)? { // FnRet?
            Token::RightArrow(_) => ret = Some(self.fn_ret()?),
            Token::LeftCurly(_) => ret = None,
            tok => return self.err(vec!["->", "{"], tok)
        }
        Ok(Term::FnSig {
            loc,
            id,
            param: Box::new(param),
            ret: ret.map(|r| Box::new(r)),
        })
    }

    fn param_list(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let mut list = Vec::new();
        loop {
            match self.peek(0)? {
                Token::LocalId(_, _) => list.push(self.param_def()?), // ParamDef
                Token::Comma(_) => { // (`,` ParamDef)*
                    self.consume()?;
                    list.push(self.param_def()?)
                }
                Token::RightParent(_) => break,
                tok => return self.err(vec!["{LocalId}", "RightParent"], tok)
            }
        }
        Ok(Term::ParamList { loc, list })
    }

    fn param_def(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let id = self.consume()?; // LocalId
        if let Token::LocalId(_, _) = id {} else {
            return self.err(vec!["{LocalId}"], id);
        }
        let col = self.consume()?;
        check_op!(self, col, ":");
        let ty = self.type_decl()?; // TypeDecl
        Ok(Term::ParamDef { loc, id, ty: Box::new(ty) })
    }

    fn fn_ret(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let right_arr = self.consume()?;
        check_op!(self, right_arr, "->");
        let ty = self.type_decl()?;
        Ok(Term::FnRet { loc, ty: Box::new(ty) })
    }

    fn fn_body(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let left_cur = self.consume()?;
        // `{`
        check_op!(self, left_cur, "{");
        let mut bb = Vec::new();
        loop {
            match self.peek(0)? { // BlockDef+
                // Until at least a basic block is parsed, `}` cannot be accepted.
                Token::Label(_, _) => bb.push(self.block_def()?),
                Token::RightCurly(_) if !bb.is_empty() => {
                    let right = self.consume()?;
                    check_op!(self, right, "}");
                    break;
                }
                tok => {
                    let mut expect = vec!["{Label}"];
                    if !bb.is_empty() { expect.push("}") }
                    return self.err(expect, tok);
                }
            }
        }
        Ok(Term::FnBody { loc, bb })
    }

    fn block_def(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let lab = self.consume()?; // Label
        if let Token::Label(_, _) = lab {} else {
            return self.err(vec!["{Label}"], lab);
        }
        let col = self.consume()?;
        // `:`
        check_op!(self, col, ":");
        let mut instr = Vec::new();
        loop {
            match self.peek(0)? {
                id if id.is_id() => instr.push(self.instr_def()?), // AssignInstr
                Token::Reserved(_, _) => instr.push(self.instr_def()?), // CtrlInstr
                Token::Label(_, _) | Token::RightCurly(_) if !instr.is_empty() => break,
                tok => {
                    let mut expect = vec!["{Id}", "{Reserved}"];
                    if !instr.is_empty() { expect.append(&mut vec!["{Label}", "}"]) }
                    return self.err(expect, tok);
                }
            }
        }
        Ok(Term::BlockDef { loc, id: lab, instr })
    }

    fn instr_def(&mut self) -> ParseResult {
        let term = match self.peek(0)? {
            id if id.is_id() => self.assign_instr(),
            Token::Reserved(_, _) => self.ctrl_instr(),
            tok => return self.err(vec!["{Id}", "{Reserved}"], tok)
        };
        let semi = self.consume()?;
        check_op!(self, semi, ";");
        term
    }

    fn assign_instr(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let id = self.consume()?; // Id
        if !id.is_id() { return self.err(vec!["{Id}"], id); }
        let arr = self.consume()?;
        check_op!(self, arr, "<-");
        let expr = self.assign_rhs()?;
        Ok(Term::AssignInstr { loc, id, rhs: Box::new(expr) })
    }

    fn assign_rhs(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let name = self.consume()?; // Reserved
        if let Token::Reserved(_, _) = name {} else {
            return self.err(vec!["{Reserved}"], name);
        }
        let ty = self.type_decl()?; // TypeDecl
        let opd = self.arith_opd()?; // ArithOpd
        Ok(Term::AssignRhs { loc, name, ty: Box::new(ty), opd: Box::new(opd) })
    }

    fn arith_opd(&mut self) -> ParseResult {
        Ok(match self.peek(0)? {
            opd if opd.is_opd() => match self.peek(1)? {
                Token::Comma(_) | Token::Semicolon(_) => self.opd_list()?, // OpdList
                Token::LeftParent(_) => self.fn_call()?, // FnCall
                tok => self.err(vec![",", ";", "(", "["], tok)?
            }
            Token::LeftSquare(_) => self.phi_list()?,
            tok => return self.err(vec!["{Operand}"], tok)
        })
    }

    fn opd_list(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let mut list = Vec::new();
        loop {
            match self.peek(0)? {
                opd if opd.is_opd() => { // Opd
                    self.consume()?;
                    list.push(opd)
                }
                Token::Comma(_) => { // `,` Opd
                    self.consume()?;
                    let opd = self.consume()?;
                    if !opd.is_opd() { return self.err(vec!["{Operand}"], opd); }
                    list.push(opd)
                }
                Token::Semicolon(_) | Token::RightParent(_) => break,
                tok => return self.err(vec!["{Operand}", ",", ";"], tok)
            }
        }
        Ok(Term::OpdList { loc, list })
    }

    fn phi_list(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let mut list = Vec::new();
        loop {
            match self.peek(0)? {
                Token::LeftSquare(_) => list.push(self.phi_opd()?),
                Token::Semicolon(_) if !list.is_empty() => break,
                tok => {
                    let mut expect = vec!["["];
                    if !list.is_empty() { expect.push(";") }
                    return self.err(expect, tok);
                }
            }
        }
        Ok(Term::PhiList { loc, list })
    }

    fn phi_opd(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let left = self.consume()?;
        // `[`
        check_op!(self, left, "[");
        let bb = match self.peek(0)? {
            Token::Label(l, s) => {
                self.consume()?; // Label
                let col = self.consume()?;
                check_op!(self, col, ":");
                Some(Token::Label(l, s))
            }
            opd if opd.is_local_opd() => None,
            tok => return self.err(vec!["{Label}", "{LocalOperand}"], tok)
        };
        let opd = self.consume()?;
        if !opd.is_local_opd() { // LocalOpd
            return self.err(vec!["{LocalOperand}"], opd);
        }
        let right = self.consume()?;
        // `]`
        check_op!(self, right, "]");
        Ok(Term::PhiOpd { loc, bb, opd })
    }

    fn fn_call(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let func = self.consume()?;
        if let Token::GlobalId(_, _) = func {} else {
            return self.err(vec!["{GlobalId}"], func);
        }
        let left = self.consume()?;
        check_op!(self, left, "(");
        let arg = self.opd_list()?;
        let right = self.consume()?;
        check_op!(self, right, ")");
        Ok(Term::FnCall { loc, func, arg: Box::new(arg) })
    }

    fn ctrl_instr(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let ctrl = match self.peek(0)? {
            Token::Reserved(_, k) if &k == "ret" => self.ret_instr()?,
            Token::Reserved(_, k) if &k == "jmp" => self.jmp_instr()?,
            Token::Reserved(_, k) if &k == "fn" => {
                self.consume()?;
                self.fn_call()?
            }
            Token::Reserved(_, k) if &k == "br" => self.branch()?,
            tok => self.err(vec!["ret", "jmp", "fn", "br"], tok)?
        };
        Ok(Term::CtrlInstr { loc, instr: Box::new(ctrl) })
    }

    fn ret_instr(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        self.consume()?; // `ret`
        let opd = match self.peek(0)? {
            opd if opd.is_opd() => {
                self.consume()?;
                Some(opd)
            }
            Token::Semicolon(_) => None,
            tok => return self.err(vec!["{Operand}"], tok)
        };
        Ok(Term::RetInstr { loc, opd })
    }

    fn jmp_instr(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        self.consume()?; // `jmp`
        match self.consume()? {
            Token::Label(l, s) => Ok(Term::JmpInstr { loc, tgt: Token::Label(l, s) }),
            tok => self.err(vec!["{Label}"], tok)
        }
    }

    fn branch(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        self.consume()?;
        let cond = self.consume()?; // Opd
        if !cond.is_opd() { return self.err(vec!["{Operand}"], cond); }
        let ques = self.consume()?;
        // `?`
        check_op!(self, ques, "?");
        let tr = self.consume()?; // Label
        if let Token::Label(_, _) = tr {} else {
            return self.err(vec!["{Label}"], tr);
        }
        let col = self.consume()?;
        // `:`
        check_op!(self, col, ":");
        let fls = self.consume()?; // Label
        if let Token::Label(_, _) = fls {} else {
            return self.err(vec!["{Label}"], fls);
        }
        Ok(Term::Branch { loc, cond, tr, fls })
    }

    fn type_decl(&mut self) -> ParseResult {
        let loc = self.loc.clone();
        let ty = self.consume()?; // Reserved
        if let Token::Reserved(_, _) = ty {} else {
            return self.err(vec!["{Reserved}"], ty);
        }
        Ok(Term::TypeDecl { loc, ty })
    }

    /// Consume one lexeme from stream
    fn consume(&mut self) -> Result<Token, CompileErr> {
        let tok = match self.buf.pop_front() {
            Some(l) => l,
            None => self.lexer.next()?
        };
        self.loc = tok.loc().clone();
        Ok(tok)
    }

    /// Look ahead certain lexeme in the stream.
    fn peek(&mut self, idx: usize) -> Result<Token, CompileErr> {
        if idx >= self.buf.len() {
            for _ in 0..(idx - self.buf.len() + 1) {
                self.buf.push_back(self.lexer.next()?)
            }
        }
        let lex = self.buf[idx].clone();
        self.loc = lex.loc();
        Ok(lex)
    }

    /// Report error with current location
    fn err(&self, exp: Vec<&str>, fnd: Token) -> ParseResult {
        Err(CompileErr {
            loc: self.loc.clone(),
            msg: format!("expect {:?}, found \"{}\"", exp, fnd.to_string()),
        })
    }
}

#[test]
fn test_parse() {
    use std::fs::File;
    let mut file = File::open("test/parse.ir").unwrap();
    let lexer = Lexer::from_read(&mut file).unwrap();
    let parser = Parser::new(lexer);
    println!("{:#?}", parser.parse())
}