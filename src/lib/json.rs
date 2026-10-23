// Copyright 2020 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Package json defines utilities for converting Starlark values
//! to/from JSON strings. The most recent IETF standard for JSON is
//! https://www.ietf.org/rfc/rfc7159.txt.
//!
//! ```text
//! json = module(
//!    encode,
//!    encode_indent,
//!    decode,
//!    indent,
//! )
//! ```
//!
//! `def encode(x)`: converts x to JSON by cases:
//!   - None, True, and False are converted to null, true, and false, respectively.
//!   - Starlark int values, no matter how large, are encoded as decimal integers.
//!   - Starlark float values are encoded using decimal point notation,
//!     even if the value is an integer.
//!     It is an error to encode a non-finite floating-point value.
//!   - Starlark strings are encoded as JSON strings, using UTF-16 escapes.
//!   - a Starlark IterableMapping (e.g. dict) is encoded as a JSON object.
//!     It is an error if any key is not a string.
//!   - any other Starlark Iterable (e.g. list, tuple) is encoded as a JSON array.
//!   - a Starlark HasAttrs (e.g. struct) is encoded as a JSON object.
//!
//! If a value matches more than one of the cases above, the first case
//! takes precedence. Encoding any other value yields an error.
//!
//! `def encode_indent(x, *, prefix="", indent="\t")`: equivalent to
//! `json.indent(json.encode(x), prefix=prefix, indent=indent)`.
//!
//! `def decode(x[, default])`: returns the Starlark value that the JSON
//! string x denotes. Numbers are parsed as int or float, depending on
//! whether they contain a decimal point; JSON objects are parsed as new
//! unfrozen Starlark dicts, and arrays as new unfrozen lists. If x is not
//! valid JSON, decode returns default if present, and fails otherwise.
//!
//! `def indent(str, *, prefix="", indent="\t")`: pretty-prints a valid
//! JSON encoding, prefixing each new line with prefix and using indent as
//! the unit of indentation.

mod encoding;

use std::rc::Rc;

use num_bigint::BigInt;

use crate::starlark::fmt::{go_quote, parse_float};
use crate::starlark::{
    float_to_string, get_attr, iterate, unpack_args, unpack_positional_args, Builtin, Dict, Error,
    Int, StringDict, Thread, Value,
};
use crate::starlarkstruct::Module;
use crate::syntax::utf8::push_rune;

/// Returns the json module.
pub fn module() -> Value {
    let mut members = StringDict::new();
    members.insert("encode".to_string(), Builtin::value("json.encode", encode));
    members.insert(
        "encode_indent".to_string(),
        Builtin::value("json.encode_indent", encode_indent),
    );
    members.insert("decode".to_string(), Builtin::value("json.decode", decode));
    members.insert("indent".to_string(), Builtin::value("json.indent", indent));
    Module::value("json", members)
}

/// Returns the address identifying x if it is a reference type in Go
/// (pointer, map or slice), for cycle detection.
fn pointer(x: &Value) -> Option<*const ()> {
    match x {
        Value::List(l) => Some(Rc::as_ptr(l) as *const ()),
        Value::Tuple(t) => {
            if t.is_empty() {
                None
            } else {
                Some(t.as_ptr() as *const ())
            }
        }
        Value::Dict(d) => Some(Rc::as_ptr(d) as *const ()),
        Value::Set(s) => Some(Rc::as_ptr(s) as *const ()),
        Value::Function(f) => Some(Rc::as_ptr(f) as *const ()),
        Value::Builtin(b) => Some(Rc::as_ptr(b) as *const ()),
        Value::Other(o) => Some(Rc::as_ptr(o) as *const ()),
        _ => None,
    }
}

/// Reports whether s contains only printable ASCII.
fn is_printable_ascii(s: &[u8]) -> bool {
    s.iter().all(|&b| (0x20..0x80).contains(&b))
}

/// Appends the JSON encoding of the string s.
fn quote(buf: &mut Vec<u8>, s: &[u8]) {
    if is_printable_ascii(s) {
        buf.extend_from_slice(go_quote(s).as_bytes());
    } else {
        encoding::append_string(buf, s);
    }
}

struct Encoder {
    buf: Vec<u8>,
    path: Vec<*const ()>,
}

impl Encoder {
    fn emit(&mut self, x: &Value) -> Result<(), Error> {
        // It is only necessary to push/pop the item when it might contain
        // itself, but omitting it in the other cases does not help much.
        let ptr = pointer(x);
        if let Some(p) = ptr {
            if self.path.contains(&p) {
                return Err(Error::new("cycle in JSON structure"));
            }
            self.path.push(p);
        }
        let res = self.emit_value(x);
        if ptr.is_some() {
            self.path.pop();
        }
        res
    }

    fn emit_value(&mut self, x: &Value) -> Result<(), Error> {
        match x {
            Value::None => self.buf.extend_from_slice(b"null"),
            Value::Bool(b) => self
                .buf
                .extend_from_slice(if *b { b"true" } else { b"false" }),
            Value::Int(i) => self.buf.extend_from_slice(i.to_string().as_bytes()),
            Value::Float(f) => {
                if !f.is_finite() {
                    return Err(Error::new(format!(
                        "cannot encode non-finite float {}",
                        float_to_string(*f)
                    )));
                }
                // Float.String always contains a decimal point. (%g does not!)
                self.buf.extend_from_slice(float_to_string(*f).as_bytes());
            }
            Value::String(s) => quote(&mut self.buf, s),
            _ => {
                if let Some(items) = mapping_items(x) {
                    return self.emit_mapping(x, items);
                }
                if let Some(mut iter) = iterate(x) {
                    // e.g. tuple, list
                    self.buf.push(b'[');
                    let mut i = 0;
                    while let Some(elem) = iter.next() {
                        if i > 0 {
                            self.buf.push(b',');
                        }
                        if let Err(err) = self.emit(&elem) {
                            return Err(Error::new(format!(
                                "at {} index {}: {}",
                                x.type_name(),
                                i,
                                err
                            )));
                        }
                        i += 1;
                    }
                    self.buf.push(b']');
                    return Ok(());
                }
                if x.has_attrs() {
                    return self.emit_attrs(x);
                }
                return Err(Error::new(format!(
                    "cannot encode {} as JSON",
                    x.type_name()
                )));
            }
        }
        Ok(())
    }

    // e.g. dict (must have string keys)
    fn emit_mapping(&mut self, x: &Value, mut items: Vec<(Value, Value)>) -> Result<(), Error> {
        self.buf.push(b'{');
        for (k, _) in &items {
            if k.as_str().is_none() {
                return Err(Error::new(format!(
                    "{} has {} key, want string",
                    x.type_name(),
                    k.type_name()
                )));
            }
        }
        items.sort_by(|a, b| a.0.as_str().cmp(&b.0.as_str()));
        for (i, (k, v)) in items.iter().enumerate() {
            if i > 0 {
                self.buf.push(b',');
            }
            quote(&mut self.buf, k.as_str().unwrap_or_default());
            self.buf.push(b':');
            if let Err(err) = self.emit(v) {
                return Err(Error::new(format!(
                    "in {} key {}: {}",
                    x.type_name(),
                    k.repr(),
                    err
                )));
            }
        }
        self.buf.push(b'}');
        Ok(())
    }

    // e.g. struct
    fn emit_attrs(&mut self, x: &Value) -> Result<(), Error> {
        self.buf.push(b'{');
        let mut names = crate::starlark::attr_names(x);
        names.sort();
        for (i, name) in names.iter().enumerate() {
            let attr = match x {
                Value::Other(o) => o.attr(x, name),
                _ => get_attr(x, name).map(Some),
            };
            let v = match attr {
                Ok(Some(v)) => v,
                Ok(None) => {
                    // x.attr_names() returned name, but x.attr(name)
                    // returned None, stating that the field doesn't exist.
                    return Err(Error::new(format!(
                        "missing attribute {}.{} (despite {} appearing in dir()",
                        x.type_name(),
                        name,
                        go_quote(name.as_bytes())
                    )));
                }
                Err(err) => {
                    return Err(Error::new(format!(
                        "cannot access attribute {}.{}: {}",
                        x.type_name(),
                        name,
                        err
                    )));
                }
            };
            if i > 0 {
                self.buf.push(b',');
            }
            quote(&mut self.buf, name.as_bytes());
            self.buf.push(b':');
            if let Err(err) = self.emit(&v) {
                return Err(Error::new(format!("in field .{}: {}", name, err)));
            }
        }
        self.buf.push(b'}');
        Ok(())
    }
}

/// Returns the items of x if it is an IterableMapping.
fn mapping_items(x: &Value) -> Option<Vec<(Value, Value)>> {
    match x {
        Value::Dict(d) => Some(d.items()),
        Value::Other(o) => o.items(),
        _ => None,
    }
}

fn encode_value(name: &str, args: &[Value], kwargs: &[(Value, Value)]) -> Result<Vec<u8>, Error> {
    let mut x = Value::None;
    unpack_positional_args(name, args, kwargs, 1, &mut [&mut x])?;
    let mut enc = Encoder {
        buf: Vec::new(),
        path: Vec::with_capacity(8),
    };
    if let Err(err) = enc.emit(&x) {
        return Err(Error::new(format!("{}: {}", name, err)));
    }
    Ok(enc.buf)
}

fn encode(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    Ok(Value::string(encode_value(b.name(), args, kwargs)?))
}

fn encode_indent(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut prefix: Vec<u8> = Vec::new(); // keyword-only
    let mut ind: Vec<u8> = b"\t".to_vec();
    unpack_args(
        b.name(),
        &[],
        kwargs,
        &mut [("prefix?", &mut prefix), ("indent?", &mut ind)],
    )?;
    // Rely on encode to parse the positional parameter (since the
    // signature matches), attributing errors to json.encode_indent.
    let s = encode_value(b.name(), args, &[])?;
    match encoding::indent(&s, &prefix, &ind) {
        Ok(out) => Ok(Value::string(out)),
        Err(err) => Err(Error::new(format!("{}: {}", b.name(), err))),
    }
}

fn indent(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut prefix: Vec<u8> = Vec::new(); // keyword-only
    let mut ind: Vec<u8> = b"\t".to_vec();
    unpack_args(
        b.name(),
        &[],
        kwargs,
        &mut [("prefix?", &mut prefix), ("indent?", &mut ind)],
    )?;
    let mut s: Vec<u8> = Vec::new(); // positional-only
    unpack_positional_args(b.name(), args, &[], 1, &mut [&mut s])?;
    match encoding::indent(&s, &prefix, &ind) {
        Ok(out) => Ok(Value::string(out)),
        Err(err) => Err(Error::new(format!("{}: {}", b.name(), err))),
    }
}

/// Formats a byte like Go's %q verb applied to a byte value: a quoted
/// character literal (strconv.QuoteRune).
fn quote_byte(b: u8) -> String {
    if b == b'\'' {
        return "'\\''".to_string();
    }
    if b == b'"' {
        return "'\"'".to_string();
    }
    let mut buf = Vec::new();
    push_rune(&mut buf, char::from(b));
    let s = go_quote(&buf);
    format!("'{}'", &s[1..s.len() - 1])
}

fn isdigit(b: u8) -> bool {
    b.is_ascii_digit()
}

/// A JSON decoder over the input s (the parse closure of Go's decode).
struct Decoder<'a> {
    s: &'a [u8],
    i: usize,
}

type Failure = String;

impl Decoder<'_> {
    /// Consumes leading spaces, and reports whether there is more input.
    fn skip_space(&mut self) -> bool {
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b != b' ' && b != b'\t' && b != b'\n' && b != b'\r' {
                return true;
            }
            self.i += 1;
        }
        false
    }

    /// Consumes leading spaces and returns the first non-space.
    /// It fails if at EOF.
    fn next(&mut self) -> Result<u8, Failure> {
        if self.skip_space() {
            return Ok(self.s[self.i]);
        }
        Err("unexpected end of file".to_string())
    }

    /// Returns the next JSON value from the input.
    /// It consumes leading but not trailing whitespace.
    fn parse(&mut self) -> Result<Value, Failure> {
        let s = self.s;
        let b = self.next()?;
        match b {
            b'"' => {
                // string

                // Find end of quotation.
                // Also, record whether trivial unquoting is safe.
                let mut safe = true;
                let mut closed = false;
                let mut j = self.i + 1;
                while j < s.len() {
                    let b = s[j];
                    if b == b'\\' {
                        safe = false;
                        j += 1; // skip x in \x
                    } else if b == b'"' {
                        closed = true;
                        j += 1; // skip '"'
                        break;
                    } else if b >= 0x80 {
                        safe = false;
                    }
                    j += 1;
                }
                if !closed {
                    return Err("unclosed string literal".to_string());
                }

                let r = &s[self.i..j];
                self.i = j;

                // unquote
                if safe {
                    return Ok(Value::string(&r[1..r.len() - 1]));
                }
                return encoding::unmarshal_string(r).map(Value::string);
            }
            b'n' => {
                if s[self.i..].starts_with(b"null") {
                    self.i += 4;
                    return Ok(Value::None);
                }
            }
            b't' => {
                if s[self.i..].starts_with(b"true") {
                    self.i += 4;
                    return Ok(Value::Bool(true));
                }
            }
            b'f' => {
                if s[self.i..].starts_with(b"false") {
                    self.i += 5;
                    return Ok(Value::Bool(false));
                }
            }
            b'[' => {
                // array
                let mut elems = Vec::new();
                self.i += 1; // '['
                let mut b = self.next()?;
                if b != b']' {
                    loop {
                        let elem = self.parse()?;
                        elems.push(elem);
                        b = self.next()?;
                        if b != b',' {
                            if b != b']' {
                                return Err(format!("got {}, want ',' or ']'", quote_byte(b)));
                            }
                            break;
                        }
                        self.i += 1; // ','
                    }
                }
                self.i += 1; // ']'
                return Ok(Value::list(elems));
            }
            b'{' => {
                // object
                let dict = Dict::new();
                self.i += 1; // '{'
                let mut b = self.next()?;
                if b != b'}' {
                    loop {
                        let key = self.parse()?;
                        if key.as_str().is_none() {
                            return Err(format!(
                                "got {} for object key, want string",
                                key.type_name()
                            ));
                        }
                        b = self.next()?;
                        if b != b':' {
                            return Err(format!(
                                "after object key, got {}, want ':' ",
                                quote_byte(b)
                            ));
                        }
                        self.i += 1; // ':'
                        let value = self.parse()?;
                        // Cannot fail: the dict is new and the key a string.
                        dict.set_key(key, value).map_err(|e| e.to_string())?;
                        b = self.next()?;
                        if b != b',' {
                            if b != b'}' {
                                return Err(format!(
                                    "in object, got {}, want ',' or '}}'",
                                    quote_byte(b)
                                ));
                            }
                            break;
                        }
                        self.i += 1; // ','
                    }
                }
                self.i += 1; // '}'
                return Ok(Value::Dict(Rc::new(dict)));
            }
            _ => {
                // number?
                if isdigit(b) || b == b'-' {
                    // scan literal. Allow [0-9+-eE.] for now.
                    let mut float = false;
                    let mut j = self.i + 1;
                    while j < s.len() {
                        let b = s[j];
                        if isdigit(b) {
                            // ok
                        } else if b == b'.' || b == b'e' || b == b'E' || b == b'+' || b == b'-' {
                            float = true;
                        } else {
                            break;
                        }
                        j += 1;
                    }
                    let num = &s[self.i..j];
                    self.i = j;
                    let invalid = || format!("invalid number: {}", String::from_utf8_lossy(num));

                    // Unlike most C-like languages,
                    // JSON disallows a leading zero before a digit.
                    let digits = if num[0] == b'-' { &num[1..] } else { num };
                    if digits.is_empty()
                        || digits[0] == b'0' && digits.len() > 1 && isdigit(digits[1])
                    {
                        return Err(invalid());
                    }

                    // parse literal
                    if float {
                        // The literal contains no letters other than e/E,
                        // so an infinite result means a range error.
                        return match parse_float(num) {
                            Some(x) if x.is_finite() => Ok(Value::Float(x)),
                            _ => Err(invalid()),
                        };
                    }
                    return match parse_bigint(num) {
                        Some(x) => Ok(Value::Int(Int::from_bigint(x))),
                        None => Err(invalid()),
                    };
                }
            }
        }
        Err(format!("unexpected character {}", quote_byte(b)))
    }
}

/// Parses a decimal integer with optional sign (big.Int.SetString base 10).
fn parse_bigint(num: &[u8]) -> Option<BigInt> {
    let (neg, digits) = match num.first() {
        Some(b'-') => (true, &num[1..]),
        Some(b'+') => (false, &num[1..]),
        _ => (false, num),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let x = BigInt::parse_bytes(digits, 10)?;
    Some(if neg { -x } else { x })
}

fn decode(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut s: Vec<u8> = Vec::new();
    let mut d: Option<Value> = None;
    unpack_args(
        b.name(),
        args,
        kwargs,
        &mut [("x", &mut s), ("default?", &mut d)],
    )?;
    if args.is_empty() {
        // "x" parameter is positional only; unpack_args does not allow us
        // to directly express "def decode(x, *, default)".
        return Err(Error::new(format!(
            "{}: unexpected keyword argument x",
            b.name()
        )));
    }

    // The decoder necessarily makes certain representation choices
    // such as list vs tuple, struct vs dict, int vs float.
    let mut dec = Decoder { s: &s, i: 0 };
    let res = dec.parse().and_then(|v| {
        if dec.skip_space() {
            return Err(format!(
                "unexpected character {} after value",
                quote_byte(dec.s[dec.i])
            ));
        }
        Ok(v)
    });
    match res {
        Ok(v) => Ok(v),
        Err(failure) => match d {
            Some(d) => Ok(d),
            None => Err(Error::new(format!(
                "json.decode: at offset {}, {}",
                dec.i, failure
            ))),
        },
    }
}
