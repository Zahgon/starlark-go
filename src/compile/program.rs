// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Compiled program representation (ported from
//! go.starlark.net/internal/compile/compile.go).
//!
//! Unlike the Go original, instructions are kept as a vector of decoded
//! `Insn` values, each carrying the source position that was current when it
//! was emitted, so no line-number table needs to be encoded or decoded.
//! Jump operands are instruction indices.

use std::fmt;
use std::rc::Rc;

use num_bigint::BigInt;

use crate::syntax::Position;

/// An Opcode is a bytecode operation. The declaration order matches the Go
/// original: the comparison and arithmetic opcodes follow the order of the
/// corresponding tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Opcode {
    Nop,
    Dup,
    Dup2,
    Pop,
    Exch,
    Lt,
    Gt,
    Ge,
    Le,
    Eql,
    Neq,
    Plus,
    Minus,
    Star,
    Slash,
    Slashslash,
    Percent,
    Amp,
    Pipe,
    Circumflex,
    Ltlt,
    Gtgt,
    In,
    Uplus,
    Uminus,
    Tilde,
    None,
    True,
    False,
    Mandatory,
    Iterpush,
    Iterpop,
    Not,
    Return,
    Setindex,
    Index,
    Setdict,
    Setdictuniq,
    Append,
    Slice,
    InplaceAdd,
    InplacePipe,
    Makedict,
    Jmp,
    Cjmp,
    Iterjmp,
    Constant,
    Maketuple,
    Makelist,
    Makefunc,
    Load,
    Setlocal,
    Setglobal,
    Local,
    Free,
    Freecell,
    Localcell,
    Setlocalcell,
    Global,
    Predeclared,
    Universal,
    Attr,
    Setfield,
    Unpack,
    Call,
    CallVar,
    CallKw,
    CallVarKw,
}

impl Opcode {
    /// The first opcode that takes an argument.
    pub const ARG_MIN: Opcode = Opcode::Jmp;

    /// The highest-numbered opcode.
    pub const MAX: Opcode = Opcode::CallVarKw;

    /// The opcode's mnemonic (as printed by the disassembler).
    pub fn name(self) -> &'static str {
        match self {
            Opcode::Nop => "nop",
            Opcode::Dup => "dup",
            Opcode::Dup2 => "dup2",
            Opcode::Pop => "pop",
            Opcode::Exch => "exch",
            Opcode::Lt => "lt",
            Opcode::Gt => "gt",
            Opcode::Ge => "ge",
            Opcode::Le => "le",
            Opcode::Eql => "eql",
            Opcode::Neq => "neq",
            Opcode::Plus => "plus",
            Opcode::Minus => "minus",
            Opcode::Star => "star",
            Opcode::Slash => "slash",
            Opcode::Slashslash => "slashslash",
            Opcode::Percent => "percent",
            Opcode::Amp => "amp",
            Opcode::Pipe => "pipe",
            Opcode::Circumflex => "circumflex",
            Opcode::Ltlt => "ltlt",
            Opcode::Gtgt => "gtgt",
            Opcode::In => "in",
            Opcode::Uplus => "uplus",
            Opcode::Uminus => "uminus",
            Opcode::Tilde => "tilde",
            Opcode::None => "none",
            Opcode::True => "true",
            Opcode::False => "false",
            Opcode::Mandatory => "mandatory",
            Opcode::Iterpush => "iterpush",
            Opcode::Iterpop => "iterpop",
            Opcode::Not => "not",
            Opcode::Return => "return",
            Opcode::Setindex => "setindex",
            Opcode::Index => "index",
            Opcode::Setdict => "setdict",
            Opcode::Setdictuniq => "setdictuniq",
            Opcode::Append => "append",
            Opcode::Slice => "slice",
            Opcode::InplaceAdd => "inplace_add",
            Opcode::InplacePipe => "inplace_pipe",
            Opcode::Makedict => "makedict",
            Opcode::Jmp => "jmp",
            Opcode::Cjmp => "cjmp",
            Opcode::Iterjmp => "iterjmp",
            Opcode::Constant => "constant",
            Opcode::Maketuple => "maketuple",
            Opcode::Makelist => "makelist",
            Opcode::Makefunc => "makefunc",
            Opcode::Load => "load",
            Opcode::Setlocal => "setlocal",
            Opcode::Setglobal => "setglobal",
            Opcode::Local => "local",
            Opcode::Free => "free",
            Opcode::Freecell => "freecell",
            Opcode::Localcell => "localcell",
            Opcode::Setlocalcell => "setlocalcell",
            Opcode::Global => "global",
            Opcode::Predeclared => "predeclared",
            Opcode::Universal => "universal",
            Opcode::Attr => "attr",
            Opcode::Setfield => "setfield",
            Opcode::Unpack => "unpack",
            Opcode::Call => "call",
            Opcode::CallVar => "call_var",
            Opcode::CallKw => "call_kw ",
            Opcode::CallVarKw => "call_var_kw",
        }
    }

    /// Reports whether the opcode takes an argument.
    pub fn has_arg(self) -> bool {
        self >= Opcode::ARG_MIN
    }
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One instruction with its operand (0 if none) and the source position
/// that was current when it was emitted (line and column; the file is that
/// of the Funcode). `line == 0` means the instruction has no position (for
/// example the jumps the code generator inserts between blocks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub op: Opcode,
    pub arg: u32,
    pub line: i32,
    pub col: i32,
}

/// A compile-time constant.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// A string literal (arbitrary bytes).
    String(Rc<[u8]>),
    /// A bytes literal.
    Bytes(Rc<[u8]>),
    /// An int literal that fits in an i64.
    Int(i64),
    /// An int literal that does not fit in an i64.
    BigInt(Rc<BigInt>),
    /// A float literal.
    Float(f64),
}

/// A Binding is the name and position of a binding identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub pos: Position,
}

/// A Program is a Starlark file in executable form.
#[derive(Debug)]
pub struct Program {
    /// Name (really, module string) and position of each load statement.
    pub loads: Vec<Binding>,
    /// Names of attributes and predeclared variables.
    pub names: Vec<String>,
    pub constants: Vec<Constant>,
    pub functions: Vec<Rc<Funcode>>,
    /// Global variables, for error messages and tracing.
    pub globals: Vec<Binding>,
    /// The module initialization function.
    pub toplevel: Rc<Funcode>,
    /// Disables the recursion check for functions in this file.
    pub recursion: bool,
}

/// A Funcode is the code of a compiled Starlark function.
#[derive(Debug)]
pub struct Funcode {
    /// Position of the def or lambda token (or of the file/expression).
    pub pos: Position,
    /// Name of this function ("lambda" for anonymous functions).
    pub name: String,
    /// Docstring of this function.
    pub doc: String,
    pub code: Vec<Insn>,
    /// Locals, parameters first.
    pub locals: Vec<Binding>,
    /// Indices of locals that require cells.
    pub cells: Vec<usize>,
    /// Free variables, for tracing.
    pub freevars: Vec<Binding>,
    pub max_stack: usize,
    pub num_params: usize,
    pub num_kwonly_params: usize,
    pub has_varargs: bool,
    pub has_kwargs: bool,
}

impl Funcode {
    /// Returns the source position for the instruction at index `pc`,
    /// with the semantics of the Go original's line-number table: the
    /// position of the last instruction at or before `pc` that carries a
    /// position (line != 0); if there is none, that of the first such
    /// instruction; if no instruction has a position, line and column 0.
    pub fn position(&self, pc: usize) -> Position {
        let upto = pc.min(self.code.len().saturating_sub(1));
        let found = self.code[..self.code.len().min(upto + 1)]
            .iter()
            .rev()
            .find(|insn| insn.line != 0)
            .or_else(|| self.code.iter().find(|insn| insn.line != 0));
        match found {
            Some(insn) => self.pos.with_line_col(insn.line, insn.col),
            None => self.pos.with_line_col(0, 0),
        }
    }
}
