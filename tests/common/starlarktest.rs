// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Test helpers for Starlark code (ported from go.starlark.net/starlarktest):
//! the assert module and its built-ins, and the reporter that collects the
//! failures reported by assert.star.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use regex::bytes::Regex;

use starlark::starlark::{
    call, exec_file, unpack_args, unpack_positional_args, Builtin, Callable, Error, StringDict,
    Thread, Value,
};
use starlark::starlarkstruct;

/// Adapts a Go regexp to the regex crate: Go treats a '{' that does not
/// begin a valid repetition as a literal.
pub fn go_regex(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' && i + 1 < b.len() {
            out.push('\\');
            i += 1;
            let ch = p[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        if c == b'{' {
            let rest = &p[i + 1..];
            let valid = rest.find('}').is_some_and(|j| {
                let body = &rest[..j];
                let mut parts = body.splitn(2, ',');
                let first = parts.next().unwrap_or("");
                !first.is_empty()
                    && first.bytes().all(|d| d.is_ascii_digit())
                    && parts
                        .next()
                        .is_none_or(|s| s.bytes().all(|d| d.is_ascii_digit()))
            });
            if !valid {
                out.push_str("\\{");
                i += 1;
                continue;
            }
        }
        let ch = p[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    out
}

/// Collects the failures reported by the assert module's `error` built-in.
pub type Reporter = RefCell<Vec<String>>;

/// Sets the reporter of failures for the thread (Go's SetReporter) and
/// returns it.
pub fn set_reporter(thread: &mut Thread) -> Rc<Reporter> {
    let r: Rc<Reporter> = Rc::new(RefCell::new(Vec::new()));
    thread.set_local("Reporter", r.clone());
    r
}

fn reporter(thread: &Thread) -> Rc<dyn Any> {
    thread
        .local("Reporter")
        .expect("starlarktest.SetReporter was not called")
}

fn catch(
    thread: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut f = Callable(Value::None);
    unpack_args("catch", args, kwargs, &mut [("fn", &mut f)])?;
    match call(thread, &f.0, &[], &[]) {
        Err(err) => Ok(Value::string(err.to_string())),
        Ok(_) => Ok(Value::None),
    }
}

fn matches(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut pattern: Vec<u8> = Vec::new();
    let mut s: Vec<u8> = Vec::new();
    unpack_args(
        "matches",
        args,
        kwargs,
        &mut [("pattern", &mut pattern), ("str", &mut s)],
    )?;
    let rx = Regex::new(&go_regex(&String::from_utf8_lossy(&pattern)))
        .map_err(|e| Error::new(format!("matches: {}", e)))?;
    Ok(Value::Bool(rx.is_match(&s)))
}

fn error_(
    thread: &mut Thread,
    _: &Builtin,
    args: &[Value],
    _: &[(Value, Value)],
) -> Result<Value, Error> {
    if args.len() != 1 {
        return Err(Error::new(format!(
            "error: got {} arguments, want 1",
            args.len()
        )));
    }
    let mut stk = thread.call_stack();
    stk.pop();
    let mut buf = format!("{}Error: ", stk);
    match &args[0] {
        Value::String(s) => buf.push_str(&String::from_utf8_lossy(s)),
        v => buf.push_str(&v.repr()),
    }
    let r = reporter(thread);
    r.downcast_ref::<Reporter>()
        .expect("reporter")
        .borrow_mut()
        .push(buf);
    Ok(Value::None)
}

fn freeze(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    if !kwargs.is_empty() {
        return Err(Error::new("freeze does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "freeze got {} arguments, wants 1",
            args.len()
        )));
    }
    args[0].freeze();
    Ok(args[0].clone())
}

fn floateq(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut xf = 0.0f64;
    let mut yf = 0.0f64;
    unpack_positional_args(b.name(), args, kwargs, 2, &mut [&mut xf, &mut yf])?;
    let res = if xf == yf {
        true
    } else if xf.is_nan() {
        yf.is_nan()
    } else if yf.is_nan() {
        false
    } else {
        let (x, y) = (xf.to_bits(), yf.to_bits());
        x == y.wrapping_add(1) || y == x.wrapping_add(1)
    };
    Ok(Value::Bool(res))
}

thread_local! {
    static ASSERT: RefCell<Option<Result<StringDict, Error>>> = const { RefCell::new(None) };
}

/// Loads the assert module, which is cached per OS thread.
pub fn load_assert_module() -> Result<StringDict, Error> {
    if let Some(r) = ASSERT.with(|a| a.borrow().clone()) {
        return r;
    }
    let mut predeclared = StringDict::new();
    predeclared.insert("error", Builtin::value("error", error_));
    predeclared.insert("catch", Builtin::value("catch", catch));
    predeclared.insert("matches", Builtin::value("matches", matches));
    predeclared.insert(
        "module",
        Builtin::value("module", starlarkstruct::make_module),
    );
    predeclared.insert("_freeze", Builtin::value("freeze", freeze));
    predeclared.insert("_floateq", Builtin::value("floateq", floateq));
    let src = std::fs::read("starlarktest/assert.star").expect("assert.star");
    let mut thread = Thread::new("");
    let r = exec_file(&mut thread, "assert.star", Some(&src), &predeclared);
    ASSERT.with(|a| *a.borrow_mut() = Some(r.clone()));
    r
}
