// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Package resolve defines a name-resolution pass for Starlark abstract
//! syntax trees (ported from go.starlark.net/resolve).
//!
//! The resolver sets the locals and free_vars of each function and the
//! binding of each syntax::Ident. It also sets the Module of a File,
//! listing the locals bound by top-level comprehensions and load
//! statements, and the globals.
//!
//! All references to names are statically resolved.  Names may be
//! predeclared, global, or local to a function or file.
//! File-local variables include those bound by top-level comprehensions
//! and by load statements. ("Top-level" means "outside of any function".)
//! The resolver maps each global name to a small integer and each local
//! name to a small integer; these integers enable a fast and compact
//! representation of globals and locals in the evaluator.
//!
//! As an optimization, the resolver classifies each predeclared name as
//! either universal (e.g. None, len) or per-module (e.g. glob in Bazel's
//! build language), enabling the evaluator to share the representation
//! of the universal environment across all modules.
//!
//! The lexical environment is a tree of blocks with the file block at
//! its root. The file's child blocks may be of two kinds: functions
//! and comprehensions, and these may have further children of either
//! kind.
//!
//! Python-style resolution requires multiple passes because a name is
//! determined to be local to a function only if the function contains a
//! "binding" use of it; similarly, a name is determined to be global (as
//! opposed to predeclared) if the module contains a top-level binding use.
//! Unlike ordinary top-level assignments, the bindings created by load
//! statements are local to the file block.
//! A non-binding use may lexically precede the binding to which it is resolved.
//! In the first pass, we inspect each function, recording in
//! 'uses' each identifier and the environment block in which it occurs.
//! If a use of a name is binding, such as a function parameter or
//! assignment, we add the name to the block's bindings mapping and add a
//! local variable to the enclosing function.
//!
//! As we finish resolving each function, we inspect all the uses within
//! that function and discard ones that were found to be function-local. The
//! remaining ones must be either free (local to some lexically enclosing
//! function), or top-level (global, predeclared, or file-local), but we cannot tell
//! which until we have finished inspecting the outermost enclosing
//! function. At that point, we can distinguish local from top-level names
//! (and this is when Python would compute free variables).
//!
//! However, Starlark additionally requires that all references to global
//! names are satisfied by some declaration in the current module;
//! Starlark permits a function to forward-reference a global or file-local
//! that has not been declared yet so long as it is declared before the end
//! of the module.  So, instead of re-resolving the unresolved references after
//! each top-level function, we defer this until the end of the module
//! and ensure that all such references are satisfied by some definition.
//!
//! At the end of the module, we visit each of the nested function blocks
//! in bottom-up order, doing a recursive lexical lookup for each
//! unresolved name.  If the name is found to be local to some enclosing
//! function, we must create a free variable (capture) for each
//! intervening function.  We enter these synthetic bindings into
//! the bindings map so that we create at most one freevar per name.  If
//! the name was not local, we check that it was defined at module level.
//!
//! We resolve all uses of locals in the module (due to load statements
//! and comprehensions) in a similar way and compute the file's set of
//! local variables.
//!
//! Starlark enforces that all global names are assigned at most once on
//! all control flow paths by forbidding if/else statements and loops at
//! top level. A global may be used before it is defined, leading to a
//! dynamic error. However, the global_reassign option (really: allow
//! top-level reassign) makes the resolver allow multiple assignments to a
//! variable at top-level. It also allows if-, for-, and while-loops at
//! top-level, which in turn may make the evaluator dynamically assign
//! multiple values to a variable at top-level. (These two roles should be
//! separated.)

mod binding;

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

pub use binding::{Binding, BindingIdent, Function, Module, Scope};

use crate::spell;
use crate::syntax::{Clause, Expr, File, FileOptions, Ident, Position, Stmt, Token};

const DOESNT: &str = "this Starlark dialect does not ";

/// An Error describes the nature and position of a resolver error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub pos: Position,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for Error {}

/// An ErrorList is a non-empty list of resolver error messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorList(pub Vec<Error>);

impl fmt::Display for ErrorList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.first() {
            Some(e) => e.fmt(f),
            None => Ok(()),
        }
    }
}

impl std::error::Error for ErrorList {}

/// A predicate on names.
pub type NamePredicate<'p> = &'p dyn Fn(&str) -> bool;

/// Resolves the specified file and records information about the
/// module in file.module.
///
/// The is_predeclared and is_universal predicates report whether a name is
/// a pre-declared identifier (visible in the current module) or a
/// universal identifier (visible in every module).
/// Clients should typically pass the module's predeclared names for the
/// first and the universe for the second.
pub fn file(
    f: &mut File,
    is_predeclared: NamePredicate<'_>,
    is_universal: NamePredicate<'_>,
) -> Result<(), ErrorList> {
    resolve_chunk(f, None, is_predeclared, is_universal)
}

/// A generalization of [`file`] that supports a non-empty initial global
/// block, as occurs in a REPL.
pub fn repl_chunk(
    f: &mut File,
    is_global: NamePredicate<'_>,
    is_predeclared: NamePredicate<'_>,
    is_universal: NamePredicate<'_>,
) -> Result<(), ErrorList> {
    resolve_chunk(f, Some(is_global), is_predeclared, is_universal)
}

fn resolve_chunk(
    f: &mut File,
    is_global: Option<NamePredicate<'_>>,
    is_predeclared: NamePredicate<'_>,
    is_universal: NamePredicate<'_>,
) -> Result<(), ErrorList> {
    let (module, errors) = {
        let file: &File = f;
        let mut r = Resolver::new(&file.options, is_global, is_predeclared, is_universal);
        r.stmts(&file.stmts);

        r.resolve_local_uses(r.env);

        // At the end of the module, resolve all non-local variable references,
        // computing closures.
        // Function bodies may contain forward references to later global declarations.
        r.resolve_non_local_uses(r.env);

        let module = Module {
            locals: std::mem::take(&mut r.module_locals),
            globals: std::mem::take(&mut r.module_globals),
        };
        (module, std::mem::take(&mut r.errors))
    };
    f.module = Some(module);

    if !errors.is_empty() {
        return Err(ErrorList(errors));
    }
    Ok(())
}

/// Resolves the specified expression using the options of the original
/// implementation's legacy global flags at their default values (Go's
/// `resolve.Expr`). It returns the local variables bound within the
/// expression.
pub fn expr(
    e: &mut Expr,
    is_predeclared: NamePredicate<'_>,
    is_universal: NamePredicate<'_>,
) -> Result<Vec<Rc<Binding>>, ErrorList> {
    expr_options(
        &FileOptions::legacy(true, false, false),
        e,
        is_predeclared,
        is_universal,
    )
}

/// Resolves the specified expression with the given options (Go's
/// `resolve.ExprOptions`). It returns the local variables bound within
/// the expression.
pub fn expr_options(
    opts: &FileOptions,
    e: &mut Expr,
    is_predeclared: NamePredicate<'_>,
    is_universal: NamePredicate<'_>,
) -> Result<Vec<Rc<Binding>>, ErrorList> {
    let e: &Expr = e;
    let mut r = Resolver::new(opts, None, is_predeclared, is_universal);
    r.expr(e);
    r.resolve_local_uses(r.env);
    r.resolve_non_local_uses(r.env); // globals & universals
    if !r.errors.is_empty() {
        return Err(ErrorList(std::mem::take(&mut r.errors)));
    }
    Ok(std::mem::take(&mut r.module_locals))
}

/// A use records an identifier and the environment in which it appears.
#[derive(Clone, Copy)]
struct Use<'a> {
    id: &'a Ident,
    env: usize,
}

struct Block<'a> {
    /// None for file block.
    parent: Option<usize>,

    /// In the file (root) block, this is None, and also in
    /// comprehension blocks; it is set only for function blocks.
    function: Option<Rc<Function>>,

    /// Maps a name to its binding.
    /// A local binding has an index into its innermost enclosing container's locals array.
    /// A free binding has an index into its innermost enclosing function's freevars array.
    bindings: HashMap<String, Rc<Binding>>,

    /// Records the child blocks of the current one.
    children: Vec<usize>,

    /// Records all identifiers seen in this container (function or file),
    /// and a reference to the environment in which they appear.
    /// As we leave each container block, we resolve them,
    /// so that only free and global ones remain.
    /// At the end of each top-level function we compute closures.
    uses: Vec<Use<'a>>,
}

impl<'a> Block<'a> {
    fn new(function: Option<Rc<Function>>) -> Block<'a> {
        Block {
            parent: None,
            function,
            bindings: HashMap::new(),
            children: Vec::new(),
            uses: Vec::new(),
        }
    }
}

/// The body of a function being resolved.
#[derive(Clone, Copy)]
enum Body<'a> {
    /// The statements of a def.
    Stmts(&'a [Stmt]),
    /// The (implicitly returned) expression of a lambda.
    Expr(&'a Expr),
}

/// The file (root) block is always the first in the arena.
const FILE_BLOCK: usize = 0;

struct Resolver<'a, 'p> {
    options: FileOptions,

    /// All blocks; FILE_BLOCK is the file block (contains load bindings).
    blocks: Vec<Block<'a>>,
    /// The current local environment (innermost block).
    env: usize,

    /// Contains the local variables of the module
    /// (due to load statements and comprehensions outside any function).
    module_locals: Vec<Rc<Binding>>,
    /// Contains the global variables of the module.
    module_globals: Vec<Rc<Binding>>,

    /// Maps each global name in the module to its binding.
    globals: HashMap<String, Rc<Binding>>,
    /// Does the same for predeclared and universal names.
    predeclared: HashMap<String, Rc<Binding>>,

    // These predicates report whether a name is
    // pre-declared, either in this module or universally,
    // or already declared in the module globals (as in a REPL).
    is_global: Option<NamePredicate<'p>>,
    is_predeclared: NamePredicate<'p>,
    is_universal: NamePredicate<'p>,

    /// Number of enclosing for/while loops in current function (or file, if top-level).
    loops: usize,
    /// Number of enclosing if statements.
    ifstmts: usize,

    errors: Vec<Error>,
}

impl<'a, 'p> Resolver<'a, 'p> {
    fn new(
        options: &FileOptions,
        is_global: Option<NamePredicate<'p>>,
        is_predeclared: NamePredicate<'p>,
        is_universal: NamePredicate<'p>,
    ) -> Resolver<'a, 'p> {
        Resolver {
            options: options.clone(),
            blocks: vec![Block::new(None)],
            env: FILE_BLOCK,
            module_locals: Vec::new(),
            module_globals: Vec::new(),
            globals: HashMap::new(),
            predeclared: HashMap::new(),
            is_global,
            is_predeclared,
            is_universal,
            loops: 0,
            ifstmts: 0,
            errors: Vec::new(),
        }
    }

    /// Returns the innermost enclosing "container" block:
    /// a function (function != None) or file (function == None).
    /// Container blocks accumulate local variable bindings.
    fn container(&self) -> usize {
        let mut b = self.env;
        loop {
            if self.blocks[b].function.is_some() || b == FILE_BLOCK {
                return b;
            }
            b = self.blocks[b].parent.unwrap_or(FILE_BLOCK);
        }
    }

    /// Reports whether the current container is the file (not a function).
    fn at_toplevel(&self) -> bool {
        self.blocks[self.container()].function.is_none()
    }

    fn push(&mut self, mut b: Block<'a>) {
        let index = self.blocks.len();
        b.parent = Some(self.env);
        self.blocks.push(b);
        self.blocks[self.env].children.push(index);
        self.env = index;
    }

    fn pop(&mut self) {
        self.env = self.blocks[self.env].parent.unwrap_or(FILE_BLOCK);
    }

    fn errorf(&mut self, pos: Position, msg: String) {
        self.errors.push(Error { pos, msg });
    }

    /// Creates a binding for id: a global (not file-local)
    /// binding at top-level, a local binding otherwise.
    /// At top-level, it reports an error if a global or file-local
    /// binding already exists, unless global_reassign.
    /// It sets id.binding to the binding (whether old or new),
    /// and returns whether a binding already existed.
    fn bind(&mut self, id: &'a Ident) -> bool {
        // Binding outside any local (comprehension/function) block?
        if self.env == FILE_BLOCK {
            let mut existing = self.blocks[FILE_BLOCK].bindings.get(&id.name).cloned();
            if existing.is_none() {
                existing = self.globals.get(&id.name).cloned();
            }
            let ok = existing.is_some();
            let bind = match existing {
                Some(b) => b,
                None => {
                    // first global binding of this name
                    let bind =
                        Binding::new(Scope::Global, self.module_globals.len(), Some(first_of(id)));
                    self.globals.insert(id.name.clone(), bind.clone());
                    self.module_globals.push(bind.clone());
                    bind
                }
            };
            if ok && !self.options.global_reassign {
                self.errorf(
                    id.name_pos.clone(),
                    format!(
                        "cannot reassign {} {} declared at {}",
                        bind.scope(),
                        id.name,
                        bind.pos()
                    ),
                );
            }
            *id.binding.borrow_mut() = Some(bind);
            return ok;
        }

        self.bind_local(id)
    }

    fn bind_local(&mut self, id: &'a Ident) -> bool {
        // Mark this name as local to current block.
        // Assign it a new local (positive) index in the current container.
        let ok = self.blocks[self.env].bindings.contains_key(&id.name);
        if !ok {
            let container = self.container();
            let bind = match &self.blocks[container].function {
                Some(f) => {
                    let mut locals = f.locals.borrow_mut();
                    let bind = Binding::new(Scope::Local, locals.len(), Some(first_of(id)));
                    locals.push(bind.clone());
                    bind
                }
                None => {
                    let bind =
                        Binding::new(Scope::Local, self.module_locals.len(), Some(first_of(id)));
                    self.module_locals.push(bind.clone());
                    bind
                }
            };
            self.blocks[self.env].bindings.insert(id.name.clone(), bind);
        }

        self.use_(id);
        ok
    }

    fn use_(&mut self, id: &'a Ident) {
        let u = Use { id, env: self.env };

        // The spec says that if there is a global binding of a name
        // then all references to that name in that block refer to the
        // global, even if the use precedes the def---just as for locals.
        // For example, in this code,
        //
        //   print(len); len=1; print(len)
        //
        // both occurrences of len refer to the len=1 binding, which
        // completely shadows the predeclared len function.
        //
        // The rationale for these semantics, which differ from Python,
        // is that the static meaning of len (a reference to a global)
        // does not change depending on where it appears in the file.
        // Of course, its dynamic meaning does change, from an error
        // into a valid reference, so it's not clear these semantics
        // have any practical advantage.
        //
        // In any case, the Bazel implementation lags behind the spec
        // and follows Python behavior, so the first use of len refers
        // to the predeclared function.  This typically used in a BUILD
        // file that redefines a predeclared name half way through,
        // for example:
        //
        //	proto_library(...) 			# built-in rule
        //      load("myproto.bzl", "proto_library")
        //	proto_library(...) 			# user-defined rule
        //
        // We will piggyback support for the legacy semantics on the
        // global_reassign flag, which is loosely related and also
        // required for Bazel.
        if self.options.global_reassign && self.env == FILE_BLOCK {
            self.use_toplevel(u);
            return;
        }

        let b = self.container();
        self.blocks[b].uses.push(u);
    }

    /// Resolves use.id as a reference to a name visible at top-level.
    /// The use.env field captures the original environment for error reporting.
    fn use_toplevel(&mut self, u: Use<'a>) -> Rc<Binding> {
        let id = u.id;
        let name = id.name.as_str();

        let bind = if let Some(prev) = self.blocks[FILE_BLOCK].bindings.get(name) {
            // use of load-defined name in file block
            prev.clone()
        } else if let Some(prev) = self.globals.get(name) {
            // use of global declared by module
            prev.clone()
        } else if self.is_global.is_some_and(|g| g(name)) {
            // use of global defined in a previous REPL chunk
            let bind = Binding::new(
                Scope::Global,
                self.module_globals.len(),
                Some(first_of(id)), // wrong: this is not even a binding use
            );
            self.globals.insert(id.name.clone(), bind.clone());
            self.module_globals.push(bind.clone());
            bind
        } else if let Some(prev) = self.predeclared.get(name) {
            // repeated use of predeclared or universal
            prev.clone()
        } else if (self.is_predeclared)(name) {
            // use of pre-declared name
            let bind = Binding::new(Scope::Predeclared, 0, None);
            self.predeclared.insert(id.name.clone(), bind.clone()); // save it
            bind
        } else if (self.is_universal)(name) {
            // use of universal name
            if !self.options.set && name == "set" {
                self.errorf(id.name_pos.clone(), format!("{DOESNT}support sets"));
            }
            let bind = Binding::new(Scope::Universal, 0, None);
            self.predeclared.insert(id.name.clone(), bind.clone()); // save it
            bind
        } else {
            let bind = Binding::new(Scope::Undefined, 0, None);
            let n = self.spellcheck(u);
            let hint = if n.is_empty() {
                String::new()
            } else {
                format!(" (did you mean {n}?)")
            };
            self.errorf(id.name_pos.clone(), format!("undefined: {name}{hint}"));
            bind
        };
        *id.binding.borrow_mut() = Some(bind.clone());
        bind
    }

    /// Returns the most likely misspelling of
    /// the name use.id in the environment use.env.
    fn spellcheck(&self, u: Use<'a>) -> String {
        let mut names: Vec<&str> = Vec::new();

        // locals
        let mut b = Some(u.env);
        while let Some(i) = b {
            names.extend(self.blocks[i].bindings.keys().map(String::as_str));
            b = self.blocks[i].parent;
        }

        // globals
        //
        // We have no way to enumerate the sets whose membership
        // tests are is_predeclared, is_universal, and is_global,
        // which includes prior names in the REPL session.
        names.extend(self.module_globals.iter().map(|bind| bind.name()));

        names.sort_unstable();
        spell::nearest(&u.id.name, &names)
    }

    /// Called when leaving a container (function/module)
    /// block.  It resolves all uses of locals/cells within that block.
    fn resolve_local_uses(&mut self, b: usize) {
        let uses = std::mem::take(&mut self.blocks[b].uses);
        let mut unresolved = Vec::new();
        for u in uses {
            match self.lookup_local(u) {
                Some(bind) if matches!(bind.scope(), Scope::Local | Scope::Cell) => {
                    *u.id.binding.borrow_mut() = Some(bind);
                }
                _ => unresolved.push(u),
            }
        }
        self.blocks[b].uses = unresolved;
    }

    fn stmts(&mut self, stmts: &'a [Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Expr(s) => self.expr(&s.x),

            Stmt::Branch(s) => {
                if self.loops == 0 && (s.token == Token::Break || s.token == Token::Continue) {
                    self.errorf(s.token_pos.clone(), format!("{} not in a loop", s.token));
                }
            }

            Stmt::If(s) => {
                if !self.options.top_level_control && self.at_toplevel() {
                    self.errorf(
                        s.if_.clone(),
                        "if statement not within a function".to_string(),
                    );
                }
                self.expr(&s.cond);
                self.ifstmts += 1;
                self.stmts(&s.true_);
                self.stmts(&s.false_);
                self.ifstmts -= 1;
            }

            Stmt::Assign(s) => {
                self.expr(&s.rhs);
                let is_augmented = s.op != Token::Eq;
                self.assign(&s.lhs, is_augmented);
            }

            Stmt::Def(s) => {
                self.bind(&s.name);
                let f = Function::new(&s.name.name, s.def.clone());
                *s.function.borrow_mut() = Some(f.clone());
                self.function(f, &s.params, Body::Stmts(&s.body));
            }

            Stmt::For(s) => {
                if !self.options.top_level_control && self.at_toplevel() {
                    self.errorf(s.for_.clone(), "for loop not within a function".to_string());
                }
                self.expr(&s.x);
                self.assign(&s.vars, false);
                self.loops += 1;
                self.stmts(&s.body);
                self.loops -= 1;
            }

            Stmt::While(s) => {
                if !self.options.while_ {
                    self.errorf(s.while_.clone(), format!("{DOESNT}support while loops"));
                }
                if !self.options.top_level_control && self.at_toplevel() {
                    self.errorf(
                        s.while_.clone(),
                        "while loop not within a function".to_string(),
                    );
                }
                self.expr(&s.cond);
                self.loops += 1;
                self.stmts(&s.body);
                self.loops -= 1;
            }

            Stmt::Return(s) => {
                if self.at_toplevel() {
                    self.errorf(
                        s.return_.clone(),
                        "return statement not within a function".to_string(),
                    );
                }
                if let Some(result) = &s.result {
                    self.expr(result);
                }
            }

            Stmt::Load(s) => {
                // A load statement may not be nested in any other statement.
                if !self.at_toplevel() {
                    self.errorf(
                        s.load.clone(),
                        "load statement within a function".to_string(),
                    );
                } else if self.loops > 0 {
                    self.errorf(s.load.clone(), "load statement within a loop".to_string());
                } else if self.ifstmts > 0 {
                    self.errorf(
                        s.load.clone(),
                        "load statement within a conditional".to_string(),
                    );
                }

                for (i, from) in s.from.iter().enumerate() {
                    if from.name.is_empty() {
                        self.errorf(from.name_pos.clone(), "load: empty identifier".to_string());
                        continue;
                    }
                    if from.name.starts_with('_') {
                        self.errorf(
                            from.name_pos.clone(),
                            format!(
                                "load: names with leading underscores are not exported: {}",
                                from.name
                            ),
                        );
                    }

                    let id = &s.to[i];
                    if self.options.load_binds_globally {
                        self.bind(id);
                    } else if self.bind_local(id) && !self.options.global_reassign {
                        // "Global" in global_reassign is a misnomer for "toplevel".
                        // Sadly we can't report the previous declaration
                        // as id.binding may not be set yet.
                        self.errorf(
                            id.name_pos.clone(),
                            format!("cannot reassign top-level {}", id.name),
                        );
                    }
                }
            }
        }
    }

    fn assign(&mut self, lhs: &'a Expr, is_augmented: bool) {
        match lhs {
            Expr::Ident(id) => {
                // x = ...
                self.bind(id);
            }

            Expr::Index(e) => {
                // x[i] = ...
                self.expr(&e.x);
                self.expr(&e.y);
            }

            Expr::Dot(e) => {
                // x.f = ...
                self.expr(&e.x);
            }

            Expr::Tuple(e) => {
                // (x, y) = ...
                if is_augmented {
                    self.errorf(
                        lhs.start(),
                        "can't use tuple expression in augmented assignment".to_string(),
                    );
                }
                for elem in &e.list {
                    self.assign(elem, is_augmented);
                }
            }

            Expr::List(e) => {
                // [x, y, z] = ...
                if is_augmented {
                    self.errorf(
                        lhs.start(),
                        "can't use list expression in augmented assignment".to_string(),
                    );
                }
                for elem in &e.list {
                    self.assign(elem, is_augmented);
                }
            }

            Expr::Paren(e) => self.assign(&e.x, is_augmented),

            _ => {
                self.errorf(lhs.start(), format!("can't assign to {}", lhs.type_name()));
            }
        }
    }

    fn expr(&mut self, e: &'a Expr) {
        match e {
            Expr::Ident(id) => self.use_(id),

            Expr::Literal(_) => {}

            Expr::List(e) => {
                for x in &e.list {
                    self.expr(x);
                }
            }

            Expr::Cond(e) => {
                self.expr(&e.cond);
                self.expr(&e.true_);
                self.expr(&e.false_);
            }

            Expr::Index(e) => {
                self.expr(&e.x);
                self.expr(&e.y);
            }

            Expr::DictEntry(e) => {
                self.expr(&e.key);
                self.expr(&e.value);
            }

            Expr::Slice(e) => {
                self.expr(&e.x);
                for x in [&e.lo, &e.hi, &e.step].into_iter().flatten() {
                    self.expr(x);
                }
            }

            Expr::Comprehension(e) => {
                // The 'in' operand of the first clause (always a ForClause)
                // is resolved in the outer block; consider: [x for x in x].
                let Some(Clause::For(clause)) = e.clauses.first() else {
                    unreachable!("comprehension must begin with a for clause");
                };
                self.expr(&clause.x);

                // A list/dict comprehension defines a new lexical block.
                // Locals defined within the block will be allotted
                // distinct slots in the locals array of the innermost
                // enclosing container (function/module) block.
                self.push(Block::new(None));

                self.assign(&clause.vars, false);

                for clause in &e.clauses[1..] {
                    match clause {
                        Clause::If(c) => self.expr(&c.cond),
                        Clause::For(c) => {
                            self.assign(&c.vars, false);
                            self.expr(&c.x);
                        }
                    }
                }
                self.expr(&e.body); // body may be DictEntry
                self.pop();
            }

            Expr::Tuple(e) => {
                for x in &e.list {
                    self.expr(x);
                }
            }

            Expr::Dict(e) => {
                for entry in &e.list {
                    self.expr(&entry.key);
                    self.expr(&entry.value);
                }
            }

            Expr::Unary(e) => {
                if let Some(x) = &e.x {
                    self.expr(x);
                }
            }

            Expr::Binary(e) => {
                self.expr(&e.x);
                self.expr(&e.y);
            }

            Expr::Dot(e) => {
                self.expr(&e.x);
                // ignore e.name
            }

            Expr::Call(call) => self.call(call),

            Expr::Lambda(e) => {
                let f = Function::new("lambda", e.lambda.clone());
                *e.function.borrow_mut() = Some(f.clone());
                self.function(f, &e.params, Body::Expr(&e.body));
            }

            Expr::Paren(e) => self.expr(&e.x),
        }
    }

    fn call(&mut self, call: &'a crate::syntax::CallExpr) {
        self.expr(&call.func);
        let mut seen_varargs = false;
        let mut seen_kwargs = false;
        let mut seen_name: Vec<&str> = Vec::new();
        let mut n = 0;
        let mut p = 0;
        for arg in &call.args {
            let pos = arg.start();
            match arg {
                Expr::Unary(unop) if unop.op == Token::Starstar => {
                    // **kwargs
                    if seen_kwargs {
                        self.errorf(pos, "multiple **kwargs not allowed".to_string());
                    }
                    seen_kwargs = true;
                    self.expr(arg);
                }
                Expr::Unary(unop) if unop.op == Token::Star => {
                    // *args
                    if seen_kwargs {
                        self.errorf(pos, "*args may not follow **kwargs".to_string());
                    } else if seen_varargs {
                        self.errorf(pos, "multiple *args not allowed".to_string());
                    }
                    seen_varargs = true;
                    self.expr(arg);
                }
                Expr::Binary(binop) if binop.op == Token::Eq => {
                    // k=v
                    n += 1;
                    if seen_kwargs {
                        self.errorf(pos, "keyword argument may not follow **kwargs".to_string());
                    } else if seen_varargs {
                        self.errorf(pos, "keyword argument may not follow *args".to_string());
                    }
                    if let Expr::Ident(x) = &*binop.x {
                        if seen_name.contains(&x.name.as_str()) {
                            self.errorf(
                                x.name_pos.clone(),
                                format!("keyword argument \"{}\" is repeated", x.name),
                            );
                        } else {
                            seen_name.push(&x.name);
                        }
                    }
                    self.expr(&binop.y);
                }
                _ => {
                    // positional argument
                    p += 1;
                    if seen_varargs {
                        self.errorf(pos, "positional argument may not follow *args".to_string());
                    } else if seen_kwargs {
                        self.errorf(
                            pos,
                            "positional argument may not follow **kwargs".to_string(),
                        );
                    } else if !seen_name.is_empty() {
                        self.errorf(pos, "positional argument may not follow named".to_string());
                    }
                    self.expr(arg);
                }
            }
        }

        // Fail gracefully if compiler-imposed limit is exceeded.
        let start = call.func.start();
        if p >= 256 {
            self.errorf(
                start.clone(),
                format!("{p} positional arguments in call, limit is 255"),
            );
        }
        if n >= 256 {
            self.errorf(
                start,
                format!("{n} keyword arguments in call, limit is 255"),
            );
        }
    }

    fn function(&mut self, function: Rc<Function>, params: &'a [Expr], body: Body<'a>) {
        // Resolve defaults in enclosing environment.
        for param in params {
            if let Expr::Binary(binary) = param {
                self.expr(&binary.y);
            }
        }

        // Enter function block.
        self.push(Block::new(Some(function.clone())));
        let b = self.env;

        // Save the current loop count and reset it for the new function
        let outer_loops = self.loops;
        self.loops = 0;

        let mut seen_optional = false;
        let mut star: Option<&'a crate::syntax::UnaryExpr> = None; // * or *args param
        let mut star_star: Option<&'a Ident> = None; // **kwargs ident
        let mut num_kwonly_params = 0;
        for param in params {
            match param {
                Expr::Ident(param) => {
                    // e.g. x
                    if let Some(ss) = star_star {
                        self.errorf(
                            param.name_pos.clone(),
                            format!("required parameter may not follow **{}", ss.name),
                        );
                    } else if star.is_some() {
                        num_kwonly_params += 1;
                    } else if seen_optional {
                        self.errorf(
                            param.name_pos.clone(),
                            "required parameter may not follow optional".to_string(),
                        );
                    }
                    if self.bind(param) {
                        self.errorf(
                            param.name_pos.clone(),
                            format!("duplicate parameter: {}", param.name),
                        );
                    }
                }

                Expr::Binary(param) => {
                    // e.g. y=dflt
                    if let Some(ss) = star_star {
                        self.errorf(
                            param.op_pos.clone(),
                            format!("optional parameter may not follow **{}", ss.name),
                        );
                    } else if star.is_some() {
                        num_kwonly_params += 1;
                    }
                    if let Expr::Ident(id) = &*param.x {
                        if self.bind(id) {
                            self.errorf(
                                param.op_pos.clone(),
                                format!("duplicate parameter: {}", id.name),
                            );
                        }
                    }
                    seen_optional = true;
                }

                Expr::Unary(param) => {
                    // * or *args or **kwargs
                    if param.op == Token::Star {
                        if let Some(ss) = star_star {
                            self.errorf(
                                param.op_pos.clone(),
                                format!("* parameter may not follow **{}", ss.name),
                            );
                        } else if star.is_some() {
                            self.errorf(
                                param.op_pos.clone(),
                                "multiple * parameters not allowed".to_string(),
                            );
                        } else {
                            star = Some(param);
                        }
                    } else {
                        if star_star.is_some() {
                            self.errorf(
                                param.op_pos.clone(),
                                "multiple ** parameters not allowed".to_string(),
                            );
                        }
                        if let Some(Expr::Ident(id)) = param.x.as_deref() {
                            star_star = Some(id);
                        }
                    }
                }

                _ => {}
            }
        }

        // Bind the *args and **kwargs parameters at the end,
        // so that regular parameters a/b/c are contiguous and
        // there is no hole for the "*":
        //   def f(a, b, *args, c=0, **kwargs)
        //   def f(a, b, *,     c=0, **kwargs)
        if let Some(star) = star {
            if let Some(Expr::Ident(id)) = star.x.as_deref() {
                // *args
                if self.bind(id) {
                    self.errorf(
                        id.name_pos.clone(),
                        format!("duplicate parameter: {}", id.name),
                    );
                }
                function.has_varargs.set(true);
            } else if num_kwonly_params == 0 {
                self.errorf(
                    star.op_pos.clone(),
                    "bare * must be followed by keyword-only parameters".to_string(),
                );
            }
        }
        if let Some(ss) = star_star {
            if self.bind(ss) {
                self.errorf(
                    ss.name_pos.clone(),
                    format!("duplicate parameter: {}", ss.name),
                );
            }
            function.has_kwargs.set(true);
        }

        function.num_kwonly_params.set(num_kwonly_params);
        match body {
            Body::Stmts(stmts) => self.stmts(stmts),
            // A lambda's body is a synthetic 'return expr' statement.
            Body::Expr(e) => self.expr(e),
        }

        // Resolve all uses of this function's local vars,
        // and keep just the remaining uses of free/global vars.
        self.resolve_local_uses(b);

        // Leave function block.
        self.pop();

        // Restore the outer loop count
        self.loops = outer_loops;

        // References within the function body to globals are not
        // resolved until the end of the module.
    }

    fn resolve_non_local_uses(&mut self, b: usize) {
        // First resolve inner blocks.
        let children = self.blocks[b].children.clone();
        for child in children {
            self.resolve_non_local_uses(child);
        }
        let uses = self.blocks[b].uses.clone();
        for u in uses {
            let bind = self.lookup_lexical(u, u.env);
            *u.id.binding.borrow_mut() = Some(bind);
        }
    }

    /// Looks up an identifier within its immediately enclosing function.
    fn lookup_local(&self, u: Use<'a>) -> Option<Rc<Binding>> {
        let mut env = Some(u.env);
        while let Some(e) = env {
            let block = &self.blocks[e];
            if let Some(bind) = block.bindings.get(&u.id.name) {
                return Some(bind.clone()); // found
            }
            if block.function.is_some() {
                break;
            }
            env = block.parent;
        }
        None // not found in this function
    }

    /// Looks up an identifier use.id within its lexically enclosing environment.
    /// The use.env field captures the original environment for error reporting.
    fn lookup_lexical(&mut self, u: Use<'a>, env: usize) -> Rc<Binding> {
        // Is this the file block?
        if env == FILE_BLOCK {
            return self.use_toplevel(u); // file-local, global, predeclared, or not found
        }

        // Defined in this block?
        if let Some(bind) = self.blocks[env].bindings.get(&u.id.name) {
            return bind.clone();
        }

        // Defined in parent block?
        let parent = self.blocks[env].parent.unwrap_or(FILE_BLOCK);
        let mut bind = self.lookup_lexical(u, parent);
        if let Some(function) = &self.blocks[env].function {
            if matches!(bind.scope(), Scope::Local | Scope::Free | Scope::Cell) {
                // Found in parent block, which belongs to enclosing function.
                // Add the parent's binding to the function's freevars,
                // and add a new 'free' binding to the inner function's block,
                // and turn the parent's local into cell.
                if bind.scope() == Scope::Local {
                    bind.scope.set(Scope::Cell);
                }
                let mut free_vars = function.free_vars.borrow_mut();
                let index = free_vars.len();
                free_vars.push(bind.clone());
                bind = Binding::new(Scope::Free, index, bind.first.clone());
            }
        }

        // Memoize, to avoid duplicate free vars
        // and redundant global (failing) lookups.
        self.blocks[env]
            .bindings
            .insert(u.id.name.clone(), bind.clone());
        bind
    }
}

fn first_of(id: &Ident) -> BindingIdent {
    BindingIdent {
        name: id.name.clone(),
        name_pos: id.name_pos.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{parse, FileOptions};

    fn is_predeclared(name: &str) -> bool {
        name == "M"
    }

    fn is_universal(name: &str) -> bool {
        name == "U" || name == "float"
    }

    #[test]
    fn test_def_varargs_and_kwargs_set() {
        let src = "def f(*args, **kwargs): pass\n";
        let mut f = parse(&FileOptions::default(), "foo.star", src.as_bytes()).unwrap();
        file(&mut f, &is_predeclared, &is_universal).unwrap();
        let Stmt::Def(def) = &f.stmts[0] else {
            panic!("not a def")
        };
        let func = def.function.borrow().clone().unwrap();
        assert!(func.has_varargs.get(), "HasVarargs not set");
        assert!(func.has_kwargs.get(), "HasKwargs not set");
    }

    #[test]
    fn test_lambda_varargs_and_kwargs_set() {
        let src = "f = lambda *args, **kwargs: 0\n";
        let mut f = parse(&FileOptions::default(), "foo.star", src.as_bytes()).unwrap();
        file(&mut f, &is_predeclared, &is_universal).unwrap();
        let Stmt::Assign(assign) = &f.stmts[0] else {
            panic!("not an assignment")
        };
        let Expr::Lambda(lambda) = &assign.rhs else {
            panic!("not a lambda")
        };
        let func = lambda.function.borrow().clone().unwrap();
        assert!(func.has_varargs.get(), "HasVarargs not set");
        assert!(func.has_kwargs.get(), "HasKwargs not set");
    }
}
