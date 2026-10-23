// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The Starlark abstract syntax tree (ported from
//! go.starlark.net/syntax/syntax.go).
//!
//! The resolver annotates the tree in place: each [`Ident`] receives its
//! [`Binding`], and each [`DefStmt`] and [`LambdaExpr`] its [`Function`].
//! These annotations use interior mutability so that the resolver can
//! record them while holding shared references to many nodes at once.

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt;

use crate::resolve::{Binding, Function, Module};
use crate::syntax::{FileOptions, Position, Token};

/// A File represents a Starlark file.
#[derive(Clone, Debug, Default)]
pub struct File {
    pub path: String,
    pub stmts: Vec<Stmt>,
    /// Set by the resolver.
    pub module: Option<Module>,
    pub options: FileOptions,
}

impl File {
    /// Returns the start and end position of the file's statements
    /// (invalid positions if there are none).
    pub fn span(&self) -> (Position, Position) {
        match (self.stmts.first(), self.stmts.last()) {
            (Some(first), Some(last)) => (first.span().0, last.span().1),
            _ => (Position::default(), Position::default()),
        }
    }
}

/// A Stmt is a Starlark statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    Assign(AssignStmt),
    Branch(BranchStmt),
    Def(DefStmt),
    Expr(ExprStmt),
    For(ForStmt),
    While(WhileStmt),
    If(IfStmt),
    Load(LoadStmt),
    Return(ReturnStmt),
}

impl Stmt {
    /// Returns the start and end position of the statement.
    pub fn span(&self) -> (Position, Position) {
        match self {
            Stmt::Assign(s) => s.span(),
            Stmt::Branch(s) => s.span(),
            Stmt::Def(s) => s.span(),
            Stmt::Expr(s) => s.x.span(),
            Stmt::For(s) => s.span(),
            Stmt::While(s) => s.span(),
            Stmt::If(s) => s.span(),
            Stmt::Load(s) => (s.load.clone(), s.rparen.clone()),
            Stmt::Return(s) => s.span(),
        }
    }

    /// Returns the start position of the statement.
    pub fn start(&self) -> Position {
        self.span().0
    }

    /// Returns the end position of the statement.
    pub fn end(&self) -> Position {
        self.span().1
    }
}

fn stmts_end(body: &[Stmt]) -> Position {
    body.last().map(Stmt::end).unwrap_or_default()
}

/// An AssignStmt represents an assignment:
///
/// ```text
/// x = 0
/// x, y = y, x
/// x += 1
/// ```
#[derive(Clone, Debug)]
pub struct AssignStmt {
    pub op_pos: Position,
    /// = EQ | {PLUS,MINUS,STAR,PERCENT}_EQ
    pub op: Token,
    pub lhs: Expr,
    pub rhs: Expr,
}

impl AssignStmt {
    pub fn span(&self) -> (Position, Position) {
        (self.lhs.start(), self.rhs.end())
    }
}

/// A DefStmt represents a function definition.
#[derive(Clone, Debug)]
pub struct DefStmt {
    pub def: Position,
    pub name: Ident,
    pub lparen: Position,
    /// param = ident | ident=expr | * | *ident | **ident
    pub params: Vec<Expr>,
    pub rparen: Position,
    pub body: Vec<Stmt>,
    /// Set by the resolver.
    pub function: RefCell<Option<Rc<Function>>>,
}

impl DefStmt {
    pub fn span(&self) -> (Position, Position) {
        (self.def.clone(), stmts_end(&self.body))
    }
}

/// An ExprStmt is an expression evaluated for side effects.
#[derive(Clone, Debug)]
pub struct ExprStmt {
    pub x: Expr,
}

/// An IfStmt is a conditional: If Cond: True; else: False.
/// 'elseif' is desugared into a chain of IfStmts.
#[derive(Clone, Debug)]
pub struct IfStmt {
    /// IF or ELIF
    pub if_: Position,
    pub cond: Expr,
    pub true_: Vec<Stmt>,
    /// ELSE or ELIF
    pub else_pos: Position,
    /// optional
    pub false_: Vec<Stmt>,
}

impl IfStmt {
    pub fn span(&self) -> (Position, Position) {
        let body = if self.false_.is_empty() {
            &self.true_
        } else {
            &self.false_
        };
        (self.if_.clone(), stmts_end(body))
    }
}

/// A LoadStmt loads another module and binds names from it:
/// load(Module, "x", y="foo").
///
/// The AST is slightly unfaithful to the concrete syntax here because
/// Starlark's load statement, so that it can be implemented in Python,
/// binds some names (like y above) with an identifier and some (like x)
/// without.  For consistency we create fake identifiers for all the
/// strings.
#[derive(Clone, Debug)]
pub struct LoadStmt {
    pub load: Position,
    /// a string
    pub module: Literal,
    /// name defined in loading module
    pub from: Vec<Ident>,
    /// name in loaded module
    pub to: Vec<Ident>,
    pub rparen: Position,
}

impl LoadStmt {
    /// Returns the name of the module loaded by this statement.
    pub fn module_name(&self) -> String {
        match &self.module.value {
            LiteralValue::String(s) => String::from_utf8_lossy(s).into_owned(),
            _ => String::new(),
        }
    }
}

/// A BranchStmt changes the flow of control: break, continue, pass.
#[derive(Clone, Debug)]
pub struct BranchStmt {
    /// = BREAK | CONTINUE | PASS
    pub token: Token,
    pub token_pos: Position,
}

impl BranchStmt {
    pub fn span(&self) -> (Position, Position) {
        (
            self.token_pos.clone(),
            self.token_pos.add(self.token.name().as_bytes()),
        )
    }
}

/// A ReturnStmt returns from a function.
#[derive(Clone, Debug)]
pub struct ReturnStmt {
    pub return_: Position,
    /// may be None
    pub result: Option<Expr>,
}

impl ReturnStmt {
    pub fn span(&self) -> (Position, Position) {
        match &self.result {
            None => (self.return_.clone(), self.return_.add(b"return")),
            Some(r) => (self.return_.clone(), r.end()),
        }
    }
}

/// A ForStmt represents a loop: for Vars in X: Body.
#[derive(Clone, Debug)]
pub struct ForStmt {
    pub for_: Position,
    /// name, or tuple of names
    pub vars: Expr,
    pub x: Expr,
    pub body: Vec<Stmt>,
}

impl ForStmt {
    pub fn span(&self) -> (Position, Position) {
        (self.for_.clone(), stmts_end(&self.body))
    }
}

/// A WhileStmt represents a while loop: while X: Body.
#[derive(Clone, Debug)]
pub struct WhileStmt {
    pub while_: Position,
    pub cond: Expr,
    pub body: Vec<Stmt>,
}

impl WhileStmt {
    pub fn span(&self) -> (Position, Position) {
        (self.while_.clone(), stmts_end(&self.body))
    }
}

/// An Expr is a Starlark expression.
#[derive(Clone, Debug)]
pub enum Expr {
    Binary(BinaryExpr),
    Call(CallExpr),
    Comprehension(Comprehension),
    Cond(CondExpr),
    /// Used only as the body of a dict comprehension.
    DictEntry(Box<DictEntry>),
    Dict(DictExpr),
    Dot(DotExpr),
    Ident(Ident),
    Index(IndexExpr),
    Lambda(LambdaExpr),
    List(ListExpr),
    Literal(Literal),
    Paren(ParenExpr),
    Slice(SliceExpr),
    Tuple(TupleExpr),
    Unary(UnaryExpr),
}

impl Expr {
    /// Returns the start and end position of the expression.
    pub fn span(&self) -> (Position, Position) {
        match self {
            Expr::Binary(e) => (e.x.start(), e.y.end()),
            Expr::Call(e) => (e.func.start(), e.rparen.add(b")")),
            Expr::Comprehension(e) => (e.lbrack.clone(), e.rbrack.add(b"]")),
            Expr::Cond(e) => (e.true_.start(), e.false_.end()),
            Expr::DictEntry(e) => e.span(),
            Expr::Dict(e) => (e.lbrace.clone(), e.rbrace.add(b"}")),
            Expr::Dot(e) => (e.x.start(), e.name.span().1),
            Expr::Ident(e) => e.span(),
            Expr::Index(e) => (e.x.start(), e.rbrack.clone()),
            Expr::Lambda(e) => (e.lambda.clone(), e.body.end()),
            Expr::List(e) => (e.lbrack.clone(), e.rbrack.add(b"]")),
            Expr::Literal(e) => e.span(),
            Expr::Paren(e) => (e.lparen.clone(), e.rparen.add(b")")),
            Expr::Slice(e) => (e.x.start(), e.rbrack.clone()),
            Expr::Tuple(e) => e.span(),
            Expr::Unary(e) => {
                let end = match &e.x {
                    Some(x) => x.end(),
                    None => e.op_pos.add(b"*"),
                };
                (e.op_pos.clone(), end)
            }
        }
    }

    /// Returns the start position of the expression.
    pub fn start(&self) -> Position {
        self.span().0
    }

    /// Returns the end position of the expression.
    pub fn end(&self) -> Position {
        self.span().1
    }

    /// Returns the lower-case name of the node type (as used by the
    /// resolver's "can't assign to" message).
    pub fn type_name(&self) -> &'static str {
        match self {
            Expr::Binary(_) => "binaryexpr",
            Expr::Call(_) => "callexpr",
            Expr::Comprehension(_) => "comprehension",
            Expr::Cond(_) => "condexpr",
            Expr::DictEntry(_) => "dictentry",
            Expr::Dict(_) => "dictexpr",
            Expr::Dot(_) => "dotexpr",
            Expr::Ident(_) => "ident",
            Expr::Index(_) => "indexexpr",
            Expr::Lambda(_) => "lambdaexpr",
            Expr::List(_) => "listexpr",
            Expr::Literal(_) => "literal",
            Expr::Paren(_) => "parenexpr",
            Expr::Slice(_) => "sliceexpr",
            Expr::Tuple(_) => "tupleexpr",
            Expr::Unary(_) => "unaryexpr",
        }
    }
}

/// An Ident represents an identifier.
#[derive(Clone, Debug)]
pub struct Ident {
    pub name_pos: Position,
    pub name: String,
    /// Set by the resolver.
    pub binding: RefCell<Option<Rc<Binding>>>,
}

impl Ident {
    pub fn new(name_pos: Position, name: String) -> Ident {
        Ident {
            name_pos,
            name,
            binding: RefCell::new(None),
        }
    }

    pub fn span(&self) -> (Position, Position) {
        (
            self.name_pos.clone(),
            self.name_pos.add(self.name.as_bytes()),
        )
    }

    /// Returns the binding recorded by the resolver, if any.
    pub fn binding(&self) -> Option<Rc<Binding>> {
        self.binding.borrow().clone()
    }
}

/// The value of a [`Literal`].
#[derive(Clone, Debug, PartialEq)]
pub enum LiteralValue {
    /// An int literal that fits in an i64.
    Int(i64),
    /// An int literal that does not fit in an i64.
    BigInt(Rc<BigInt>),
    /// A float literal.
    Float(f64),
    /// The decoded value of a string or bytes literal.
    String(Vec<u8>),
}

/// A Literal represents a literal string or number.
#[derive(Clone, Debug)]
pub struct Literal {
    /// = STRING | BYTES | INT | FLOAT
    pub token: Token,
    pub token_pos: Position,
    /// uninterpreted text
    pub raw: String,
    pub value: LiteralValue,
}

impl Literal {
    pub fn span(&self) -> (Position, Position) {
        (
            self.token_pos.clone(),
            self.token_pos.add(self.raw.as_bytes()),
        )
    }
}

/// A ParenExpr represents a parenthesized expression: (X).
#[derive(Clone, Debug)]
pub struct ParenExpr {
    pub lparen: Position,
    pub x: Box<Expr>,
    pub rparen: Position,
}

/// A CallExpr represents a function call expression: Fn(Args).
#[derive(Clone, Debug)]
pub struct CallExpr {
    pub func: Box<Expr>,
    pub lparen: Position,
    /// arg = expr | ident=expr | *expr | **expr
    pub args: Vec<Expr>,
    pub rparen: Position,
}

/// A DotExpr represents a field or method selector: X.Name.
#[derive(Clone, Debug)]
pub struct DotExpr {
    pub x: Box<Expr>,
    pub dot: Position,
    pub name: Ident,
}

/// A Comprehension represents a list or dict comprehension:
/// [Body for ... if ...] or {Body for ... if ...}
#[derive(Clone, Debug)]
pub struct Comprehension {
    /// {x:y for ...} or {x for ...}, not [x for ...]
    pub curly: bool,
    pub lbrack: Position,
    pub body: Box<Expr>,
    pub clauses: Vec<Clause>,
    pub rbrack: Position,
}

/// A clause of a comprehension.
#[derive(Clone, Debug)]
pub enum Clause {
    For(ForClause),
    If(IfClause),
}

impl Clause {
    pub fn span(&self) -> (Position, Position) {
        match self {
            Clause::For(c) => (c.for_.clone(), c.x.end()),
            Clause::If(c) => (c.if_.clone(), c.cond.end()),
        }
    }
}

/// A ForClause represents a for clause in a list comprehension: for Vars in X.
#[derive(Clone, Debug)]
pub struct ForClause {
    pub for_: Position,
    /// name, or tuple of names
    pub vars: Expr,
    pub in_: Position,
    pub x: Expr,
}

/// An IfClause represents an if clause in a list comprehension: if Cond.
#[derive(Clone, Debug)]
pub struct IfClause {
    pub if_: Position,
    pub cond: Expr,
}

/// A DictExpr represents a dictionary literal: { List }.
#[derive(Clone, Debug)]
pub struct DictExpr {
    pub lbrace: Position,
    pub list: Vec<DictEntry>,
    pub rbrace: Position,
}

/// A DictEntry represents a dictionary entry: Key: Value.
/// Used only within a DictExpr or as the body of a dict comprehension.
#[derive(Clone, Debug)]
pub struct DictEntry {
    pub key: Expr,
    pub colon: Position,
    pub value: Expr,
}

impl DictEntry {
    pub fn span(&self) -> (Position, Position) {
        (self.key.start(), self.value.end())
    }
}

/// A LambdaExpr represents an inline function abstraction.
#[derive(Clone, Debug)]
pub struct LambdaExpr {
    pub lambda: Position,
    /// param = ident | ident=expr | * | *ident | **ident
    pub params: Vec<Expr>,
    pub body: Box<Expr>,
    /// Set by the resolver.
    pub function: RefCell<Option<Rc<Function>>>,
}

/// A ListExpr represents a list literal: [ List ].
#[derive(Clone, Debug)]
pub struct ListExpr {
    pub lbrack: Position,
    pub list: Vec<Expr>,
    pub rbrack: Position,
}

/// CondExpr represents the conditional: X if COND else ELSE.
#[derive(Clone, Debug)]
pub struct CondExpr {
    pub if_: Position,
    pub cond: Box<Expr>,
    pub true_: Box<Expr>,
    pub else_pos: Position,
    pub false_: Box<Expr>,
}

/// A TupleExpr represents a tuple literal: (List).
#[derive(Clone, Debug)]
pub struct TupleExpr {
    /// optional (e.g. in x, y = 0, 1), but required if List is empty
    pub lparen: Position,
    pub list: Vec<Expr>,
    pub rparen: Position,
}

impl TupleExpr {
    pub fn span(&self) -> (Position, Position) {
        if self.lparen.is_valid() {
            (self.lparen.clone(), self.rparen.clone())
        } else {
            let start = self.list.first().map(Expr::start).unwrap_or_default();
            let end = self.list.last().map(Expr::end).unwrap_or_default();
            (start, end)
        }
    }
}

/// A UnaryExpr represents a unary expression: Op X.
///
/// As a special case, UnaryOp{Op:Star} may also represent
/// the star parameter in def f(*args) or def f(*, x).
#[derive(Clone, Debug)]
pub struct UnaryExpr {
    pub op_pos: Position,
    pub op: Token,
    /// may be None if Op==STAR
    pub x: Option<Box<Expr>>,
}

/// A BinaryExpr represents a binary expression: X Op Y.
///
/// As a special case, BinaryExpr{Op:EQ} may also
/// represent a named argument in a call f(k=v)
/// or a named parameter in a function declaration
/// def f(param=default).
#[derive(Clone, Debug)]
pub struct BinaryExpr {
    pub x: Box<Expr>,
    pub op_pos: Position,
    pub op: Token,
    pub y: Box<Expr>,
}

/// A SliceExpr represents a slice or substring expression: X[Lo:Hi:Step].
#[derive(Clone, Debug)]
pub struct SliceExpr {
    pub x: Box<Expr>,
    pub lbrack: Position,
    pub lo: Option<Box<Expr>>,
    pub hi: Option<Box<Expr>>,
    pub step: Option<Box<Expr>>,
    pub rbrack: Position,
}

/// An IndexExpr represents an index expression: X[Y].
#[derive(Clone, Debug)]
pub struct IndexExpr {
    pub x: Box<Expr>,
    pub lbrack: Position,
    pub y: Box<Expr>,
    pub rbrack: Position,
}
