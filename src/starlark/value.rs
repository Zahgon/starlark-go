// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The data types of Starlark and their basic operations (ported from
//! value.go).
//!
//! Starlark values are represented by the [`Value`] enum. The following
//! built-in types are known to the evaluator:
//!
//! ```text
//! None        -- NoneType
//! Bool        -- bool
//! Bytes       -- bytes
//! Int         -- int
//! Float       -- float
//! String      -- string
//! List        -- list
//! Tuple       -- tuple
//! Dict        -- dict
//! Set         -- set
//! Function    -- function (implemented in Starlark)
//! Builtin     -- builtin_function_or_method (function or method implemented in Rust)
//! ```
//!
//! Other types (range, string.elems, modules, library types such as
//! time.Time) are represented by `Value::Other`, holding an implementation
//! of the [`StarlarkValue`] trait. Its methods with default
//! implementations correspond to the optional interfaces of the Go
//! original (Callable, Comparable, Iterable, Sequence, Indexable, Mapping,
//! HasBinary, HasAttrs, HasSetField, HasSetIndex, HasSetKey, HasUnary).

use std::any::Any;
use std::cell::{Cell, Ref, RefCell};
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use super::eval::{EvalError, Thread};
use super::hashtable::{hash_string, HashTable};
use super::int::{cmp_int_float, finite_float_to_int, Int};
use crate::compile::{Funcode, Program};
use crate::syntax::{self, Position, Token};

/// An error returned by a Starlark operation.
///
/// `Eval` errors carry a copy of the call stack at the moment of the error
/// (Go's *EvalError); `NoSuchAttr` is Go's NoSuchAttrError; all other
/// errors are plain messages.
#[derive(Clone, Debug)]
pub enum Error {
    Msg(String),
    NoSuchAttr(String),
    Eval(Rc<EvalError>),
    /// A message that wraps the error that caused it (Go's wrappedError,
    /// whose cause is reachable through Unwrap).
    Wrapped(String, Rc<Error>),
}

impl Error {
    /// Returns a plain error with the given message (Go's errors.New /
    /// fmt.Errorf).
    pub fn new(msg: impl Into<String>) -> Error {
        Error::Msg(msg.into())
    }

    /// Returns the evaluation error, if this is one.
    pub fn as_eval(&self) -> Option<&EvalError> {
        match self {
            Error::Eval(e) => Some(e),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Msg(m) | Error::NoSuchAttr(m) | Error::Wrapped(m, _) => f.write_str(m),
            Error::Eval(e) => f.write_str(&e.msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Wrapped(_, cause) => Some(cause.as_ref()),
            Error::Eval(e) => e
                .cause
                .as_ref()
                .map(|c| c as &(dyn std::error::Error + 'static)),
            Error::Msg(_) | Error::NoSuchAttr(_) => None,
        }
    }
}

impl From<String> for Error {
    fn from(s: String) -> Error {
        Error::Msg(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Error {
        Error::Msg(s.to_string())
    }
}

/// The Side argument to [`StarlarkValue::binary`] indicates whether the
/// receiver is the left or right operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// An iterator over a sequence of values (Go's Iterator). Dropping the
/// iterator corresponds to Go's Done.
pub trait ValueIterator {
    /// Returns the next element, or None if the iterator is exhausted.
    fn next(&mut self) -> Option<Value>;
}

/// The signature of a built-in function implemented in Rust.
pub type BuiltinFn =
    dyn Fn(&mut Thread, &Builtin, &[Value], &[(Value, Value)]) -> Result<Value, Error>;

/// A value type defined outside the core evaluator (by a library such as
/// time, or by the evaluator itself for less common types such as range).
///
/// The required methods correspond to Go's Value interface; the methods
/// with defaults correspond to the optional interfaces. A default that
/// returns `None` (or `false`) means the interface is not implemented.
#[allow(clippy::len_without_is_empty)] // mirrors Go's Sequence/Indexable Len
pub trait StarlarkValue: Any {
    /// Returns a short string describing the value's type (Go's Type).
    fn type_name(&self) -> &str;

    /// Appends the string representation of the value (Go's String).
    fn repr(&self, out: &mut String);

    /// Returns a function of x such that Equals(x, y) => Hash(x) == Hash(y),
    /// or an error if the value is unhashable.
    fn hash(&self) -> Result<u32, Error>;

    /// Returns the value as `Any`, for downcasting.
    fn as_any(&self) -> &dyn Any;

    /// Returns the truth value of the object.
    fn truth(&self) -> bool {
        true
    }

    /// Marks the value and all values reachable from it as frozen.
    fn freeze(&self) {}

    /// Comparable: compares the value to another of the same type_name.
    /// The comparison operation is one of EQL, NEQ, LT, LE, GT, or GE.
    fn compare_same_type(
        &self,
        _op: Token,
        _y: &Value,
        _depth: i32,
    ) -> Option<Result<bool, Error>> {
        None
    }

    /// TotallyOrdered: compares the value to another of the same type_name.
    /// Used only if `compare_same_type` returns None.
    fn cmp(&self, _y: &Value, _depth: i32) -> Option<Result<Ordering, Error>> {
        None
    }

    /// HasBinary: implements a binary operator with `y` as the other
    /// operand. Ok(None) means the operation is not handled.
    fn binary(&self, _op: Token, _y: &Value, _side: Side) -> Result<Option<Value>, Error> {
        Ok(None)
    }

    /// HasUnary: implements a unary operator (+, -, ~). Ok(None) means
    /// the operation is not handled.
    fn unary(&self, _op: Token) -> Result<Option<Value>, Error> {
        Ok(None)
    }

    /// HasAttrs: reports whether the value has fields or methods.
    fn has_attrs(&self) -> bool {
        false
    }

    /// HasAttrs: returns the named attribute. `this` is the value itself
    /// (for binding methods). Ok(None) means no such attribute.
    fn attr(&self, _this: &Value, _name: &str) -> Result<Option<Value>, Error> {
        Ok(None)
    }

    /// HasAttrs: returns the attribute names.
    fn attr_names(&self) -> Vec<String> {
        Vec::new()
    }

    /// HasSetField: sets a field (x.f = v). None means not supported.
    /// Return `Error::NoSuchAttr` to have the runtime add a spelling hint.
    fn set_field(&self, _name: &str, _v: Value) -> Option<Result<(), Error>> {
        None
    }

    /// Iterable: returns a new iterator over the value.
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        None
    }

    /// Sequence or Indexable: returns the length of the value.
    fn len(&self) -> Option<usize> {
        None
    }

    /// Indexable: reports whether `index` is supported.
    fn is_indexable(&self) -> bool {
        false
    }

    /// Indexable: returns element i, where 0 <= i < len().
    fn index(&self, _i: usize) -> Value {
        Value::None
    }

    /// Sliceable: reports whether `slice` is supported.
    fn is_sliceable(&self) -> bool {
        false
    }

    /// Sliceable: for positive strides (step > 0), 0 <= start <= end <= n.
    /// For negative strides (step < 0), -1 <= end <= start < n.
    fn slice(&self, _start: i64, _end: i64, _step: i64) -> Value {
        Value::None
    }

    /// Container: reports whether the value contains y (`y in x`).
    fn has(&self, _y: &Value) -> Option<Result<bool, Error>> {
        None
    }

    /// Mapping: returns the value for key k, Ok(None) if not found.
    fn get(&self, _k: &Value) -> Option<Result<Option<Value>, Error>> {
        None
    }

    /// IterableMapping: returns all key/value pairs.
    fn items(&self) -> Option<Vec<(Value, Value)>> {
        None
    }

    /// HasSetKey: implements x[k] = v.
    fn set_key(&self, _k: Value, _v: Value) -> Option<Result<(), Error>> {
        None
    }

    /// HasSetIndex: implements x[i] = v, where 0 <= i < len().
    fn set_index(&self, _i: usize, _v: Value) -> Option<Result<(), Error>> {
        None
    }

    /// Callable: reports whether the value may be called.
    fn is_callable(&self) -> bool {
        false
    }

    /// Callable: the name of the function.
    fn name(&self) -> String {
        String::new()
    }

    /// Callable: calls the value. Clients should use `eval::call`.
    fn call(
        &self,
        _thread: &mut Thread,
        _args: &[Value],
        _kwargs: &[(Value, Value)],
    ) -> Result<Value, Error> {
        Err(Error::new(format!(
            "invalid call of non-function ({})",
            self.type_name()
        )))
    }

    /// Callable: the position of the callable, if it has one.
    fn position(&self) -> Option<Position> {
        None
    }
}

/// A Starlark value.
#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Int(Int),
    Float(f64),
    /// A text string: an immutable sequence of bytes.
    String(Rc<[u8]>),
    /// A binary string.
    Bytes(Rc<[u8]>),
    List(Rc<List>),
    Tuple(Rc<[Value]>),
    Dict(Rc<Dict>),
    Set(Rc<Set>),
    Function(Rc<Function>),
    Builtin(Rc<Builtin>),
    Other(Rc<dyn StarlarkValue>),
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

/// The empty tuple.
pub fn empty_tuple() -> Rc<[Value]> {
    Rc::from(Vec::new())
}

impl Value {
    /// Returns a string value with the given contents.
    pub fn string(s: impl AsRef<[u8]>) -> Value {
        Value::String(Rc::from(s.as_ref()))
    }

    /// Returns a bytes value with the given contents.
    pub fn bytes(s: impl AsRef<[u8]>) -> Value {
        Value::Bytes(Rc::from(s.as_ref()))
    }

    /// Returns an int value.
    pub fn int(i: i64) -> Value {
        Value::Int(Int::from_i64(i))
    }

    /// Returns a new list value containing the specified elements.
    pub fn list(elems: Vec<Value>) -> Value {
        Value::List(Rc::new(List::new(elems)))
    }

    /// Returns a tuple value containing the specified elements.
    pub fn tuple(elems: Vec<Value>) -> Value {
        Value::Tuple(Rc::from(elems))
    }

    /// Wraps an implementation of StarlarkValue.
    pub fn other(v: impl StarlarkValue) -> Value {
        Value::Other(Rc::new(v))
    }

    /// Returns a short string describing the value's type.
    pub fn type_name(&self) -> &str {
        match self {
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Dict(_) => "dict",
            Value::Set(_) => "set",
            Value::Function(_) => "function",
            Value::Builtin(_) => "builtin_function_or_method",
            Value::Other(o) => o.type_name(),
        }
    }

    /// Returns the string representation of the value (Go's String):
    /// strings are quoted as if by Python's repr.
    pub fn repr(&self) -> String {
        let mut out = String::new();
        write_value(&mut out, self, &mut Vec::new());
        out
    }

    /// Returns the truth value of the object.
    pub fn truth(&self) -> bool {
        match self {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(i) => i.sign() != 0,
            Value::Float(f) => *f != 0.0,
            Value::String(s) | Value::Bytes(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            Value::Dict(d) => !d.is_empty(),
            Value::Set(s) => !s.is_empty(),
            Value::Function(_) | Value::Builtin(_) => true,
            Value::Other(o) => o.truth(),
        }
    }

    /// Returns the hash of the value, or an error if it is unhashable.
    pub fn hash(&self) -> Result<u32, Error> {
        match self {
            Value::None => Ok(0),
            Value::Bool(b) => Ok(*b as u32),
            Value::Int(i) => Ok(i.hash()),
            Value::Float(f) => {
                // Equal float and int values must yield the same hash.
                if f.is_finite() {
                    Ok(finite_float_to_int(*f).hash())
                } else {
                    Ok(1618033) // NaN, +/-Inf
                }
            }
            Value::String(s) | Value::Bytes(s) => Ok(hash_string(s)),
            Value::List(_) => Err(Error::new("unhashable type: list")),
            Value::Tuple(t) => {
                // Use same algorithm as Python.
                let mut x: u32 = 0x345678;
                let mut mult: u32 = 1000003;
                for elem in t.iter() {
                    let y = elem.hash()?;
                    x ^= y.wrapping_mul(mult);
                    mult = mult.wrapping_add(82520u32.wrapping_add((t.len() + t.len()) as u32));
                }
                Ok(x)
            }
            Value::Dict(_) => Err(Error::new("unhashable type: dict")),
            Value::Set(_) => Err(Error::new("unhashable type: set")),
            Value::Function(f) => Ok(hash_string(f.name().as_bytes())),
            Value::Builtin(b) => {
                let mut h = hash_string(b.name().as_bytes());
                if b.recv.is_some() {
                    h ^= 5521;
                }
                Ok(h)
            }
            Value::Other(o) => o.hash(),
        }
    }

    /// Causes the value, and all values transitively reachable from it
    /// through collections and closures, to be marked as frozen.
    pub fn freeze(&self) {
        match self {
            Value::List(l) => l.freeze(),
            Value::Tuple(t) => {
                for elem in t.iter() {
                    elem.freeze();
                }
            }
            Value::Dict(d) => d.ht.freeze(),
            Value::Set(s) => s.ht.freeze(),
            Value::Function(f) => f.freeze(),
            Value::Builtin(b) => {
                if let Some(recv) = &b.recv {
                    recv.freeze();
                }
            }
            Value::Other(o) => o.freeze(),
            _ => {}
        }
    }

    /// Returns the contents of a string value.
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the float value of an int or float.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(i.to_f64()),
            _ => None,
        }
    }

    /// Downcasts a `Value::Other` to the concrete type T.
    pub fn downcast_ref<T: StarlarkValue>(&self) -> Option<&T> {
        match self {
            Value::Other(o) => o.as_any().downcast_ref::<T>(),
            _ => None,
        }
    }

    /// Reports whether the value is callable.
    pub fn is_callable(&self) -> bool {
        match self {
            Value::Function(_) | Value::Builtin(_) => true,
            Value::Other(o) => o.is_callable(),
            _ => false,
        }
    }

    /// Reports whether the value is iterable.
    pub fn is_iterable(&self) -> bool {
        match self {
            Value::List(_) | Value::Tuple(_) | Value::Dict(_) | Value::Set(_) => true,
            Value::Other(o) => {
                // Checking requires creating (and dropping) an iterator.
                o.iterate().is_some()
            }
            _ => false,
        }
    }

    /// Reports whether the value is Indexable.
    pub fn is_indexable(&self) -> bool {
        match self {
            Value::String(_) | Value::Bytes(_) | Value::List(_) | Value::Tuple(_) => true,
            Value::Other(o) => o.is_indexable(),
            _ => false,
        }
    }

    /// Reports whether the value has attributes (HasAttrs).
    pub fn has_attrs(&self) -> bool {
        match self {
            Value::String(_)
            | Value::Bytes(_)
            | Value::List(_)
            | Value::Dict(_)
            | Value::Set(_) => true,
            Value::Other(o) => o.has_attrs(),
            _ => false,
        }
    }

    /// Returns the name of a callable value.
    pub fn callable_name(&self) -> String {
        match self {
            Value::Function(f) => f.name().to_string(),
            Value::Builtin(b) => b.name().to_string(),
            Value::Other(o) => o.name(),
            _ => String::new(),
        }
    }

    /// Returns element i of an Indexable value (0 <= i < len).
    pub fn index(&self, i: usize) -> Value {
        match self {
            Value::String(s) => Value::String(Rc::from(&s[i..i + 1])),
            Value::Bytes(s) => Value::Bytes(Rc::from(&s[i..i + 1])),
            Value::List(l) => l.elems.borrow()[i].clone(),
            Value::Tuple(t) => t[i].clone(),
            Value::Other(o) => o.index(i),
            _ => Value::None,
        }
    }

    /// Reports whether two values are the same object (Go's interface
    /// equality, used when a type defines no comparison).
    pub fn is_identical(&self, y: &Value) -> bool {
        match (self, y) {
            (Value::None, Value::None) => true,
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Builtin(a), Value::Builtin(b)) => Rc::ptr_eq(a, b),
            (Value::List(a), Value::List(b)) => Rc::ptr_eq(a, b),
            (Value::Dict(a), Value::Dict(b)) => Rc::ptr_eq(a, b),
            (Value::Set(a), Value::Set(b)) => Rc::ptr_eq(a, b),
            (Value::Other(a), Value::Other(b)) => std::ptr::addr_eq(Rc::as_ptr(a), Rc::as_ptr(b)),
            _ => false,
        }
    }

    /// Returns the value of a String or Bytes as a Rust string (lossily
    /// for invalid UTF-8; for messages only).
    pub fn to_display_string(&self) -> String {
        match self {
            Value::String(s) => String::from_utf8_lossy(s).into_owned(),
            _ => self.repr(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Value {
        Value::int(i)
    }
}

impl From<Int> for Value {
    fn from(i: Int) -> Value {
        Value::Int(i)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Value {
        Value::Float(f)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::string(s)
    }
}

/// Formats a Starlark float (Go's Float.format): conv is 'g' (the
/// default used by str, the shortest representation that always contains
/// a '.' or an 'e'), or one of 'e', 'E', 'f', 'F', 'G' with precision 6.
pub fn format_float(out: &mut String, f: f64, conv: u8) {
    if !f.is_finite() {
        if f == f64::INFINITY {
            out.push_str("+inf");
        } else if f == f64::NEG_INFINITY {
            out.push_str("-inf");
        } else {
            out.push_str("nan");
        }
        return;
    }
    if conv == b'g' || conv == b'G' {
        let s = super::fmt::format_float(f, conv, -1);
        out.push_str(&s);
        // Ensure result always has a decimal point if no exponent.
        // "123" -> "123.0"
        let e = (conv - b'g' + b'e') as char;
        if !s.contains(e) && !s.contains('.') {
            out.push_str(".0");
        }
        return;
    }
    out.push_str(&super::fmt::format_float(f, conv, 6));
}

/// Returns the Starlark string representation of a float (str(f)).
pub fn float_to_string(f: f64) -> String {
    let mut s = String::new();
    format_float(&mut s, f, b'g');
    s
}

/// Performs a three-valued comparison on floats, which are totally
/// ordered with NaN > +Inf.
pub fn float_cmp(x: f64, y: f64) -> i32 {
    if x > y {
        return 1;
    } else if x < y {
        return -1;
    } else if x == y {
        return 0;
    }
    // At least one operand is NaN.
    if !x.is_nan() {
        -1 // y is NaN
    } else if !y.is_nan() {
        1 // x is NaN
    } else {
        0 // both NaN
    }
}

/// Floored float modulo, with the sign of y.
pub fn float_mod(x: f64, y: f64) -> f64 {
    let mut z = x % y;
    if (x < 0.0) != (y < 0.0) && z != 0.0 {
        z += y;
    }
    z
}

// ---- List ----

/// A Starlark list value.
pub struct List {
    pub(crate) elems: RefCell<Vec<Value>>,
    frozen: Cell<bool>,
    itercount: Cell<u32>,
}

impl List {
    /// Returns a list containing the specified elements.
    pub fn new(elems: Vec<Value>) -> List {
        List {
            elems: RefCell::new(elems),
            frozen: Cell::new(false),
            itercount: Cell::new(0),
        }
    }

    /// Returns the number of elements.
    pub fn len(&self) -> usize {
        self.elems.borrow().len()
    }

    /// Reports whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a read-only view of the elements.
    pub fn elems(&self) -> Ref<'_, Vec<Value>> {
        self.elems.borrow()
    }

    /// Returns element i (0 <= i < len).
    pub fn index(&self, i: usize) -> Value {
        self.elems.borrow()[i].clone()
    }

    /// Reports whether the list is frozen.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    pub(crate) fn freeze(&self) {
        if !self.frozen.get() {
            self.frozen.set(true);
            let elems = self.elems.borrow().clone();
            for elem in &elems {
                elem.freeze();
            }
        }
    }

    /// Reports an error if the list should not be mutated.
    /// verb+" list" should describe the operation.
    pub fn check_mutable(&self, verb: &str) -> Result<(), Error> {
        if self.frozen.get() {
            return Err(Error::new(format!("cannot {} frozen list", verb)));
        }
        if self.itercount.get() > 0 {
            return Err(Error::new(format!("cannot {} list during iteration", verb)));
        }
        Ok(())
    }

    /// Implements x[i] = v (0 <= i < len).
    pub fn set_index(&self, i: usize, v: Value) -> Result<(), Error> {
        self.check_mutable("assign to element of")?;
        self.elems.borrow_mut()[i] = v;
        Ok(())
    }

    /// Appends an element.
    pub fn append(&self, v: Value) -> Result<(), Error> {
        self.check_mutable("append to")?;
        self.elems.borrow_mut().push(v);
        Ok(())
    }

    /// Removes all elements.
    pub fn clear(&self) -> Result<(), Error> {
        self.check_mutable("clear")?;
        self.elems.borrow_mut().clear();
        Ok(())
    }

    /// Appends without checking mutability (used by the interpreter for
    /// list comprehensions, whose list is not yet visible).
    pub(crate) fn push_unchecked(&self, v: Value) {
        self.elems.borrow_mut().push(v);
    }
}

struct ListIterator {
    l: Rc<List>,
    i: usize,
}

impl ValueIterator for ListIterator {
    fn next(&mut self) -> Option<Value> {
        let elems = self.l.elems.borrow();
        if self.i < elems.len() {
            let v = elems[self.i].clone();
            self.i += 1;
            return Some(v);
        }
        None
    }
}

impl Drop for ListIterator {
    fn drop(&mut self) {
        if !self.l.frozen.get() {
            self.l
                .itercount
                .set(self.l.itercount.get().saturating_sub(1));
        }
    }
}

struct TupleIterator {
    t: Rc<[Value]>,
    i: usize,
}

impl ValueIterator for TupleIterator {
    fn next(&mut self) -> Option<Value> {
        let v = self.t.get(self.i)?.clone();
        self.i += 1;
        Some(v)
    }
}

// ---- Dict and Set ----

/// A Starlark dictionary.
#[derive(Default)]
pub struct Dict {
    pub(crate) ht: HashTable,
}

impl Dict {
    /// Returns a new, empty dictionary.
    pub fn new() -> Dict {
        Dict::default()
    }

    pub fn len(&self) -> usize {
        self.ht.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ht.is_empty()
    }

    /// Returns the value for key k, Ok(None) if not found.
    pub fn get(&self, k: &Value) -> Result<Option<Value>, Error> {
        self.ht.lookup(k)
    }

    /// Implements d[k] = v.
    pub fn set_key(&self, k: Value, v: Value) -> Result<(), Error> {
        self.ht.insert(k, v)
    }

    /// Removes key k, returning its value if present.
    pub fn delete(&self, k: &Value) -> Result<Option<Value>, Error> {
        self.ht.delete(k)
    }

    pub fn clear(&self) -> Result<(), Error> {
        self.ht.clear()
    }

    /// Returns all key/value pairs in insertion order.
    pub fn items(&self) -> Vec<(Value, Value)> {
        self.ht.items()
    }

    /// Returns the keys in insertion order.
    pub fn keys(&self) -> Vec<Value> {
        self.ht.keys()
    }

    /// Returns the underlying hash table.
    pub fn table(&self) -> &HashTable {
        &self.ht
    }

    /// Returns x | y.
    pub fn union(&self, y: &Dict) -> Dict {
        let z = Dict::new();
        z.ht.init(); // a lower bound
        let _ = z.ht.add_all(&self.ht); // can't fail
        let _ = z.ht.add_all(&y.ht); // can't fail
        z
    }
}

/// A Starlark set.
#[derive(Default)]
pub struct Set {
    pub(crate) ht: HashTable,
}

impl Set {
    /// Returns a new, empty set.
    pub fn new() -> Set {
        Set::default()
    }

    pub fn len(&self) -> usize {
        self.ht.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ht.is_empty()
    }

    pub fn has(&self, k: &Value) -> Result<bool, Error> {
        Ok(self.ht.lookup(k)?.is_some())
    }

    pub fn insert(&self, k: Value) -> Result<(), Error> {
        self.ht.insert(k, Value::None)
    }

    pub fn delete(&self, k: &Value) -> Result<bool, Error> {
        Ok(self.ht.delete(k)?.is_some())
    }

    pub fn clear(&self) -> Result<(), Error> {
        self.ht.clear()
    }

    /// Returns the elements in insertion order.
    pub fn elems(&self) -> Vec<Value> {
        self.ht.keys()
    }

    /// Returns the underlying hash table.
    pub fn table(&self) -> &HashTable {
        &self.ht
    }

    pub(crate) fn clone_set(&self) -> Set {
        let set = Set::new();
        for k in self.ht.keys() {
            let _ = set.insert(k); // can't fail
        }
        set
    }

    pub fn union(&self, iter: &mut dyn ValueIterator) -> Result<Value, Error> {
        let set = self.clone_set();
        while let Some(x) = iter.next() {
            set.insert(x)?;
        }
        Ok(Value::Set(Rc::new(set)))
    }

    pub fn insert_all(&self, iter: &mut dyn ValueIterator) -> Result<(), Error> {
        while let Some(x) = iter.next() {
            self.insert(x)?;
        }
        Ok(())
    }

    pub fn difference(&self, other: &mut dyn ValueIterator) -> Result<Value, Error> {
        let diff = self.clone_set();
        while let Some(x) = other.next() {
            diff.delete(&x)?;
        }
        Ok(Value::Set(Rc::new(diff)))
    }

    pub fn is_superset(&self, other: &mut dyn ValueIterator) -> Result<bool, Error> {
        while let Some(x) = other.next() {
            if !self.has(&x)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn is_subset(&self, other: &mut dyn ValueIterator) -> Result<bool, Error> {
        Ok(self.ht.count(other)? == self.len())
    }

    pub fn intersection(&self, other: &mut dyn ValueIterator) -> Result<Value, Error> {
        let intersect = Set::new();
        while let Some(x) = other.next() {
            if self.has(&x)? {
                intersect.insert(x)?;
            }
        }
        Ok(Value::Set(Rc::new(intersect)))
    }

    pub fn symmetric_difference(&self, other: &mut dyn ValueIterator) -> Result<Value, Error> {
        let diff = self.clone_set();
        while let Some(x) = other.next() {
            let found = diff.delete(&x)?;
            if !found {
                let _ = diff.insert(x); // can't fail
            }
        }
        Ok(Value::Set(Rc::new(diff)))
    }
}

/// Iterates over the keys of a dict or set.
struct KeyIterator {
    owner: Value,
    cursor: Option<usize>,
}

impl KeyIterator {
    fn table(&self) -> &HashTable {
        match &self.owner {
            Value::Dict(d) => &d.ht,
            Value::Set(s) => &s.ht,
            _ => unreachable!("key iterator over non-table"),
        }
    }

    fn start(owner: Value) -> KeyIterator {
        let mut it = KeyIterator {
            owner,
            cursor: None,
        };
        it.cursor = it.table().iter_start();
        it
    }
}

impl ValueIterator for KeyIterator {
    fn next(&mut self) -> Option<Value> {
        let mut cursor = self.cursor;
        let k = self.table().iter_next(&mut cursor);
        self.cursor = cursor;
        k
    }
}

impl Drop for KeyIterator {
    fn drop(&mut self) {
        self.table().iter_done();
    }
}

// ---- Function, Module, Builtin ----

/// A Module represents an evaluated Starlark module. It is the dynamic
/// counterpart to a Program. All functions in the same program share a
/// Module.
pub struct Module {
    pub(crate) program: Rc<Program>,
    pub(crate) predeclared: super::eval::StringDict,
    pub(crate) globals: RefCell<Vec<Option<Value>>>,
    pub(crate) constants: Vec<Value>,
}

impl Module {
    /// Returns the compiled program from which this module was constructed.
    pub fn program(&self) -> &Rc<Program> {
        &self.program
    }

    /// Returns a new StringDict containing all global variables so far
    /// defined in the module.
    pub fn globals(&self) -> super::eval::StringDict {
        let mut r = super::eval::StringDict::new();
        let globals = self.globals.borrow();
        for (i, id) in self.program.globals.iter().enumerate() {
            if let Some(Some(v)) = globals.get(i) {
                r.insert(id.name.clone(), v.clone());
            }
        }
        r
    }

    /// Returns the predeclared environment used to construct this module.
    pub fn predeclared(&self) -> &super::eval::StringDict {
        &self.predeclared
    }
}

/// A Function is a function defined by a Starlark def statement or lambda
/// expression. The initialization behavior of a Starlark module is also
/// represented by a Function.
pub struct Function {
    pub(crate) funcode: Rc<Funcode>,
    pub(crate) module: Rc<Module>,
    pub(crate) defaults: Vec<Value>,
    pub(crate) freevars: Vec<Value>,
    frozen: Cell<bool>,
}

impl Function {
    pub(crate) fn new(
        funcode: Rc<Funcode>,
        module: Rc<Module>,
        defaults: Vec<Value>,
        freevars: Vec<Value>,
    ) -> Function {
        Function {
            funcode,
            module,
            defaults,
            freevars,
            frozen: Cell::new(false),
        }
    }

    /// The function's name ("lambda" for anonymous functions).
    pub fn name(&self) -> &str {
        &self.funcode.name
    }

    /// The function's docstring.
    pub fn doc(&self) -> &str {
        &self.funcode.doc
    }

    /// The position of the def or lambda token.
    pub fn position(&self) -> Position {
        self.funcode.pos.clone()
    }

    /// The module in which the function was defined.
    pub fn module(&self) -> &Rc<Module> {
        &self.module
    }

    /// Returns the global variables so far defined in the function's module.
    pub fn globals(&self) -> super::eval::StringDict {
        self.module.globals()
    }

    pub fn num_params(&self) -> usize {
        self.funcode.num_params
    }

    pub fn num_kwonly_params(&self) -> usize {
        self.funcode.num_kwonly_params
    }

    pub fn has_varargs(&self) -> bool {
        self.funcode.has_varargs
    }

    pub fn has_kwargs(&self) -> bool {
        self.funcode.has_kwargs
    }

    /// Returns the name and position of the ith parameter.
    pub fn param(&self, i: usize) -> (&str, &Position) {
        let id = &self.funcode.locals[i];
        (&id.name, &id.pos)
    }

    /// Returns the default value of the specified parameter
    /// (0 <= i < `num_params()`), or None if the parameter is not optional.
    pub fn param_default(&self, i: usize) -> Option<Value> {
        assert!(i < self.num_params(), "param_default({i})");

        // fn.defaults omits all required params up to the first optional param. It
        // also does not include *args or **kwargs at the end.
        let mut first_opt_idx = self.num_params() as isize - self.defaults.len() as isize;
        if self.has_varargs() {
            first_opt_idx -= 1;
        }
        if self.has_kwargs() {
            first_opt_idx -= 1;
        }
        let i = i as isize;
        if i < first_opt_idx || i >= first_opt_idx + self.defaults.len() as isize {
            return None;
        }

        let dflt = &self.defaults[(i - first_opt_idx) as usize];
        if super::interp::is_mandatory(dflt) {
            return None;
        }
        Some(dflt.clone())
    }

    /// Returns the number of free variables of the function.
    pub fn num_free_vars(&self) -> usize {
        self.funcode.freevars.len()
    }

    /// Returns the binding (name and binding position) and value of the
    /// i'th free variable of the function. The value is None if the
    /// variable has not yet been assigned.
    pub fn free_var(&self, i: usize) -> (&crate::compile::Binding, Option<Value>) {
        (
            &self.funcode.freevars[i],
            super::interp::cell_contents(&self.freevars[i]),
        )
    }

    fn freeze(&self) {
        // The Go original has no guard here and overflows its stack on
        // closures that refer to themselves; the guard avoids that.
        if self.frozen.get() {
            return;
        }
        self.frozen.set(true);
        for v in &self.defaults {
            v.freeze();
        }
        for v in &self.freevars {
            v.freeze();
        }
    }
}

/// A Builtin is a function implemented in Rust.
pub struct Builtin {
    name: Rc<str>,
    func: Rc<BuiltinFn>,
    recv: Option<Value>,
}

impl Builtin {
    /// Returns a new builtin_function_or_method with the specified name
    /// and implementation.
    pub fn new(
        name: &str,
        func: impl Fn(&mut Thread, &Builtin, &[Value], &[(Value, Value)]) -> Result<Value, Error>
            + 'static,
    ) -> Builtin {
        Builtin {
            name: Rc::from(name),
            func: Rc::new(func),
            recv: None,
        }
    }

    /// Returns a new builtin as a Value.
    pub fn value(
        name: &str,
        func: impl Fn(&mut Thread, &Builtin, &[Value], &[(Value, Value)]) -> Result<Value, Error>
            + 'static,
    ) -> Value {
        Value::Builtin(Rc::new(Builtin::new(name, func)))
    }

    /// The builtin's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The receiver of a bound method, if any.
    pub fn receiver(&self) -> Option<&Value> {
        self.recv.as_ref()
    }

    /// Returns a new Builtin value representing a method closure, that is,
    /// a built-in function bound to a receiver value.
    pub fn bind_receiver(&self, recv: Value) -> Builtin {
        Builtin {
            name: self.name.clone(),
            func: self.func.clone(),
            recv: Some(recv),
        }
    }

    /// Calls the implementation. Clients should use `eval::call`.
    pub fn call_internal(
        &self,
        thread: &mut Thread,
        args: &[Value],
        kwargs: &[(Value, Value)],
    ) -> Result<Value, Error> {
        (self.func)(thread, self, args, kwargs)
    }
}

// ---- string iterables ----

/// An iterable whose iterator yields a sequence of elements (bytes),
/// either numerically or as successive substrings. It is an indexable
/// sequence.
pub(crate) struct StringElems {
    pub(crate) s: Rc<[u8]>,
    pub(crate) ords: bool,
}

impl StringElems {
    fn elem(&self, i: usize) -> Value {
        if self.ords {
            Value::int(self.s[i] as i64)
        } else {
            Value::String(Rc::from(&self.s[i..i + 1]))
        }
    }
}

struct StringElemsIterator {
    s: Rc<[u8]>,
    ords: bool,
    i: usize,
}

impl ValueIterator for StringElemsIterator {
    fn next(&mut self) -> Option<Value> {
        if self.i == self.s.len() {
            return None;
        }
        let i = self.i;
        self.i += 1;
        Some(if self.ords {
            Value::int(self.s[i] as i64)
        } else {
            Value::String(Rc::from(&self.s[i..i + 1]))
        })
    }
}

fn compare_by_identity(op: Token, eq: bool, x: &str, y: &str) -> Option<Result<bool, Error>> {
    match op {
        Token::Eql => Some(Ok(eq)),
        Token::Neq => Some(Ok(!eq)),
        _ => Some(Err(Error::new(format!(
            "{} {} {} not implemented",
            x, op, y
        )))),
    }
}

impl StarlarkValue for StringElems {
    fn type_name(&self) -> &str {
        "string.elems"
    }
    fn repr(&self, out: &mut String) {
        out.push_str(&syntax::quote(&self.s, false));
        out.push_str(if self.ords {
            ".elem_ords()"
        } else {
            ".elems()"
        });
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new(format!("unhashable: {}", self.type_name())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn compare_same_type(&self, op: Token, y: &Value, _depth: i32) -> Option<Result<bool, Error>> {
        let eq = y
            .downcast_ref::<StringElems>()
            .is_some_and(|y| y.s == self.s && y.ords == self.ords);
        compare_by_identity(op, eq, self.type_name(), y.type_name())
    }
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        Some(Box::new(StringElemsIterator {
            s: self.s.clone(),
            ords: self.ords,
            i: 0,
        }))
    }
    fn len(&self) -> Option<usize> {
        Some(self.s.len())
    }
    fn is_indexable(&self) -> bool {
        true
    }
    fn index(&self, i: usize) -> Value {
        self.elem(i)
    }
}

/// An iterable whose iterator yields a sequence of Unicode code points,
/// either numerically or as successive substrings. It is not indexable.
pub(crate) struct StringCodepoints {
    pub(crate) s: Rc<[u8]>,
    pub(crate) ords: bool,
}

struct StringCodepointsIterator {
    s: Rc<[u8]>,
    ords: bool,
    i: usize,
}

impl ValueIterator for StringCodepointsIterator {
    fn next(&mut self) -> Option<Value> {
        let s = &self.s[self.i..];
        if s.is_empty() {
            return None;
        }
        let (r, sz) = syntax::utf8::decode_rune(s);
        let v = if !self.ords {
            if r == syntax::utf8::RUNE_ERROR {
                let mut b = Vec::new();
                syntax::utf8::push_rune(&mut b, r);
                Value::string(b)
            } else {
                Value::String(Rc::from(&s[..sz]))
            }
        } else {
            Value::int(r as i64)
        };
        self.i += sz;
        Some(v)
    }
}

impl StarlarkValue for StringCodepoints {
    fn type_name(&self) -> &str {
        "string.codepoints"
    }
    fn repr(&self, out: &mut String) {
        out.push_str(&syntax::quote(&self.s, false));
        out.push_str(if self.ords {
            ".codepoint_ords()"
        } else {
            ".codepoints()"
        });
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new(format!("unhashable: {}", self.type_name())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn compare_same_type(&self, op: Token, y: &Value, _depth: i32) -> Option<Result<bool, Error>> {
        let eq = y
            .downcast_ref::<StringCodepoints>()
            .is_some_and(|y| y.s == self.s && y.ords == self.ords);
        compare_by_identity(op, eq, self.type_name(), y.type_name())
    }
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        Some(Box::new(StringCodepointsIterator {
            s: self.s.clone(),
            ords: self.ords,
            i: 0,
        }))
    }
}

/// An iterable returned by bytes.elems(), whose iterator yields a
/// sequence of numeric bytes values.
pub(crate) struct BytesIterable {
    pub(crate) bytes: Rc<[u8]>,
}

impl StarlarkValue for BytesIterable {
    fn type_name(&self) -> &str {
        "bytes.elems"
    }
    fn repr(&self, out: &mut String) {
        out.push_str(&syntax::quote(&self.bytes, true));
        out.push_str(".elems()");
    }
    fn hash(&self) -> Result<u32, Error> {
        Err(Error::new(format!("unhashable: {}", self.type_name())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn compare_same_type(&self, op: Token, y: &Value, _depth: i32) -> Option<Result<bool, Error>> {
        let eq = y
            .downcast_ref::<BytesIterable>()
            .is_some_and(|y| y.bytes == self.bytes);
        compare_by_identity(op, eq, self.type_name(), y.type_name())
    }
    fn iterate(&self) -> Option<Box<dyn ValueIterator>> {
        Some(Box::new(StringElemsIterator {
            s: self.bytes.clone(),
            ords: true,
            i: 0,
        }))
    }
}

// ---- generic operations ----

/// Returns the string form of value v (Go's toString / writeValue).
pub fn to_string(v: &Value) -> String {
    v.repr()
}

/// Writes x to out. `path` is used to detect cycles: it contains the
/// lists and dicts currently being printed.
pub fn write_value(out: &mut String, x: &Value, path: &mut Vec<*const ()>) {
    match x {
        Value::None => out.push_str("None"),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Float(f) => format_float(out, *f, b'g'),
        Value::String(s) => out.push_str(&syntax::quote(s, false)),
        Value::Bytes(s) => out.push_str(&syntax::quote(s, true)),
        Value::List(l) => {
            out.push('[');
            let p = Rc::as_ptr(l) as *const ();
            if path.contains(&p) {
                out.push_str("..."); // list contains itself
            } else {
                let elems = l.elems.borrow().clone();
                path.push(p);
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_value(out, elem, path);
                }
                path.pop();
            }
            out.push(']');
        }
        Value::Tuple(t) => {
            out.push('(');
            for (i, elem) in t.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, elem, path);
            }
            if t.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        Value::Function(f) => {
            out.push_str("<function ");
            out.push_str(f.name());
            out.push('>');
        }
        Value::Builtin(b) => match &b.recv {
            Some(recv) => {
                out.push_str("<built-in method ");
                out.push_str(b.name());
                out.push_str(" of ");
                out.push_str(recv.type_name());
                out.push_str(" value>");
            }
            None => {
                out.push_str("<built-in function ");
                out.push_str(b.name());
                out.push('>');
            }
        },
        Value::Dict(d) => {
            out.push('{');
            let p = Rc::as_ptr(d) as *const ();
            if path.contains(&p) {
                out.push_str("..."); // dict contains itself
            } else {
                let mut sep = "";
                for (k, v) in d.items() {
                    out.push_str(sep);
                    write_value(out, &k, path);
                    out.push_str(": ");
                    path.push(p); // cycle check
                    write_value(out, &v, path);
                    path.pop();
                    sep = ", ";
                }
            }
            out.push('}');
        }
        Value::Set(s) => {
            out.push_str("set([");
            for (i, k) in s.elems().iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, k, path);
            }
            out.push_str("])");
        }
        Value::Other(o) => o.repr(out),
    }
}

/// CompareLimit is the depth limit on recursive comparison operations
/// such as == and <. Comparison of data structures deeper than this limit
/// may fail.
pub const COMPARE_LIMIT: i32 = 10;

/// Reports whether two Starlark values are equal.
pub fn equal(x: &Value, y: &Value) -> Result<bool, Error> {
    if let Value::String(xs) = x {
        // fast path for an important special case
        return Ok(matches!(y, Value::String(ys) if xs == ys));
    }
    equal_depth(x, y, COMPARE_LIMIT)
}

/// Reports whether two Starlark values are equal, with a recursion limit.
pub fn equal_depth(x: &Value, y: &Value, depth: i32) -> Result<bool, Error> {
    compare_depth(Token::Eql, x, y, depth)
}

/// Compares two Starlark values. The comparison operation must be one of
/// EQL, NEQ, LT, LE, GT, or GE.
pub fn compare(op: Token, x: &Value, y: &Value) -> Result<bool, Error> {
    compare_depth(op, x, y, COMPARE_LIMIT)
}

fn same_type(x: &Value, y: &Value) -> bool {
    match (x, y) {
        (Value::Other(a), Value::Other(b)) => a.type_name() == b.type_name(),
        _ => std::mem::discriminant(x) == std::mem::discriminant(y),
    }
}

fn not_implemented(x: &Value, op: Token, y: &Value) -> Error {
    Error::new(format!(
        "{} {} {} not implemented",
        x.type_name(),
        op,
        y.type_name()
    ))
}

/// Compares two Starlark values, limiting the depth of recursion in
/// cyclic data structures.
pub fn compare_depth(op: Token, x: &Value, y: &Value, depth: i32) -> Result<bool, Error> {
    if depth < 1 {
        return Err(Error::new("comparison exceeded maximum recursion depth"));
    }
    if same_type(x, y) {
        return match (x, y) {
            (Value::Bool(a), Value::Bool(b)) => Ok(threeway(op, *a as i32 - *b as i32)),
            (Value::Int(a), Value::Int(b)) => Ok(threeway(op, ord_to_i32(a.cmp(b)))),
            (Value::Float(a), Value::Float(b)) => Ok(threeway(op, float_cmp(*a, *b))),
            (Value::String(a), Value::String(b)) | (Value::Bytes(a), Value::Bytes(b)) => {
                Ok(threeway(op, ord_to_i32(a[..].cmp(&b[..]))))
            }
            (Value::List(a), Value::List(b)) => {
                // It's tempting to check x == y as an optimization here,
                // but wrong because a list containing NaN is not equal to itself.
                let (ae, be) = (a.elems.borrow().clone(), b.elems.borrow().clone());
                slice_compare(op, &ae, &be, depth)
            }
            (Value::Tuple(a), Value::Tuple(b)) => slice_compare(op, a, b, depth),
            (Value::Dict(a), Value::Dict(b)) => match op {
                Token::Eql => dicts_equal(a, b, depth),
                Token::Neq => Ok(!dicts_equal(a, b, depth)?),
                _ => Err(not_implemented(x, op, y)),
            },
            (Value::Set(a), Value::Set(b)) => set_compare(op, a, b, x, y),
            (Value::Other(a), _) => {
                if let Some(r) = a.compare_same_type(op, y, depth) {
                    return r;
                }
                if let Some(r) = a.cmp(y, depth) {
                    return Ok(threeway(op, ord_to_i32(r?)));
                }
                identity_compare(op, x, y)
            }
            _ => identity_compare(op, x, y),
        };
    }

    // different types

    // int/float ordered comparisons
    match (x, y) {
        (Value::Int(a), Value::Float(f)) => {
            let cmp = if f.is_nan() {
                -1 // y is NaN
            } else if !f.is_infinite() {
                cmp_int_float(a, *f) // y is finite
            } else if *f > 0.0 {
                -1 // y is +Inf
            } else {
                1 // y is -Inf
            };
            return Ok(threeway(op, cmp));
        }
        (Value::Float(f), Value::Int(b)) => {
            let cmp = if f.is_nan() {
                1 // x is NaN
            } else if !f.is_infinite() {
                -cmp_int_float(b, *f) // x is finite
            } else if *f > 0.0 {
                1 // x is +Inf
            } else {
                -1 // x is -Inf
            };
            return Ok(threeway(op, cmp));
        }
        _ => {}
    }

    // All other values of different types compare unequal.
    match op {
        Token::Eql => Ok(false),
        Token::Neq => Ok(true),
        _ => Err(not_implemented(x, op, y)),
    }
}

fn identity_compare(op: Token, x: &Value, y: &Value) -> Result<bool, Error> {
    match op {
        Token::Eql => Ok(x.is_identical(y)),
        Token::Neq => Ok(!x.is_identical(y)),
        _ => Err(not_implemented(x, op, y)),
    }
}

fn ord_to_i32(o: Ordering) -> i32 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

fn slice_compare(op: Token, x: &[Value], y: &[Value], depth: i32) -> Result<bool, Error> {
    // Fast path: check length.
    if x.len() != y.len() && (op == Token::Eql || op == Token::Neq) {
        return Ok(op == Token::Neq);
    }

    // Find first element that is not equal in both lists.
    for (xi, yi) in x.iter().zip(y.iter()) {
        if !equal_depth(xi, yi, depth - 1)? {
            return match op {
                Token::Eql => Ok(false),
                Token::Neq => Ok(true),
                _ => compare_depth(op, xi, yi, depth - 1),
            };
        }
    }

    Ok(threeway(op, ord_to_i32(x.len().cmp(&y.len()))))
}

fn dicts_equal(x: &Dict, y: &Dict, depth: i32) -> Result<bool, Error> {
    if x.len() != y.len() {
        return Ok(false);
    }
    for (key, xval) in x.items() {
        match y.get(&key) {
            Ok(Some(yval)) => {
                if !equal_depth(&xval, &yval, depth - 1)? {
                    return Ok(false);
                }
            }
            Ok(None) => return Ok(false),
            // Every key of x is hashable, so a failed lookup in y can only
            // mean the key is absent; the original treats it the same way.
            Err(_) => return Ok(false),
        }
    }
    Ok(true)
}

fn sets_equal(x: &Set, y: &Set) -> bool {
    if x.len() != y.len() {
        return false;
    }
    for k in x.elems() {
        match y.has(&k) {
            Ok(true) => {}
            // Elements of x are hashable, so an error from y.has can only
            // mean the element is absent; the original treats it the same way.
            Ok(false) | Err(_) => return false,
        }
    }
    true
}

fn set_compare(op: Token, x: &Set, y: &Set, xv: &Value, yv: &Value) -> Result<bool, Error> {
    let iter = || KeyIterator::start(yv.clone());
    match op {
        Token::Eql => Ok(sets_equal(x, y)),
        Token::Neq => Ok(!sets_equal(x, y)),
        Token::Ge => {
            // superset
            if x.len() < y.len() {
                return Ok(false);
            }
            x.is_superset(&mut iter())
        }
        Token::Le => {
            // subset
            if x.len() > y.len() {
                return Ok(false);
            }
            x.is_subset(&mut iter())
        }
        Token::Gt => {
            // proper superset
            if x.len() <= y.len() {
                return Ok(false);
            }
            x.is_superset(&mut iter())
        }
        Token::Lt => {
            // proper subset
            if x.len() >= y.len() {
                return Ok(false);
            }
            x.is_subset(&mut iter())
        }
        _ => Err(not_implemented(xv, op, yv)),
    }
}

/// Interprets a three-way comparison value cmp (-1, 0, +1) as a boolean
/// comparison (e.g. x < y).
pub fn threeway(op: Token, cmp: i32) -> bool {
    match op {
        Token::Eql => cmp == 0,
        Token::Neq => cmp != 0,
        Token::Le => cmp <= 0,
        Token::Lt => cmp < 0,
        Token::Ge => cmp >= 0,
        Token::Gt => cmp > 0,
        _ => unreachable!("threeway: {}", op),
    }
}

/// Returns the length of a string or sequence value, and None for all
/// others. A string has a known length but is not directly iterable.
pub fn len(x: &Value) -> Option<usize> {
    match x {
        Value::String(s) | Value::Bytes(s) => Some(s.len()),
        Value::List(l) => Some(l.len()),
        Value::Tuple(t) => Some(t.len()),
        Value::Dict(d) => Some(d.len()),
        Value::Set(s) => Some(s.len()),
        Value::Other(o) => o.len(),
        _ => None,
    }
}

/// Returns a new iterator for the value if iterable, None otherwise.
pub fn iterate(x: &Value) -> Option<Box<dyn ValueIterator>> {
    match x {
        Value::List(l) => {
            if !l.frozen.get() {
                l.itercount.set(l.itercount.get() + 1);
            }
            Some(Box::new(ListIterator { l: l.clone(), i: 0 }))
        }
        Value::Tuple(t) => Some(Box::new(TupleIterator { t: t.clone(), i: 0 })),
        Value::Dict(_) | Value::Set(_) => Some(Box::new(KeyIterator::start(x.clone()))),
        Value::Other(o) => o.iterate(),
        _ => None,
    }
}

/// Iterates over all elements of an iterable value, calling f for each.
pub fn for_each(x: &Value, mut f: impl FnMut(Value) -> Result<(), Error>) -> Result<bool, Error> {
    let Some(mut it) = iterate(x) else {
        return Ok(false);
    };
    while let Some(v) = it.next() {
        f(v)?;
    }
    Ok(true)
}

/// Returns the elements of an iterable value, or None if not iterable.
pub fn collect(x: &Value) -> Option<Vec<Value>> {
    let mut it = iterate(x)?;
    let mut out = Vec::with_capacity(len(x).unwrap_or(0));
    while let Some(v) = it.next() {
        out.push(v);
    }
    Some(out)
}

/// Returns the Go `signum` of x.
pub(crate) fn signum(x: i64) -> i64 {
    x.signum()
}

/// Slices a byte string with the given indices and step (Go's
/// String.Slice / Bytes.Slice).
pub(crate) fn slice_bytes(s: &[u8], start: i64, end: i64, step: i64) -> Vec<u8> {
    if step == 1 {
        return s[start as usize..end as usize].to_vec();
    }
    let sign = signum(step);
    let mut out = Vec::new();
    let mut i = start;
    while signum(end - i) == sign {
        out.push(s[i as usize]);
        i += step;
    }
    out
}

/// Slices a sequence of values with the given indices and step.
pub(crate) fn slice_values(s: &[Value], start: i64, end: i64, step: i64) -> Vec<Value> {
    if step == 1 {
        return s[start as usize..end as usize].to_vec();
    }
    let sign = signum(step);
    let mut out = Vec::new();
    let mut i = start;
    while signum(end - i) == sign {
        out.push(s[i as usize].clone());
        i += step;
    }
    out
}
