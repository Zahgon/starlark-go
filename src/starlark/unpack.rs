// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Helpers used by built-in functions to interpret their call arguments
//! (ported from unpack.go).
//!
//! Each parameter variable implements [`Unpacker`]. Its initial value is
//! the parameter's default: it is left unchanged if the argument is
//! absent. Use `Option<T>` for parameters whose absence must be
//! distinguishable (Go's nil default).
//!
//! ```text
//! let mut x = Value::int(0);
//! let mut base: Option<Value> = None;
//! unpack_args("int", args, kwargs, &mut [("x", &mut x), ("base?", &mut base)])?;
//! ```

use std::rc::Rc;

use super::eval::nearest;
use super::int::{as_signed, as_unsigned, Int};
use super::value::{Dict, Error, List, Set, Value};

/// An Unpacker defines how an argument value is converted into a
/// parameter variable.
pub trait Unpacker {
    /// Converts v and stores it into self, or returns an error describing
    /// the mismatch (e.g. "got int, want string"). On failure, self is not
    /// modified.
    fn unpack(&mut self, v: &Value) -> Result<(), Error>;
}

/// A parameter that accepts any callable value (Go's Callable).
#[derive(Clone)]
pub struct Callable(pub Value);

/// A parameter that accepts any iterable value (Go's Iterable).
#[derive(Clone)]
pub struct Iterable(pub Value);

/// A parameter that accepts a bytes value.
#[derive(Clone)]
pub struct BytesArg(pub Rc<[u8]>);

/// Conversion of a value to a parameter type; implemented by the types
/// that may be used directly (and wrapped in Option) as parameters.
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self, Error>;
}

fn want(v: &Value, what: &str) -> Error {
    Error::new(format!("got {}, want {}", v.type_name(), what))
}

impl FromValue for Value {
    fn from_value(v: &Value) -> Result<Self, Error> {
        Ok(v.clone())
    }
}

impl FromValue for bool {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Bool(b) => Ok(*b),
            _ => Err(want(v, "bool")),
        }
    }
}

/// Starlark strings are byte strings; `Vec<u8>` is Go's `string` parameter.
impl FromValue for Vec<u8> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::String(s) => Ok(s.to_vec()),
            _ => Err(want(v, "string")),
        }
    }
}

/// A `String` parameter accepts a Starlark string; invalid UTF-8 is
/// replaced by U+FFFD. Prefer `Vec<u8>` where the exact bytes matter.
impl FromValue for String {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::String(s) => Ok(String::from_utf8_lossy(s).into_owned()),
            _ => Err(want(v, "string")),
        }
    }
}

impl FromValue for f64 {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Float(f) => Ok(*f),
            _ => Err(want(v, "float")),
        }
    }
}

impl FromValue for Int {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Int(i) => Ok(i.clone()),
            _ => Err(want(v, "int")),
        }
    }
}

impl FromValue for Rc<List> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::List(l) => Ok(l.clone()),
            _ => Err(want(v, "list")),
        }
    }
}

impl FromValue for Rc<Dict> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Dict(d) => Ok(d.clone()),
            _ => Err(want(v, "dict")),
        }
    }
}

impl FromValue for Rc<Set> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Set(s) => Ok(s.clone()),
            _ => Err(want(v, "set")),
        }
    }
}

impl FromValue for Rc<[Value]> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Tuple(t) => Ok(t.clone()),
            _ => Err(want(v, "tuple")),
        }
    }
}

impl FromValue for BytesArg {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Bytes(b) => Ok(BytesArg(b.clone())),
            _ => Err(want(v, "bytes")),
        }
    }
}

impl FromValue for Callable {
    fn from_value(v: &Value) -> Result<Self, Error> {
        if v.is_callable() {
            Ok(Callable(v.clone()))
        } else {
            Err(want(v, "callable"))
        }
    }
}

impl FromValue for Iterable {
    fn from_value(v: &Value) -> Result<Self, Error> {
        if v.is_iterable() {
            Ok(Iterable(v.clone()))
        } else {
            Err(want(v, "iterable"))
        }
    }
}

macro_rules! signed {
    ($($t:ty => $bits:expr),*) => {$(
        impl FromValue for $t {
            fn from_value(v: &Value) -> Result<Self, Error> {
                Ok(as_signed(v, $bits)? as $t)
            }
        }
    )*};
}

macro_rules! unsigned {
    ($($t:ty => $bits:expr),*) => {$(
        impl FromValue for $t {
            fn from_value(v: &Value) -> Result<Self, Error> {
                Ok(as_unsigned(v, $bits)? as $t)
            }
        }
    )*};
}

// Go's int and uint are 64 bits wide on the platforms of interest.
signed!(i8 => 8, i16 => 16, i32 => 32, i64 => 64, isize => 64);
unsigned!(u8 => 8, u16 => 16, u32 => 32, u64 => 64, usize => 64);

macro_rules! unpackers {
    ($($t:ty),*) => {$(
        impl Unpacker for $t {
            fn unpack(&mut self, v: &Value) -> Result<(), Error> {
                *self = <$t>::from_value(v)?;
                Ok(())
            }
        }
        impl Unpacker for Option<$t> {
            fn unpack(&mut self, v: &Value) -> Result<(), Error> {
                *self = Some(<$t>::from_value(v)?);
                Ok(())
            }
        }
    )*};
}

unpackers!(
    Value,
    bool,
    Vec<u8>,
    String,
    f64,
    Int,
    Rc<List>,
    Rc<Dict>,
    Rc<Set>,
    Rc<[Value]>,
    BytesArg,
    Callable,
    Iterable,
    i8,
    i16,
    i32,
    i64,
    isize,
    u8,
    u16,
    u32,
    u64,
    usize
);

fn param_name(name: &str) -> (&str, bool) {
    if let Some(n) = name.strip_suffix("??") {
        (n, true)
    } else if let Some(n) = name.strip_suffix('?') {
        (n, false)
    } else {
        (name, false)
    }
}

/// Unpacks the positional and keyword arguments into the supplied
/// parameter variables. `pairs` is a list of names and variables.
///
/// If the parameter name ends with "?", it is optional. If it ends with
/// "??", it is optional and treats the None value as if the argument was
/// absent. If a parameter is marked optional, then all following
/// parameters are implicitly optional whether or not they are marked.
pub fn unpack_args(
    fnname: &str,
    args: &[Value],
    kwargs: &[(Value, Value)],
    pairs: &mut [(&str, &mut dyn Unpacker)],
) -> Result<(), Error> {
    let nparams = pairs.len();
    let mut defined = vec![false; nparams];

    // positional arguments
    if args.len() > nparams {
        return Err(Error::new(format!(
            "{}: got {} arguments, want at most {}",
            fnname,
            args.len(),
            nparams
        )));
    }
    for (i, arg) in args.iter().enumerate() {
        defined[i] = true;
        let (name, skip_none) = param_name(pairs[i].0);
        if skip_none && matches!(arg, Value::None) {
            continue;
        }
        if let Err(err) = pairs[i].1.unpack(arg) {
            return Err(Error::new(format!(
                "{}: for parameter {}: {}",
                fnname, name, err
            )));
        }
    }

    // keyword arguments
    'kwloop: for (k, arg) in kwargs {
        let name = String::from_utf8_lossy(k.as_str().unwrap_or_default()).into_owned();
        // Go formats the name (a starlark.String) with %s, i.e. quoted.
        let qname = k.repr();
        for i in 0..nparams {
            let (pname, skip_none) = param_name(pairs[i].0);
            if pname == name {
                // found it
                if defined[i] {
                    return Err(Error::new(format!(
                        "{}: got multiple values for keyword argument {}",
                        fnname, qname
                    )));
                }
                defined[i] = true;

                if skip_none && matches!(arg, Value::None) {
                    continue 'kwloop;
                }

                if let Err(err) = pairs[i].1.unpack(arg) {
                    return Err(Error::new(format!(
                        "{}: for parameter {}: {}",
                        fnname, qname, err
                    )));
                }
                continue 'kwloop;
            }
        }
        let mut msg = format!("{}: unexpected keyword argument {}", fnname, qname);
        // The Go original collects every other entry of its flat
        // name/pointer list here, i.e. only the first half of the names.
        let names: Vec<String> = (0..nparams)
            .step_by(2)
            .map(|i| param_name(flat_name(pairs, i)).0.to_string())
            .collect();
        if let Some(n) = nearest(&name, &names) {
            msg = format!("{} (did you mean {}?)", msg, n);
        }
        return Err(Error::new(msg));
    }

    // Check that all non-optional parameters are defined.
    for i in 0..nparams {
        let name = pairs[i].0;
        if name.ends_with('?') {
            break; // optional
        }
        // (We needn't check the first len(args).)
        if i < args.len() {
            continue;
        }
        if !defined[i] {
            return Err(Error::new(format!(
                "{}: missing argument for {}",
                fnname, name
            )));
        }
    }

    Ok(())
}

/// Returns the i'th element of Go's flat (name, pointer, name, pointer...)
/// list, viewed as names: odd elements are pointers, whose "name" is the
/// following name (only even indices are ever requested).
fn flat_name<'a>(pairs: &'a [(&str, &mut dyn Unpacker)], i: usize) -> &'a str {
    pairs[i / 2].0
}

/// Unpacks the positional arguments into corresponding variables.
///
/// Reports an error if the number of arguments is less than min or
/// greater than vars.len(), if kwargs is nonempty, or if any conversion
/// fails.
pub fn unpack_positional_args(
    fnname: &str,
    args: &[Value],
    kwargs: &[(Value, Value)],
    min: usize,
    vars: &mut [&mut dyn Unpacker],
) -> Result<(), Error> {
    check_positional_args(fnname, args, kwargs, min, vars.len())?;
    for (i, arg) in args.iter().enumerate() {
        if let Err(err) = vars[i].unpack(arg) {
            return Err(Error::new(format!(
                "{}: for parameter {}: {}",
                fnname,
                i + 1,
                err
            )));
        }
    }
    Ok(())
}

fn check_positional_args(
    fnname: &str,
    args: &[Value],
    kwargs: &[(Value, Value)],
    min: usize,
    max: usize,
) -> Result<(), Error> {
    if !kwargs.is_empty() {
        return Err(Error::new(format!(
            "{}: unexpected keyword arguments",
            fnname
        )));
    }
    if args.len() < min {
        let atleast = if min < max { "at least " } else { "" };
        return Err(Error::new(format!(
            "{}: got {} arguments, want {}{}",
            fnname,
            args.len(),
            atleast,
            min
        )));
    }
    if args.len() > max {
        let atmost = if max > min { "at most " } else { "" };
        return Err(Error::new(format!(
            "{}: got {} arguments, want {}{}",
            fnname,
            args.len(),
            atmost,
            max
        )));
    }
    Ok(())
}

/// Unpacks a single value into a parameter variable (Go's UnpackArg).
pub fn unpack_arg(v: &Value, ptr: &mut dyn Unpacker) -> Result<(), Error> {
    ptr.unpack(v)
}
