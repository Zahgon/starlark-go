// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Threads, evaluation entry points and the primitive operations of the
//! bytecode interpreter (ported from eval.go).

use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::rc::Rc;

use super::int::{as_int32, number_to_int, Int};
use super::library::{builtin_attr, builtin_attr_names, universe_get, universe_has};
use super::value::{
    float_mod, format_float, iterate, len, write_value, Dict, Error, Function, List, Module, Side,
    Value,
};
use crate::compile;
use crate::resolve;
use crate::syntax::{self, FileOptions, Position, Token};

/// A StringDict is a mapping from names to values, and represents an
/// environment such as the global variables of a module. It is not a true
/// Starlark value.
#[derive(Clone, Default)]
pub struct StringDict(BTreeMap<String, Value>);

impl StringDict {
    /// Returns a new, empty StringDict.
    pub fn new() -> StringDict {
        StringDict(BTreeMap::new())
    }

    /// Returns the sorted keys.
    pub fn keys(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    /// Reports whether the dictionary contains the specified key.
    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn insert(&mut self, key: impl Into<String>, v: Value) {
        self.0.insert(key.into(), v);
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates over the entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }

    /// Freezes all values.
    pub fn freeze(&self) {
        for v in self.0.values() {
            v.freeze();
        }
    }
}

impl fmt::Display for StringDict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = String::from("{");
        let mut sep = "";
        for (name, v) in &self.0 {
            buf.push_str(sep);
            buf.push_str(name);
            buf.push_str(": ");
            write_value(&mut buf, v, &mut Vec::new());
            sep = ", ";
        }
        buf.push('}');
        f.write_str(&buf)
    }
}

impl FromIterator<(String, Value)> for StringDict {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> StringDict {
        StringDict(iter.into_iter().collect())
    }
}

/// The client-supplied implementation of the 'print' function.
pub type PrintFn = dyn Fn(&Thread, &[u8]);

/// The client-supplied implementation of module loading.
pub type LoadFn = dyn Fn(&mut Thread, &str) -> Result<StringDict, Error>;

/// A frame records a call to a Starlark function (including module
/// toplevel) or a built-in function or method.
pub(crate) struct Frame {
    pub(crate) callable: Value,
    pub(crate) pc: usize,
    /// The local variables of a Starlark function call (empty otherwise).
    pub(crate) locals: Vec<Option<Value>>,
}

impl Frame {
    /// Returns the source position of the current point of execution in
    /// this frame.
    pub(crate) fn position(&self) -> Position {
        match &self.callable {
            Value::Function(f) => f.funcode.position(self.pc),
            Value::Other(o) => match o.position() {
                Some(p) => p,
                None => builtin_position(),
            },
            _ => builtin_position(),
        }
    }

    fn as_call_frame(&self) -> CallFrame {
        CallFrame {
            name: self.callable.callable_name(),
            pos: self.position(),
        }
    }
}

const BUILTIN_FILENAME: &str = "<builtin>";

fn builtin_position() -> Position {
    Position::new(Rc::from(BUILTIN_FILENAME), 0, 0)
}

/// A Thread contains the state of a Starlark thread, such as its call
/// stack and thread-local storage.
pub struct Thread {
    /// Name is an optional name that describes the thread, for debugging.
    pub name: String,

    /// The stack of (internal) call frames.
    pub(crate) stack: Vec<Frame>,

    /// Print is the client-supplied implementation of the Starlark 'print'
    /// function. If None, the message and a newline are written to stderr.
    pub print: Option<Rc<PrintFn>>,

    /// Load is the client-supplied implementation of module loading.
    /// Repeated calls with the same module name must return the same
    /// module environment or error.
    pub load: Option<Rc<LoadFn>>,

    /// A count of abstract computation steps executed by this thread.
    pub steps: u64,
    max_steps: u64,
    initialized: bool,

    cancel_reason: Option<String>,

    /// Arbitrary thread-local values belonging to the client.
    locals: HashMap<String, Rc<dyn Any>>,
}

impl Default for Thread {
    fn default() -> Thread {
        Thread::new("")
    }
}

impl Thread {
    /// Returns a new thread with the given name.
    pub fn new(name: &str) -> Thread {
        Thread {
            name: name.to_string(),
            stack: Vec::new(),
            print: None,
            load: None,
            steps: 0,
            max_steps: 0,
            initialized: false,
            cancel_reason: None,
            locals: HashMap::new(),
        }
    }

    /// Returns the current value of Steps.
    pub fn execution_steps(&self) -> u64 {
        self.steps
    }

    /// Sets a limit on the number of Starlark computation steps that may
    /// be executed by this thread.
    pub fn set_max_execution_steps(&mut self, max: u64) {
        self.max_steps = max;
    }

    /// Resets the cancellation state.
    pub fn uncancel(&mut self) {
        self.cancel_reason = None;
    }

    /// Causes execution of Starlark code in the thread to fail with an
    /// EvalError that includes the specified reason.
    pub fn cancel(&mut self, reason: &str) {
        if self.cancel_reason.is_none() {
            self.cancel_reason = Some(reason.to_string());
        }
    }

    /// Sets the thread-local value associated with the specified key.
    pub fn set_local(&mut self, key: &str, value: Rc<dyn Any>) {
        self.locals.insert(key.to_string(), value);
    }

    /// Returns the thread-local value associated with the specified key.
    pub fn local(&self, key: &str) -> Option<Rc<dyn Any>> {
        self.locals.get(key).cloned()
    }

    /// Returns a copy of the specified frame of the callstack. Depth 0
    /// means the frame of the built-in itself, 1 is its caller, and so on.
    pub fn call_frame(&self, depth: usize) -> CallFrame {
        self.stack[self.stack.len() - 1 - depth].as_call_frame()
    }

    /// Returns a new vector containing the thread's stack of call frames.
    pub fn call_stack(&self) -> CallStack {
        CallStack(self.stack.iter().map(|fr| fr.as_call_frame()).collect())
    }

    /// Returns the number of frames in the current call stack.
    pub fn call_stack_depth(&self) -> usize {
        self.stack.len()
    }

    /// Returns the debugger interface for the specified frame of the
    /// interpreter's call stack. Frame numbering is as for `call_frame`:
    /// 0 <= depth < `call_stack_depth()`.
    ///
    /// This function is intended for use in debugging tools.
    /// Most applications should have no need for it; use `call_frame` instead.
    pub fn debug_frame(&self, depth: usize) -> DebugFrame<'_> {
        DebugFrame {
            frame: &self.stack[self.stack.len() - 1 - depth],
        }
    }

    pub(crate) fn eval_error(&self, err: Error) -> Error {
        Error::Eval(Rc::new(EvalError {
            msg: err.to_string(),
            call_stack: self.call_stack(),
            cause: Some(err),
        }))
    }

    /// Increments the thread's step counter and reports whether the
    /// thread has exceeded its step limit or been cancelled.
    pub(crate) fn check_steps(&mut self) -> bool {
        self.steps = self.steps.wrapping_add(STEP_COST);
        self.steps >= self.max_steps || self.cancel_reason.is_some()
    }

    pub(crate) fn check_steps_slow(&mut self) -> Result<(), Error> {
        if self.steps >= self.max_steps {
            self.cancel("too many steps");
        }
        if let Some(reason) = &self.cancel_reason {
            return Err(Error::new(format!(
                "Starlark computation cancelled: {}",
                reason
            )));
        }
        Ok(())
    }

    /// Writes a message using the thread's print function (or stderr).
    pub fn print_message(&self, msg: &[u8]) {
        match &self.print {
            Some(p) => p(self, msg),
            None => {
                use std::io::Write;
                let mut err = std::io::stderr().lock();
                // Like the original's fmt.Fprintln(os.Stderr, ...), print ignores write errors.
                let _ = err.write_all(msg);
                let _ = err.write_all(b"\n");
            }
        }
    }
}

/// DebugFrame is the debugger API for a frame of the interpreter's call
/// stack.
///
/// Most applications have no need for this API; use CallFrame instead.
///
/// It may be tempting to use this interface when implementing built-in
/// functions. Beware that reflection over the call stack is easily
/// abused, leading to built-in functions whose behavior is mysterious
/// and unpredictable.
pub struct DebugFrame<'a> {
    frame: &'a Frame,
}

impl DebugFrame<'_> {
    /// Returns the frame's function.
    pub fn callable(&self) -> &Value {
        &self.frame.callable
    }

    /// Returns the number of local variables of this frame.
    /// It is zero unless `callable()` is a Starlark function.
    pub fn num_locals(&self) -> usize {
        self.frame.locals.len()
    }

    /// Returns the binding (name and binding position) and value of the
    /// i'th local variable of the frame's function.
    /// Beware: the value may be None if it has not yet been assigned!
    ///
    /// The index i must be less than `num_locals()`.
    pub fn local(&self, i: usize) -> (&compile::Binding, Option<&Value>) {
        let Value::Function(f) = &self.frame.callable else {
            panic!("local({i}) of a frame that is not a Starlark function");
        };
        (&f.funcode.locals[i], self.frame.locals[i].as_ref())
    }

    /// Returns the current position of execution in this frame.
    pub fn position(&self) -> Position {
        self.frame.position()
    }
}

/// Empirical average number of instructions per back-edge or call.
const STEP_COST: u64 = 10;

/// A CallFrame represents the function name and current position of
/// execution of an enclosing call frame.
#[derive(Clone, Debug)]
pub struct CallFrame {
    pub name: String,
    pub pos: Position,
}

/// A CallStack is a stack of call frames, outermost first.
#[derive(Clone, Debug, Default)]
pub struct CallStack(pub Vec<CallFrame>);

impl CallStack {
    /// Returns a copy of the frame at depth i. At(0) returns the topmost frame.
    pub fn at(&self, i: usize) -> &CallFrame {
        &self.0[self.0.len() - 1 - i]
    }

    /// Removes and returns the topmost frame.
    pub fn pop(&mut self) -> Option<CallFrame> {
        self.0.pop()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for CallStack {
    /// A user-friendly description of the stack.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.0.is_empty() {
            f.write_str("Traceback (most recent call last):\n")?;
        }
        for fr in &self.0 {
            writeln!(f, "  {}: in {}", fr.pos, fr.name)?;
        }
        Ok(())
    }
}

/// An EvalError is a Starlark evaluation error and a copy of the thread's
/// stack at the moment of the error.
#[derive(Clone, Debug)]
pub struct EvalError {
    pub msg: String,
    pub call_stack: CallStack,
    pub cause: Option<Error>,
}

impl EvalError {
    /// Returns a user-friendly error message describing the stack of calls
    /// that led to this error.
    pub fn backtrace(&self) -> String {
        // If the topmost stack frame is a built-in function,
        // remove it from the stack and add print "Error in fn:".
        let mut stack = self.call_stack.clone();
        let mut suffix = String::new();
        if let Some(last) = stack.0.last() {
            if last.pos.filename() == BUILTIN_FILENAME {
                suffix = format!(" in {}", last.name);
                stack.0.pop();
            }
        }
        format!("{}Error{}: {}", stack, suffix, self.msg)
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

/// A Program is a compiled Starlark program.
#[derive(Clone)]
pub struct Program {
    compiled: Rc<compile::Program>,
}

impl Program {
    /// Returns the compiled form of the program.
    pub fn compiled(&self) -> &Rc<compile::Program> {
        &self.compiled
    }

    /// Returns the name of the file from which this program was loaded.
    pub fn filename(&self) -> &str {
        self.compiled.toplevel.pos.filename()
    }

    /// Returns the number of load statements in the compiled program.
    pub fn num_loads(&self) -> usize {
        self.compiled.loads.len()
    }

    /// Returns the name and position of the i'th module directly loaded by
    /// this one.
    pub fn load(&self, i: usize) -> (&str, &Position) {
        let id = &self.compiled.loads[i];
        (&id.name, &id.pos)
    }

    /// Creates a set of global variables for the program, executes the
    /// toplevel code of the specified program, and returns a new, unfrozen
    /// dictionary of the globals. A (partial) dictionary is returned even
    /// in case of error.
    pub fn init(
        &self,
        thread: &mut Thread,
        predeclared: &StringDict,
    ) -> (StringDict, Option<Error>) {
        let toplevel = make_toplevel_function(&self.compiled, predeclared);
        let res = call(thread, &Value::Function(toplevel.clone()), &[], &[]);
        // Convert the global environment to a map.
        (toplevel.globals(), res.err())
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.filename())
    }
}

/// Converts an I/O error to the text Go's os package would report for
/// the given operation and file name.
pub fn go_path_error(op: &str, path: &str, err: &std::io::Error) -> String {
    let text = match err.raw_os_error() {
        Some(2) => "no such file or directory".to_string(),
        Some(13) => "permission denied".to_string(),
        Some(20) => "not a directory".to_string(),
        Some(21) => "is a directory".to_string(),
        Some(36) => "file name too long".to_string(),
        Some(40) => "too many levels of symbolic links".to_string(),
        _ => {
            let s = err.to_string();
            let s = match s.find(" (os error") {
                Some(i) => s[..i].to_string(),
                None => s,
            };
            let mut c = s.chars();
            match c.next() {
                Some(first) => first.to_lowercase().collect::<String>() + c.as_str(),
                None => s,
            }
        }
    };
    format!("{} {}: {}", op, path, text)
}

/// Reads a source file like Go's os.ReadFile, with Go's error texts.
pub fn read_source_file(filename: &str) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    let mut f = std::fs::File::open(filename)
        .map_err(|e| Error::new(go_path_error("open", filename, &e)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .map_err(|e| Error::new(go_path_error("read", filename, &e)))?;
    Ok(buf)
}

/// Parses, resolves, and executes a Starlark file in the specified global
/// environment. If `src` is None, the file is read from `filename`.
///
/// If execution fails during evaluation, it returns an `Error::Eval`
/// containing a backtrace. On success the globals are frozen.
pub fn exec_file_options(
    opts: &FileOptions,
    thread: &mut Thread,
    filename: &str,
    src: Option<&[u8]>,
    predeclared: &StringDict,
) -> Result<StringDict, Error> {
    // Parse, resolve, and compile a Starlark source file.
    let prog = source_program_options(opts, filename, src, &|name| predeclared.has(name))?;
    let (g, err) = prog.init(thread, predeclared);
    g.freeze();
    match err {
        Some(e) => Err(e),
        None => Ok(g),
    }
}

/// Like `exec_file_options`, using the legacy options of the command-line
/// tool (see `legacy_file_options`).
pub fn exec_file(
    thread: &mut Thread,
    filename: &str,
    src: Option<&[u8]>,
    predeclared: &StringDict,
) -> Result<StringDict, Error> {
    exec_file_options(&legacy_file_options(), thread, filename, src, predeclared)
}

thread_local! {
    static LEGACY_OPTIONS: RefCell<FileOptions> = const { RefCell::new(FileOptions {
        set: true,
        while_: false,
        top_level_control: false,
        global_reassign: false,
        load_binds_globally: false,
        recursion: false,
    }) };
}

/// Returns the legacy file options (Go's syntax.LegacyFileOptions, which
/// reflect the resolve.Allow* global variables set by the CLI flags).
pub fn legacy_file_options() -> FileOptions {
    LEGACY_OPTIONS.with(|o| o.borrow().clone())
}

/// Sets the legacy file options (the CLI's dialect flags).
pub fn set_legacy_file_options(opts: FileOptions) {
    LEGACY_OPTIONS.with(|o| *o.borrow_mut() = opts);
}

/// Produces a new program by parsing, resolving, and compiling a
/// Starlark source file.
pub fn source_program_options(
    opts: &FileOptions,
    filename: &str,
    src: Option<&[u8]>,
    is_predeclared: &dyn Fn(&str) -> bool,
) -> Result<Program, Error> {
    let data;
    let src = match src {
        Some(s) => s,
        None => {
            data = read_source_file(filename)?;
            &data
        }
    };
    let mut f = syntax::parse(opts, filename, src).map_err(|e| Error::new(e.to_string()))?;
    file_program(&mut f, is_predeclared)
}

/// Produces a new program by resolving and compiling the Starlark source
/// file syntax tree. Resolving a syntax tree mutates it.
pub fn file_program(
    f: &mut syntax::File,
    is_predeclared: &dyn Fn(&str) -> bool,
) -> Result<Program, Error> {
    resolve::file(f, is_predeclared, &universe_has).map_err(|e| Error::new(e.to_string()))?;
    Ok(Program {
        compiled: Rc::new(compile_file(f)),
    })
}

fn compile_file(f: &syntax::File) -> compile::Program {
    let pos = match f.stmts.first() {
        Some(stmt) => stmt.start(),
        None => Position::new(Rc::from(f.path.as_str()), 1, 1),
    };
    let (locals, globals) = match &f.module {
        Some(m) => (&m.locals[..], &m.globals[..]),
        None => (&[][..], &[][..]),
    };
    compile::file(&f.options, &f.stmts, pos, "<toplevel>", locals, globals)
}

/// Compiles and executes file f in the specified thread and global
/// environment. This is a variant of ExecFile specialized to the needs of
/// a REPL, in which a sequence of input chunks, each syntactically a File,
/// manipulates the same set of module globals, which are not frozen after
/// execution.
pub fn exec_repl_chunk(
    f: &mut syntax::File,
    thread: &mut Thread,
    globals: &mut StringDict,
) -> Result<(), Error> {
    let predeclared = StringDict::new();

    // -- variant of FileProgram --
    {
        let g = &*globals;
        resolve::repl_chunk(
            f,
            &|name| g.has(name),
            &|name| predeclared.has(name),
            &universe_has,
        )
        .map_err(|e| Error::new(e.to_string()))?;
    }
    let compiled = Rc::new(compile_file(f));

    // -- variant of Program.Init --
    let toplevel = make_toplevel_function(&compiled, &predeclared);

    // Initialize module globals from parameter.
    {
        let mut mg = toplevel.module.globals.borrow_mut();
        for (i, id) in compiled.globals.iter().enumerate() {
            if let Some(v) = globals.get(&id.name) {
                mg[i] = Some(v.clone());
            }
        }
    }

    let res = call(thread, &Value::Function(toplevel.clone()), &[], &[]);

    // Reflect changes to globals back to parameter, even after an error.
    let mg = toplevel.module.globals.borrow();
    for (i, id) in compiled.globals.iter().enumerate() {
        if let Some(v) = &mg[i] {
            globals.insert(id.name.clone(), v.clone());
        }
    }
    res.map(|_| ())
}

fn make_toplevel_function(prog: &Rc<compile::Program>, predeclared: &StringDict) -> Rc<Function> {
    // Create the Starlark value denoted by each program constant c.
    let constants = prog
        .constants
        .iter()
        .map(|c| match c {
            compile::Constant::Int(i) => Value::int(*i),
            compile::Constant::BigInt(b) => Value::Int(Int::from_bigint((**b).clone())),
            compile::Constant::String(s) => Value::String(s.clone()),
            compile::Constant::Bytes(s) => Value::Bytes(s.clone()),
            compile::Constant::Float(f) => Value::Float(*f),
        })
        .collect();

    let module = Rc::new(Module {
        program: prog.clone(),
        predeclared: predeclared.clone(),
        globals: RefCell::new(vec![None; prog.globals.len()]),
        constants,
    });
    Rc::new(Function::new(
        prog.toplevel.clone(),
        module,
        Vec::new(),
        Vec::new(),
    ))
}

/// Parses, resolves, and evaluates an expression within the specified
/// (predeclared) environment.
pub fn eval_options(
    opts: &FileOptions,
    thread: &mut Thread,
    filename: &str,
    src: &[u8],
    env: &StringDict,
) -> Result<Value, Error> {
    let mut expr =
        syntax::parse_expr(opts, filename, src).map_err(|e| Error::new(e.to_string()))?;
    let f = make_expr_func(opts, &mut expr, env)?;
    call(thread, &Value::Function(f), &[], &[])
}

/// Like `eval_options`, using the legacy file options.
pub fn eval(
    thread: &mut Thread,
    filename: &str,
    src: &[u8],
    env: &StringDict,
) -> Result<Value, Error> {
    eval_options(&legacy_file_options(), thread, filename, src, env)
}

/// Resolves and evaluates an expression within the specified
/// (predeclared) environment. Resolving an expression mutates it.
pub fn eval_expr_options(
    opts: &FileOptions,
    thread: &mut Thread,
    expr: &mut syntax::Expr,
    env: &StringDict,
) -> Result<Value, Error> {
    let f = make_expr_func(opts, expr, env)?;
    call(thread, &Value::Function(f), &[], &[])
}

/// Returns a no-argument function whose body is expr.
fn make_expr_func(
    opts: &FileOptions,
    expr: &mut syntax::Expr,
    env: &StringDict,
) -> Result<Rc<Function>, Error> {
    let locals = resolve::expr_options(opts, expr, &|name| env.has(name), &universe_has)
        .map_err(|e| Error::new(e.to_string()))?;
    let prog = Rc::new(compile::expr(opts, expr, "<expr>", &locals));
    Ok(make_toplevel_function(&prog, env))
}

// The following functions are primitive operations of the byte code interpreter.

/// list += iterable
pub(crate) fn list_extend(x: &List, y: &Value) {
    if let Value::List(ylist) = y {
        // fast path: list += list
        let ye = ylist.elems.borrow().clone();
        x.elems.borrow_mut().extend(ye);
    } else if let Some(mut iter) = iterate(y) {
        while let Some(z) = iter.next() {
            x.elems.borrow_mut().push(z);
        }
    }
}

/// Implements x.dot.
pub fn get_attr(x: &Value, name: &str) -> Result<Value, Error> {
    if !x.has_attrs() {
        return Err(Error::new(format!(
            "{} has no .{} field or method",
            x.type_name(),
            name
        )));
    }

    let (res, names) = match x {
        Value::Other(o) => (o.attr(x, name), None),
        _ => (Ok(builtin_attr(x, name)), None::<Vec<String>>),
    };
    let errmsg = match res {
        Ok(Some(v)) => return Ok(v), // success
        // (nil, nil) => generic error
        Ok(None) => format!("{} has no .{} field or method", x.type_name(), name),
        Err(Error::NoSuchAttr(m)) => m,
        Err(e) => return Err(e), // return error as is
    };

    // add spelling hint
    let names = names.unwrap_or_else(|| attr_names(x));
    let errmsg = match nearest(name, &names) {
        Some(n) => format!("{} (did you mean .{}?)", errmsg, n),
        None => errmsg,
    };
    Err(Error::new(errmsg))
}

/// Returns the attribute names of a value with attributes.
pub fn attr_names(x: &Value) -> Vec<String> {
    match x {
        Value::Other(o) => o.attr_names(),
        _ => builtin_attr_names(x),
    }
}

/// Implements x.name = y.
pub fn set_field(x: &Value, name: &str, y: Value) -> Result<(), Error> {
    if let Value::Other(o) = x {
        if let Some(res) = o.set_field(name, y) {
            return match res {
                Err(Error::NoSuchAttr(m)) => {
                    // No such field: check spelling.
                    match nearest(name, &o.attr_names()) {
                        Some(n) => Err(Error::new(format!("{} (did you mean .{}?)", m, n))),
                        None => Err(Error::NoSuchAttr(m)),
                    }
                }
                r => r,
            };
        }
    }
    Err(Error::new(format!(
        "can't assign to .{} field of {}",
        name,
        x.type_name()
    )))
}

/// Implements x[y].
pub fn get_index(x: &Value, y: &Value) -> Result<Value, Error> {
    let mapping = match x {
        Value::Dict(d) => Some(d.get(y)),
        Value::Other(o) => o.get(y),
        _ => None,
    };
    if let Some(res) = mapping {
        return match res? {
            Some(z) => Ok(z),
            None => Err(Error::new(format!(
                "key {} not in {}",
                y.repr(),
                x.type_name()
            ))),
        };
    }
    if x.is_indexable() {
        // string, list, tuple
        let n = len(x).unwrap_or(0) as i64;
        let i = as_int32(y).map_err(|e| Error::new(format!("{} index: {}", x.type_name(), e)))?;
        let orig = i as i64;
        let mut i = orig;
        if i < 0 {
            i += n;
        }
        if i < 0 || i >= n {
            return Err(out_of_range(orig, n, x));
        }
        return Ok(x.index(i as usize));
    }
    Err(Error::new(format!(
        "unhandled index operation {}[{}]",
        x.type_name(),
        y.type_name()
    )))
}

pub(crate) fn out_of_range(i: i64, n: i64, x: &Value) -> Error {
    if n == 0 {
        Error::new(format!("index {} out of range: empty {}", i, x.type_name()))
    } else {
        Error::new(format!(
            "{} index {} out of range [{}:{}]",
            x.type_name(),
            i,
            -n,
            n - 1
        ))
    }
}

/// Implements x[y] = z.
pub fn set_index(x: &Value, y: &Value, z: Value) -> Result<(), Error> {
    match x {
        Value::Dict(d) => d.set_key(y.clone(), z),
        Value::List(l) => {
            let n = l.len() as i64;
            let i = as_int32(y)? as i64;
            let orig = i;
            let i = if i < 0 { i + n } else { i };
            if i < 0 || i >= n {
                return Err(out_of_range(orig, n, x));
            }
            l.set_index(i as usize, z)
        }
        Value::Other(o) => {
            if let Some(r) = o.set_key(y.clone(), z.clone()) {
                return r;
            }
            if o.is_indexable() {
                let n = o.len().unwrap_or(0) as i64;
                let i = as_int32(y)? as i64;
                let orig = i;
                let i = if i < 0 { i + n } else { i };
                if i < 0 || i >= n {
                    return Err(out_of_range(orig, n, x));
                }
                if let Some(r) = o.set_index(i as usize, z) {
                    return r;
                }
            }
            Err(Error::new(format!(
                "{} value does not support item assignment",
                x.type_name()
            )))
        }
        _ => Err(Error::new(format!(
            "{} value does not support item assignment",
            x.type_name()
        ))),
    }
}

/// Applies a unary operator (+, -, ~, not) to its operand.
pub fn unary(op: Token, x: &Value) -> Result<Value, Error> {
    // The NOT operator is not customizable.
    if op == Token::Not {
        return Ok(Value::Bool(!x.truth()));
    }

    // Int, Float, and user-defined types
    match x {
        Value::Int(i) => {
            if let Some(v) = i.unary(op) {
                return Ok(v);
            }
        }
        Value::Float(f) => match op {
            Token::Minus => return Ok(Value::Float(-f)),
            Token::Plus => return Ok(Value::Float(*f)),
            _ => {}
        },
        Value::Other(o) => {
            // Ok(None) => unhandled
            if let Some(v) = o.unary(op)? {
                return Ok(v);
            }
        }
        _ => {}
    }

    Err(Error::new(format!(
        "unknown unary op: {} {}",
        op,
        x.type_name()
    )))
}

fn concat<T: Clone>(a: &[T], b: &[T]) -> Vec<T> {
    let mut z = Vec::with_capacity(a.len() + b.len());
    z.extend_from_slice(a);
    z.extend_from_slice(b);
    z
}

fn key_iter(v: &Value) -> Box<dyn super::value::ValueIterator> {
    iterate(v).expect("set is iterable")
}

/// Applies a strict binary operator (not AND or OR) to its operands. For
/// equality tests or ordered comparisons, use `compare` instead.
pub fn binary(op: Token, x: &Value, y: &Value) -> Result<Value, Error> {
    use Value::{Bytes, Float, Int as I, List as L, Set as S, String as Str, Tuple as T};
    let res: Option<Value> = match op {
        Token::Plus => match (x, y) {
            (Str(a), Str(b)) => Some(Value::string(concat(a, b))),
            (Bytes(a), Bytes(b)) => Some(Value::bytes(concat(a, b))),
            (I(a), I(b)) => Some(I(a.add(b))),
            (I(a), Float(b)) => Some(Float(a.finite_float()? + b)),
            (Float(a), Float(b)) => Some(Float(a + b)),
            (Float(a), I(b)) => Some(Float(a + b.finite_float()?)),
            (L(a), L(b)) => {
                let z = concat(&a.elems.borrow(), &b.elems.borrow());
                Some(Value::list(z))
            }
            (T(a), T(b)) => Some(Value::tuple(concat(a, b))),
            _ => None,
        },
        Token::Minus => match (x, y) {
            (I(a), I(b)) => Some(I(a.sub(b))),
            (I(a), Float(b)) => Some(Float(a.finite_float()? - b)),
            (Float(a), Float(b)) => Some(Float(a - b)),
            (Float(a), I(b)) => Some(Float(a - b.finite_float()?)),
            (S(a), S(_)) => Some(a.difference(&mut *key_iter(y))?),
            _ => None,
        },
        Token::Star => match (x, y) {
            (I(a), I(b)) => Some(I(a.mul(b))),
            (I(a), Float(b)) => Some(Float(a.finite_float()? * b)),
            (I(a), Str(s)) => Some(Value::string(string_repeat(s, a)?)),
            (I(a), Bytes(s)) => Some(Value::bytes(string_repeat(s, a)?)),
            (I(a), L(l)) => {
                let elems = l.elems.borrow().clone();
                Some(Value::list(tuple_repeat(&elems, a)?))
            }
            (I(a), T(t)) => Some(Value::tuple(tuple_repeat(t, a)?)),
            (Float(a), Float(b)) => Some(Float(a * b)),
            (Float(a), I(b)) => Some(Float(a * b.finite_float()?)),
            (Str(s), I(b)) => Some(Value::string(string_repeat(s, b)?)),
            (Bytes(s), I(b)) => Some(Value::bytes(string_repeat(s, b)?)),
            (L(l), I(b)) => {
                let elems = l.elems.borrow().clone();
                Some(Value::list(tuple_repeat(&elems, b)?))
            }
            (T(t), I(b)) => Some(Value::tuple(tuple_repeat(t, b)?)),
            _ => None,
        },
        Token::Slash => match (x, y) {
            (I(a), I(b)) => {
                let xf = a.finite_float()?;
                let yf = b.finite_float()?;
                if yf == 0.0 {
                    return Err(Error::new("floating-point division by zero"));
                }
                Some(Float(xf / yf))
            }
            (I(a), Float(b)) => {
                let xf = a.finite_float()?;
                if *b == 0.0 {
                    return Err(Error::new("floating-point division by zero"));
                }
                Some(Float(xf / b))
            }
            (I(a), _) => {
                a.finite_float()?;
                None
            }
            (Float(a), Float(b)) => {
                if *b == 0.0 {
                    return Err(Error::new("floating-point division by zero"));
                }
                Some(Float(a / b))
            }
            (Float(a), I(b)) => {
                let yf = b.finite_float()?;
                if yf == 0.0 {
                    return Err(Error::new("floating-point division by zero"));
                }
                Some(Float(a / yf))
            }
            _ => None,
        },
        Token::Slashslash => match (x, y) {
            (I(a), I(b)) => {
                if b.sign() == 0 {
                    return Err(Error::new("floored division by zero"));
                }
                Some(I(a.div(b)))
            }
            (I(a), Float(b)) => {
                let xf = a.finite_float()?;
                if *b == 0.0 {
                    return Err(Error::new("floored division by zero"));
                }
                Some(Float((xf / b).floor()))
            }
            (Float(a), Float(b)) => {
                if *b == 0.0 {
                    return Err(Error::new("floored division by zero"));
                }
                Some(Float((a / b).floor()))
            }
            (Float(a), I(b)) => {
                let yf = b.finite_float()?;
                if yf == 0.0 {
                    return Err(Error::new("floored division by zero"));
                }
                Some(Float((a / yf).floor()))
            }
            _ => None,
        },
        Token::Percent => match (x, y) {
            (I(a), I(b)) => {
                if b.sign() == 0 {
                    return Err(Error::new("integer modulo by zero"));
                }
                Some(I(a.modulo(b)))
            }
            (I(a), Float(b)) => {
                let xf = a.finite_float()?;
                if *b == 0.0 {
                    return Err(Error::new("floating-point modulo by zero"));
                }
                Some(Float(float_mod(xf, *b)))
            }
            (Float(a), Float(b)) => {
                if *b == 0.0 {
                    return Err(Error::new("floating-point modulo by zero"));
                }
                Some(Float(float_mod(*a, *b)))
            }
            (Float(a), I(b)) => {
                if b.sign() == 0 {
                    return Err(Error::new("floating-point modulo by zero"));
                }
                let yf = b.finite_float()?;
                Some(Float(float_mod(*a, yf)))
            }
            (Str(s), _) => return interpolate(s, y),
            _ => None,
        },
        Token::NotIn => {
            let z = binary(Token::In, x, y)?;
            return Ok(Value::Bool(!z.truth()));
        }
        Token::In => match y {
            // List, Tuple, Set, String, Bytes, rangeValue etc.
            L(l) => {
                let elems = l.elems.borrow().clone();
                Some(Value::Bool(contains(&elems, x)?))
            }
            T(t) => Some(Value::Bool(contains(t, x)?)),
            S(s) => Some(Value::Bool(s.has(x)?)),
            Str(s) => match x {
                Str(needle) => Some(Value::Bool(bytes_contains(s, needle))),
                _ => {
                    return Err(Error::new(format!(
                        "'in <string>' requires string as left operand, not {}",
                        x.type_name()
                    )))
                }
            },
            Bytes(b) => Some(Value::Bool(bytes_has(b, x)?)),
            Value::Dict(d) => {
                // Ignore error from Get as we cannot distinguish true
                // errors (value cycle, type error) from "key not found".
                Some(Value::Bool(matches!(d.get(x), Ok(Some(_)))))
            }
            Value::Other(o) => {
                if let Some(r) = o.has(x) {
                    Some(Value::Bool(r?))
                } else {
                    o.get(x).map(|r| Value::Bool(matches!(r, Ok(Some(_)))))
                }
            }
            _ => None,
        },
        Token::Pipe => match (x, y) {
            (I(a), I(b)) => Some(I(a.or(b))),
            (Value::Dict(a), Value::Dict(b)) => Some(Value::Dict(Rc::new(a.union(b)))),
            (S(a), S(_)) => Some(a.union(&mut *key_iter(y))?),
            _ => None,
        },
        Token::Amp => match (x, y) {
            (I(a), I(b)) => Some(I(a.and(b))),
            (S(a), S(_)) => Some(a.intersection(&mut *key_iter(y))?),
            _ => None,
        },
        Token::Circumflex => match (x, y) {
            (I(a), I(b)) => Some(I(a.xor(b))),
            (S(a), S(_)) => Some(a.symmetric_difference(&mut *key_iter(y))?),
            _ => None,
        },
        Token::Ltlt | Token::Gtgt => match x {
            I(a) => {
                let y = as_int32(y)?;
                if y < 0 {
                    return Err(Error::new(format!("negative shift count: {}", y)));
                }
                if op == Token::Ltlt {
                    if y >= 512 {
                        return Err(Error::new(format!("shift count too large: {}", y)));
                    }
                    Some(I(a.lsh(y as u32)))
                } else {
                    Some(I(a.rsh(y as u32)))
                }
            }
            _ => None,
        },
        _ => {
            // unknown operator
            return Err(unknown_binary(op, x, y));
        }
    };
    if let Some(z) = res {
        return Ok(z);
    }

    // user-defined types
    // Ok(None) => unhandled
    if let Value::Other(o) = x {
        if let Some(z) = o.binary(op, y, Side::Left)? {
            return Ok(z);
        }
    }
    if let Value::Other(o) = y {
        if let Some(z) = o.binary(op, x, Side::Right)? {
            return Ok(z);
        }
    }

    // unsupported operand types
    Err(unknown_binary(op, x, y))
}

fn unknown_binary(op: Token, x: &Value, y: &Value) -> Error {
    Error::new(format!(
        "unknown binary op: {} {} {}",
        x.type_name(),
        op,
        y.type_name()
    ))
}

fn contains(elems: &[Value], y: &Value) -> Result<bool, Error> {
    for x in elems {
        if super::value::equal(x, y)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reports whether `needle` occurs in `s` (Go's strings.Contains).
pub fn bytes_contains(s: &[u8], needle: &[u8]) -> bool {
    find_bytes(s, needle).is_some()
}

/// Returns the index of the first occurrence of `needle` in `s`.
pub fn find_bytes(s: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > s.len() {
        return None;
    }
    s.windows(needle.len()).position(|w| w == needle)
}

/// Returns the index of the last occurrence of `needle` in `s`.
pub fn rfind_bytes(s: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(s.len());
    }
    if needle.len() > s.len() {
        return None;
    }
    s.windows(needle.len()).rposition(|w| w == needle)
}

fn bytes_has(b: &[u8], y: &Value) -> Result<bool, Error> {
    match y {
        Value::Bytes(needle) => Ok(bytes_contains(b, needle)),
        Value::Int(_) => {
            let by = super::int::as_unsigned(y, 8)
                .map_err(|e| Error::new(format!("int in bytes: {}", e)))?;
            Ok(b.contains(&(by as u8)))
        }
        _ => Err(Error::new(format!(
            "'in bytes' requires bytes or int as left operand, not {}",
            y.type_name()
        ))),
    }
}

// It's always possible to overeat in small bites but we'll
// try to stop someone swallowing the world in one gulp.
const MAX_ALLOC: u64 = 1 << 30;

fn tuple_repeat(elems: &[Value], n: &Int) -> Result<Vec<Value>, Error> {
    if elems.is_empty() {
        return Ok(Vec::new());
    }
    let i = as_int32(&Value::Int(n.clone()))
        .map_err(|_| Error::new(format!("repeat count {} too large", n)))?;
    if i < 1 {
        return Ok(Vec::new());
    }
    // Inv: i > 0, len > 0
    let sz = (elems.len() as u128) * (i as u128);
    if sz >= MAX_ALLOC as u128 {
        // Don't print sz.
        return Err(Error::new(format!(
            "excessive repeat ({} * {} elements)",
            elems.len(),
            i
        )));
    }
    let mut res = Vec::with_capacity(sz as usize);
    for _ in 0..i {
        res.extend_from_slice(elems);
    }
    Ok(res)
}

fn string_repeat(s: &[u8], n: &Int) -> Result<Vec<u8>, Error> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let i = as_int32(&Value::Int(n.clone()))
        .map_err(|_| Error::new(format!("repeat count {} too large", n)))?;
    if i < 1 {
        return Ok(Vec::new());
    }
    // Inv: i > 0, len > 0
    let sz = (s.len() as u128) * (i as u128);
    if sz >= MAX_ALLOC as u128 {
        // Don't print sz.
        return Err(Error::new(format!(
            "excessive repeat ({} * {} elements)",
            s.len(),
            i
        )));
    }
    Ok(s.repeat(i as usize))
}

/// Calls the function fn with the specified positional and keyword
/// arguments.
pub fn call(
    thread: &mut Thread,
    f: &Value,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    if !f.is_callable() {
        return Err(Error::new(format!(
            "invalid call of non-function ({})",
            f.type_name()
        )));
    }

    if !thread.initialized {
        // one-time initialization of thread
        thread.initialized = true;
        if thread.max_steps == 0 {
            thread.max_steps = u64::MAX;
        }
    }

    thread.stack.push(Frame {
        callable: f.clone(),
        pc: 0,
        locals: Vec::new(),
    }); // push

    let result = match f {
        Value::Function(func) => super::interp::call_internal(func, thread, args, kwargs),
        Value::Builtin(b) => b.call_internal(thread, args, kwargs),
        Value::Other(o) => o.call(thread, args, kwargs),
        _ => unreachable!("callable"),
    };

    // Always return an EvalError with an accurate frame.
    let result = match result {
        Err(e) if e.as_eval().is_none() => Err(thread.eval_error(e)),
        r => r,
    };

    thread.stack.pop(); // pop
    result
}

/// Implements x[lo:hi:step].
pub(crate) fn slice(x: &Value, lo: &Value, hi: &Value, step: &Value) -> Result<Value, Error> {
    let sliceable = match x {
        Value::String(_) | Value::Bytes(_) | Value::List(_) | Value::Tuple(_) => true,
        Value::Other(o) => o.is_sliceable(),
        _ => false,
    };
    if !sliceable {
        return Err(Error::new(format!(
            "invalid slice operand {}",
            x.type_name()
        )));
    }

    let n = len(x).unwrap_or(0) as i64;
    let mut stepv: i64 = 1;
    if !matches!(step, Value::None) {
        stepv =
            as_int32(step).map_err(|e| Error::new(format!("invalid slice step: {}", e)))? as i64;
        if stepv == 0 {
            return Err(Error::new("zero is not a valid slice step"));
        }
    }

    let (start, end);
    if stepv > 0 {
        // positive stride
        // default indices are [0:n].
        let (s, mut e) = indices(Some(lo), Some(hi), n)?;
        if e < s {
            e = s; // => empty result
        }
        start = s;
        end = e;
    } else {
        // negative stride
        // default indices are effectively [n-1:-1], though to
        // get this effect using explicit indices requires
        // [n-1:-1-n:-1] because of the treatment of -ve values.
        let mut s = n - 1;
        as_index(Some(lo), n, &mut s)
            .map_err(|e| Error::new(format!("invalid start index: {}", e)))?;
        if s >= n {
            s = n - 1;
        }

        let mut e = -1;
        as_index(Some(hi), n, &mut e)
            .map_err(|e| Error::new(format!("invalid end index: {}", e)))?;
        if e < -1 {
            e = -1;
        }

        if s < e {
            s = e; // => empty result
        }
        start = s;
        end = e;
    }

    Ok(match x {
        Value::String(s) => Value::string(super::value::slice_bytes(s, start, end, stepv)),
        Value::Bytes(s) => Value::bytes(super::value::slice_bytes(s, start, end, stepv)),
        Value::List(l) => Value::list(super::value::slice_values(
            &l.elems.borrow(),
            start,
            end,
            stepv,
        )),
        Value::Tuple(t) => Value::tuple(super::value::slice_values(t, start, end, stepv)),
        Value::Other(o) => o.slice(start, end, stepv),
        _ => unreachable!("sliceable"),
    })
}

/// Converts start_ and end_ to indices in the range [0:len]. The start
/// index defaults to 0 and the end index defaults to len. An index
/// -len < i < 0 is treated like i+len. All other indices outside the
/// range are clamped to the nearest value in the range. Beware: start may
/// be greater than end. This function is suitable only for slices with
/// positive strides.
pub fn indices(
    start_: Option<&Value>,
    end_: Option<&Value>,
    len: i64,
) -> Result<(i64, i64), Error> {
    let mut start = 0;
    as_index(start_, len, &mut start)
        .map_err(|e| Error::new(format!("invalid start index: {}", e)))?;
    // Clamp to [0:len].
    start = start.clamp(0, len);

    let mut end = len;
    as_index(end_, len, &mut end).map_err(|e| Error::new(format!("invalid end index: {}", e)))?;
    // Clamp to [0:len].
    end = end.clamp(0, len);

    Ok((start, end))
}

/// Sets *result to the integer value of v, adding len to it if it is
/// negative. If v is absent or None, *result is unchanged.
fn as_index(v: Option<&Value>, len: i64, result: &mut i64) -> Result<(), Error> {
    if let Some(v) = v {
        if !matches!(v, Value::None) {
            *result = as_int32(v)? as i64;
            if *result < 0 {
                *result += len;
            }
        }
    }
    Ok(())
}

/// Sets the values of the formal parameters of function fn based on the
/// actual parameter values in args and kwargs.
pub(crate) fn set_args(
    locals: &mut [Option<Value>],
    f: &Function,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<(), Error> {
    // This is the general schema of a function:
    //
    //   def f(p1, p2=dp2, p3=dp3, *args, k1, k2=dk2, k3, **kwargs)
    //
    // The p parameters are non-kwonly, and may be specified positionally.
    // The k parameters are kwonly, and must be specified by name.
    // The defaults tuple is (dp2, dp3, mandatory, dk2, mandatory).
    //
    // Arguments are processed as follows:
    // - positional arguments are bound to a prefix of [p1, p2, p3].
    // - surplus positional arguments are bound to *args.
    // - keyword arguments are bound to any of {p1, p2, p3, k1, k2, k3};
    //   duplicate bindings are rejected.
    // - surplus keyword arguments are bound to **kwargs.
    // - defaults are bound to each parameter from p2 to k3 if no value was set.
    //   default values come from the tuple above.
    //   It is an error if the tuple entry for an unset parameter is 'mandatory'.

    // Nullary function?
    if f.num_params() == 0 {
        let nactual = args.len() + kwargs.len();
        if nactual > 0 {
            return Err(Error::new(format!(
                "function {} accepts no arguments ({} given)",
                f.name(),
                nactual
            )));
        }
        return Ok(());
    }

    // nparams is the number of ordinary parameters (sans *args and **kwargs).
    let mut nparams = f.num_params();
    let mut kwdict: Option<Rc<Dict>> = None;
    if f.has_kwargs() {
        nparams -= 1;
        let d = Rc::new(Dict::new());
        locals[nparams] = Some(Value::Dict(d.clone()));
        kwdict = Some(d);
    }
    if f.has_varargs() {
        nparams -= 1;
    }

    // nonkwonly is the number of non-kwonly parameters.
    let nonkwonly = nparams - f.num_kwonly_params();

    // Too many positional args?
    let mut n = args.len();
    if args.len() > nonkwonly {
        if !f.has_varargs() {
            return Err(Error::new(format!(
                "function {} accepts {}{} positional argument{} ({} given)",
                f.name(),
                if f.defaults.len() > f.num_kwonly_params() {
                    "at most "
                } else {
                    ""
                },
                nonkwonly,
                if nonkwonly == 1 { "" } else { "s" },
                args.len()
            )));
        }
        n = nonkwonly;
    }

    // Bind positional arguments to non-kwonly parameters.
    for i in 0..n {
        locals[i] = Some(args[i].clone());
    }

    // Bind surplus positional arguments to *args parameter.
    if f.has_varargs() {
        locals[nparams] = Some(Value::tuple(args[n..].to_vec()));
    }

    // Bind keyword arguments to parameters.
    let param_idents = &f.funcode.locals[..nparams];
    for (k, v) in kwargs {
        let kname = k.as_str().unwrap_or_default();
        if let Some(i) = param_idents.iter().position(|p| p.name.as_bytes() == kname) {
            if locals[i].is_some() {
                return Err(Error::new(format!(
                    "function {} got multiple values for parameter {}",
                    f.name(),
                    k.repr()
                )));
            }
            locals[i] = Some(v.clone());
            continue;
        }
        let Some(kwdict) = &kwdict else {
            return Err(Error::new(format!(
                "function {} got an unexpected keyword argument {}",
                f.name(),
                k.repr()
            )));
        };
        let oldlen = kwdict.len();
        kwdict.set_key(k.clone(), v.clone())?;
        if kwdict.len() == oldlen {
            return Err(Error::new(format!(
                "function {} got multiple values for parameter {}",
                f.name(),
                k.repr()
            )));
        }
    }

    // Are defaults required?
    if n < nparams || f.num_kwonly_params() > 0 {
        let m = nparams - f.defaults.len(); // first default

        // Report errors for missing required arguments.
        let mut missing: Vec<&str> = Vec::new();
        let mut i = n;
        while i < m {
            if locals[i].is_none() {
                missing.push(&param_idents[i].name);
            }
            i += 1;
        }

        // Bind default values to parameters.
        while i < nparams {
            if locals[i].is_none() {
                let dflt = &f.defaults[i - m];
                if super::interp::is_mandatory(dflt) {
                    missing.push(&param_idents[i].name);
                } else {
                    locals[i] = Some(dflt.clone());
                }
            }
            i += 1;
        }

        if !missing.is_empty() {
            return Err(Error::new(format!(
                "function {} missing {} argument{} ({})",
                f.name(),
                missing.len(),
                if missing.len() > 1 { "s" } else { "" },
                missing.join(", ")
            )));
        }
    }
    Ok(())
}

/// Implements the string interpolation operator, format % x.
pub fn interpolate(format: &[u8], x: &Value) -> Result<Value, Error> {
    let mut buf: Vec<u8> = Vec::new();
    let mut index = 0;
    let tuple: Option<&Rc<[Value]>> = match x {
        Value::Tuple(t) => Some(t),
        _ => None,
    };
    let nargs = tuple.map_or(1, |t| t.len());
    let is_mapping = match x {
        Value::Dict(_) => true,
        Value::Other(o) => o.get(&Value::None).is_some(),
        _ => false,
    };
    let mut format = format;
    loop {
        let Some(i) = format.iter().position(|&c| c == b'%') else {
            buf.extend_from_slice(format);
            break;
        };
        buf.extend_from_slice(&format[..i]);
        format = &format[i + 1..];

        if format.first() == Some(&b'%') {
            buf.push(b'%');
            format = &format[1..];
            continue;
        }

        let arg: Value;
        if format.first() == Some(&b'(') {
            // keyword argument: %(name)s.
            format = &format[1..];
            let Some(j) = format.iter().position(|&c| c == b')') else {
                return Err(Error::new("incomplete format key"));
            };
            let key = &format[..j];
            // As in the original (`v, found, _ := dict.Get(...)`), a lookup error such as an
            // unhashable key is reported as "key not found" below, so it is deliberately not
            // propagated here.
            let found = match x {
                Value::Dict(d) => d.get(&Value::string(key)).ok().flatten(),
                Value::Other(o) => match o.get(&Value::string(key)) {
                    Some(r) => r.ok().flatten(),
                    None => return Err(Error::new("format requires a mapping")),
                },
                _ => return Err(Error::new("format requires a mapping")),
            };
            match found {
                Some(v) => arg = v,
                None => {
                    return Err(Error::new(format!(
                        "key not found: {}",
                        String::from_utf8_lossy(key)
                    )));
                }
            }
            format = &format[j + 1..];
        } else {
            // positional argument: %s.
            if index >= nargs {
                return Err(Error::new("not enough arguments for format string"));
            }
            arg = match tuple {
                Some(t) => t[index].clone(),
                None => x.clone(),
            };
        }

        // NOTE: Starlark does not support any of these optional Python features:
        // - optional conversion flags: [#0- +], etc.
        // - optional minimum field width (number or *).
        // - optional precision (.123 or *)
        // - optional length modifier

        // conversion type
        let Some(&c) = format.first() else {
            return Err(Error::new("incomplete format"));
        };
        match c {
            b's' | b'r' => match (&arg, c) {
                (Value::String(s), b's') => buf.extend_from_slice(s),
                _ => buf.extend_from_slice(arg.repr().as_bytes()),
            },
            b'd' | b'i' | b'o' | b'x' | b'X' => {
                let i = number_to_int(&arg).map_err(|e| {
                    Error::new(format!("%{} format requires integer: {}", c as char, e))
                })?;
                let s = match c {
                    b'o' => i.text(8),
                    b'x' => i.text(16),
                    b'X' => i.text(16).to_ascii_uppercase(),
                    _ => i.text(10),
                };
                buf.extend_from_slice(s.as_bytes());
            }
            b'e' | b'f' | b'g' | b'E' | b'F' | b'G' => {
                let Some(f) = arg.as_float() else {
                    return Err(Error::new(format!(
                        "%{} format requires float, not {}",
                        c as char,
                        arg.type_name()
                    )));
                };
                let mut s = String::new();
                format_float(&mut s, f, c);
                buf.extend_from_slice(s.as_bytes());
            }
            b'c' => match &arg {
                Value::Int(_) => {
                    // chr(int)
                    let r = as_int32(&arg)
                        .ok()
                        .and_then(|r| u32::try_from(r).ok())
                        .filter(|&r| r <= 0x10FFFF);
                    let Some(r) = r else {
                        return Err(Error::new(format!(
                            "%c format requires a valid Unicode code point, got {}",
                            arg.repr()
                        )));
                    };
                    syntax::utf8::push_rune(
                        &mut buf,
                        char::from_u32(r).unwrap_or(syntax::utf8::RUNE_ERROR),
                    );
                }
                Value::String(s) => {
                    let (r, size) = syntax::utf8::decode_rune(s);
                    if size != s.len() || s.is_empty() {
                        return Err(Error::new("%c format requires a single-character string"));
                    }
                    syntax::utf8::push_rune(&mut buf, r);
                }
                _ => {
                    return Err(Error::new(format!(
                        "%c format requires int or single-character string, not {}",
                        arg.type_name()
                    )));
                }
            },
            b'%' => buf.push(b'%'),
            _ => {
                let (r, _) = syntax::utf8::decode_rune(format);
                return Err(Error::new(format!("unknown conversion %{}", go_char(c, r))));
            }
        }
        format = &format[1..];
        index += 1;
    }

    if index < nargs && !is_mapping {
        return Err(Error::new("too many arguments for format string"));
    }

    Ok(Value::string(buf))
}

/// Formats a byte as Go's `%c` verb does for a byte value: bytes >= 0x80
/// are the code points U+0080..U+00FF.
fn go_char(c: u8, _r: char) -> char {
    char::from_u32(c as u32).unwrap_or(syntax::utf8::RUNE_ERROR)
}

/// Returns the element of candidates nearest to x, if any is close.
pub(crate) fn nearest<S: AsRef<str>>(x: &str, candidates: &[S]) -> Option<String> {
    let n = crate::spell::nearest(x, candidates);
    if n.is_empty() {
        None
    } else {
        Some(n)
    }
}

/// Returns the universe value of the given name (for the interpreter).
pub(crate) fn universal(name: &str) -> Option<Value> {
    universe_get(name)
}
