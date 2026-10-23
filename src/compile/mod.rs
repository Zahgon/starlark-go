// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The Starlark bytecode compiler (ported from
//! go.starlark.net/internal/compile).
//!
//! The compiler generates code with optional uint32 operands for a
//! virtual machine with the following components:
//!   - a program counter, which is an index into the code array.
//!   - an operand stack, whose maximum size is computed for each function by the compiler.
//!   - an stack of active iterators.
//!   - an array of local variables.
//!     The number of local variables and their indices are computed by the resolver.
//!     Locals (possibly including parameters) that are shared with nested functions
//!     are 'cells': their locals array slot will contain a value of type 'cell',
//!     an indirect value in a box that is explicitly read/updated by instructions.
//!   - an array of free variables, for nested functions.
//!     Free variables are a subset of the ancestors' cell variables.
//!     As with locals and cells, these are computed by the resolver.
//!   - an array of global variables, shared among all functions in the same module.
//!     All elements are initially nil.
//!   - two maps of predeclared and universal identifiers.
//!
//! Each instruction records the source position (line and column) that was
//! current when it was generated, or line 0 if none.
//!
//! The function body is first converted to a control-flow graph of blocks,
//! which is then linearized exactly as in the Go original, so that the
//! instruction stream (and thus every runtime error position) is the same.

mod program;

use std::collections::HashMap;
use std::rc::Rc;

pub use program::{Binding, Constant, Funcode, Insn, Opcode, Program};

use crate::resolve::{self, Scope};
use crate::syntax::{
    BinaryExpr, CallExpr, Clause, Comprehension, Expr, FileOptions, Ident, LiteralValue, Position,
    Stmt, Token,
};

/// Compiles an expression to a program whose toplevel function evaluates it.
/// The options must be consistent with those used when parsing expr.
pub fn expr(opts: &FileOptions, e: &Expr, name: &str, locals: &[Rc<resolve::Binding>]) -> Program {
    let pos = e.start();
    compile_program(opts, Body::Return(e), pos, name, locals, &[])
}

/// Compiles the statements of a file into a program.
/// The options must be consistent with those used when parsing stmts.
pub fn file(
    opts: &FileOptions,
    stmts: &[Stmt],
    pos: Position,
    name: &str,
    locals: &[Rc<resolve::Binding>],
    globals: &[Rc<resolve::Binding>],
) -> Program {
    compile_program(opts, Body::Stmts(stmts), pos, name, locals, globals)
}

fn compile_program(
    opts: &FileOptions,
    body: Body<'_>,
    pos: Position,
    name: &str,
    locals: &[Rc<resolve::Binding>],
    globals: &[Rc<resolve::Binding>],
) -> Program {
    let mut pcomp = PComp::default();
    let toplevel = pcomp.function(name, pos, body, locals, &[]);
    Program {
        loads: pcomp.loads,
        names: pcomp.names,
        constants: pcomp.constants,
        functions: pcomp.functions,
        globals: bindings(globals),
        toplevel: Rc::new(toplevel),
        recursion: opts.recursion,
    }
}

/// The body of a function: a list of statements, or a single expression
/// whose value is returned (a lambda, or an expression program).
#[derive(Clone, Copy)]
enum Body<'a> {
    Stmts(&'a [Stmt]),
    Return(&'a Expr),
}

/// Converts resolve.Bindings to compiled form.
fn bindings(bindings: &[Rc<resolve::Binding>]) -> Vec<Binding> {
    bindings
        .iter()
        .map(|bind| Binding {
            name: bind.name().to_string(),
            pos: bind.pos(),
        })
        .collect()
}

/// The key under which a constant is interned. As in the Go original,
/// constants are interned by value, except that each big integer literal
/// is distinct.
#[derive(PartialEq, Eq, Hash)]
enum ConstantKey {
    String(Vec<u8>),
    Bytes(Vec<u8>),
    Int(i64),
    Float(u64),
    BigInt(usize),
}

/// Holds the compiler state for a Program.
#[derive(Default)]
struct PComp {
    loads: Vec<Binding>,
    names: Vec<String>,
    constants: Vec<Constant>,
    functions: Vec<Rc<Funcode>>,

    name_index: HashMap<String, u32>,
    constant_index: HashMap<ConstantKey, u32>,
}

impl PComp {
    fn function(
        &mut self,
        name: &str,
        pos: Position,
        body: Body<'_>,
        locals: &[Rc<resolve::Binding>],
        freevars: &[Rc<resolve::Binding>],
    ) -> Funcode {
        let mut fcomp = FComp {
            pcomp: self,
            pos: pos.clone(),
            loops: Vec::new(),
            blocks: Vec::new(),
            block: None,
        };

        // Convert AST to a CFG of instructions.
        let entry = fcomp.new_block();
        fcomp.block = Some(entry);
        match body {
            Body::Stmts(stmts) => fcomp.stmts(stmts),
            Body::Return(e) => fcomp.return_stmt(Some(e)),
        }
        if fcomp.block.is_some() {
            fcomp.emit(Opcode::None);
            fcomp.emit(Opcode::Return);
        }

        // Linearize the CFG:
        // compute order, address, and initial
        // stack depth of each reachable block.
        let mut lin = Linearization::default();
        fcomp.set_initial_stack(entry, 0);
        fcomp.visit(entry, &mut lin);

        let code = fcomp.generate(&lin.order);

        // Record indices of locals that require cells.
        let cells = locals
            .iter()
            .enumerate()
            .filter(|(_, local)| local.scope() == Scope::Cell)
            .map(|(i, _)| i)
            .collect();

        Funcode {
            pos,
            name: name.to_string(),
            doc: doc_string_from_body(body),
            code,
            locals: bindings(locals),
            cells,
            freevars: bindings(freevars),
            max_stack: lin.maxstack,
            num_params: 0,
            num_kwonly_params: 0,
            has_varargs: false,
            has_kwargs: false,
        }
    }

    /// Returns the index of the specified name
    /// within the name pool, adding it if necessary.
    fn name_index(&mut self, name: &str) -> u32 {
        if let Some(&index) = self.name_index.get(name) {
            return index;
        }
        let index = self.names.len() as u32;
        self.name_index.insert(name.to_string(), index);
        self.names.push(name.to_string());
        index
    }

    /// Returns the index of the specified constant
    /// within the constant pool, adding it if necessary.
    fn constant_index(&mut self, v: Constant) -> u32 {
        let key = match &v {
            Constant::String(s) => ConstantKey::String(s.to_vec()),
            Constant::Bytes(s) => ConstantKey::Bytes(s.to_vec()),
            Constant::Int(i) => ConstantKey::Int(*i),
            Constant::Float(f) => ConstantKey::Float(f.to_bits()),
            Constant::BigInt(b) => ConstantKey::BigInt(Rc::as_ptr(b) as usize),
        };
        if let Some(&index) = self.constant_index.get(&key) {
            return index;
        }
        let index = self.constants.len() as u32;
        self.constant_index.insert(key, index);
        self.constants.push(v);
        index
    }

    /// Returns the index of the specified function
    /// within the function pool, adding it.
    fn function_index(&mut self, f: Funcode) -> u32 {
        let index = self.functions.len() as u32;
        self.functions.push(Rc::new(f));
        index
    }
}

fn doc_string_from_body(body: Body<'_>) -> String {
    let Body::Stmts(stmts) = body else {
        return String::new();
    };
    let Some(Stmt::Expr(stmt)) = stmts.first() else {
        return String::new();
    };
    let Expr::Literal(lit) = &stmt.x else {
        return String::new();
    };
    if lit.token != Token::String {
        return String::new();
    }
    match &lit.value {
        LiteralValue::String(s) => String::from_utf8_lossy(s).into_owned(),
        _ => String::new(),
    }
}

struct Loop {
    break_: usize,
    continue_: usize,
}

struct Block {
    insns: Vec<Insn>,

    // If the last insn is a RETURN, jmp and cjmp are None.
    // If the last insn is a CJMP or ITERJMP,
    //  cjmp and jmp are the "true" and "false" successors.
    // Otherwise, jmp is the sole successor.
    jmp: Option<usize>,
    cjmp: Option<usize>,

    /// For stack depth computation (-1 => not yet known).
    initialstack: isize,

    /// Position in the linear order (-1 => not visited yet).
    index: isize,
    /// Address (instruction index) of the first instruction.
    addr: u32,
}

/// The state of the linearization of a function's CFG.
#[derive(Default)]
struct Linearization {
    /// The instruction count so far.
    pc: u32,
    /// The reachable blocks, in order.
    order: Vec<usize>,
    maxstack: usize,
}

/// Holds the compiler state for a Funcode.
struct FComp<'p> {
    pcomp: &'p mut PComp,
    /// Current position of generated code.
    pos: Position,
    loops: Vec<Loop>,
    blocks: Vec<Block>,
    /// The current block, or None after an unconditional transfer of control.
    block: Option<usize>,
}

/// Returns the effect on the size of the operand stack of an instruction.
fn stack_effect(insn: &Insn) -> isize {
    let arg = insn.arg as isize;
    match insn.op {
        Opcode::Amp => -1,
        Opcode::Append => -2,
        Opcode::Attr => 0,
        Opcode::Call | Opcode::CallKw | Opcode::CallVar | Opcode::CallVarKw => {
            let mut se = -(2 * (arg & 0xff) + (arg >> 8));
            if insn.op != Opcode::Call {
                se -= 1;
            }
            if insn.op == Opcode::CallVarKw {
                se -= 1;
            }
            se
        }
        Opcode::Circumflex => -1,
        Opcode::Cjmp => -1,
        Opcode::Constant => 1,
        Opcode::Dup2 => 2,
        Opcode::Dup => 1,
        Opcode::Eql => -1,
        Opcode::Exch => 0,
        Opcode::False => 1,
        Opcode::Free => 1,
        Opcode::Freecell => 1,
        Opcode::Ge => -1,
        Opcode::Global => 1,
        Opcode::Gt => -1,
        Opcode::Gtgt => -1,
        Opcode::In => -1,
        Opcode::Index => -1,
        Opcode::InplaceAdd => -1,
        Opcode::InplacePipe => -1,
        // Stack effect differs by successor:
        // +1 for jmp/false/ok
        //  0 for cjmp/true/exhausted
        // Handled specially in caller.
        Opcode::Iterjmp => 0,
        Opcode::Iterpop => 0,
        Opcode::Iterpush => -1,
        Opcode::Jmp => 0,
        Opcode::Le => -1,
        Opcode::Load => -1,
        Opcode::Local => 1,
        Opcode::Localcell => 1,
        Opcode::Lt => -1,
        Opcode::Ltlt => -1,
        Opcode::Makedict => 1,
        Opcode::Makefunc => 0,
        Opcode::Makelist | Opcode::Maketuple => 1 - arg,
        Opcode::Mandatory => 1,
        Opcode::Minus => -1,
        Opcode::Neq => -1,
        Opcode::None => 1,
        Opcode::Nop => 0,
        Opcode::Not => 0,
        Opcode::Percent => -1,
        Opcode::Pipe => -1,
        Opcode::Plus => -1,
        Opcode::Pop => -1,
        Opcode::Predeclared => 1,
        Opcode::Return => -1,
        Opcode::Setlocalcell => -1,
        Opcode::Setdict => -3,
        Opcode::Setdictuniq => -3,
        Opcode::Setfield => -2,
        Opcode::Setglobal => -1,
        Opcode::Setindex => -3,
        Opcode::Setlocal => -1,
        Opcode::Slash => -1,
        Opcode::Slashslash => -1,
        Opcode::Slice => -3,
        Opcode::Star => -1,
        Opcode::Tilde => 0,
        Opcode::True => 1,
        Opcode::Uminus => 0,
        Opcode::Universal => 1,
        Opcode::Unpack => arg - 1,
        Opcode::Uplus => 0,
    }
}

/// A summand of an n-ary sum: either an expression, or the folded sum of
/// adjacent literals of the same kind.
enum Summand<'a> {
    Expr(&'a Expr),
    String(Vec<u8>),
    Bytes(Vec<u8>),
    List(Vec<&'a Expr>),
    Tuple(Vec<&'a Expr>),
}

/// Reports whether e is a statically addable
/// expression: a [s]tring, [b]ytes, [l]ist, or [t]uple.
fn addable(e: &Expr) -> Option<char> {
    match e {
        // Only string, list and tuple operands are folded; numeric constants are not.
        Expr::Literal(lit) => match lit.token {
            Token::String => Some('s'),
            Token::Bytes => Some('b'),
            _ => None,
        },
        Expr::List(_) => Some('l'),
        Expr::Tuple(_) => Some('t'),
        _ => None,
    }
}

/// Returns the sum of args, which are all addable values of the type
/// indicated by code.
fn add<'a>(code: char, args: &[&'a Expr]) -> Summand<'a> {
    match code {
        's' | 'b' => {
            let mut buf = Vec::new();
            for arg in args {
                if let Expr::Literal(lit) = arg {
                    if let LiteralValue::String(s) = &lit.value {
                        buf.extend_from_slice(s);
                    }
                }
            }
            if code == 'b' {
                Summand::Bytes(buf)
            } else {
                Summand::String(buf)
            }
        }
        'l' => {
            let mut elems = Vec::new();
            for arg in args {
                if let Expr::List(list) = arg {
                    elems.extend(list.list.iter());
                }
            }
            Summand::List(elems)
        }
        _ => {
            let mut elems = Vec::new();
            for arg in args {
                if let Expr::Tuple(tuple) = arg {
                    elems.extend(tuple.list.iter());
                }
            }
            Summand::Tuple(elems)
        }
    }
}

fn unparen(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(p) => unparen(&p.x),
        _ => e,
    }
}

/// Maps an augmented assignment token (e.g. `+=`) to its binary operator.
fn augmented_op(op: Token) -> Token {
    match op {
        Token::PlusEq => Token::Plus,
        Token::MinusEq => Token::Minus,
        Token::StarEq => Token::Star,
        Token::SlashEq => Token::Slash,
        Token::SlashslashEq => Token::Slashslash,
        Token::PercentEq => Token::Percent,
        Token::AmpEq => Token::Amp,
        Token::PipeEq => Token::Pipe,
        Token::CircumflexEq => Token::Circumflex,
        Token::LtltEq => Token::Ltlt,
        Token::GtgtEq => Token::Gtgt,
        _ => op,
    }
}

fn ident_binding(id: &Ident) -> Rc<resolve::Binding> {
    match id.binding() {
        Some(bind) => bind,
        None => panic!("{}: unresolved identifier {}", id.name_pos, id.name),
    }
}

impl FComp<'_> {
    /// Returns a new block.
    fn new_block(&mut self) -> usize {
        self.blocks.push(Block {
            insns: Vec::new(),
            jmp: None,
            cjmp: None,
            initialstack: -1,
            index: -1,
            addr: 0,
        });
        self.blocks.len() - 1
    }

    fn set_initial_stack(&mut self, b: usize, depth: isize) {
        let block = &mut self.blocks[b];
        if block.initialstack == -1 {
            block.initialstack = depth;
        }
    }

    /// Follows a chain of empty blocks (jump threading; empty cycles are
    /// impossible).
    fn thread(&self, mut b: usize) -> usize {
        while self.blocks[b].insns.is_empty() {
            match self.blocks[b].jmp {
                Some(next) => b = next,
                None => break,
            }
        }
        b
    }

    /// Computes the order, address, and initial stack depth of the block
    /// and its successors.
    fn visit(&mut self, b: usize, lin: &mut Linearization) {
        if self.blocks[b].index >= 0 {
            return; // already visited
        }
        self.blocks[b].index = lin.order.len() as isize;
        self.blocks[b].addr = lin.pc;
        lin.order.push(b);

        let mut stack = self.blocks[b].initialstack;
        let mut cjmp_insn: Option<usize> = None;
        let mut isiterjmp = 0;
        for (i, insn) in self.blocks[b].insns.iter().enumerate() {
            lin.pc += 1;
            match insn.op {
                Opcode::Iterjmp => {
                    isiterjmp = 1;
                    cjmp_insn = Some(i);
                }
                Opcode::Cjmp => cjmp_insn = Some(i),
                _ => {}
            }

            // Compute effect on stack.
            stack += stack_effect(insn);
            let depth = (stack + isiterjmp).max(0) as usize;
            if depth > lin.maxstack {
                lin.maxstack = depth;
            }
        }

        // Place the jmp block next.
        if let Some(jmp) = self.blocks[b].jmp {
            let jmp = self.thread(jmp);
            self.blocks[b].jmp = Some(jmp);

            self.set_initial_stack(jmp, stack + isiterjmp);
            if self.blocks[jmp].index < 0 {
                // Successor is not yet visited:
                // place it next and fall through.
                self.visit(jmp, lin);
            } else {
                // Successor already visited;
                // explicit backward jump required.
                lin.pc += 1;
            }
        }

        // Then the cjmp block.
        if let Some(cjmp) = self.blocks[b].cjmp {
            let cjmp = self.thread(cjmp);
            self.blocks[b].cjmp = Some(cjmp);

            self.set_initial_stack(cjmp, stack);
            self.visit(cjmp, lin);

            // Patch the CJMP/ITERJMP, if present.
            if let Some(i) = cjmp_insn {
                self.blocks[b].insns[i].arg = self.blocks[cjmp].addr;
            }
        }
    }

    /// Emits the linear instruction stream from the CFG.
    fn generate(&mut self, order: &[usize]) -> Vec<Insn> {
        let mut code = Vec::new();
        for &b in order {
            let block = &self.blocks[b];
            code.extend_from_slice(&block.insns);
            if let Some(jmp) = block.jmp {
                let target = &self.blocks[jmp];
                if target.index != block.index + 1 {
                    code.push(Insn {
                        op: Opcode::Jmp,
                        arg: target.addr,
                        line: 0,
                        col: 0,
                    });
                }
            }
        }
        code
    }

    fn current(&mut self) -> &mut Block {
        let b = self
            .block
            .expect("internal error: no current block (unreachable code)");
        &mut self.blocks[b]
    }

    /// Emits an instruction to the current block.
    fn emit(&mut self, op: Opcode) {
        self.emit1(op, 0);
    }

    /// Emits an instruction with an immediate operand.
    fn emit1(&mut self, op: Opcode, arg: u32) {
        let insn = Insn {
            op,
            arg,
            line: self.pos.line,
            col: self.pos.col,
        };
        self.current().insns.push(insn);
        self.pos.line = 0;
        self.pos.col = 0;
    }

    /// Emits a jump to the specified block.
    /// On return, the current block is unset.
    fn jump(&mut self, b: usize) {
        self.current().jmp = Some(b);
        self.block = None;
    }

    /// Emits a conditional jump (CJMP or ITERJMP)
    /// to the specified true/false blocks.
    /// (For ITERJMP, the cases are jmp/f/ok and cjmp/t/exhausted.)
    /// On return, the current block is unset.
    fn condjump(&mut self, op: Opcode, t: usize, f: usize) {
        self.emit1(op, 0); // fill in address later
        self.current().cjmp = Some(t);
        self.jump(f);
    }

    /// Emits code to push the specified string.
    fn string(&mut self, s: &[u8]) {
        let index = self.pcomp.constant_index(Constant::String(Rc::from(s)));
        self.emit1(Opcode::Constant, index);
    }

    /// Sets the current source position.
    /// It should be called prior to any operation that can fail dynamically.
    /// All positions are assumed to belong to the same file.
    fn set_pos(&mut self, pos: &Position) {
        self.pos = pos.clone();
    }

    /// Emits code to store the top-of-stack value
    /// to the specified local, cell, or global variable.
    fn set(&mut self, id: &Ident) {
        let bind = ident_binding(id);
        let index = bind.index as u32;
        match bind.scope() {
            Scope::Local => self.emit1(Opcode::Setlocal, index),
            Scope::Cell => self.emit1(Opcode::Setlocalcell, index),
            Scope::Global => self.emit1(Opcode::Setglobal, index),
            scope => panic!(
                "{}: set({}): not global/local/cell ({})",
                id.name_pos, id.name, scope
            ),
        }
    }

    /// Emits code to push the value of the specified variable.
    fn lookup(&mut self, id: &Ident) {
        let bind = ident_binding(id);
        if bind.scope() != Scope::Universal {
            // (universal lookup can't fail)
            self.set_pos(&id.name_pos);
        }
        let index = bind.index as u32;
        match bind.scope() {
            Scope::Local => self.emit1(Opcode::Local, index),
            Scope::Free => self.emit1(Opcode::Freecell, index),
            Scope::Cell => self.emit1(Opcode::Localcell, index),
            Scope::Global => self.emit1(Opcode::Global, index),
            Scope::Predeclared => {
                let index = self.pcomp.name_index(&id.name);
                self.emit1(Opcode::Predeclared, index);
            }
            Scope::Universal => {
                let index = self.pcomp.name_index(&id.name);
                self.emit1(Opcode::Universal, index);
            }
            Scope::Undefined => panic!(
                "{}: compiler.lookup({}): scope = {}",
                id.name_pos,
                id.name,
                bind.scope()
            ),
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Expr(s) => {
                if let Expr::Literal(_) = s.x {
                    // Opt: don't compile doc comments only to pop them.
                    return;
                }
                self.expr(&s.x);
                self.emit(Opcode::Pop);
            }

            Stmt::Branch(s) => {
                // Resolver invariant: break/continue appear only within loops.
                match s.token {
                    Token::Break => {
                        let b = self.innermost_loop().break_;
                        self.jump(b);
                        self.block = Some(self.new_block()); // dead code
                    }
                    Token::Continue => {
                        let b = self.innermost_loop().continue_;
                        self.jump(b);
                        self.block = Some(self.new_block()); // dead code
                    }
                    _ => {
                        // pass: no-op
                    }
                }
            }

            Stmt::If(s) => {
                // Keep consistent with CondExpr.
                let t = self.new_block();
                let f = self.new_block();
                let done = self.new_block();

                self.ifelse(&s.cond, t, f);

                self.block = Some(t);
                self.stmts(&s.true_);
                self.jump(done);

                self.block = Some(f);
                self.stmts(&s.false_);
                self.jump(done);

                self.block = Some(done);
            }

            Stmt::Assign(s) => {
                if s.op == Token::Eq {
                    // simple assignment: x = y
                    self.expr(&s.rhs);
                    self.assign(&s.op_pos, &s.lhs);
                } else {
                    self.augmented_assign(s);
                }
            }

            Stmt::Def(s) => {
                let function = s.function.borrow().clone();
                let Some(function) = function else {
                    panic!("{}: unresolved function {}", s.def, s.name.name);
                };
                self.function(&function, &s.params, Body::Stmts(&s.body));
                self.set(&s.name);
            }

            Stmt::For(s) => {
                // Keep consistent with ForClause.
                let head = self.new_block();
                let body = self.new_block();
                let tail = self.new_block();

                self.expr(&s.x);
                self.set_pos(&s.for_);
                self.emit(Opcode::Iterpush);
                self.jump(head);

                self.block = Some(head);
                self.condjump(Opcode::Iterjmp, tail, body);

                self.block = Some(body);
                self.assign(&s.for_, &s.vars);
                self.loops.push(Loop {
                    break_: tail,
                    continue_: head,
                });
                self.stmts(&s.body);
                self.loops.pop();
                self.jump(head);

                self.block = Some(tail);
                self.emit(Opcode::Iterpop);
            }

            Stmt::While(s) => {
                let head = self.new_block();
                let body = self.new_block();
                let done = self.new_block();

                self.jump(head);
                self.block = Some(head);
                self.ifelse(&s.cond, body, done);

                self.block = Some(body);
                self.loops.push(Loop {
                    break_: done,
                    continue_: head,
                });
                self.stmts(&s.body);
                self.loops.pop();
                self.jump(head);

                self.block = Some(done);
            }

            Stmt::Return(s) => self.return_stmt(s.result.as_ref()),

            Stmt::Load(s) => {
                for from in &s.from {
                    self.string(from.name.as_bytes());
                }
                let module = match &s.module.value {
                    LiteralValue::String(m) => m.clone(),
                    _ => Vec::new(),
                };
                self.pcomp.loads.push(Binding {
                    name: String::from_utf8_lossy(&module).into_owned(),
                    pos: s.module.token_pos.clone(),
                });
                self.string(&module);
                self.set_pos(&s.load);
                self.emit1(Opcode::Load, s.from.len() as u32);
                for to in s.to.iter().rev() {
                    self.set(to);
                }
            }
        }
    }

    fn innermost_loop(&self) -> &Loop {
        self.loops
            .last()
            .expect("internal error: break/continue outside loop")
    }

    fn return_stmt(&mut self, result: Option<&Expr>) {
        match result {
            Some(e) => self.expr(e),
            None => self.emit(Opcode::None),
        }
        self.emit(Opcode::Return);
        self.block = Some(self.new_block()); // dead code
    }

    /// Compiles an augmented assignment: x += y.
    fn augmented_assign(&mut self, s: &crate::syntax::AssignStmt) {
        /// How to store the result.
        enum Target<'a> {
            Ident(&'a Ident),
            Index(&'a Position),
            Field(&'a Position, u32),
        }

        // Evaluate "address" of x exactly once to avoid duplicate side-effects.
        let target = match unparen(&s.lhs) {
            Expr::Ident(lhs) => {
                // x = ...
                self.lookup(lhs);
                Target::Ident(lhs)
            }

            Expr::Index(lhs) => {
                // x[y] = ...
                self.expr(&lhs.x);
                self.expr(&lhs.y);
                self.emit(Opcode::Dup2);
                self.set_pos(&lhs.lbrack);
                self.emit(Opcode::Index);
                Target::Index(&lhs.lbrack)
            }

            Expr::Dot(lhs) => {
                // x.f = ...
                self.expr(&lhs.x);
                self.emit(Opcode::Dup);
                let name = self.pcomp.name_index(&lhs.name.name);
                self.set_pos(&lhs.dot);
                self.emit1(Opcode::Attr, name);
                Target::Field(&lhs.dot, name)
            }

            lhs => panic!("{}: unexpected augmented assignment target", lhs.start()),
        };

        self.expr(&s.rhs);

        // In-place x+=y and x|=y have special semantics:
        // the resulting x aliases the original x.
        match s.op {
            Token::PlusEq => {
                self.set_pos(&s.op_pos);
                self.emit(Opcode::InplaceAdd);
            }
            Token::PipeEq => {
                self.set_pos(&s.op_pos);
                self.emit(Opcode::InplacePipe);
            }
            op => self.binop(&s.op_pos, augmented_op(op)),
        }

        match target {
            Target::Ident(lhs) => self.set(lhs),
            Target::Index(lbrack) => {
                self.set_pos(lbrack);
                self.emit(Opcode::Setindex);
            }
            Target::Field(dot, name) => {
                self.set_pos(dot);
                self.emit1(Opcode::Setfield, name);
            }
        }
    }

    /// Implements lhs = rhs for arbitrary expressions lhs.
    /// RHS is on top of stack, consumed.
    fn assign(&mut self, pos: &Position, lhs: &Expr) {
        match lhs {
            Expr::Paren(lhs) => {
                // (lhs) = rhs
                self.assign(pos, &lhs.x);
            }

            Expr::Ident(lhs) => {
                // x = rhs
                self.set(lhs);
            }

            Expr::Tuple(lhs) => {
                // x, y = rhs
                self.assign_sequence(pos, &lhs.list);
            }

            Expr::List(lhs) => {
                // [x, y] = rhs
                self.assign_sequence(pos, &lhs.list);
            }

            Expr::Index(lhs) => {
                // x[y] = rhs
                self.expr(&lhs.x);
                self.emit(Opcode::Exch);
                self.expr(&lhs.y);
                self.emit(Opcode::Exch);
                self.set_pos(&lhs.lbrack);
                self.emit(Opcode::Setindex);
            }

            Expr::Dot(lhs) => {
                // x.f = rhs
                self.expr(&lhs.x);
                self.emit(Opcode::Exch);
                self.set_pos(&lhs.dot);
                let name = self.pcomp.name_index(&lhs.name.name);
                self.emit1(Opcode::Setfield, name);
            }

            lhs => panic!("{}: unexpected assignment target", lhs.start()),
        }
    }

    fn assign_sequence(&mut self, pos: &Position, lhs: &[Expr]) {
        self.set_pos(pos);
        self.emit1(Opcode::Unpack, lhs.len() as u32);
        for elem in lhs {
            self.assign(pos, elem);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Paren(e) => self.expr(&e.x),

            Expr::Ident(id) => self.lookup(id),

            Expr::Literal(lit) => {
                let v = match &lit.value {
                    LiteralValue::Int(i) => Constant::Int(*i),
                    LiteralValue::BigInt(b) => Constant::BigInt(b.clone()),
                    LiteralValue::Float(f) => Constant::Float(*f),
                    LiteralValue::String(s) if lit.token == Token::Bytes => {
                        Constant::Bytes(Rc::from(s.as_slice()))
                    }
                    LiteralValue::String(s) => Constant::String(Rc::from(s.as_slice())),
                };
                let index = self.pcomp.constant_index(v);
                self.emit1(Opcode::Constant, index);
            }

            Expr::List(e) => {
                for x in &e.list {
                    self.expr(x);
                }
                self.emit1(Opcode::Makelist, e.list.len() as u32);
            }

            Expr::Cond(e) => {
                // Keep consistent with IfStmt.
                let t = self.new_block();
                let f = self.new_block();
                let done = self.new_block();

                self.ifelse(&e.cond, t, f);

                self.block = Some(t);
                self.expr(&e.true_);
                self.jump(done);

                self.block = Some(f);
                self.expr(&e.false_);
                self.jump(done);

                self.block = Some(done);
            }

            Expr::Index(e) => {
                self.expr(&e.x);
                self.expr(&e.y);
                self.set_pos(&e.lbrack);
                self.emit(Opcode::Index);
            }

            Expr::Slice(e) => {
                self.set_pos(&e.lbrack);
                self.expr(&e.x);
                for x in [&e.lo, &e.hi, &e.step] {
                    match x {
                        Some(x) => self.expr(x),
                        None => self.emit(Opcode::None),
                    }
                }
                self.emit(Opcode::Slice);
            }

            Expr::Comprehension(e) => {
                if e.curly {
                    self.emit(Opcode::Makedict);
                } else {
                    self.emit1(Opcode::Makelist, 0);
                }
                self.comprehension(e, 0);
            }

            Expr::Tuple(e) => self.tuple(e.list.iter()),

            Expr::Dict(e) => {
                self.emit(Opcode::Makedict);
                for entry in &e.list {
                    self.emit(Opcode::Dup);
                    self.expr(&entry.key);
                    self.expr(&entry.value);
                    self.set_pos(&entry.colon);
                    self.emit(Opcode::Setdictuniq);
                }
            }

            Expr::Unary(e) => {
                if let Some(x) = &e.x {
                    self.expr(x);
                }
                self.set_pos(&e.op_pos);
                match e.op {
                    Token::Minus => self.emit(Opcode::Uminus),
                    Token::Plus => self.emit(Opcode::Uplus),
                    Token::Not => self.emit(Opcode::Not),
                    Token::Tilde => self.emit(Opcode::Tilde),
                    op => panic!("{}: unexpected unary op: {}", e.op_pos, op),
                }
            }

            Expr::Binary(e) => match e.op {
                // short-circuit operators
                Token::Or => {
                    // x or y  =>  if x then x else y
                    let done = self.new_block();
                    let y = self.new_block();

                    self.expr(&e.x);
                    self.emit(Opcode::Dup);
                    self.condjump(Opcode::Cjmp, done, y);

                    self.block = Some(y);
                    self.emit(Opcode::Pop); // discard X
                    self.expr(&e.y);
                    self.jump(done);

                    self.block = Some(done);
                }

                Token::And => {
                    // x and y  =>  if x then y else x
                    let done = self.new_block();
                    let y = self.new_block();

                    self.expr(&e.x);
                    self.emit(Opcode::Dup);
                    self.condjump(Opcode::Cjmp, y, done);

                    self.block = Some(y);
                    self.emit(Opcode::Pop); // discard X
                    self.expr(&e.y);
                    self.jump(done);

                    self.block = Some(done);
                }

                Token::Plus => self.plus(e),

                _ => {
                    // all other strict binary operator (includes comparisons)
                    self.expr(&e.x);
                    self.expr(&e.y);
                    self.binop(&e.op_pos, e.op);
                }
            },

            Expr::Dot(e) => {
                self.expr(&e.x);
                self.set_pos(&e.dot);
                let name = self.pcomp.name_index(&e.name.name);
                self.emit1(Opcode::Attr, name);
            }

            Expr::Call(call) => self.call(call),

            Expr::Lambda(e) => {
                let function = e.function.borrow().clone();
                let Some(function) = function else {
                    panic!("{}: unresolved lambda", e.lambda);
                };
                self.function(&function, &e.params, Body::Return(&e.body));
            }

            Expr::DictEntry(entry) => panic!("{}: unexpected dict entry", entry.colon),
        }
    }

    /// Emits optimized code for ((a+b)+...)+z that avoids naive
    /// quadratic behavior for strings, tuples, and lists,
    /// and folds together adjacent literals of the same type.
    fn plus(&mut self, e: &BinaryExpr) {
        // Gather all the right operands of the left tree of plusses.
        // A tree (((a+b)+c)+d) becomes args=[a +b +c +d].
        let mut args: Vec<(&Expr, Position)> = Vec::with_capacity(2); // common case: 2 operands
        let mut plus = e;
        loop {
            args.push((unparen(&plus.y), plus.op_pos.clone()));
            let left = unparen(&plus.x);
            match left {
                Expr::Binary(x) if x.op == Token::Plus => plus = x,
                _ => {
                    args.push((left, Position::default()));
                    break;
                }
            }
        }
        // Reverse args to syntactic order.
        args.reverse();

        // Fold sums of adjacent literals of the same type: ""+"", []+[], ()+().
        let mut out: Vec<(Summand<'_>, Position)> = Vec::with_capacity(args.len());
        let mut i = 0;
        while i < args.len() {
            let mut j = i + 1;
            let mut summand = Summand::Expr(args[i].0);
            if let Some(code) = addable(args[i].0) {
                while j < args.len() && addable(args[j].0) == Some(code) {
                    j += 1;
                }
                if j > i + 1 {
                    let exprs: Vec<&Expr> = args[i..j].iter().map(|(x, _)| *x).collect();
                    summand = add(code, &exprs);
                }
            }
            out.push((summand, args[i].1.clone()));
            i = j;
        }

        // Emit code for an n-ary sum (n > 0).
        let mut out = out.into_iter();
        if let Some((first, _)) = out.next() {
            self.summand(first);
        }
        for (summand, plus_pos) in out {
            self.summand(summand);
            self.set_pos(&plus_pos);
            self.emit(Opcode::Plus);
        }

        // If len(args) > 2, use of an accumulator instead of a chain of
        // PLUS operations may be more efficient.
        // However, no gain was measured on a workload analogous to Bazel loading.
        //
        // We cannot use a single n-ary SUM operation
        //    a b c SUM<3>
        // because we need to report a distinct error for each
        // individual '+' operation.
    }

    /// Emits code to push the value of a summand.
    fn summand(&mut self, s: Summand<'_>) {
        match s {
            Summand::Expr(e) => self.expr(e),
            Summand::String(s) => {
                let index = self.pcomp.constant_index(Constant::String(Rc::from(s)));
                self.emit1(Opcode::Constant, index);
            }
            Summand::Bytes(s) => {
                let index = self.pcomp.constant_index(Constant::Bytes(Rc::from(s)));
                self.emit1(Opcode::Constant, index);
            }
            Summand::List(elems) => {
                for x in &elems {
                    self.expr(x);
                }
                self.emit1(Opcode::Makelist, elems.len() as u32);
            }
            Summand::Tuple(elems) => self.tuple(elems.into_iter()),
        }
    }

    fn binop(&mut self, pos: &Position, op: Token) {
        self.set_pos(pos);
        let opcode = match op {
            // arithmetic
            Token::Plus => Opcode::Plus,
            Token::Minus => Opcode::Minus,
            Token::Star => Opcode::Star,
            Token::Slash => Opcode::Slash,
            Token::Slashslash => Opcode::Slashslash,
            Token::Percent => Opcode::Percent,
            Token::Amp => Opcode::Amp,
            Token::Pipe => Opcode::Pipe,
            Token::Circumflex => Opcode::Circumflex,
            Token::Ltlt => Opcode::Ltlt,
            Token::Gtgt => Opcode::Gtgt,
            Token::In => Opcode::In,
            Token::NotIn => {
                self.emit(Opcode::In);
                Opcode::Not
            }

            // comparisons
            Token::Eql => Opcode::Eql,
            Token::Neq => Opcode::Neq,
            Token::Gt => Opcode::Gt,
            Token::Lt => Opcode::Lt,
            Token::Le => Opcode::Le,
            Token::Ge => Opcode::Ge,

            _ => panic!("{pos}: unexpected binary op: {op}"),
        };
        self.emit(opcode);
    }

    fn call(&mut self, call: &CallExpr) {
        // of built-ins: x.f(...) to avoid materializing a closure.

        // usual case
        self.expr(&call.func);
        let (op, arg) = self.args(call);
        self.set_pos(&call.lparen);
        self.emit1(op, arg);
    }

    /// Emits code to push a tuple of positional arguments
    /// and a tuple of named arguments containing alternating keys and values.
    /// Either or both tuples may be empty.
    fn args(&mut self, call: &CallExpr) -> (Opcode, u32) {
        let mut callmode = 0;
        // Compute the number of each kind of parameter.
        let mut p = 0; // number of positional arguments
        let mut n = 0; // number of named arguments
        let mut varargs = None;
        let mut kwargs = None;
        for arg in &call.args {
            match arg {
                Expr::Binary(binary) if binary.op == Token::Eq => {
                    // named argument (name, value)
                    if let Expr::Ident(name) = &*binary.x {
                        self.string(name.name.as_bytes());
                    }
                    self.expr(&binary.y);
                    n += 1;
                    continue;
                }
                Expr::Unary(unary) if unary.op == Token::Star => {
                    callmode |= 1;
                    varargs = unary.x.as_deref();
                    continue;
                }
                Expr::Unary(unary) if unary.op == Token::Starstar => {
                    callmode |= 2;
                    kwargs = unary.x.as_deref();
                    continue;
                }
                _ => {}
            }

            // positional argument
            self.expr(arg);
            p += 1;
        }

        // Python2 and Python3 both permit named arguments
        // to appear both before and after a *args argument:
        //   f(1, 2, x=3, *[4], y=5, **dict(z=6))
        //
        // They also differ in their evaluation order:
        //  Python2: 1 2 3 5 4 6 (*args and **kwargs evaluated last)
        //  Python3: 1 2 4 3 5 6 (positional args evaluated before named args)
        // Starlark-in-Java historically used a third order:
        //  Lexical: 1 2 3 4 5 6 (all args evaluated left-to-right)
        //
        // After discussion in github.com/bazelbuild/starlark#13, the
        // spec now requires Starlark to statically reject named
        // arguments after *args (e.g. y=5), and to use Python2-style
        // evaluation order. This is both easy to implement and
        // consistent with lexical order:
        //
        //   f(1, 2, x=3, *[4], **dict(z=6)) # 1 2 3 4 6

        // *args
        if let Some(varargs) = varargs {
            self.expr(varargs);
        }

        // **kwargs
        if let Some(kwargs) = kwargs {
            self.expr(kwargs);
        }

        if p >= 256 || n >= 256 {
            // resolve already checked this; should be unreachable
            panic!("too many arguments in call");
        }

        let op = match callmode {
            0 => Opcode::Call,
            1 => Opcode::CallVar,
            2 => Opcode::CallKw,
            _ => Opcode::CallVarKw,
        };
        (op, (p << 8 | n) as u32)
    }

    fn tuple<'e>(&mut self, elems: impl ExactSizeIterator<Item = &'e Expr>) {
        let n = elems.len();
        for elem in elems {
            self.expr(elem);
        }
        self.emit1(Opcode::Maketuple, n as u32);
    }

    fn comprehension(&mut self, comp: &Comprehension, clause_index: usize) {
        if clause_index == comp.clauses.len() {
            self.emit(Opcode::Dup); // accumulator
            if comp.curly {
                // dict: {k:v for ...}
                // Parser ensures that body is of form k:v.
                // Python-style set comprehensions {body for vars in x}
                // are not supported.
                let Expr::DictEntry(entry) = &*comp.body else {
                    panic!("{}: dict comprehension body is not k:v", comp.lbrack);
                };
                self.expr(&entry.key);
                self.expr(&entry.value);
                self.set_pos(&entry.colon);
                self.emit(Opcode::Setdict);
            } else {
                // list: [body for vars in x]
                self.expr(&comp.body);
                self.emit(Opcode::Append);
            }
            return;
        }

        match &comp.clauses[clause_index] {
            Clause::If(clause) => {
                let t = self.new_block();
                let done = self.new_block();
                self.ifelse(&clause.cond, t, done);

                self.block = Some(t);
                self.comprehension(comp, clause_index + 1);
                self.jump(done);

                self.block = Some(done);
            }

            Clause::For(clause) => {
                // Keep consistent with ForStmt.
                let head = self.new_block();
                let body = self.new_block();
                let tail = self.new_block();

                self.expr(&clause.x);
                self.set_pos(&clause.for_);
                self.emit(Opcode::Iterpush);
                self.jump(head);

                self.block = Some(head);
                self.condjump(Opcode::Iterjmp, tail, body);

                self.block = Some(body);
                self.assign(&clause.for_, &clause.vars);
                self.comprehension(comp, clause_index + 1);
                self.jump(head);

                self.block = Some(tail);
                self.emit(Opcode::Iterpop);
            }
        }
    }

    fn function(&mut self, f: &resolve::Function, params: &[Expr], body: Body<'_>) {
        // Evaluation of the defaults may fail, so record the position.
        self.set_pos(&f.pos);

        // To reduce allocation, we emit a combined tuple
        // for the defaults and the freevars.
        // The function knows where to split it at run time.

        // Generate tuple of parameter defaults. For:
        //  def f(p1, p2=dp2, p3=dp3, *, k1, k2=dk2, k3, **kwargs)
        // the tuple is:
        //  (dp2, dp3, MANDATORY, dk2, MANDATORY).
        let mut ndefaults = 0;
        let mut seen_star = false;
        for param in params {
            match param {
                Expr::Binary(param) => {
                    self.expr(&param.y);
                    ndefaults += 1;
                }
                Expr::Unary(_) => seen_star = true, // * or *args (also **kwargs)
                Expr::Ident(_) if seen_star => {
                    self.emit(Opcode::Mandatory);
                    ndefaults += 1;
                }
                _ => {}
            }
        }

        // Capture the cells of the function's
        // free variables from the lexical environment.
        let freevars = f.free_vars.borrow().clone();
        for freevar in &freevars {
            // Don't call self.lookup because we want
            // the cell itself, not its content.
            match freevar.scope() {
                Scope::Free => self.emit1(Opcode::Free, freevar.index as u32),
                Scope::Cell => self.emit1(Opcode::Local, freevar.index as u32),
                _ => {}
            }
        }

        self.emit1(Opcode::Maketuple, (ndefaults + freevars.len()) as u32);

        let locals = f.locals.borrow().clone();
        let mut funcode = self
            .pcomp
            .function(&f.name, f.pos.clone(), body, &locals, &freevars);

        // def f(a, *, b=1) has only 2 parameters.
        let mut num_params = params.len();
        if f.num_kwonly_params.get() > 0 && !f.has_varargs.get() {
            num_params -= 1;
        }

        funcode.num_params = num_params;
        funcode.num_kwonly_params = f.num_kwonly_params.get();
        funcode.has_varargs = f.has_varargs.get();
        funcode.has_kwargs = f.has_kwargs.get();
        let index = self.pcomp.function_index(funcode);
        self.emit1(Opcode::Makefunc, index);
    }

    /// Emits a Boolean control flow decision.
    /// On return, the current block is unset.
    fn ifelse(&mut self, cond: &Expr, t: usize, f: usize) {
        match cond {
            Expr::Unary(u) if u.op == Token::Not => {
                if let Some(x) = &u.x {
                    // if not x then goto t else goto f
                    //    =>
                    // if x then goto f else goto t
                    self.ifelse(x, f, t);
                    return;
                }
            }

            Expr::Binary(b) => match b.op {
                Token::And => {
                    // if x and y then goto t else goto f
                    //    =>
                    // if x then ifelse(y, t, f) else goto f
                    self.expr(&b.x);
                    let y = self.new_block();
                    self.condjump(Opcode::Cjmp, y, f);

                    self.block = Some(y);
                    self.ifelse(&b.y, t, f);
                    return;
                }

                Token::Or => {
                    // if x or y then goto t else goto f
                    //    =>
                    // if x then goto t else ifelse(y, t, f)
                    self.expr(&b.x);
                    let y = self.new_block();
                    self.condjump(Opcode::Cjmp, t, y);

                    self.block = Some(y);
                    self.ifelse(&b.y, t, f);
                    return;
                }

                Token::NotIn => {
                    // if x not in y then goto t else goto f
                    //    =>
                    // if x in y then goto f else goto t
                    self.expr(&b.x);
                    self.expr(&b.y);
                    self.binop(&b.op_pos, Token::In);
                    self.condjump(Opcode::Cjmp, f, t);
                    return;
                }

                _ => {}
            },

            _ => {}
        }

        // general case
        self.expr(cond);
        self.condjump(Opcode::Cjmp, t, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{parse_expr, quote};

    /// A trivial disassembler tailored to the accumulator test.
    fn disassemble(prog: &Program) -> String {
        let f = &prog.toplevel;
        let mut out = Vec::new();
        for insn in &f.code {
            let mut s = insn.op.name().to_string();
            if insn.op.has_arg() {
                let arg = insn.arg as usize;
                match insn.op {
                    Opcode::Constant => match &prog.constants[arg] {
                        Constant::String(x) => s += &format!(" {}", quote(x, false)),
                        Constant::Int(i) => s += &format!(" {i}"),
                        c => s += &format!(" {c:?}"),
                    },
                    Opcode::Local => s += &format!(" {}", f.locals[arg].name),
                    Opcode::Predeclared => s += &format!(" {}", prog.names[arg]),
                    _ => s += &format!("<{arg}>"),
                }
            }
            out.push(s);
        }
        out.join("; ")
    }

    /// Ensures that the compiler generates optimized code for
    /// n-ary addition of strings, lists, and tuples.
    #[test]
    fn test_plus_folding() {
        let is_predeclared = |name: &str| name == "x";
        let is_universal = |_: &str| false;
        let tests = [
            // string folding
            (r#""a" + "b" + "c" + "d""#, r#"constant "abcd"; return"#),
            // string folding with variable:
            (
                r#""a" + "b" + x + "c" + "d""#,
                r#"constant "ab"; predeclared x; plus; constant "cd"; plus; return"#,
            ),
            // list folding
            (
                "[1] + [2] + [3]",
                "constant 1; constant 2; constant 3; makelist<3>; return",
            ),
            // list folding with variable
            (
                "[1] + [2] + x + [3]",
                "constant 1; constant 2; makelist<2>; predeclared x; plus; \
                 constant 3; makelist<1>; plus; return",
            ),
            // tuple folding
            (
                "() + (1,) + (2, 3)",
                "constant 1; constant 2; constant 3; maketuple<3>; return",
            ),
            // tuple folding with variable
            (
                "() + (1,) + x + (2, 3)",
                "constant 1; maketuple<1>; predeclared x; plus; \
                 constant 2; constant 3; maketuple<2>; plus; return",
            ),
        ];
        let opts = FileOptions::default();
        for (src, want) in tests {
            let mut e = parse_expr(&opts, "in.star", src.as_bytes()).unwrap();
            let locals = resolve::expr(&mut e, &is_predeclared, &is_universal).unwrap();
            let got = disassemble(&expr(&opts, &e, "<expr>", &locals));
            assert_eq!(got, want, "expression <<{src}>>");
        }
    }

    #[test]
    fn test_positions_and_jumps() {
        let src = "def f(x):\n    for y in x:\n        if y:\n            break\n    return x\n";
        let mut file =
            crate::syntax::parse(&FileOptions::default(), "p.star", src.as_bytes()).unwrap();
        resolve::file(&mut file, &|_| false, &|_| false).unwrap();
        let m = file.module.clone().unwrap();
        let pos = file.stmts[0].start();
        let prog = super::file(
            &file.options,
            &file.stmts,
            pos,
            "<toplevel>",
            &m.locals,
            &m.globals,
        );
        let f = &prog.functions[0];
        let ops: Vec<String> = f
            .code
            .iter()
            .map(|i| {
                format!(
                    "{}{}@{}:{}",
                    i.op.name(),
                    if i.op.has_arg() {
                        format!("<{}>", i.arg)
                    } else {
                        String::new()
                    },
                    i.line,
                    i.col
                )
            })
            .collect();
        assert_eq!(
            ops.join(" "),
            "local<0>@2:14 iterpush@2:5 iterjmp<7>@0:0 setlocal<1>@0:0 local<1>@3:12 \
             cjmp<7>@0:0 jmp<2>@0:0 iterpop@0:0 local<0>@5:12 return@0:0"
        );
        assert_eq!(f.max_stack, 1);
        assert_eq!(f.num_params, 1);
    }
}
