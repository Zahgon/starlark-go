// Copyright 2019 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Resolver data types saved in the syntax tree (ported from
//! go.starlark.net/resolve/binding.go).
//!
//! We cannot guarantee API stability for these types
//! as they are closely tied to the implementation.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;

use crate::syntax::Position;

/// The Scope of Binding indicates what kind of scope it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// name is not defined
    Undefined,
    /// name is local to its function or file
    Local,
    /// name is function-local but shared with a nested function
    Cell,
    /// name is cell of some enclosing function
    Free,
    /// name is global to module
    Global,
    /// name is predeclared for this module (e.g. glob)
    Predeclared,
    /// name is universal (e.g. len)
    Universal,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Undefined => "undefined",
            Scope::Local => "local",
            Scope::Cell => "cell",
            Scope::Free => "free",
            Scope::Global => "global",
            Scope::Predeclared => "predeclared",
            Scope::Universal => "universal",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The name and position of the identifier of a binding use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingIdent {
    pub name: String,
    pub name_pos: Position,
}

/// A Binding contains resolver information about an identifier.
/// The resolver populates the binding field of each syntax::Ident.
/// The Binding ties together all identifiers that denote the same variable.
#[derive(Debug)]
pub struct Binding {
    /// The scope; a Local binding may later become a Cell.
    pub scope: Cell<Scope>,

    /// Index records the index into the enclosing
    /// - Function/Module locals, if scope==Local
    /// - Function free_vars,     if scope==Free
    /// - Module globals,         if scope==Global.
    ///
    /// It is zero if scope is Predeclared, Universal, or Undefined.
    pub index: usize,

    /// First binding use (iff scope==Local/Free/Global).
    pub first: Option<BindingIdent>,
}

impl Binding {
    pub(crate) fn new(scope: Scope, index: usize, first: Option<BindingIdent>) -> Rc<Binding> {
        Rc::new(Binding {
            scope: Cell::new(scope),
            index,
            first,
        })
    }

    /// Returns the current scope of the binding.
    pub fn scope(&self) -> Scope {
        self.scope.get()
    }

    /// Returns the name of the first binding use ("" if none).
    pub fn name(&self) -> &str {
        self.first.as_ref().map_or("", |f| f.name.as_str())
    }

    /// Returns the position of the first binding use (invalid if none).
    pub fn pos(&self) -> Position {
        self.first
            .as_ref()
            .map(|f| f.name_pos.clone())
            .unwrap_or_default()
    }
}

/// A Module contains resolver information about a file.
/// The resolver populates the module field of each syntax::File.
#[derive(Clone, Debug, Default)]
pub struct Module {
    /// The file's (comprehension-)local variables.
    pub locals: Vec<Rc<Binding>>,
    /// The file's global variables.
    pub globals: Vec<Rc<Binding>>,
}

/// A Function contains resolver information about a named or anonymous
/// function. The resolver populates the function field of each
/// syntax::DefStmt and syntax::LambdaExpr. (The parameters and body are
/// those of the syntax node; a lambda's body is implicitly returned.)
#[derive(Debug)]
pub struct Function {
    /// of DEF or LAMBDA
    pub pos: Position,
    /// name of def, or "lambda"
    pub name: String,

    /// whether params includes *args (convenience)
    pub has_varargs: Cell<bool>,
    /// whether params includes **kwargs (convenience)
    pub has_kwargs: Cell<bool>,
    /// number of keyword-only optional parameters
    pub num_kwonly_params: Cell<usize>,
    /// this function's local/cell variables, parameters first
    pub locals: RefCell<Vec<Rc<Binding>>>,
    /// enclosing cells to capture in closure
    pub free_vars: RefCell<Vec<Rc<Binding>>>,
}

impl Function {
    pub(crate) fn new(name: &str, pos: Position) -> Rc<Function> {
        Rc::new(Function {
            pos,
            name: name.to_string(),
            has_varargs: Cell::new(false),
            has_kwargs: Cell::new(false),
            num_kwonly_params: Cell::new(0),
            locals: RefCell::new(Vec::new()),
            free_vars: RefCell::new(Vec::new()),
        })
    }
}
