// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! A read/eval/print loop for Starlark (ported from go.starlark.net/repl).
//!
//! If an input line can be parsed as an expression, the REPL parses and
//! evaluates it and prints its result. Otherwise the REPL reads lines
//! until a blank line, then tries again to parse the multi-line input as
//! an expression. If the input still cannot be parsed as an expression,
//! the REPL parses and executes it as a file (a list of statements), for
//! side effects.
//!
//! Unlike the Go original, which uses a readline library, input lines are
//! read from stdin without line editing; when stdin is a terminal, the
//! prompts ">>> " and "... " are written to stdout.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::rc::Rc;

use crate::starlark::{
    eval_expr_options, exec_file_options, exec_repl_chunk, Error, LoadFn, StringDict, Thread, Value,
};
use crate::syntax::{self, FileOptions, Stmt};

/// Executes a read, eval, print loop using the legacy file options.
pub fn repl(thread: &mut Thread, globals: &mut StringDict) {
    repl_options(&crate::starlark::legacy_file_options(), thread, globals);
}

/// Executes a read, eval, print loop.
pub fn repl_options(opts: &FileOptions, thread: &mut Thread, globals: &mut StringDict) {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut input = stdin.lock();
    while rep(opts, &mut input, interactive, thread, globals) {}
}

/// Reads, evaluates, and prints one item. It returns false at end of
/// input. Starlark errors are printed.
fn rep(
    opts: &FileOptions,
    input: &mut dyn BufRead,
    interactive: bool,
    thread: &mut Thread,
    globals: &mut StringDict,
) -> bool {
    let mut eof = false;

    // readline returns EOF or a line including "\n".
    let mut prompt = ">>> ";
    let mut readline = || -> std::io::Result<Option<Vec<u8>>> {
        if interactive {
            print!("{}", prompt);
            // A prompt that cannot be flushed is not an error, as in the original.
            let _ = std::io::stdout().flush();
        }
        prompt = "... ";
        let mut line = Vec::new();
        let n = input.read_until(b'\n', &mut line)?;
        if n == 0 {
            eof = true;
            return Ok(None);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        line.push(b'\n');
        Ok(Some(line))
    };

    // Treat load bindings as global (like they used to be) in the REPL.
    let mut opts = opts.clone();
    opts.load_binds_globally = true;

    // parse
    let parsed = syntax::parse_compound_stmt(&opts, "<stdin>", &mut readline);
    let mut f = match parsed {
        Ok(f) => f,
        Err(err) => {
            if eof {
                return false;
            }
            print_error(&Error::new(err.to_string()));
            return true;
        }
    };

    if f.stmts.len() == 1 && matches!(f.stmts[0], Stmt::Expr(_)) {
        let Some(Stmt::Expr(stmt)) = f.stmts.pop() else {
            return true;
        };
        let mut expr = stmt.x;
        // eval
        let fopts = f.options.clone();
        match eval_expr_options(&fopts, thread, &mut expr, globals) {
            Err(err) => {
                print_error(&err);
                return true;
            }
            Ok(v) => {
                // store the result in "_" variable to hold the value of last expression, similar to Python REPL
                globals.insert("_", v.clone());

                // print
                if !matches!(v, Value::None) {
                    println!("{}", v.repr());
                }
            }
        }
    } else if let Err(err) = exec_repl_chunk(&mut f, thread, globals) {
        print_error(&err);
        return true;
    }

    true
}

/// Prints the error to stderr, or its backtrace if it is a Starlark
/// evaluation error.
pub fn print_error(err: &Error) {
    // Keep stdout and stderr in order; a failed flush is not itself reportable.
    let _ = std::io::stdout().flush();
    match err.as_eval() {
        Some(e) => eprintln!("{}", e.backtrace()),
        None => eprintln!("{}", err),
    }
}

/// Returns a simple sequential implementation of module loading suitable
/// for use in the REPL, using the legacy file options.
pub fn make_load() -> Rc<LoadFn> {
    make_load_options(crate::starlark::legacy_file_options())
}

/// Returns a simple sequential implementation of module loading suitable
/// for use in the REPL. Each function returned by make_load_options
/// accesses a distinct private cache.
pub fn make_load_options(opts: FileOptions) -> Rc<LoadFn> {
    // None marks a load in progress.
    let cache: RefCell<HashMap<String, Option<Result<StringDict, Error>>>> =
        RefCell::new(HashMap::new());
    Rc::new(
        move |thread: &mut Thread, module: &str| -> Result<StringDict, Error> {
            if let Some(e) = cache.borrow().get(module) {
                return match e {
                    Some(r) => r.clone(),
                    // request for package whose loading is in progress
                    None => Err(Error::new("cycle in load graph")),
                };
            }

            // Add a placeholder to indicate "load in progress".
            cache.borrow_mut().insert(module.to_string(), None);

            // Load it.
            let mut child = Thread::new(&format!("exec {}", module));
            child.load = thread.load.clone();
            let result = exec_file_options(&opts, &mut child, module, None, &StringDict::new());

            // Update the cache.
            cache
                .borrow_mut()
                .insert(module.to_string(), Some(result.clone()));
            result
        },
    )
}
