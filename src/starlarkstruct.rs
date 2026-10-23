// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Struct and module values (ported from go.starlark.net/starlarkstruct).

use std::any::Any;
use std::cell::Cell;

use crate::starlark::{
    equal, equal_depth, fmt::go_quote, hash_string, unpack_positional_args, Builtin, Error,
    StarlarkValue, StringDict, Thread, Value,
};
use crate::syntax::Token;

/// A Module is a named collection of values, typically a suite of
/// functions imported by a load statement.
///
/// It differs from Struct primarily in that its string representation
/// does not enumerate its fields.
pub struct Module {
    pub name: String,
    pub members: StringDict,
}

impl Module {
    /// Returns a module with the given name and members.
    pub fn new(name: &str, members: StringDict) -> Module {
        Module {
            name: name.to_string(),
            members,
        }
    }

    /// Returns a module value with the given name and members.
    pub fn value(name: &str, members: StringDict) -> Value {
        Value::other(Module::new(name, members))
    }
}

impl StarlarkValue for Module {
    fn type_name(&self) -> &str {
        "module"
    }
    fn repr(&self, out: &mut String) {
        out.push_str("<module ");
        out.push_str(&go_quote(self.name.as_bytes()));
        out.push('>');
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new(format!("unhashable: {}", self.type_name())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn freeze(&self) {
        self.members.freeze();
    }
    fn has_attrs(&self) -> bool {
        true
    }
    fn attr(&self, _this: &Value, name: &str) -> Result<Option<Value>, Error> {
        Ok(self.members.get(name).cloned())
    }
    fn attr_names(&self) -> Vec<String> {
        self.members.keys()
    }
}

/// May be used as the implementation of a Starlark built-in function,
/// module(name, **kwargs). It returns a new module with the specified
/// name and members.
pub fn make_module(
    _: &mut Thread,
    b: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut name: Vec<u8> = Vec::new();
    unpack_positional_args(b.name(), args, &[], 1, &mut [&mut name])?;
    let mut members = StringDict::new();
    for (k, v) in kwargs {
        members.insert(
            String::from_utf8_lossy(k.as_str().unwrap_or_default()).into_owned(),
            v.clone(),
        );
    }
    Ok(Module::value(&String::from_utf8_lossy(&name), members))
}

/// The default constructor of a struct: the string "struct".
pub fn default_constructor() -> Value {
    Value::string("struct")
}

/// Make is the implementation of a built-in function that instantiates
/// an immutable struct from the specified keyword arguments.
pub fn make(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    if !args.is_empty() {
        return Err(Error::new("struct: unexpected positional arguments"));
    }
    Ok(Value::other(Struct::from_keywords(
        default_constructor(),
        kwargs,
    )))
}

/// Struct is an immutable Starlark type that maps field names to values.
/// It is not iterable and does not support len.
pub struct Struct {
    constructor: Value,
    entries: Vec<(String, Value)>, // sorted by name
    frozen: Cell<bool>,
}

impl Struct {
    /// Returns a new struct instance whose fields are specified by the
    /// key/value pairs in kwargs.
    pub fn from_keywords(constructor: Value, kwargs: &[(Value, Value)]) -> Struct {
        let mut entries: Vec<(String, Value)> = kwargs
            .iter()
            .map(|(k, v)| {
                (
                    String::from_utf8_lossy(k.as_str().unwrap_or_default()).into_owned(),
                    v.clone(),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Struct {
            constructor,
            entries,
            frozen: Cell::new(false),
        }
    }

    /// Returns a new struct instance whose elements are those of d.
    pub fn from_string_dict(constructor: Value, d: &StringDict) -> Struct {
        let entries = d.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Struct {
            constructor,
            entries,
            frozen: Cell::new(false),
        }
    }

    /// Returns the constructor used to create this struct.
    pub fn constructor(&self) -> &Value {
        &self.constructor
    }

    /// Returns the number of fields.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the struct has no fields.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the name and value of the field at position i, where
    /// positions are those of the attribute names (sorted by name).
    pub fn attr_at(&self, i: usize) -> (&str, &Value) {
        let e = &self.entries[i];
        (&e.0, &e.1)
    }

    /// Returns the fields, sorted by name.
    pub fn entries(&self) -> &[(String, Value)] {
        &self.entries
    }

    /// Adds the struct's fields to d.
    pub fn to_string_dict(&self, d: &mut StringDict) {
        for (k, v) in &self.entries {
            d.insert(k.clone(), v.clone());
        }
    }
}

fn structs_equal(x: &Struct, y: &Struct, depth: i32) -> Result<bool, Error> {
    if x.len() != y.len() {
        return Ok(false);
    }

    match equal(&x.constructor, &y.constructor) {
        Err(err) => {
            return Err(Error::new(format!(
                "error comparing struct constructors {} and {}: {}",
                x.constructor, y.constructor, err
            )))
        }
        Ok(false) => return Ok(false),
        Ok(true) => {}
    }

    for (xe, ye) in x.entries.iter().zip(y.entries.iter()) {
        if xe.0 != ye.0 || !equal_depth(&xe.1, &ye.1, depth - 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

impl StarlarkValue for Struct {
    fn type_name(&self) -> &str {
        "struct"
    }
    fn repr(&self, out: &mut String) {
        match &self.constructor {
            // avoid String()'s quotation
            Value::String(s) => out.push_str(&String::from_utf8_lossy(s)),
            c => out.push_str(&c.repr()),
        }
        out.push('(');
        for (i, (name, v)) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(name);
            out.push_str(" = ");
            out.push_str(&v.repr());
        }
        out.push(')');
    }
    fn hash(&self) -> Result<u32, Error> {
        let mut x: u32 = 8731;
        let mut m: u32 = 9839;
        for (name, v) in &self.entries {
            let namehash = hash_string(name.as_bytes());
            x ^= namehash.wrapping_mul(3);
            let y = v.hash()?;
            x ^= y.wrapping_mul(m);
            m = m.wrapping_add(7349);
        }
        Ok(x)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn freeze(&self) {
        if !self.frozen.get() {
            self.frozen.set(true);
            for (_, v) in &self.entries {
                v.freeze();
            }
        }
    }
    fn binary(
        &self,
        op: Token,
        y: &Value,
        side: crate::starlark::Side,
    ) -> Result<Option<Value>, Error> {
        let Some(ys) = y.downcast_ref::<Struct>() else {
            return Ok(None);
        };
        if op != Token::Plus {
            return Ok(None);
        }
        let (x, y) = if side == crate::starlark::Side::Right {
            (ys, self)
        } else {
            (self, ys)
        };

        match equal(&x.constructor, &y.constructor) {
            Err(err) => {
                return Err(Error::new(format!(
                    "in {} + {}: error comparing constructors: {}",
                    x.constructor, y.constructor, err
                )))
            }
            Ok(false) => {
                return Err(Error::new(format!(
                    "cannot add structs of different constructors: {} + {}",
                    x.constructor, y.constructor
                )))
            }
            Ok(true) => {}
        }

        let mut z = StringDict::new();
        x.to_string_dict(&mut z);
        y.to_string_dict(&mut z);
        Ok(Some(Value::other(Struct::from_string_dict(
            x.constructor.clone(),
            &z,
        ))))
    }
    fn has_attrs(&self) -> bool {
        true
    }
    fn attr(&self, _this: &Value, name: &str) -> Result<Option<Value>, Error> {
        if let Ok(i) = self.entries.binary_search_by(|e| e.0.as_str().cmp(name)) {
            return Ok(Some(self.entries[i].1.clone()));
        }
        let ctor = match &self.constructor {
            Value::String(s) if &s[..] == b"struct" => String::new(),
            c => format!("{} ", c.repr()),
        };
        Err(Error::NoSuchAttr(format!(
            "{}struct has no .{} attribute",
            ctor, name
        )))
    }
    fn attr_names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.0.clone()).collect()
    }
    fn compare_same_type(&self, op: Token, y: &Value, depth: i32) -> Option<Result<bool, Error>> {
        let y = y.downcast_ref::<Struct>()?;
        Some(match op {
            Token::Eql => structs_equal(self, y, depth),
            Token::Neq => structs_equal(self, y, depth).map(|eq| !eq),
            _ => Err(Error::new(format!(
                "{} {} {} not implemented",
                "struct", op, "struct"
            ))),
        })
    }
}
