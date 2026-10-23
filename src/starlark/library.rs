// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The library of built-ins (ported from library.go).
//!
//! Built-ins must explicitly check the "frozen" flag before updating
//! mutable types such as lists and dicts.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::eval::{
    call, find_bytes, indices, list_extend, out_of_range, rfind_bytes, StringDict, Thread,
};
use super::fmt::{go_quote, parse_float};
use super::hashtable::soft_hash_string;
use super::int::{as_int32, as_unsigned, number_to_int, parse_digits, Int};
use super::unpack::{unpack_args, unpack_positional_args, Callable, Iterable};
use super::value::{
    compare, iterate, len, write_value, Builtin, BytesIterable, Dict, Error, Set, StarlarkValue,
    StringCodepoints, StringElems, Value, ValueIterator,
};
use crate::syntax::utf8::{decode_last_rune, decode_rune, push_rune, valid, RUNE_ERROR};
use crate::syntax::{self, Token};
use crate::unicode;

type Args<'a> = &'a [Value];
type Kwargs<'a> = &'a [(Value, Value)];
type BuiltinResult = Result<Value, Error>;
type BuiltinPtr = fn(&mut Thread, &Builtin, Args, Kwargs) -> BuiltinResult;

const UNIVERSE_FUNCS: &[(&str, BuiltinPtr)] = &[
    ("abs", abs),
    ("any", any_),
    ("all", all),
    ("bool", bool_),
    ("bytes", bytes_),
    ("chr", chr),
    ("dict", dict),
    ("dir", dir),
    ("enumerate", enumerate),
    ("fail", fail),
    ("float", float),
    ("getattr", getattr),
    ("hasattr", hasattr),
    ("hash", hash),
    ("int", int_),
    ("len", len_),
    ("list", list),
    ("max", minmax),
    ("min", minmax),
    ("ord", ord),
    ("print", print),
    ("range", range_),
    ("repr", repr),
    ("reversed", reversed),
    ("set", set),
    ("sorted", sorted),
    ("str", str_),
    ("tuple", tuple),
    ("type", type_),
    ("zip", zip),
];

// methods of built-in types

const BYTES_METHODS: &[(&str, BuiltinPtr)] = &[("elems", bytes_elems)];

const DICT_METHODS: &[(&str, BuiltinPtr)] = &[
    ("clear", dict_clear),
    ("get", dict_get),
    ("items", dict_items),
    ("keys", dict_keys),
    ("pop", dict_pop),
    ("popitem", dict_popitem),
    ("setdefault", dict_setdefault),
    ("update", dict_update),
    ("values", dict_values),
];

const LIST_METHODS: &[(&str, BuiltinPtr)] = &[
    ("append", list_append),
    ("clear", list_clear),
    ("extend", list_extend_),
    ("index", list_index),
    ("insert", list_insert),
    ("pop", list_pop),
    ("remove", list_remove),
];

const STRING_METHODS: &[(&str, BuiltinPtr)] = &[
    ("capitalize", string_capitalize),
    ("codepoint_ords", string_iterable),
    ("codepoints", string_iterable), // sic
    ("count", string_count),
    ("elem_ords", string_iterable),
    ("elems", string_iterable),      // sic
    ("endswith", string_startswith), // sic
    ("find", string_find),
    ("format", string_format),
    ("index", string_index),
    ("isalnum", string_isalnum),
    ("isalpha", string_isalpha),
    ("isdigit", string_isdigit),
    ("islower", string_islower),
    ("isspace", string_isspace),
    ("istitle", string_istitle),
    ("isupper", string_isupper),
    ("join", string_join),
    ("lower", string_lower),
    ("lstrip", string_strip), // sic
    ("partition", string_partition),
    ("removeprefix", string_removefix),
    ("removesuffix", string_removefix),
    ("replace", string_replace),
    ("rfind", string_rfind),
    ("rindex", string_rindex),
    ("rpartition", string_partition), // sic
    ("rsplit", string_split),         // sic
    ("rstrip", string_strip),         // sic
    ("split", string_split),
    ("splitlines", string_splitlines),
    ("startswith", string_startswith),
    ("strip", string_strip),
    ("title", string_title),
    ("upper", string_upper),
];

const SET_METHODS: &[(&str, BuiltinPtr)] = &[
    ("add", set_add),
    ("clear", set_clear),
    ("difference", set_difference),
    ("discard", set_discard),
    ("intersection", set_intersection),
    ("issubset", set_issubset),
    ("issuperset", set_issuperset),
    ("pop", set_pop),
    ("remove", set_remove),
    ("symmetric_difference", set_symmetric_difference),
    ("union", set_union),
    ("update", set_update),
];

struct Tables {
    universe: StringDict,
    methods: HashMap<&'static str, HashMap<&'static str, Rc<Builtin>>>,
}

fn make_table(funcs: &[(&'static str, BuiltinPtr)]) -> HashMap<&'static str, Rc<Builtin>> {
    funcs
        .iter()
        .map(|&(name, f)| (name, Rc::new(Builtin::new(name, f))))
        .collect()
}

thread_local! {
    static TABLES: RefCell<Tables> = RefCell::new(init_tables());
}

fn init_tables() -> Tables {
    let mut universe = StringDict::new();
    universe.insert("None", Value::None);
    universe.insert("True", Value::Bool(true));
    universe.insert("False", Value::Bool(false));
    for &(name, f) in UNIVERSE_FUNCS {
        universe.insert(name, Builtin::value(name, f));
    }
    let mut methods = HashMap::new();
    methods.insert("bytes", make_table(BYTES_METHODS));
    methods.insert("dict", make_table(DICT_METHODS));
    methods.insert("list", make_table(LIST_METHODS));
    methods.insert("string", make_table(STRING_METHODS));
    methods.insert("set", make_table(SET_METHODS));
    Tables { universe, methods }
}

/// Returns the universal built-in of the given name (Go's Universe[name]).
pub fn universe_get(name: &str) -> Option<Value> {
    TABLES.with(|t| t.borrow().universe.get(name).cloned())
}

/// Reports whether the universe defines the given name.
pub fn universe_has(name: &str) -> bool {
    TABLES.with(|t| t.borrow().universe.has(name))
}

/// Adds or replaces a universal built-in. All values in the universe must
/// be immutable.
pub fn universe_insert(name: &str, v: Value) {
    TABLES.with(|t| t.borrow_mut().universe.insert(name, v));
}

/// Returns a copy of the universe.
pub fn universe() -> StringDict {
    TABLES.with(|t| t.borrow().universe.clone())
}

fn methods_key(recv: &Value) -> Option<&'static str> {
    match recv {
        Value::String(_) => Some("string"),
        Value::Bytes(_) => Some("bytes"),
        Value::List(_) => Some("list"),
        Value::Dict(_) => Some("dict"),
        Value::Set(_) => Some("set"),
        _ => None,
    }
}

/// Returns the method `name` of a built-in type bound to recv, if any.
pub(crate) fn builtin_attr(recv: &Value, name: &str) -> Option<Value> {
    let key = methods_key(recv)?;
    let b = TABLES.with(|t| {
        t.borrow()
            .methods
            .get(key)
            .and_then(|m| m.get(name).cloned())
    })?;
    Some(Value::Builtin(Rc::new(b.bind_receiver(recv.clone()))))
}

/// Returns the sorted method names of a built-in type.
pub(crate) fn builtin_attr_names(recv: &Value) -> Vec<String> {
    let Some(key) = methods_key(recv) else {
        return Vec::new();
    };
    let mut names: Vec<String> = TABLES
        .with(|t| {
            t.borrow()
                .methods
                .get(key)
                .map(|m| m.keys().map(|k| k.to_string()).collect())
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Returns an error of the form "name: msg" where name is b.name().
pub fn name_err(b: &Builtin, msg: impl std::fmt::Display) -> Error {
    Error::new(format!("{}: {}", b.name(), msg))
}

fn recv(b: &Builtin) -> &Value {
    b.receiver().expect("method has a receiver")
}

fn recv_str(b: &Builtin) -> Rc<[u8]> {
    match recv(b) {
        Value::String(s) | Value::Bytes(s) => s.clone(),
        _ => Rc::from(Vec::new()),
    }
}

fn recv_list(b: &Builtin) -> &Rc<super::value::List> {
    match recv(b) {
        Value::List(l) => l,
        _ => unreachable!("list method"),
    }
}

fn recv_dict(b: &Builtin) -> &Rc<Dict> {
    match recv(b) {
        Value::Dict(d) => d,
        _ => unreachable!("dict method"),
    }
}

fn recv_set(b: &Builtin) -> &Rc<Set> {
    match recv(b) {
        Value::Set(s) => s,
        _ => unreachable!("set method"),
    }
}

fn iter_of(v: &Value) -> Box<dyn ValueIterator> {
    iterate(v).expect("iterable")
}

// ---- built-in functions ----

fn abs(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::None;
    unpack_positional_args("abs", args, kwargs, 1, &mut [&mut x])?;
    match &x {
        Value::Float(f) => Ok(Value::Float(f.abs())),
        Value::Int(i) => {
            if i.sign() >= 0 {
                Ok(x.clone())
            } else {
                Ok(Value::Int(i.neg()))
            }
        }
        _ => Err(Error::new(format!(
            "got {}, want int or float",
            x.type_name()
        ))),
    }
}

fn all(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable = Iterable(Value::None);
    unpack_positional_args("all", args, kwargs, 1, &mut [&mut iterable])?;
    let mut iter = iter_of(&iterable.0);
    while let Some(x) = iter.next() {
        if !x.truth() {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn any_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable = Iterable(Value::None);
    unpack_positional_args("any", args, kwargs, 1, &mut [&mut iterable])?;
    let mut iter = iter_of(&iterable.0);
    while let Some(x) = iter.next() {
        if x.truth() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn bool_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::Bool(false);
    unpack_positional_args("bool", args, kwargs, 0, &mut [&mut x])?;
    Ok(Value::Bool(x.truth()))
}

fn bytes_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("bytes does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "bytes: got {} arguments, want exactly 1",
            args.len()
        )));
    }
    let x = &args[0];
    match x {
        Value::Bytes(_) => Ok(x.clone()),
        // Invalid encodings are replaced by that of U+FFFD.
        Value::String(s) => Ok(Value::bytes(utf8_transcode(s))),
        _ => {
            // iterable of numeric byte values
            let Some(mut iter) = iterate(x) else {
                // Unlike string(foo), which stringifies it, bytes(foo) is an error.
                return Err(Error::new(format!(
                    "bytes: got {}, want string, bytes, or iterable of ints",
                    x.type_name()
                )));
            };
            let mut buf = Vec::with_capacity(len(x).unwrap_or(0));
            let mut i = 0;
            while let Some(elem) = iter.next() {
                let b = as_unsigned(&elem, 8)
                    .map_err(|e| Error::new(format!("bytes: at index {}, {}", i, e)))?;
                buf.push(b as u8);
                i += 1;
            }
            Ok(Value::bytes(buf))
        }
    }
}

fn chr(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("chr does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "chr: got {} arguments, want 1",
            args.len()
        )));
    }
    let i = as_int32(&args[0]).map_err(|e| Error::new(format!("chr: {}", e)))?;
    if i < 0 {
        return Err(Error::new(format!(
            "chr: Unicode code point {} out of range (<0)",
            i
        )));
    }
    if i > 0x10FFFF {
        return Err(Error::new(format!(
            "chr: Unicode code point U+{:X} out of range (>0x10FFFF)",
            i
        )));
    }
    let mut buf = Vec::new();
    push_rune(&mut buf, char::from_u32(i as u32).unwrap_or(RUNE_ERROR));
    Ok(Value::string(buf))
}

fn dict(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if args.len() > 1 {
        return Err(Error::new(format!(
            "dict: got {} arguments, want at most 1",
            args.len()
        )));
    }
    let dict = Rc::new(Dict::new());
    update_dict(&dict, args, kwargs).map_err(|e| Error::new(format!("dict: {}", e)))?;
    Ok(Value::Dict(dict))
}

fn dir(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("dir does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "dir: got {} arguments, want 1",
            args.len()
        )));
    }

    let mut names = Vec::new();
    if args[0].has_attrs() {
        names = super::eval::attr_names(&args[0]);
    }
    names.sort();
    Ok(Value::list(names.into_iter().map(Value::string).collect()))
}

fn enumerate(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable = Iterable(Value::None);
    let mut start: i64 = 0;
    unpack_positional_args(
        "enumerate",
        args,
        kwargs,
        1,
        &mut [&mut iterable, &mut start],
    )?;

    let mut iter = iter_of(&iterable.0);
    let mut pairs = Vec::with_capacity(len(&iterable.0).unwrap_or(0));
    let mut i: i64 = 0;
    while let Some(x) = iter.next() {
        pairs.push(Value::tuple(vec![Value::int(start.wrapping_add(i)), x]));
        i += 1;
    }
    Ok(Value::list(pairs))
}

fn fail(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut sep: Vec<u8> = b" ".to_vec();
    unpack_args("fail", &[], kwargs, &mut [("sep?", &mut sep)])?;
    let mut buf: Vec<u8> = b"fail: ".to_vec();
    for (i, v) in args.iter().enumerate() {
        if i > 0 {
            buf.extend_from_slice(&sep);
        }
        match v {
            Value::String(s) => buf.extend_from_slice(s),
            _ => buf.extend_from_slice(v.repr().as_bytes()),
        }
    }
    Err(Error::new(String::from_utf8_lossy(&buf).into_owned()))
}

fn float(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("float does not accept keyword arguments"));
    }
    if args.is_empty() {
        return Ok(Value::Float(0.0));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "float got {} arguments, wants 1",
            args.len()
        )));
    }
    match &args[0] {
        Value::Bool(b) => Ok(Value::Float(if *b { 1.0 } else { 0.0 })),
        Value::Int(i) => Ok(Value::Float(i.finite_float()?)),
        Value::Float(_) => Ok(args[0].clone()),
        Value::String(s) => {
            if s.is_empty() {
                return Err(Error::new("float: empty string"));
            }
            // +/- NaN or Inf or Infinity (case insensitive)?
            let eq = |w: &str| s.eq_ignore_ascii_case(w.as_bytes());
            match s[s.len() - 1] {
                b'y' | b'Y' => {
                    if eq("infinity") || eq("+infinity") {
                        return Ok(Value::Float(f64::INFINITY));
                    } else if eq("-infinity") {
                        return Ok(Value::Float(f64::NEG_INFINITY));
                    }
                }
                b'f' | b'F' => {
                    if eq("inf") || eq("+inf") {
                        return Ok(Value::Float(f64::INFINITY));
                    } else if eq("-inf") {
                        return Ok(Value::Float(f64::NEG_INFINITY));
                    }
                }
                b'n' | b'N' if eq("nan") || eq("+nan") || eq("-nan") => {
                    return Ok(Value::Float(f64::NAN));
                }
                _ => {}
            }
            let f = parse_float(s);
            if f.is_some_and(|f| f.is_infinite()) {
                return Err(Error::new("floating-point number too large"));
            }
            match f {
                Some(f) => Ok(Value::Float(f)),
                None => Err(Error::new(format!(
                    "invalid float literal: {}",
                    String::from_utf8_lossy(s)
                ))),
            }
        }
        x => Err(Error::new(format!(
            "float got {}, want number or string",
            x.type_name()
        ))),
    }
}

fn getattr(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut object = Value::None;
    let mut name: Vec<u8> = Vec::new();
    let mut dflt: Option<Value> = None;
    unpack_positional_args(
        "getattr",
        args,
        kwargs,
        2,
        &mut [&mut object, &mut name, &mut dflt],
    )?;
    let name = String::from_utf8_lossy(&name).into_owned();
    if object.has_attrs() {
        let res = match &object {
            Value::Other(o) => o.attr(&object, &name),
            _ => Ok(builtin_attr(&object, &name)),
        };
        match res {
            Err(err) => {
                // An error could mean the field doesn't exist,
                // or it exists but could not be computed.
                if let Some(d) = dflt {
                    return Ok(d);
                }
                return Err(name_err(b, err));
            }
            Ok(Some(v)) => return Ok(v),
            Ok(None) => {} // no such field
        }
    }
    if let Some(d) = dflt {
        return Ok(d);
    }
    Err(Error::new(format!(
        "getattr: {} has no .{} field or method",
        object.type_name(),
        name
    )))
}

fn hasattr(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut object = Value::None;
    let mut name: Vec<u8> = Vec::new();
    unpack_positional_args("hasattr", args, kwargs, 2, &mut [&mut object, &mut name])?;
    let name = String::from_utf8_lossy(&name).into_owned();
    if object.has_attrs() {
        let res = match &object {
            Value::Other(o) => o.attr(&object, &name),
            _ => Ok(builtin_attr(&object, &name)),
        };
        match res {
            Ok(v) => return Ok(Value::Bool(v.is_some())),
            Err(_) => {
                // An error does not conclusively indicate presence or
                // absence of a field: it could occur while computing
                // the value of a present attribute, or it could be a
                // "no such attribute" error with details.
                if super::eval::attr_names(&object).contains(&name) {
                    return Ok(Value::Bool(true));
                }
            }
        }
    }
    Ok(Value::Bool(false))
}

fn hash(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::None;
    unpack_positional_args("hash", args, kwargs, 1, &mut [&mut x])?;

    let h: i64 = match &x {
        // The Starlark spec requires that the hash function be
        // deterministic across all runs, motivated by the need
        // for reproducibility of builds.
        Value::String(s) => java_string_hash(s) as i64,
        Value::Bytes(s) => soft_hash_string(s) as i64, // FNV32
        _ => {
            return Err(Error::new(format!(
                "hash: got {}, want string or bytes",
                x.type_name()
            )))
        }
    };
    Ok(Value::int(h))
}

/// Returns the same hash as would be produced by java.lang.String.hashCode
/// (in the Go original's interpretation: each code point is added whole,
/// as utf16.IsSurrogate never holds for a decoded rune).
fn java_string_hash(s: &[u8]) -> i32 {
    let mut h: i32 = 0;
    let mut i = 0;
    while i < s.len() {
        let (r, sz) = decode_rune(&s[i..]);
        h = h.wrapping_mul(31).wrapping_add(r as i32); // r may be U+FFFD
        i += sz;
    }
    h
}

fn int_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::Int(Int::zero());
    let mut base: Option<Value> = None;
    unpack_args(
        "int",
        args,
        kwargs,
        &mut [("x", &mut x), ("base?", &mut base)],
    )?;

    if let Value::String(s) = &x {
        let mut b = 10;
        if let Some(base) = &base {
            b = as_int32(base).map_err(|_| {
                Error::new(format!("int: for base, got {}, want int", base.type_name()))
            })?;
            if b != 0 && !(2..=36).contains(&b) {
                return Err(Error::new("int: base must be an integer >= 2 && <= 36"));
            }
        }
        return match parse_int(s, b as u32) {
            Some(res) => Ok(Value::Int(res)),
            None => Err(Error::new(format!(
                "int: invalid literal with base {}: {}",
                b,
                String::from_utf8_lossy(s)
            ))),
        };
    }

    if base.is_some() {
        return Err(Error::new(
            "int: can't convert non-string with explicit base",
        ));
    }

    if let Value::Bool(b) = &x {
        return Ok(Value::int(*b as i64));
    }

    let i = number_to_int(&x).map_err(|e| Error::new(format!("int: {}", e)))?;
    Ok(Value::Int(i))
}

/// Defines the behavior of int(string, base=int). It returns None on error.
fn parse_int(s: &[u8], mut base: u32) -> Option<Int> {
    let mut s = s;
    // remove sign
    let mut neg = false;
    if !s.is_empty() {
        if s[0] == b'+' {
            s = &s[1..];
        } else if s[0] == b'-' {
            neg = true;
            s = &s[1..];
        }
    }

    // remove optional base prefix
    let mut baseprefix = 0;
    if s.len() > 1 && s[0] == b'0' {
        if s.len() > 2 {
            baseprefix = match s[1] {
                b'o' | b'O' => 8,
                b'x' | b'X' => 16,
                b'b' | b'B' => 2,
                _ => 0,
            };
        }
        if baseprefix != 0 {
            // Remove the base prefix if it matches
            // the explicit base, or if base=0.
            if base == 0 || baseprefix == base {
                base = baseprefix;
                s = &s[2..];
            }
        } else {
            // For automatic base detection,
            // a string starting with zero
            // must be all zeros.
            // Thus we reject int("0755", 0).
            if base == 0 {
                if s[1..].iter().any(|&c| c != b'0') {
                    return None;
                }
                return Some(Int::zero());
            }
        }
    }
    if base == 0 {
        base = 10;
    }

    // we explicitly handled sign above.
    // if a sign remains, it is invalid.
    if !s.is_empty() && (s[0] == b'-' || s[0] == b'+') {
        return None;
    }

    // s has no sign or base prefix.
    let i = parse_digits(s, base)?;
    let res = Int::from_bigint(i);
    Some(if neg { res.neg() } else { res })
}

fn len_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::None;
    unpack_positional_args("len", args, kwargs, 1, &mut [&mut x])?;
    match len(&x) {
        Some(n) => Ok(Value::int(n as i64)),
        None => Err(Error::new(format!(
            "len: value of type {} has no len",
            x.type_name()
        ))),
    }
}

fn list(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable: Option<Iterable> = None;
    unpack_positional_args("list", args, kwargs, 0, &mut [&mut iterable])?;
    let mut elems = Vec::new();
    if let Some(iterable) = iterable {
        let mut iter = iter_of(&iterable.0);
        elems.reserve(len(&iterable.0).unwrap_or(0));
        while let Some(x) = iter.next() {
            elems.push(x);
        }
    }
    Ok(Value::list(elems))
}

fn minmax(thread: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if args.is_empty() {
        return Err(Error::new(format!(
            "{} requires at least one positional argument",
            b.name()
        )));
    }
    let mut key_func: Option<Callable> = None;
    unpack_args(b.name(), &[], kwargs, &mut [("key?", &mut key_func)])?;
    let op = if b.name() == "max" {
        Token::Gt
    } else {
        Token::Lt
    };
    let iterable = if args.len() == 1 {
        args[0].clone()
    } else {
        Value::tuple(args.to_vec())
    };
    let Some(mut iter) = iterate(&iterable) else {
        return Err(Error::new(format!(
            "{}: {} value is not iterable",
            b.name(),
            iterable.type_name()
        )));
    };
    let Some(mut extremum) = iter.next() else {
        return Err(name_err(b, "argument is an empty sequence"));
    };

    let mut extreme_key = match &key_func {
        None => extremum.clone(),
        // to preserve backtrace, don't modify error
        Some(f) => call(thread, &f.0, &[extremum.clone()], &[])?,
    };

    while let Some(x) = iter.next() {
        let key = match &key_func {
            None => x.clone(),
            Some(f) => call(thread, &f.0, std::slice::from_ref(&x), &[])?,
        };
        match compare(op, &key, &extreme_key) {
            Err(err) => return Err(name_err(b, err)),
            Ok(true) => {
                extremum = x;
                extreme_key = key;
            }
            Ok(false) => {}
        }
    }
    Ok(extremum)
}

fn ord(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("ord does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "ord: got {} arguments, want 1",
            args.len()
        )));
    }
    match &args[0] {
        Value::String(s) => {
            // ord(string) returns int value of sole rune.
            let (r, sz) = decode_rune(s);
            if sz == 0 || sz != s.len() {
                let n = syntax::rune_count(s);
                return Err(Error::new(format!(
                    "ord: string encodes {} Unicode code points, want 1",
                    n
                )));
            }
            Ok(Value::int(r as i64))
        }
        Value::Bytes(s) => {
            // ord(bytes) returns int value of sole byte.
            if s.len() != 1 {
                return Err(Error::new(format!(
                    "ord: bytes has length {}, want 1",
                    s.len()
                )));
            }
            Ok(Value::int(s[0] as i64))
        }
        x => Err(Error::new(format!(
            "ord: got {}, want string or bytes",
            x.type_name()
        ))),
    }
}

fn print(thread: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut sep: Vec<u8> = b" ".to_vec();
    unpack_args("print", &[], kwargs, &mut [("sep?", &mut sep)])?;
    let mut buf: Vec<u8> = Vec::new();
    for (i, v) in args.iter().enumerate() {
        if i > 0 {
            buf.extend_from_slice(&sep);
        }
        match v {
            Value::String(s) | Value::Bytes(s) => buf.extend_from_slice(s),
            _ => buf.extend_from_slice(v.repr().as_bytes()),
        }
    }
    thread.print_message(&buf);
    Ok(Value::None)
}

fn range_(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut start: i64 = 0;
    let mut stop: i64 = 0;
    let mut step: i64 = 1;
    unpack_positional_args(
        "range",
        args,
        kwargs,
        1,
        &mut [&mut start, &mut stop, &mut step],
    )?;

    if args.len() == 1 {
        // range(stop)
        stop = start;
        start = 0;
    }
    if step == 0 {
        // we were given range(start, stop, 0)
        return Err(name_err(b, "step argument must not be zero"));
    }

    Ok(Value::other(RangeValue {
        start,
        stop,
        step,
        len: range_len(start, stop, step),
    }))
}

/// A RangeValue is a comparable, immutable, indexable sequence of integers
/// defined by the three parameters to a range(...) call.
/// Invariant: step != 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RangeValue {
    start: i64,
    stop: i64,
    step: i64,
    len: i64,
}

/// Calculates the length of a range with the provided start, stop, and
/// step. The caller must ensure that step is non-zero.
fn range_len(start: i64, stop: i64, step: i64) -> i64 {
    if step > 0 {
        if stop > start {
            return (stop.wrapping_sub(1).wrapping_sub(start)) / step + 1;
        }
    } else if start > stop {
        return (start.wrapping_sub(1).wrapping_sub(stop)) / step.wrapping_neg() + 1;
    }
    0
}

impl RangeValue {
    fn at(&self, i: i64) -> Value {
        Value::int(self.start.wrapping_add(i.wrapping_mul(self.step)))
    }

    fn contains(&self, x: &Int) -> bool {
        let Ok(x32) = as_int32(&Value::Int(x.clone())) else {
            return false; // out of range
        };
        let delta = (x32 as i64).wrapping_sub(self.start);
        let (quo, rem) = (delta.wrapping_div(self.step), delta.wrapping_rem(self.step));
        rem == 0 && 0 <= quo && quo < self.len
    }
}

fn range_equal(x: &RangeValue, y: &RangeValue) -> bool {
    // Two ranges compare equal if they denote the same sequence.
    if x.len != y.len {
        return false; // sequences differ in length
    }
    if x.len == 0 {
        return true; // both sequences are empty
    }
    if x.start != y.start {
        return false; // first element differs
    }
    x.len == 1 || x.step == y.step
}

struct RangeIterator {
    r: RangeValue,
    i: i64,
}

impl ValueIterator for RangeIterator {
    fn next(&mut self) -> Option<Value> {
        if self.i < self.r.len {
            let v = self.r.at(self.i);
            self.i += 1;
            return Some(v);
        }
        None
    }
}

impl StarlarkValue for RangeValue {
    fn type_name(&self) -> &str {
        "range"
    }
    fn repr(&self, out: &mut String) {
        if self.step != 1 {
            out.push_str(&format!(
                "range({}, {}, {})",
                self.start, self.stop, self.step
            ));
        } else if self.start != 0 {
            out.push_str(&format!("range({}, {})", self.start, self.stop));
        } else {
            out.push_str(&format!("range({})", self.stop));
        }
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new("unhashable: range"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn truth(&self) -> bool {
        self.len > 0
    }
    fn compare_same_type(&self, op: Token, y: &Value, _depth: i32) -> Option<Result<bool, Error>> {
        let y = y.downcast_ref::<RangeValue>()?;
        Some(match op {
            Token::Eql => Ok(range_equal(self, y)),
            Token::Neq => Ok(!range_equal(self, y)),
            _ => Err(Error::new(format!(
                "{} {} {} not implemented",
                "range", op, "range"
            ))),
        })
    }
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        Some(Box::new(RangeIterator { r: *self, i: 0 }))
    }
    fn len(&self) -> Option<usize> {
        Some(self.len.max(0) as usize)
    }
    fn is_indexable(&self) -> bool {
        true
    }
    fn index(&self, i: usize) -> Value {
        self.at(i as i64)
    }
    fn is_sliceable(&self) -> bool {
        true
    }
    fn slice(&self, start: i64, end: i64, step: i64) -> Value {
        let new_start = self.start.wrapping_add(self.step.wrapping_mul(start));
        let new_stop = self.start.wrapping_add(self.step.wrapping_mul(end));
        let new_step = self.step.wrapping_mul(step);
        Value::other(RangeValue {
            start: new_start,
            stop: new_stop,
            step: new_step,
            len: range_len(new_start, new_stop, new_step),
        })
    }
    fn has(&self, y: &Value) -> Option<Result<bool, Error>> {
        Some(match number_to_int(y) {
            Ok(i) => Ok(self.contains(&i)),
            Err(_) => Err(Error::new(format!(
                "'in <range>' requires integer as left operand, not {}",
                y.type_name()
            ))),
        })
    }
}

fn repr(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::None;
    unpack_positional_args("repr", args, kwargs, 1, &mut [&mut x])?;
    Ok(Value::string(x.repr()))
}

fn reversed(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable = Iterable(Value::None);
    unpack_positional_args("reversed", args, kwargs, 1, &mut [&mut iterable])?;
    let mut iter = iter_of(&iterable.0);
    let mut elems = Vec::with_capacity(len(&args[0]).unwrap_or(0));
    while let Some(x) = iter.next() {
        elems.push(x);
    }
    elems.reverse();
    Ok(Value::list(elems))
}

fn set(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable: Option<Iterable> = None;
    unpack_positional_args("set", args, kwargs, 0, &mut [&mut iterable])?;
    let set = Rc::new(Set::new());
    if let Some(iterable) = iterable {
        let mut iter = iter_of(&iterable.0);
        while let Some(x) = iter.next() {
            set.insert(x).map_err(|e| name_err(b, e))?;
        }
    }
    Ok(Value::Set(set))
}

fn sorted(thread: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    // Oddly, Python's sorted permits all arguments to be positional, thus so do we.
    let mut iterable = Iterable(Value::None);
    let mut key: Option<Callable> = None;
    let mut reverse = false;
    unpack_args(
        "sorted",
        args,
        kwargs,
        &mut [
            ("iterable", &mut iterable),
            ("key?", &mut key),
            ("reverse?", &mut reverse),
        ],
    )?;

    let mut values = Vec::with_capacity(len(&iterable.0).unwrap_or(0));
    {
        let mut iter = iter_of(&iterable.0);
        while let Some(x) = iter.next() {
            values.push(x);
        }
    }

    // Derive keys from values by applying key function.
    let mut keys: Option<Vec<Value>> = None;
    if let Some(key) = &key {
        let mut ks = Vec::with_capacity(values.len());
        for v in &values {
            // to preserve backtrace, don't modify error
            ks.push(call(thread, &key.0, std::slice::from_ref(v), &[])?);
        }
        keys = Some(ks);
    }

    let mut err: Option<Error> = None;
    {
        let mut less = |vals: &[Value], keys: &Option<Vec<Value>>, i: usize, j: usize| -> bool {
            let ks = keys.as_deref().unwrap_or(vals);
            match compare(Token::Lt, &ks[i], &ks[j]) {
                Ok(ok) => ok,
                Err(e) => {
                    err = Some(e);
                    false
                }
            }
        };
        let mut data = SortData {
            values: &mut values,
            keys: &mut keys,
            reverse,
            less: &mut less,
        };
        stable(&mut data);
    }
    match err {
        Some(e) => Err(e),
        None => Ok(Value::list(values)),
    }
}

type LessFn<'a> = dyn FnMut(&[Value], &Option<Vec<Value>>, usize, usize) -> bool + 'a;

struct SortData<'a, 'b> {
    values: &'a mut Vec<Value>,
    keys: &'a mut Option<Vec<Value>>,
    reverse: bool,
    less: &'a mut LessFn<'b>,
}

impl SortData<'_, '_> {
    fn len(&self) -> usize {
        self.values.len()
    }
    fn less(&mut self, i: usize, j: usize) -> bool {
        let (i, j) = if self.reverse { (j, i) } else { (i, j) };
        (self.less)(self.values, self.keys, i, j)
    }
    fn swap(&mut self, i: usize, j: usize) {
        if let Some(k) = self.keys.as_mut() {
            k.swap(i, j);
        }
        self.values.swap(i, j);
    }
}

// The following is a transliteration of Go's sort.Stable (insertion sort
// of blocks followed by SymMerge), so that the sequence of comparisons,
// and hence the reported error and the order of incomparable elements,
// is the same as in the Go original.

fn stable(data: &mut SortData) {
    let n = data.len();
    let mut block_size = 20; // must be > 0
    let (mut a, mut b) = (0, block_size);
    while b <= n {
        insertion_sort(data, a, b);
        a = b;
        b += block_size;
    }
    insertion_sort(data, a, n);

    while block_size < n {
        a = 0;
        b = 2 * block_size;
        while b <= n {
            sym_merge(data, a, a + block_size, b);
            a = b;
            b += 2 * block_size;
        }
        let m = a + block_size;
        if m < n {
            sym_merge(data, a, m, n);
        }
        block_size *= 2;
    }
}

fn insertion_sort(data: &mut SortData, a: usize, b: usize) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && data.less(j, j - 1) {
            data.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn sym_merge(data: &mut SortData, a: usize, m: usize, b: usize) {
    // Avoid unnecessary recursions of symMerge
    // by direct insertion of data[a] into data[m:b]
    // if data[a:m] only contains one element.
    if m - a == 1 {
        // Use binary search to find the lowest index i
        // such that data[i] >= data[a] for m <= i < b.
        // Exit the search loop with i == b in case no such index exists.
        let (mut i, mut j) = (m, b);
        while i < j {
            let h = (i + j) >> 1;
            if data.less(h, a) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        // Swap values until data[a] reaches the position before i.
        let mut k = a;
        while k + 1 < i {
            data.swap(k, k + 1);
            k += 1;
        }
        return;
    }

    // Avoid unnecessary recursions of symMerge
    // by direct insertion of data[m] into data[a:m]
    // if data[m:b] only contains one element.
    if b - m == 1 {
        // Use binary search to find the lowest index i
        // such that data[i] > data[m] for a <= i < m.
        // Exit the search loop with i == m in case no such index exists.
        let (mut i, mut j) = (a, m);
        while i < j {
            let h = (i + j) >> 1;
            if !data.less(m, h) {
                i = h + 1;
            } else {
                j = h;
            }
        }
        // Swap values until data[m] reaches the position i.
        let mut k = m;
        while k > i {
            data.swap(k, k - 1);
            k -= 1;
        }
        return;
    }

    let mid = (a + b) >> 1;
    let n = mid + m;
    let (mut start, mut r) = if m > mid { (n - b, mid) } else { (a, m) };
    let p = n - 1;

    while start < r {
        let c = (start + r) >> 1;
        if !data.less(p - c, c) {
            start = c + 1;
        } else {
            r = c;
        }
    }

    let end = n - start;
    if start < m && m < end {
        rotate(data, start, m, end);
    }
    if a < start && start < mid {
        sym_merge(data, a, start, mid);
    }
    if mid < end && end < b {
        sym_merge(data, mid, end, b);
    }
}

fn rotate(data: &mut SortData, a: usize, m: usize, b: usize) {
    let mut i = m - a;
    let mut j = b - m;

    while i != j {
        if i > j {
            swap_range(data, m - i, m, j);
            i -= j;
        } else {
            swap_range(data, m - i, m + j - i, i);
            j -= i;
        }
    }
    // i == j
    swap_range(data, m - i, m, i);
}

fn swap_range(data: &mut SortData, a: usize, b: usize, n: usize) {
    for i in 0..n {
        data.swap(a + i, b + i);
    }
}

fn str_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("str does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "str: got {} arguments, want exactly 1",
            args.len()
        )));
    }
    match &args[0] {
        Value::String(_) => Ok(args[0].clone()),
        // Invalid encodings are replaced by that of U+FFFD.
        Value::Bytes(b) => Ok(Value::string(utf8_transcode(b))),
        x => Ok(Value::string(x.repr())),
    }
}

/// Returns the UTF-8-to-UTF-8 transcoding of s. The effect is that each
/// code unit that is part of an invalid sequence is replaced by U+FFFD.
pub fn utf8_transcode(s: &[u8]) -> Vec<u8> {
    if valid(s) {
        return s.to_vec();
    }
    let mut out = Vec::with_capacity(s.len());
    for_each_rune(s, |r, _| push_rune(&mut out, r));
    out
}

/// Calls f for each rune of s, decoding as Go's `for range` does
/// (invalid bytes decode to U+FFFD with width 1), with its byte offset.
pub fn for_each_rune(s: &[u8], mut f: impl FnMut(char, usize)) {
    let mut i = 0;
    while i < s.len() {
        let (r, sz) = decode_rune(&s[i..]);
        f(r, i);
        i += sz;
    }
}

fn tuple(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut iterable: Option<Iterable> = None;
    unpack_positional_args("tuple", args, kwargs, 0, &mut [&mut iterable])?;
    let Some(iterable) = iterable else {
        return Ok(Value::tuple(Vec::new()));
    };
    let mut iter = iter_of(&iterable.0);
    let mut elems = Vec::with_capacity(len(&iterable.0).unwrap_or(0));
    while let Some(x) = iter.next() {
        elems.push(x);
    }
    Ok(Value::tuple(elems))
}

fn type_(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("type does not accept keyword arguments"));
    }
    if args.len() != 1 {
        return Err(Error::new(format!(
            "type: got {} arguments, want exactly 1",
            args.len()
        )));
    }
    Ok(Value::string(args[0].type_name()))
}

fn zip(_: &mut Thread, _: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if !kwargs.is_empty() {
        return Err(Error::new("zip does not accept keyword arguments"));
    }
    let cols = args.len();
    let mut rows: i64 = 0;
    let mut iters = Vec::with_capacity(cols);
    for (i, seq) in args.iter().enumerate() {
        let Some(it) = iterate(seq) else {
            return Err(Error::new(format!(
                "zip: argument #{} is not iterable: {}",
                i + 1,
                seq.type_name()
            )));
        };
        iters.push(it);
        let n = len(seq).map_or(-1, |n| n as i64);
        if i == 0 || n < rows {
            rows = n; // possibly -1
        }
    }
    let mut result = Vec::new();
    if rows >= 0 {
        // length known
        for _ in 0..rows {
            let tuple: Vec<Value> = iters
                .iter_mut()
                .map(|it| it.next().unwrap_or(Value::None))
                .collect();
            result.push(Value::tuple(tuple));
        }
    } else {
        // length not known
        'outer: loop {
            let mut tuple = Vec::with_capacity(cols);
            for it in iters.iter_mut() {
                match it.next() {
                    Some(v) => tuple.push(v),
                    None => break 'outer,
                }
            }
            result.push(Value::tuple(tuple));
        }
    }
    Ok(Value::list(result))
}

// ---- methods of built-in types ---

fn dict_get(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut key = Value::None;
    let mut dflt: Option<Value> = None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut key, &mut dflt])?;
    match recv_dict(b).get(&key) {
        Err(err) => Err(name_err(b, err)),
        Ok(Some(v)) => Ok(v),
        Ok(None) => Ok(dflt.unwrap_or(Value::None)),
    }
}

fn dict_clear(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    recv_dict(b).clear()?;
    Ok(Value::None)
}

fn dict_items(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let items = recv_dict(b).items();
    Ok(Value::list(
        items
            .into_iter()
            .map(|(k, v)| Value::tuple(vec![k, v]))
            .collect(),
    ))
}

fn dict_keys(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    Ok(Value::list(recv_dict(b).keys()))
}

fn dict_pop(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut k = Value::None;
    let mut d: Option<Value> = None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut k, &mut d])?;
    match recv_dict(b).delete(&k) {
        Err(err) => Err(name_err(b, err)), // dict is frozen or key is unhashable
        Ok(Some(v)) => Ok(v),
        Ok(None) => match d {
            Some(d) => Ok(d),
            None => Err(name_err(b, "missing key")),
        },
    }
}

fn dict_popitem(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_dict(b);
    let Some(k) = recv.ht.first() else {
        return Err(name_err(b, "empty dict"));
    };
    let v = recv.delete(&k).map_err(|e| name_err(b, e))?; // dict is frozen
    Ok(Value::tuple(vec![k, v.unwrap_or(Value::None)]))
}

fn dict_setdefault(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut key = Value::None;
    let mut dflt = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut key, &mut dflt])?;
    let dict = recv_dict(b);
    match dict.get(&key) {
        Err(err) => Err(name_err(b, err)),
        Ok(Some(v)) => Ok(v),
        Ok(None) => {
            dict.set_key(key, dflt.clone())
                .map_err(|e| name_err(b, e))?;
            Ok(dflt)
        }
    }
}

fn dict_update(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    if args.len() > 1 {
        return Err(Error::new(format!(
            "update: got {} arguments, want at most 1",
            args.len()
        )));
    }
    update_dict(recv_dict(b), args, kwargs).map_err(|e| Error::new(format!("update: {}", e)))?;
    Ok(Value::None)
}

fn dict_values(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let items = recv_dict(b).items();
    Ok(Value::list(items.into_iter().map(|(_, v)| v).collect()))
}

fn list_append(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut object = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut object])?;
    let recv = recv_list(b);
    recv.check_mutable("append to")
        .map_err(|e| name_err(b, e))?;
    recv.push_unchecked(object);
    Ok(Value::None)
}

fn list_clear(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    recv_list(b).clear().map_err(|e| name_err(b, e))?;
    Ok(Value::None)
}

fn list_extend_(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_list(b);
    let mut iterable = Iterable(Value::None);
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut iterable])?;
    recv.check_mutable("extend").map_err(|e| name_err(b, e))?;
    list_extend(recv, &iterable.0);
    Ok(Value::None)
}

fn list_index(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut value = Value::None;
    let mut start_: Option<Value> = None;
    let mut end_: Option<Value> = None;
    unpack_positional_args(
        b.name(),
        args,
        kwargs,
        1,
        &mut [&mut value, &mut start_, &mut end_],
    )?;

    let recv = recv_list(b);
    let (start, end) =
        indices(start_.as_ref(), end_.as_ref(), recv.len() as i64).map_err(|e| name_err(b, e))?;

    for i in start..end {
        let elem = recv.index(i as usize);
        if super::value::equal(&elem, &value).map_err(|e| name_err(b, e))? {
            return Ok(Value::int(i));
        }
    }
    Err(name_err(b, "value not in list"))
}

fn list_insert(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_list(b);
    let mut index: i64 = 0;
    let mut object = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 2, &mut [&mut index, &mut object])?;
    recv.check_mutable("insert into")
        .map_err(|e| name_err(b, e))?;

    let n = recv.len() as i64;
    if index < 0 {
        index += n;
    }

    let mut elems = recv.elems.borrow_mut();
    if index >= n {
        // end
        elems.push(object);
    } else {
        if index < 0 {
            index = 0; // start
        }
        elems.insert(index as usize, object);
    }
    Ok(Value::None)
}

fn list_remove(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_list(b);
    let mut value = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut value])?;
    recv.check_mutable("remove from")
        .map_err(|e| name_err(b, e))?;
    let elems = recv.elems.borrow().clone();
    for (i, elem) in elems.iter().enumerate() {
        if super::value::equal(elem, &value).map_err(|e| Error::new(format!("remove: {}", e)))? {
            recv.elems.borrow_mut().remove(i);
            return Ok(Value::None);
        }
    }
    Err(Error::new("remove: element not found"))
}

fn list_pop(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let list = recv_list(b);
    let n = list.len() as i64;
    let mut i = n - 1;
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [&mut i])?;
    let orig = i;
    if i < 0 {
        i += n;
    }
    if i < 0 || i >= n {
        return Err(name_err(b, out_of_range(orig, n, recv(b))));
    }
    list.check_mutable("pop from").map_err(|e| name_err(b, e))?;
    let res = list.elems.borrow_mut().remove(i as usize);
    Ok(res)
}

/// Maps each rune of s (decoded as by Go, invalid bytes becoming U+FFFD).
fn map_runes(s: &[u8], f: impl Fn(char) -> char) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for_each_rune(s, |r, _| push_rune(&mut out, f(r)));
    out
}

/// Go's strings.ToLower.
pub fn to_lower(s: &[u8]) -> Vec<u8> {
    map_runes(s, unicode::to_lower)
}

/// Go's strings.ToUpper.
pub fn to_upper(s: &[u8]) -> Vec<u8> {
    map_runes(s, unicode::to_upper)
}

fn string_capitalize(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let s = recv_str(b);
    let mut res = Vec::with_capacity(s.len());
    for_each_rune(&s, |r, i| {
        let r = if i == 0 {
            unicode::to_title(r)
        } else {
            unicode::to_lower(r)
        };
        push_rune(&mut res, r);
    });
    Ok(Value::string(res))
}

/// Returns an unspecified iterable value whose iterator yields:
/// - elems: successive 1-byte substrings
/// - codepoints: successive substrings that encode a single Unicode code point.
/// - elem_ords: numeric values of successive bytes
/// - codepoint_ords: numeric values of successive Unicode code points
fn string_iterable(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let s = recv_str(b);
    let name = b.name().as_bytes();
    let ords = name[name.len() - 2] == b'd';
    let codepoints = name[0] == b'c';
    if codepoints {
        Ok(Value::other(StringCodepoints { s, ords }))
    } else {
        Ok(Value::other(StringElems { s, ords }))
    }
}

fn bytes_elems(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    Ok(Value::other(BytesIterable { bytes: recv_str(b) }))
}

/// Go's strings.Count.
pub fn count_bytes(s: &[u8], sub: &[u8]) -> usize {
    if sub.is_empty() {
        return syntax::rune_count(s) + 1;
    }
    let mut n = 0;
    let mut s = s;
    while let Some(i) = find_bytes(s, sub) {
        n += 1;
        s = &s[i + sub.len()..];
    }
    n
}

fn string_count(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut sub: Vec<u8> = Vec::new();
    let mut start_: Option<Value> = None;
    let mut end_: Option<Value> = None;
    unpack_positional_args(
        b.name(),
        args,
        kwargs,
        1,
        &mut [&mut sub, &mut start_, &mut end_],
    )?;

    let recv = recv_str(b);
    let (start, end) =
        indices(start_.as_ref(), end_.as_ref(), recv.len() as i64).map_err(|e| name_err(b, e))?;

    let slice: &[u8] = if start < end {
        &recv[start as usize..end as usize]
    } else {
        &[]
    };
    Ok(Value::int(count_bytes(slice, &sub) as i64))
}

fn all_runes(
    b: &Builtin,
    args: Args,
    kwargs: Kwargs,
    pred: impl Fn(char) -> bool,
) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_str(b);
    let mut ok = true;
    for_each_rune(&recv, |r, _| {
        if !pred(r) {
            ok = false;
        }
    });
    Ok(Value::Bool(ok && !recv.is_empty()))
}

fn string_isalnum(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    all_runes(b, args, kwargs, |r| {
        unicode::is_letter(r) || unicode::is_digit(r)
    })
}

fn string_isalpha(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    all_runes(b, args, kwargs, unicode::is_letter)
}

fn string_isdigit(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    all_runes(b, args, kwargs, unicode::is_digit)
}

fn string_islower(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_str(b);
    Ok(Value::Bool(
        is_cased_string(&recv) && *recv == *to_lower(&recv),
    ))
}

/// Reports whether its argument contains any cased code points.
fn is_cased_string(s: &[u8]) -> bool {
    let mut cased = false;
    for_each_rune(s, |r, _| {
        if is_cased_rune(r) {
            cased = true;
        }
    });
    cased
}

fn is_cased_rune(r: char) -> bool {
    // It's unclear what the correct behavior is for a rune such as 'ﬃ',
    // a lowercase letter with no upper or title case and no SimpleFold.
    r.is_ascii_lowercase() || r.is_ascii_uppercase() || unicode::simple_fold(r) != r
}

fn string_isspace(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    all_runes(b, args, kwargs, unicode::is_space)
}

fn string_istitle(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_str(b);

    // Python semantics differ from x==strings.{To,}Title(x) in Go:
    // "uppercase characters may only follow uncased characters and
    // lowercase characters only cased ones."
    let mut cased = false;
    let mut prev_cased = false;
    let mut i = 0;
    while i < recv.len() {
        let (r, sz) = decode_rune(&recv[i..]);
        i += sz;
        if r.is_ascii_uppercase() || unicode::is_title(r) {
            // e.g. "ǅ"
            if prev_cased {
                return Ok(Value::Bool(false));
            }
            prev_cased = true;
            cased = true;
        } else if unicode::is_lower(r) {
            if !prev_cased {
                return Ok(Value::Bool(false));
            }
            prev_cased = true;
            cased = true;
        } else if unicode::is_upper(r) {
            return Ok(Value::Bool(false));
        } else {
            prev_cased = false;
        }
    }
    Ok(Value::Bool(cased))
}

fn string_isupper(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_str(b);
    Ok(Value::Bool(
        is_cased_string(&recv) && *recv == *to_upper(&recv),
    ))
}

fn string_find(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    string_find_impl(b, args, kwargs, true, false)
}

fn string_format(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_str(b);
    let mut format: &[u8] = &recv;
    let mut auto = false;
    let mut manual = false; // kinds of positional indexing used
    let mut buf: Vec<u8> = Vec::new();
    let mut index = 0usize;
    loop {
        let brace = format.iter().position(|&c| c == b'{');
        let mut literal = match brace {
            Some(i) => &format[..i],
            None => format,
        };

        // Replace "}}" with "}" in non-field portion, rejecting a lone '}'.
        loop {
            let Some(j) = literal.iter().position(|&c| c == b'}') else {
                buf.extend_from_slice(literal);
                break;
            };
            if literal.len() == j + 1 || literal[j + 1] != b'}' {
                return Err(Error::new("format: single '}' in format"));
            }
            buf.extend_from_slice(&literal[..j + 1]);
            literal = &literal[j + 2..];
        }

        let Some(i) = brace else {
            break; // end of format string
        };

        if i + 1 < format.len() && format[i + 1] == b'{' {
            // "{{" means a literal '{'
            buf.push(b'{');
            format = &format[i + 2..];
            continue;
        }

        format = &format[i + 1..];
        let Some(i) = format.iter().position(|&c| c == b'}') else {
            return Err(Error::new("format: unmatched '{' in format"));
        };

        let mut conv: &[u8] = b"s";
        let mut spec: &[u8] = b"";

        let mut field = &format[..i];
        format = &format[i + 1..];

        let name: &[u8];
        match field.iter().position(|&c| c == b'!') {
            None => {
                // "name" or "name:spec"
                match field.iter().position(|&c| c == b':') {
                    None => name = field,
                    Some(i) => {
                        name = &field[..i];
                        spec = &field[i + 1..];
                    }
                }
            }
            Some(i) => {
                // "name!conv" or "name!conv:spec"
                name = &field[..i];
                field = &field[i + 1..];
                // "conv" or "conv:spec"
                match field.iter().position(|&c| c == b':') {
                    None => conv = field,
                    Some(j) => {
                        conv = &field[..j];
                        spec = &field[j + 1..];
                    }
                }
            }
        }

        let arg: Value;
        if name.is_empty() {
            // "{}": automatic indexing
            if manual {
                return Err(Error::new(
                    "format: cannot switch from manual field specification to automatic field numbering",
                ));
            }
            auto = true;
            if index >= args.len() {
                return Err(Error::new("format: tuple index out of range"));
            }
            arg = args[index].clone();
            index += 1;
        } else if let Some(num) = decimal(name) {
            // positional argument
            if auto {
                return Err(Error::new(
                    "format: cannot switch from automatic field numbering to manual field specification",
                ));
            }
            manual = true;
            if num < 0 || num as u64 >= args.len() as u64 {
                return Err(Error::new("format: tuple index out of range"));
            }
            arg = args[num as usize].clone();
        } else {
            // keyword argument
            let found = kwargs
                .iter()
                .find(|(k, _)| k.as_str() == Some(name))
                .map(|(_, v)| v.clone());
            match found {
                Some(v) => arg = v,
                None => {
                    // Starlark does not support Python's x.y or a[i] syntaxes,
                    // or nested use of {...}.
                    let n = String::from_utf8_lossy(name);
                    if name.contains(&b'.') {
                        return Err(Error::new(format!(
                            "format: attribute syntax x.y is not supported in replacement fields: {}",
                            n
                        )));
                    }
                    if name.contains(&b'[') {
                        return Err(Error::new(format!(
                            "format: element syntax a[i] is not supported in replacement fields: {}",
                            n
                        )));
                    }
                    if name.contains(&b'{') {
                        return Err(Error::new(
                            "format: nested replacement fields not supported",
                        ));
                    }
                    return Err(Error::new(format!("format: keyword {} not found", n)));
                }
            }
        }

        if !spec.is_empty() {
            // Starlark does not support Python's format_spec features.
            return Err(Error::new(format!(
                "format spec features not supported in replacement fields: {}",
                String::from_utf8_lossy(spec)
            )));
        }

        match conv {
            b"s" => match &arg {
                Value::String(s) => buf.extend_from_slice(s),
                _ => buf.extend_from_slice(arg.repr().as_bytes()),
            },
            b"r" => buf.extend_from_slice(arg.repr().as_bytes()),
            _ => {
                return Err(Error::new(format!(
                    "format: unknown conversion {}",
                    go_quote(conv)
                )))
            }
        }
    }
    Ok(Value::string(buf))
}

/// Interprets s as a sequence of decimal digits.
fn decimal(s: &[u8]) -> Option<i64> {
    let mut x: i64 = 0;
    for &c in s {
        let digit = c.wrapping_sub(b'0');
        if digit > 9 {
            return None;
        }
        x = x.wrapping_mul(10).wrapping_add(digit as i64);
        if x < 0 {
            return None; // underflow
        }
    }
    Some(x)
}

fn string_index(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    string_find_impl(b, args, kwargs, false, false)
}

fn string_join(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_str(b);
    let mut iterable = Iterable(Value::None);
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut iterable])?;
    let mut iter = iter_of(&iterable.0);
    let mut buf: Vec<u8> = Vec::new();
    let mut i = 0;
    while let Some(x) = iter.next() {
        if i > 0 {
            buf.extend_from_slice(&recv);
        }
        match &x {
            Value::String(s) => buf.extend_from_slice(s),
            _ => {
                return Err(Error::new(format!(
                    "join: in list, want string, got {}",
                    x.type_name()
                )))
            }
        }
        i += 1;
    }
    Ok(Value::string(buf))
}

fn string_lower(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    Ok(Value::string(to_lower(&recv_str(b))))
}

fn string_partition(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_str(b);
    let mut sep: Vec<u8> = Vec::new();
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut sep])?;
    if sep.is_empty() {
        return Err(name_err(b, "empty separator"));
    }
    let forward = b.name().as_bytes()[0] == b'p';
    let i = if forward {
        find_bytes(&recv, &sep)
    } else {
        rfind_bytes(&recv, &sep)
    };
    let tuple = match i {
        None => {
            if forward {
                vec![
                    Value::string(&recv[..]),
                    Value::string(""),
                    Value::string(""),
                ]
            } else {
                vec![
                    Value::string(""),
                    Value::string(""),
                    Value::string(&recv[..]),
                ]
            }
        }
        Some(i) => vec![
            Value::string(&recv[..i]),
            Value::string(&sep),
            Value::string(&recv[i + sep.len()..]),
        ],
    };
    Ok(Value::tuple(tuple))
}

fn string_removefix(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_str(b);
    let mut fix: Vec<u8> = Vec::new();
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut fix])?;
    let s: &[u8] = if b.name().as_bytes()["remove".len()] == b'p' {
        recv.strip_prefix(&fix[..]).unwrap_or(&recv)
    } else {
        recv.strip_suffix(&fix[..]).unwrap_or(&recv)
    };
    Ok(Value::string(s))
}

/// Go's strings.Replace.
pub fn replace_bytes(s: &[u8], old: &[u8], new: &[u8], n: i64) -> Vec<u8> {
    if old == new || n == 0 {
        return s.to_vec(); // avoid allocation
    }

    // Compute number of replacements.
    let m = count_bytes(s, old) as i64;
    if m == 0 {
        return s.to_vec(); // avoid allocation
    }
    let n = if n < 0 || m < n { m } else { n };

    // Apply replacements to buffer.
    let mut out = Vec::with_capacity(s.len());
    let mut start = 0;
    for i in 0..n {
        let mut j = start;
        if old.is_empty() {
            if i > 0 {
                let (_, wid) = decode_rune(&s[start..]);
                j += wid;
            }
        } else {
            j += find_bytes(&s[start..], old).unwrap_or(0);
        }
        out.extend_from_slice(&s[start..j]);
        out.extend_from_slice(new);
        start = j + old.len();
    }
    out.extend_from_slice(&s[start..]);
    out
}

fn string_replace(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_str(b);
    let mut old: Vec<u8> = Vec::new();
    let mut new: Vec<u8> = Vec::new();
    let mut count: i64 = -1;
    unpack_positional_args(
        b.name(),
        args,
        kwargs,
        2,
        &mut [&mut old, &mut new, &mut count],
    )?;
    Ok(Value::string(replace_bytes(&recv, &old, &new, count)))
}

fn string_rfind(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    string_find_impl(b, args, kwargs, true, true)
}

fn string_rindex(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    string_find_impl(b, args, kwargs, false, true)
}

fn string_startswith(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut x = Value::None;
    let mut start = Value::None;
    let mut end = Value::None;
    unpack_positional_args(
        b.name(),
        args,
        kwargs,
        1,
        &mut [&mut x, &mut start, &mut end],
    )?;

    // compute effective substring.
    let recv = recv_str(b);
    let (start, mut end) =
        indices(Some(&start), Some(&end), recv.len() as i64).map_err(|e| name_err(b, e))?;
    if end < start {
        end = start; // => empty result
    }
    let s = &recv[start as usize..end as usize];

    let ends = b.name().as_bytes()[0] == b'e'; // endswith
    let f = |s: &[u8], fix: &[u8]| {
        if ends {
            s.ends_with(fix)
        } else {
            s.starts_with(fix)
        }
    };

    match &x {
        Value::Tuple(t) => {
            for (i, x) in t.iter().enumerate() {
                let Value::String(prefix) = x else {
                    return Err(Error::new(format!(
                        "{}: want string, got {}, for element {}",
                        b.name(),
                        x.type_name(),
                        i
                    )));
                };
                if f(s, prefix) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        Value::String(fix) => Ok(Value::Bool(f(s, fix))),
        _ => Err(Error::new(format!(
            "{}: got {}, want string or tuple of string",
            b.name(),
            x.type_name()
        ))),
    }
}

/// Reports whether the rune r occurs in cutset (Go's strings.ContainsRune,
/// under which U+FFFD matches any invalid byte as well).
fn cutset_has(cutset: &[u8], r: char) -> bool {
    let mut found = false;
    for_each_rune(cutset, |c, _| {
        if c == r {
            found = true;
        }
    });
    found
}

/// Go's strings.TrimLeftFunc.
pub fn trim_left_func(s: &[u8], f: impl Fn(char) -> bool) -> &[u8] {
    let mut i = 0;
    while i < s.len() {
        let (r, sz) = decode_rune(&s[i..]);
        if !f(r) {
            return &s[i..];
        }
        i += sz;
    }
    &s[s.len()..]
}

/// Go's strings.TrimRightFunc.
pub fn trim_right_func(s: &[u8], f: impl Fn(char) -> bool) -> &[u8] {
    let mut i = s.len();
    while i > 0 {
        let (r, sz) = decode_last_rune(&s[..i]);
        if !f(r) {
            return &s[..i];
        }
        i -= sz;
    }
    &s[..0]
}

fn string_strip(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut chars: Vec<u8> = Vec::new();
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [&mut chars])?;
    let recv = recv_str(b);
    let in_chars = |r: char| cutset_has(&chars, r);
    let s: &[u8] = match b.name().as_bytes()[0] {
        b's' => {
            // strip
            if !chars.is_empty() {
                trim_right_func(trim_left_func(&recv, in_chars), in_chars)
            } else {
                trim_right_func(trim_left_func(&recv, unicode::is_space), unicode::is_space)
            }
        }
        b'l' => {
            // lstrip
            if !chars.is_empty() {
                trim_left_func(&recv, in_chars)
            } else {
                trim_left_func(&recv, unicode::is_space)
            }
        }
        _ => {
            // rstrip
            if !chars.is_empty() {
                trim_right_func(&recv, in_chars)
            } else {
                trim_right_func(&recv, unicode::is_space)
            }
        }
    };
    Ok(Value::string(s))
}

fn string_title(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let s = recv_str(b);

    // Python semantics differ from x==strings.{To,}Title(x) in Go:
    // "uppercase characters may only follow uncased characters and
    // lowercase characters only cased ones."
    let mut buf = Vec::with_capacity(s.len());
    let mut prev_cased = false;
    for_each_rune(&s, |r, _| {
        let r = if prev_cased {
            unicode::to_lower(r)
        } else {
            unicode::to_title(r)
        };
        prev_cased = is_cased_rune(r);
        push_rune(&mut buf, r);
    });
    Ok(Value::string(buf))
}

fn string_upper(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    Ok(Value::string(to_upper(&recv_str(b))))
}

/// Go's strings.Fields: splits s around runs of white space.
pub fn fields(s: &[u8]) -> Vec<&[u8]> {
    let mut res = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i < s.len() {
        let (r, sz) = decode_rune(&s[i..]);
        if unicode::is_space(r) {
            if let Some(st) = start {
                res.push(&s[st..i]);
                start = None;
            }
        } else if start.is_none() {
            start = Some(i);
        }
        i += sz;
    }
    if let Some(st) = start {
        res.push(&s[st..]);
    }
    res
}

/// Go's strings.Split (sep non-empty).
pub fn split_bytes<'a>(s: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    split_n(s, sep, -1)
}

/// Go's strings.SplitN (sep non-empty).
pub fn split_n<'a>(s: &'a [u8], sep: &[u8], n: i64) -> Vec<&'a [u8]> {
    if n == 0 {
        return Vec::new();
    }
    let mut res = Vec::new();
    let mut s = s;
    while n < 0 || (res.len() as i64) < n - 1 {
        match find_bytes(s, sep) {
            Some(i) => {
                res.push(&s[..i]);
                s = &s[i + sep.len()..];
            }
            None => break,
        }
    }
    res.push(s);
    res
}

fn string_split(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let recv = recv_str(b);
    let mut sep_: Option<Value> = None;
    let mut maxsplit: i64 = -1;
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [&mut sep_, &mut maxsplit])?;

    let res: Vec<Vec<u8>> = match &sep_ {
        None | Some(Value::None) => {
            // special case: split on whitespace
            if maxsplit < 0 {
                fields(&recv).into_iter().map(|f| f.to_vec()).collect()
            } else if b.name() == "split" {
                splitspace(&recv, maxsplit as usize)
            } else {
                // rsplit
                rsplitspace(&recv, maxsplit as usize)
            }
        }
        Some(Value::String(sep)) => {
            if sep.is_empty() {
                return Err(Error::new("split: empty separator"));
            }
            // usual case: split on non-empty separator
            if maxsplit < 0 {
                split_bytes(&recv, sep)
                    .into_iter()
                    .map(|f| f.to_vec())
                    .collect()
            } else if b.name() == "split" {
                split_n(&recv, sep, maxsplit.wrapping_add(1))
                    .into_iter()
                    .map(|f| f.to_vec())
                    .collect()
            } else {
                // rsplit
                let mut res: Vec<Vec<u8>> = split_bytes(&recv, sep)
                    .into_iter()
                    .map(|f| f.to_vec())
                    .collect();
                let excess = res.len() as i64 - maxsplit;
                if excess > 0 {
                    let excess = excess as usize;
                    let head = res[..excess].join(&sep[..]);
                    let mut out = vec![head];
                    out.extend(res.drain(excess..));
                    res = out;
                }
                res
            }
        }
        Some(sep) => {
            return Err(Error::new(format!(
                "split: got {} for separator, want string",
                sep.type_name()
            )));
        }
    };

    Ok(Value::list(res.into_iter().map(Value::string).collect()))
}

// Precondition: max >= 0.
fn rsplitspace(s: &[u8], max: usize) -> Vec<Vec<u8>> {
    let mut res: Vec<Vec<u8>> = Vec::new();
    let mut end: Option<usize> = None; // index of field end, or None in a region of spaces.
    let mut i = s.len();
    while i > 0 {
        let (r, sz) = decode_last_rune(&s[..i]);
        if unicode::is_space(r) {
            if let Some(e) = end {
                if res.len() == max {
                    break; // let this field run to the start
                }
                res.push(s[i..e].to_vec());
                end = None;
            }
        } else if end.is_none() {
            end = Some(i);
        }
        i -= sz;
    }
    if let Some(e) = end {
        res.push(s[..e].to_vec());
    }
    res.reverse();
    res
}

// Precondition: max >= 0.
fn splitspace(s: &[u8], max: usize) -> Vec<Vec<u8>> {
    let mut res: Vec<Vec<u8>> = Vec::new();
    let mut start: Option<usize> = None; // index of field start, or None in a region of spaces
    let mut i = 0;
    while i < s.len() {
        let (r, sz) = decode_rune(&s[i..]);
        if unicode::is_space(r) {
            if let Some(st) = start {
                if res.len() == max {
                    break; // let this field run to the end
                }
                res.push(s[st..i].to_vec());
                start = None;
            }
        } else if start.is_none() {
            start = Some(i);
        }
        i += sz;
    }
    if let Some(st) = start {
        res.push(s[st..].to_vec());
    }
    res
}

fn string_splitlines(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut keepends = false;
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [&mut keepends])?;
    let s = recv_str(b);
    let mut lines: Vec<Vec<u8>> = Vec::new();
    if !s.is_empty() {
        if keepends {
            // strings.SplitAfter(s, "\n")
            let mut rest: &[u8] = &s;
            while let Some(i) = rest.iter().position(|&c| c == b'\n') {
                lines.push(rest[..i + 1].to_vec());
                rest = &rest[i + 1..];
            }
            lines.push(rest.to_vec());
        } else {
            lines = split_bytes(&s, b"\n")
                .into_iter()
                .map(|l| l.to_vec())
                .collect();
        }
        if s.ends_with(b"\n") {
            lines.pop();
        }
    }
    Ok(Value::list(lines.into_iter().map(Value::string).collect()))
}

fn set_add(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut elem = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut elem])?;
    let recv = recv_set(b);
    // It is always an error to attempt to mutate a set that cannot be mutated
    recv.ht
        .check_mutable("insert into")
        .map_err(|e| name_err(b, e))?;
    if recv.has(&elem).map_err(|e| name_err(b, e))? {
        return Ok(Value::None);
    }
    recv.insert(elem).map_err(|e| name_err(b, e))?;
    Ok(Value::None)
}

fn set_clear(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_set(b);
    if !recv.is_empty() {
        recv.clear().map_err(|e| name_err(b, e))?;
    }
    Ok(Value::None)
}

fn set_op(
    b: &Builtin,
    args: Args,
    kwargs: Kwargs,
    op: impl FnOnce(&Set, &mut dyn ValueIterator) -> Result<Value, Error>,
) -> BuiltinResult {
    let mut other = Iterable(Value::None);
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut other])?;
    let mut iter = iter_of(&other.0);
    op(recv_set(b), &mut *iter).map_err(|e| name_err(b, e))
}

fn set_difference(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    set_op(b, args, kwargs, |s, it| s.difference(it))
}

fn set_intersection(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    set_op(b, args, kwargs, |s, it| s.intersection(it))
}

fn set_issubset(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    set_op(b, args, kwargs, |s, it| s.is_subset(it).map(Value::Bool))
}

fn set_issuperset(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    set_op(b, args, kwargs, |s, it| s.is_superset(it).map(Value::Bool))
}

fn set_discard(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut k = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut k])?;
    let recv = recv_set(b);
    // It is always an error to attempt to mutate a set that cannot be mutated
    recv.ht
        .check_mutable("delete from")
        .map_err(|e| name_err(b, e))?;
    if !recv.has(&k).map_err(|e| name_err(b, e))? {
        return Ok(Value::None);
    }
    recv.delete(&k).map_err(|e| name_err(b, e))?; // set is frozen
    Ok(Value::None)
}

fn set_pop(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    unpack_positional_args(b.name(), args, kwargs, 0, &mut [])?;
    let recv = recv_set(b);
    let Some(k) = recv.ht.first() else {
        return Err(name_err(b, "empty set"));
    };
    recv.delete(&k).map_err(|e| name_err(b, e))?; // set is frozen
    Ok(k)
}

fn set_remove(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let mut k = Value::None;
    unpack_positional_args(b.name(), args, kwargs, 1, &mut [&mut k])?;
    match recv_set(b).delete(&k) {
        Err(err) => Err(name_err(b, err)), // dict is frozen or key is unhashable
        Ok(true) => Ok(Value::None),
        Ok(false) => Err(name_err(b, "missing key")),
    }
}

fn set_symmetric_difference(
    _: &mut Thread,
    b: &Builtin,
    args: Args,
    kwargs: Kwargs,
) -> BuiltinResult {
    set_op(b, args, kwargs, |s, it| s.symmetric_difference(it))
}

fn set_union(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    let receiver_set = Rc::new(recv_set(b).clone_set());
    set_update_impl(&receiver_set, args, kwargs).map_err(|e| name_err(b, e))?;
    Ok(Value::Set(receiver_set))
}

fn set_update(_: &mut Thread, b: &Builtin, args: Args, kwargs: Kwargs) -> BuiltinResult {
    set_update_impl(recv_set(b), args, kwargs).map_err(|e| name_err(b, e))?;
    Ok(Value::None)
}

/// Common implementation of string_{r}{find,index}.
fn string_find_impl(
    b: &Builtin,
    args: Args,
    kwargs: Kwargs,
    allow_error: bool,
    last: bool,
) -> BuiltinResult {
    let mut sub: Vec<u8> = Vec::new();
    let mut start_: Option<Value> = None;
    let mut end_: Option<Value> = None;
    unpack_positional_args(
        b.name(),
        args,
        kwargs,
        1,
        &mut [&mut sub, &mut start_, &mut end_],
    )?;

    let s = recv_str(b);
    let (start, end) =
        indices(start_.as_ref(), end_.as_ref(), s.len() as i64).map_err(|e| name_err(b, e))?;
    let slice: &[u8] = if start < end {
        &s[start as usize..end as usize]
    } else {
        &[]
    };

    let i = if last {
        rfind_bytes(slice, &sub)
    } else {
        find_bytes(slice, &sub)
    };
    match i {
        None => {
            if !allow_error {
                return Err(name_err(b, "substring not found"));
            }
            Ok(Value::int(-1))
        }
        Some(i) => Ok(Value::int(i as i64 + start)),
    }
}

/// Common implementation of builtin dict function and dict.update method.
/// Precondition: updates.len() == 0 or 1.
pub(crate) fn update_dict(dict: &Dict, updates: &[Value], kwargs: Kwargs) -> Result<(), Error> {
    if let Some(updates) = updates.first() {
        let items = match updates {
            Value::Dict(d) => Some(d.items()),
            Value::Other(o) => o.items(),
            _ => None,
        };
        match items {
            Some(items) => {
                // Iterate over dict's key/value pairs, not just keys.
                for (k, v) in items {
                    dict.set_key(k, v)?; // dict is frozen
                }
            }
            None => {
                // all other sequences
                let Some(mut iter) = iterate(updates) else {
                    return Err(Error::new(format!(
                        "got {}, want iterable",
                        updates.type_name()
                    )));
                };
                let mut i = 0;
                while let Some(pair) = iter.next() {
                    let Some(mut iter2) = iterate(&pair) else {
                        return Err(Error::new(format!(
                            "dictionary update sequence element #{} is not iterable ({})",
                            i,
                            pair.type_name()
                        )));
                    };
                    match len(&pair) {
                        None => {
                            return Err(Error::new(format!(
                                "dictionary update sequence element #{} has unknown length ({})",
                                i,
                                pair.type_name()
                            )));
                        }
                        Some(2) => {}
                        Some(n) => {
                            return Err(Error::new(format!(
                                "dictionary update sequence element #{} has length {}, want 2",
                                i, n
                            )));
                        }
                    }
                    let k = iter2.next().unwrap_or(Value::None);
                    let v = iter2.next().unwrap_or(Value::None);
                    dict.set_key(k, v)?;
                    i += 1;
                }
            }
        }
    }

    // Then add the kwargs.
    let before = dict.len();
    for (k, v) in kwargs {
        dict.set_key(k.clone(), v.clone())?; // dict is frozen
    }
    // In the common case, each kwarg will add another dict entry.
    // If that's not so, check whether it is because there was a duplicate kwarg.
    if dict.len() < before + kwargs.len() {
        let mut keys: Vec<&[u8]> = Vec::new();
        for (k, _) in kwargs {
            let ks = k.as_str().unwrap_or_default();
            if keys.contains(&ks) {
                return Err(Error::new(format!("duplicate keyword arg: {}", k.repr())));
            }
            keys.push(ks);
        }
    }

    Ok(())
}

fn set_update_impl(s: &Set, args: Args, kwargs: Kwargs) -> Result<(), Error> {
    if !kwargs.is_empty() {
        return Err(Error::new("does not accept keyword arguments"));
    }

    for (i, arg) in args.iter().enumerate() {
        let Some(mut iter) = iterate(arg) else {
            return Err(Error::new(format!(
                "argument #{} is not iterable: {}",
                i + 1,
                arg.type_name()
            )));
        };
        s.insert_all(&mut *iter)?;
    }

    Ok(())
}

/// Writes the value as `str` would (strings unquoted).
pub fn write_str_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::String(s) => out.extend_from_slice(s),
        _ => {
            let mut s = String::new();
            write_value(&mut s, v, &mut Vec::new());
            out.extend_from_slice(s.as_bytes());
        }
    }
}
