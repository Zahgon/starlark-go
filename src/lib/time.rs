// Copyright 2021 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Package time provides time-related constants and functions.
//!
//! The module defines the following functions:
//!
//! ```text
//! from_timestamp(sec, nsec) - Converts the given Unix time corresponding to the number of seconds
//!                             and (optionally) nanoseconds since January 1, 1970 UTC into an object
//!                             of type Time. For more details, refer to https://pkg.go.dev/time#Unix.
//!
//! is_valid_timezone(loc) - Reports whether loc is a valid time zone name.
//!
//! now() - Returns the current local time.
//!
//! parse_duration(d) - Parses the given duration string. For more details, refer to
//!                     https://pkg.go.dev/time#ParseDuration.
//!
//! parse_time(x, format, location) - Parses the given time string using a specific time format and location.
//!                                  The expected arguments are a time string (mandatory), a time format
//!                                  (optional, set to RFC3339 by default, e.g. "2021-03-22T23:20:50.52Z")
//!                                  and a name of location (optional, set to UTC by default). For more details,
//!                                  refer to https://pkg.go.dev/time#Parse and https://pkg.go.dev/time#ParseInLocation.
//!
//! time(year, month, day, hour, minute, second, nanosecond, location) - Returns the Time corresponding to
//!                                                                      yyyy-mm-dd hh:mm:ss + nsec nanoseconds
//!                                                                      in the appropriate zone for that time
//!                                                                      in the given location. All the parameters
//!                                                                      are optional.
//! ```
//!
//! The module also defines the following constants:
//!
//! ```text
//! nanosecond - A duration representing one nanosecond.
//! microsecond - A duration representing one microsecond.
//! millisecond - A duration representing one millisecond.
//! second - A duration representing one second.
//! minute - A duration representing one minute.
//! hour - A duration representing one hour.
//! ```

#[path = "gotime/mod.rs"]
mod gotime;

use std::any::Any;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use crate::starlark::{
    fmt::go_quote, unpack_args, unpack_positional_args, Builtin, Error, Side, StarlarkValue,
    StringDict, Thread, Unpacker, Value,
};
use crate::starlarkstruct::Module;
use crate::syntax::Token;

/// Returns the time module.
pub fn module() -> Value {
    let mut members = StringDict::new();
    members.insert(
        "from_timestamp",
        Builtin::value("from_timestamp", from_timestamp),
    );
    members.insert(
        "is_valid_timezone",
        Builtin::value("is_valid_timezone", is_valid_timezone),
    );
    members.insert("now", Builtin::value("now", now));
    members.insert(
        "parse_duration",
        Builtin::value("parse_duration", parse_duration),
    );
    members.insert("parse_time", Builtin::value("parse_time", parse_time));
    members.insert("time", Builtin::value("time", new_time));

    members.insert("nanosecond", Value::other(Duration(1)));
    members.insert("microsecond", Value::other(Duration(1_000)));
    members.insert("millisecond", Value::other(Duration(1_000_000)));
    members.insert("second", Value::other(Duration(1_000_000_000)));
    members.insert("minute", Value::other(Duration(60_000_000_000)));
    members.insert("hour", Value::other(Duration(3_600_000_000_000)));
    Module::value("time", members)
}

fn parse_duration(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut d = Duration(0);
    unpack_positional_args("parse_duration", args, kwargs, 1, &mut [&mut d])?;
    Ok(Value::other(d))
}

fn is_valid_timezone(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut s: Vec<u8> = Vec::new();
    unpack_positional_args("is_valid_timezone", args, kwargs, 1, &mut [&mut s])?;
    Ok(Value::Bool(gotime::load_location(&s).is_ok()))
}

fn parse_time(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut x: Vec<u8> = Vec::new();
    let mut location: Vec<u8> = b"UTC".to_vec();
    let mut format: Vec<u8> = gotime::RFC3339.to_vec();
    unpack_args(
        "parse_time",
        args,
        kwargs,
        &mut [
            ("x", &mut x),
            ("format?", &mut format),
            ("location?", &mut location),
        ],
    )?;

    if location == b"UTC" {
        let t = gotime::parse(&format, &x).map_err(Error::new)?;
        return Ok(Value::other(Time(t)));
    }

    let loc = gotime::load_location(&location).map_err(|e| Error::new(e.0))?;
    let t = gotime::parse_in_location(&format, &x, loc).map_err(Error::new)?;
    Ok(Value::other(Time(t)))
}

fn from_timestamp(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut sec: i64 = 0;
    let mut nsec: i64 = 0;
    unpack_positional_args(
        "from_timestamp",
        args,
        kwargs,
        1,
        &mut [&mut sec, &mut nsec],
    )?;
    Ok(Value::other(Time(gotime::Time::unix(sec, nsec))))
}

/// A function that reports the current time (Go's NowFunc type).
pub type NowFunc = dyn Fn() -> Time;

/// A per-thread clock function that may fail (the type accepted by
/// Go's SetNow).
pub type NowErrFunc = dyn Fn() -> Result<Time, Error>;

thread_local! {
    // Go's NowFunc global variable. Starlark values are confined to one
    // OS thread, so the Rust port keeps one per OS thread.
    static NOW_FUNC: RefCell<Option<Rc<NowFunc>>> =
        RefCell::new(Some(Rc::new(|| Time(gotime::Time::now()))));
}

/// Returns the function that reports the current time (Go's NowFunc),
/// or None if it has been unset.
pub fn now_func() -> Option<Rc<NowFunc>> {
    NOW_FUNC.with(|f| f.borrow().clone())
}

/// Replaces the function that reports the current time (an assignment to
/// Go's NowFunc), for example by applications that require their Starlark
/// scripts to be fully deterministic. It returns the previous function.
///
/// Prefer `set_now`, which sets the clock function of a single thread.
pub fn set_now_func(f: Option<Rc<NowFunc>>) -> Option<Rc<NowFunc>> {
    NOW_FUNC.with(|g| std::mem::replace(&mut *g.borrow_mut(), f))
}

const CONTEXT_KEY: &str = "time.now";

/// Sets the thread's optional clock function.
/// If set, it will be used in preference to `now_func()` when the
/// thread requests the current time by executing a call to time.now.
pub fn set_now(thread: &mut Thread, now_func: Rc<NowErrFunc>) {
    thread.set_local(CONTEXT_KEY, Rc::new(now_func));
}

/// Returns the clock function previously associated with this thread
/// (Go's Now).
pub fn thread_now(thread: &Thread) -> Option<Rc<NowErrFunc>> {
    let local = thread.local(CONTEXT_KEY)?;
    local.downcast_ref::<Rc<NowErrFunc>>().cloned()
}

fn now(
    thread: &mut Thread,
    _: &Builtin,
    _args: &[Value],
    _kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    if let Some(now_err_func) = thread_now(thread) {
        return Ok(Value::other(now_err_func()?));
    }
    match now_func() {
        Some(now_func) => Ok(Value::other(now_func())),
        None => Err(Error::new("time.now() is not available")),
    }
}

/// Duration is a Starlark representation of a duration.
#[derive(Clone, Copy)]
pub struct Duration(pub i64);

/// Unpack is a custom argument unpacker.
impl Unpacker for Duration {
    fn unpack(&mut self, v: &Value) -> Result<(), Error> {
        if let Some(d) = v.downcast_ref::<Duration>() {
            *self = *d;
            return Ok(());
        }
        if let Value::String(s) = v {
            let d = gotime::parse_duration(s).map_err(Error::new)?;
            *self = Duration(d);
            return Ok(());
        }
        Err(Error::new(format!(
            "got {}, want a duration, string, or int",
            v.type_name()
        )))
    }
}

const DIVISION_BY_ZERO: &str = "time.duration division by zero";

impl StarlarkValue for Duration {
    fn type_name(&self) -> &str {
        "time.duration"
    }

    fn repr(&self, out: &mut String) {
        out.push_str(&gotime::duration_string(self.0));
    }

    fn hash(&self) -> Result<u32, Error> {
        Ok((self.0 as u32) ^ ((self.0 >> 32) as u32))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Reports whether the duration is non-zero.
    fn truth(&self) -> bool {
        self.0 != 0
    }

    fn has_attrs(&self) -> bool {
        true
    }

    fn attr(&self, _this: &Value, name: &str) -> Result<Option<Value>, Error> {
        let d = self.0;
        Ok(Some(match name {
            "hours" => Value::Float(gotime::duration_hours(d)),
            "minutes" => Value::Float(gotime::duration_minutes(d)),
            "seconds" => Value::Float(gotime::duration_seconds(d)),
            "milliseconds" => Value::int(d / 1_000_000),
            "microseconds" => Value::int(d / 1_000),
            "nanoseconds" => Value::int(d),
            _ => {
                return Err(Error::new(format!(
                    "unrecognized {} attribute {}",
                    self.type_name(),
                    go_quote(name.as_bytes())
                )))
            }
        }))
    }

    fn attr_names(&self) -> Vec<String> {
        [
            "hours",
            "minutes",
            "seconds",
            "milliseconds",
            "microseconds",
            "nanoseconds",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn cmp(&self, y: &Value, _depth: i32) -> Option<Result<Ordering, Error>> {
        let y = y.downcast_ref::<Duration>()?;
        Some(Ok(self.0.cmp(&y.0)))
    }

    /// Implements binary operators:
    ///
    /// ```text
    /// duration + duration = duration
    /// duration + time = time
    /// duration - duration = duration
    /// duration / duration = float
    /// duration / int = duration
    /// duration / float = duration
    /// duration // duration = int
    /// duration * int = duration
    /// ```
    fn binary(&self, op: Token, y: &Value, side: Side) -> Result<Option<Value>, Error> {
        let x = self.0;
        match op {
            Token::Plus => {
                if let Some(y) = y.downcast_ref::<Duration>() {
                    return Ok(Some(Value::other(Duration(x.wrapping_add(y.0)))));
                }
                if let Some(y) = y.downcast_ref::<Time>() {
                    return Ok(Some(Value::other(Time(y.0.add(x)))));
                }
            }
            Token::Minus => {
                if let Some(y) = y.downcast_ref::<Duration>() {
                    return Ok(Some(Value::other(Duration(x.wrapping_sub(y.0)))));
                }
            }
            Token::Slash => {
                if let Some(y) = y.downcast_ref::<Duration>() {
                    if y.0 == 0 {
                        return Err(Error::new(DIVISION_BY_ZERO));
                    }
                    return Ok(Some(Value::Float(x as f64 / y.0 as f64)));
                }
                match y {
                    Value::Int(i) => {
                        if side == Side::Right {
                            return Err(Error::new("unsupported operation"));
                        }
                        let i = i.to_i64().ok_or_else(|| {
                            Error::new("int value out of range (want signed 64-bit value)")
                        })?;
                        if i == 0 {
                            return Err(Error::new(DIVISION_BY_ZERO));
                        }
                        return Ok(Some(Value::other(Duration(x.wrapping_div(i)))));
                    }
                    Value::Float(f) => {
                        if *f == 0.0 {
                            return Err(Error::new(DIVISION_BY_ZERO));
                        }
                        return Ok(Some(Value::other(Duration((x as f64 / f) as i64))));
                    }
                    _ => {}
                }
            }
            Token::Slashslash => {
                if let Some(y) = y.downcast_ref::<Duration>() {
                    if y.0 == 0 {
                        return Err(Error::new(DIVISION_BY_ZERO));
                    }
                    return Ok(Some(Value::int(x.wrapping_div(y.0))));
                }
            }
            Token::Star => {
                if let Value::Int(i) = y {
                    let i = i.to_i64().ok_or_else(|| {
                        Error::new("int value out of range (want signed 64-bit value)")
                    })?;
                    return Ok(Some(Value::other(Duration(x.wrapping_mul(i)))));
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

/// Time is a Starlark representation of a moment in time.
#[derive(Clone)]
pub struct Time(gotime::Time);

fn new_time(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let (mut year, mut month, mut day, mut hour, mut min, mut sec, mut nsec) =
        (0i64, 0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    let mut loc: Vec<u8> = Vec::new();
    unpack_args(
        "time",
        args,
        kwargs,
        &mut [
            ("year?", &mut year),
            ("month?", &mut month),
            ("day?", &mut day),
            ("hour?", &mut hour),
            ("minute?", &mut min),
            ("second?", &mut sec),
            ("nanosecond?", &mut nsec),
            ("location?", &mut loc),
        ],
    )?;
    if !args.is_empty() {
        return Err(Error::new("time: unexpected positional arguments"));
    }
    let location = gotime::load_location(&loc).map_err(|e| Error::new(e.0))?;
    Ok(Value::other(Time(gotime::Time::date(
        year, month, day, hour, min, sec, nsec, location,
    ))))
}

/// The layout used by Time's String method.
const STRING_LAYOUT: &[u8] = b"2006-01-02 15:04:05.999999999 -0700 MST";

impl StarlarkValue for Time {
    fn type_name(&self) -> &str {
        "time.time"
    }

    /// Returns the time formatted using the format string
    /// "2006-01-02 15:04:05.999999999 -0700 MST".
    fn repr(&self, out: &mut String) {
        out.push_str(&String::from_utf8_lossy(&self.0.format(STRING_LAYOUT)));
    }

    fn hash(&self) -> Result<u32, Error> {
        let n = self.0.unix_nano();
        Ok((n as u32) ^ ((n >> 32) as u32))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Returns the truth value of the object: false only for the zero time.
    fn truth(&self) -> bool {
        !self.0.is_zero()
    }

    fn has_attrs(&self) -> bool {
        true
    }

    fn attr(&self, this: &Value, name: &str) -> Result<Option<Value>, Error> {
        let t = &self.0;
        Ok(Some(match name {
            "year" => Value::int(t.year()),
            "month" => Value::int(t.month()),
            "day" => Value::int(t.day()),
            "hour" => Value::int(t.hour()),
            "minute" => Value::int(t.minute()),
            "second" => Value::int(t.second()),
            "nanosecond" => Value::int(t.nanosecond()),
            "unix" => Value::int(t.unix_seconds()),
            "unix_nano" => Value::int(t.unix_nano()),
            "in_location" => method("in_location", time_in, this),
            "format" => method("format", time_format, this),
            _ => return Ok(None), // no such method
        }))
    }

    fn attr_names(&self) -> Vec<String> {
        [
            "format",
            "in_location",
            "year",
            "month",
            "day",
            "hour",
            "minute",
            "second",
            "nanosecond",
            "unix",
            "unix_nano",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn cmp(&self, y: &Value, _depth: i32) -> Option<Result<Ordering, Error>> {
        let (x, y) = (&self.0, &y.downcast_ref::<Time>()?.0);
        Some(Ok(if x.before(y) {
            Ordering::Less
        } else if x.after(y) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }))
    }

    /// Implements binary operators:
    ///
    /// ```text
    /// time + duration = time
    /// time - duration = time
    /// time - time = duration
    /// ```
    fn binary(&self, op: Token, y: &Value, _side: Side) -> Result<Option<Value>, Error> {
        let x = &self.0;
        match op {
            Token::Plus => {
                if let Some(y) = y.downcast_ref::<Duration>() {
                    return Ok(Some(Value::other(Time(x.add(y.0)))));
                }
            }
            Token::Minus => {
                if let Some(y) = y.downcast_ref::<Duration>() {
                    return Ok(Some(Value::other(Time(x.add(y.0.wrapping_neg())))));
                }
                if let Some(y) = y.downcast_ref::<Time>() {
                    // time - time = duration
                    return Ok(Some(Value::other(Duration(x.sub(&y.0)))));
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

/// The signature of a method of Time: (receiver, args, kwargs).
type TimeMethod = fn(&gotime::Time, &[Value], &[(Value, Value)]) -> Result<Value, Error>;

/// Returns the named method bound to the receiver.
fn method(name: &'static str, f: TimeMethod, recv: &Value) -> Value {
    let b = Builtin::new(
        name,
        move |_: &mut Thread, b: &Builtin, args: &[Value], kwargs: &[(Value, Value)]| match b
            .receiver()
            .and_then(|r| r.downcast_ref::<Time>())
        {
            Some(t) => f(&t.0, args, kwargs),
            None => Err(Error::new(format!("{}: invalid receiver", name))),
        },
    );
    Value::Builtin(Rc::new(b.bind_receiver(recv.clone())))
}

fn time_format(
    recv: &gotime::Time,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut x: Vec<u8> = Vec::new();
    unpack_positional_args("format", args, kwargs, 1, &mut [&mut x])?;
    Ok(Value::string(recv.format(&x)))
}

fn time_in(recv: &gotime::Time, args: &[Value], kwargs: &[(Value, Value)]) -> Result<Value, Error> {
    let mut x: Vec<u8> = Vec::new();
    unpack_positional_args("in_location", args, kwargs, 1, &mut [&mut x])?;
    let loc = gotime::load_location(&x).map_err(|e| Error::new(e.0))?;
    Ok(Value::other(Time(recv.in_location(loc))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::starlark::{call, get_attr};

    fn now_builtin() -> Value {
        get_attr(&module(), "now").expect("time.now")
    }

    fn date() -> Time {
        Time(gotime::Time::date(1, 2, 3, 4, 5, 6, 7, None))
    }

    #[test]
    fn test_per_thread_now_returns_correct_time() {
        let mut th = Thread::new("");
        set_now(&mut th, Rc::new(|| Ok(date())));

        let res = call(&mut th, &now_builtin(), &[], &[]).unwrap_or_else(|err| panic!("{}", err));

        let ret_time = res.downcast_ref::<Time>().expect("Time");
        assert!(
            ret_time.0.equal(&date().0),
            "Expected time to be equal {} {}",
            res,
            Value::other(date())
        );
    }

    #[test]
    fn test_per_thread_now_returns_error() {
        let mut th = Thread::new("");
        set_now(&mut th, Rc::new(|| Err(Error::new("no time"))));

        let err = call(&mut th, &now_builtin(), &[], &[]).err();
        // The error returned by the clock function is the cause of the
        // evaluation error (Go: errors.Is(err, e)).
        let cause = err
            .as_ref()
            .and_then(|e| e.as_eval())
            .and_then(|e| e.cause.as_ref());
        assert!(
            matches!(cause, Some(Error::Msg(m)) if m == "no time"),
            "Expected equal error no time {:?}",
            err.map(|e| e.to_string())
        );
    }

    #[test]
    fn test_global_now_returns_correct_time() {
        let th = &mut Thread::new("");

        let old_now = set_now_func(Some(Rc::new(date)));

        let res = call(th, &now_builtin(), &[], &[]);
        set_now_func(old_now);
        let res = res.unwrap_or_else(|err| panic!("{}", err));

        let ret_time = res.downcast_ref::<Time>().expect("Time");
        assert!(
            ret_time.0.equal(&date().0),
            "Expected time to be equal {} {}",
            res,
            Value::other(date())
        );
    }

    #[test]
    fn test_global_now_returns_error_when_nil() {
        let th = &mut Thread::new("");

        let old_now = set_now_func(None);

        let res = call(th, &now_builtin(), &[], &[]);
        set_now_func(old_now);
        assert!(res.is_err(), "Expected to get an error");
    }
}
