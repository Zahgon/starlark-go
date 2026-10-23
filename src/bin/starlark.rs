// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The starlark command interprets a Starlark file.
//! With no arguments, it starts a read-eval-print loop (REPL).
//!
//! Command-line flags are parsed with the rules of Go's flag package. The
//! profiling flags and -disassemble are accepted but have no effect.

use std::io::{IsTerminal, Write};

use starlark::repl;
use starlark::starlark::{exec_file, set_legacy_file_options, universe_insert, StringDict, Thread};
use starlark::syntax::FileOptions;

const USAGE: &str = "  -c prog
    \texecute program prog
  -cpuprofile string
    \tgather Go CPU profile in this file
  -disassemble
    \tshow disassembly during compilation of each function
  -float
    \tobsolete; no effect (default true)
  -globalreassign
    \tallow reassignment of globals, and if/for/while statements at top level
  -lambda
    \tobsolete; no effect (default true)
  -memprofile string
    \tgather Go memory profile in this file
  -profile string
    \tgather Starlark time profile in this file
  -recursion
    \tallow while statements and recursive functions
  -set
    \tobsolete; no effect (default true)
  -showenv
    \ton success, print final global environment
";

/// The values of the command-line flags.
#[derive(Default)]
struct Flags {
    showenv: bool,
    execprog: String,
    recursion: bool,
    globalreassign: bool,
    set: bool,
}

/// The outcome of a failed flag parse.
enum FlagError {
    /// -h or -help: print usage and exit 0.
    Help,
    /// A malformed command line: print the message and usage, exit 2.
    Usage(String),
}

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Str,
}

fn flag_kind(name: &str) -> Option<Kind> {
    match name {
        "c" | "cpuprofile" | "memprofile" | "profile" => Some(Kind::Str),
        "disassemble" | "float" | "globalreassign" | "lambda" | "recursion" | "set" | "showenv" => {
            Some(Kind::Bool)
        }
        _ => None,
    }
}

/// Parses a boolean like Go's strconv.ParseBool.
fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn go_quote(s: &str) -> String {
    starlark::starlark::fmt::go_quote(s.as_bytes())
}

/// Parses the command line like Go's flag.Parse, returning the flags and
/// the remaining (non-flag) arguments.
fn parse_flags(mut args: &[String]) -> Result<(Flags, Vec<String>), FlagError> {
    let mut flags = Flags {
        set: true,
        ..Flags::default()
    };
    while let Some(s) = args.first() {
        if s.len() < 2 || !s.starts_with('-') {
            break;
        }
        let mut num_minuses = 1;
        if s.as_bytes()[1] == b'-' {
            num_minuses += 1;
            if s.len() == 2 {
                // "--" terminates the flags
                args = &args[1..];
                break;
            }
        }
        let mut name = &s[num_minuses..];
        if name.is_empty() || name.starts_with('-') || name.starts_with('=') {
            return Err(FlagError::Usage(format!("bad flag syntax: {}", s)));
        }

        // it's a flag. does it have an argument?
        args = &args[1..];
        let mut value: Option<&str> = None;
        if let Some(i) = name[1..].find('=') {
            value = Some(&name[i + 2..]);
            name = &name[..i + 1];
        }

        let Some(kind) = flag_kind(name) else {
            if name == "help" || name == "h" {
                // special case for nice help message.
                return Err(FlagError::Help);
            }
            return Err(FlagError::Usage(format!(
                "flag provided but not defined: -{}",
                name
            )));
        };

        match kind {
            Kind::Bool => {
                // special case: doesn't need an arg
                let v = match value {
                    Some(v) => match parse_bool(v) {
                        Some(b) => b,
                        None => {
                            return Err(FlagError::Usage(format!(
                                "invalid boolean value {} for -{}: parse error",
                                go_quote(v),
                                name
                            )))
                        }
                    },
                    None => true,
                };
                match name {
                    "showenv" => flags.showenv = v,
                    "recursion" => flags.recursion = v,
                    "globalreassign" => flags.globalreassign = v,
                    "set" => flags.set = v,
                    _ => {} // -float, -lambda: obsolete; -disassemble: not supported
                }
            }
            Kind::Str => {
                // It must have a value, which might be the next argument.
                let v = match value {
                    Some(v) => v.to_string(),
                    None => match args.first() {
                        Some(next) => {
                            args = &args[1..];
                            next.clone()
                        }
                        None => {
                            return Err(FlagError::Usage(format!(
                                "flag needs an argument: -{}",
                                name
                            )))
                        }
                    },
                };
                if name == "c" {
                    flags.execprog = v;
                }
            }
        }
    }
    Ok((flags, args.to_vec()))
}

fn do_main() -> i32 {
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "starlark".to_string());
    let usage = || {
        eprint!("Usage of {}:\n{}", prog, USAGE);
    };
    let (flags, rest) = match parse_flags(argv.get(1..).unwrap_or(&[])) {
        Ok(r) => r,
        Err(FlagError::Help) => {
            usage();
            return 0;
        }
        Err(FlagError::Usage(msg)) => {
            eprintln!("{}", msg);
            usage();
            return 2;
        }
    };

    set_legacy_file_options(FileOptions::legacy(
        flags.set,
        flags.globalreassign,
        flags.recursion,
    ));

    let mut thread = Thread::new("");
    thread.load = Some(repl::make_load());
    let mut globals = StringDict::new();

    // Ideally this statement would update the predeclared environment.
    for (name, module) in starlark::lib::modules() {
        universe_insert(name, module);
    }

    if rest.len() == 1 || !flags.execprog.is_empty() {
        let (filename, src) = if !flags.execprog.is_empty() {
            // Execute provided program.
            ("cmdline".to_string(), Some(flags.execprog.as_bytes()))
        } else {
            // Execute specified file.
            (rest[0].clone(), None)
        };
        thread.name = format!("exec {}", filename);
        match exec_file(&mut thread, &filename, src, &StringDict::new()) {
            Ok(g) => globals = g,
            Err(err) => {
                repl::print_error(&err);
                return 1;
            }
        }
    } else if rest.is_empty() {
        let stdin_is_terminal = std::io::stdin().is_terminal();
        if stdin_is_terminal {
            println!("Welcome to Starlark (go.starlark.net)");
        }
        thread.name = "REPL".to_string();
        repl::repl(&mut thread, &mut globals);
        if stdin_is_terminal {
            println!();
        }
    } else {
        eprintln!("starlark: want at most one Starlark file name");
        return 1;
    }

    // Print the global environment.
    if flags.showenv {
        let mut err = std::io::stderr().lock();
        for (name, v) in globals.iter() {
            if !name.starts_with('_') {
                // Like the original's fmt.Fprintf, a failed write to stderr is ignored.
                let _ = writeln!(err, "{} = {}", name, v.repr());
            }
        }
    }

    0
}

fn main() {
    // Starlark recursion (with -recursion) may nest up to 100000 calls
    // deep, so the interpreter runs on a thread with a large stack.
    let child = std::thread::Builder::new()
        .stack_size(4 << 30)
        .spawn(do_main);
    let code = match child {
        Ok(handle) => handle.join().unwrap_or(2),
        Err(_) => do_main(),
    };
    // The exit status is already decided; a failed final flush cannot change it.
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
