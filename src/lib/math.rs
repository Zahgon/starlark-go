// Copyright 2021 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Package math provides basic constants and mathematical functions
//! (ported from go.starlark.net/lib/math).
//!
//! The module defines the following functions:
//!
//! ```text
//! ceil(x) - Returns the ceiling of x, the smallest integer greater than or equal to x.
//! copysign(x, y) - Returns a value with the magnitude of x and the sign of y.
//! fabs(x) - Returns the absolute value of x as float.
//! floor(x) - Returns the floor of x, the largest integer less than or equal to x.
//! mod(x, y) - Returns the floating-point remainder of x/y. The magnitude of the result is less than y and its sign agrees with that of x.
//! pow(x, y) - Returns x**y, the base-x exponential of y.
//! remainder(x, y) - Returns the IEEE 754 floating-point remainder of x/y.
//! round(x) - Returns the nearest integer, rounding half away from zero.
//!
//! exp(x) - Returns e raised to the power x, where e = 2.718281… is the base of natural logarithms.
//! sqrt(x) - Returns the square root of x.
//!
//! acos(x) - Returns the arc cosine of x, in radians.
//! asin(x) - Returns the arc sine of x, in radians.
//! atan(x) - Returns the arc tangent of x, in radians.
//! atan2(y, x) - Returns atan(y / x), in radians.
//!               The result is between -pi and pi.
//!               The vector in the plane from the origin to point (x, y) makes this angle with the positive X axis.
//!               The point of atan2() is that the signs of both inputs are known to it, so it can compute the correct
//!               quadrant for the angle.
//!               For example, atan(1) and atan2(1, 1) are both pi/4, but atan2(-1, -1) is -3*pi/4.
//! cos(x) - Returns the cosine of x, in radians.
//! hypot(x, y) - Returns the Euclidean norm, sqrt(x*x + y*y). This is the length of the vector from the origin to point (x, y).
//! sin(x) - Returns the sine of x, in radians.
//! tan(x) - Returns the tangent of x, in radians.
//!
//! degrees(x) - Converts angle x from radians to degrees.
//! radians(x) - Converts angle x from degrees to radians.
//!
//! acosh(x) - Returns the inverse hyperbolic cosine of x.
//! asinh(x) - Returns the inverse hyperbolic sine of x.
//! atanh(x) - Returns the inverse hyperbolic tangent of x.
//! cosh(x) - Returns the hyperbolic cosine of x.
//! sinh(x) - Returns the hyperbolic sine of x.
//! tanh(x) - Returns the hyperbolic tangent of x.
//!
//! log(x, base) - Returns the logarithm of x in the given base, or natural logarithm by default.
//!
//! gamma(x) - Returns the Gamma function of x.
//! ```
//!
//! All functions accept both int and float values as arguments.
//!
//! The module also defines approximations of the following constants:
//!
//! ```text
//! e - The base of natural logarithms, approximately 2.71828.
//! pi - The ratio of a circle's circumference to its diameter, approximately 3.14159.
//! ```
//!
//! The functions reproduce the results of Go's math package (on
//! linux/arm64) bit for bit; see the private `gomath` module.

#[path = "gomath/mod.rs"]
mod gomath;

use crate::starlark::{
    number_to_int, unpack_positional_args, Builtin, Error, StringDict, Thread, Unpacker, Value,
};
use crate::starlarkstruct::Module;

/// Returns the math module.
pub fn module() -> Value {
    let mut members = StringDict::new();
    members.insert("ceil", Builtin::value("ceil", ceil));
    members.insert("copysign", new_binary_builtin("copysign", gomath::copysign));
    members.insert("fabs", new_unary_builtin("fabs", gomath::abs));
    members.insert("floor", Builtin::value("floor", floor));
    members.insert("mod", new_binary_builtin("mod", gomath::fmod));
    members.insert("pow", new_binary_builtin("pow", gomath::pow));
    members.insert(
        "remainder",
        new_binary_builtin("remainder", gomath::remainder),
    );
    members.insert("round", new_unary_builtin("round", gomath::round));

    members.insert("exp", new_unary_builtin("exp", gomath::exp));
    members.insert("sqrt", new_unary_builtin("sqrt", gomath::sqrt));

    members.insert("acos", new_unary_builtin("acos", gomath::acos));
    members.insert("asin", new_unary_builtin("asin", gomath::asin));
    members.insert("atan", new_unary_builtin("atan", gomath::atan));
    members.insert("atan2", new_binary_builtin("atan2", gomath::atan2));
    members.insert("cos", new_unary_builtin("cos", gomath::cos));
    members.insert("hypot", new_binary_builtin("hypot", gomath::hypot));
    members.insert("sin", new_unary_builtin("sin", gomath::sin));
    members.insert("tan", new_unary_builtin("tan", gomath::tan));

    members.insert("degrees", new_unary_builtin("degrees", degrees));
    members.insert("radians", new_unary_builtin("radians", radians));

    members.insert("acosh", new_unary_builtin("acosh", gomath::acosh));
    members.insert("asinh", new_unary_builtin("asinh", gomath::asinh));
    members.insert("atanh", new_unary_builtin("atanh", gomath::atanh));
    members.insert("cosh", new_unary_builtin("cosh", gomath::cosh));
    members.insert("sinh", new_unary_builtin("sinh", gomath::sinh));
    members.insert("tanh", new_unary_builtin("tanh", gomath::tanh));

    members.insert("log", Builtin::value("log", log));

    members.insert("gamma", new_unary_builtin("gamma", gomath::gamma));

    members.insert("e", Value::Float(gomath::E));
    members.insert("pi", Value::Float(gomath::PI));
    Module::value("math", members)
}

/// An Unpacker that converts a Starlark int or float to f64.
struct FloatOrInt(f64);

impl Unpacker for FloatOrInt {
    fn unpack(&mut self, v: &Value) -> Result<(), Error> {
        match v {
            Value::Int(i) => {
                self.0 = i.to_f64();
                Ok(())
            }
            Value::Float(f) => {
                self.0 = *f;
                Ok(())
            }
            _ => Err(Error::new(format!(
                "got {}, want float or int",
                v.type_name()
            ))),
        }
    }
}

/// Wraps a unary floating-point function as a Starlark built-in that
/// accepts int or float arguments.
fn new_unary_builtin(name: &'static str, f: fn(f64) -> f64) -> Value {
    Builtin::value(
        name,
        move |_: &mut Thread, _: &Builtin, args: &[Value], kwargs: &[(Value, Value)]| {
            let mut x = FloatOrInt(0.0);
            unpack_positional_args(name, args, kwargs, 1, &mut [&mut x])?;
            Ok(Value::Float(f(x.0)))
        },
    )
}

/// Wraps a binary floating-point function as a Starlark built-in that
/// accepts int or float arguments.
fn new_binary_builtin(name: &'static str, f: fn(f64, f64) -> f64) -> Value {
    Builtin::value(
        name,
        move |_: &mut Thread, _: &Builtin, args: &[Value], kwargs: &[(Value, Value)]| {
            let mut x = FloatOrInt(0.0);
            let mut y = FloatOrInt(0.0);
            unpack_positional_args(name, args, kwargs, 2, &mut [&mut x, &mut y])?;
            Ok(Value::Float(f(x.0, y.0)))
        },
    )
}

/// Wraps the log function as a Starlark built-in that accepts int or
/// float arguments.
fn log(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut x = FloatOrInt(0.0);
    let mut base = FloatOrInt(gomath::E);
    unpack_positional_args("log", args, kwargs, 1, &mut [&mut x, &mut base])?;
    if base.0 == 1.0 {
        return Err(Error::new("division by zero"));
    }
    Ok(Value::Float(gomath::log(x.0) / gomath::log(base.0)))
}

fn ceil(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut x = Value::None;
    unpack_positional_args("ceil", args, kwargs, 1, &mut [&mut x])?;
    match &x {
        Value::Int(_) => Ok(x),
        Value::Float(f) => Ok(Value::Int(number_to_int(&Value::Float(gomath::ceil(*f)))?)),
        _ => Err(Error::new(format!(
            "got {}, want float or int",
            x.type_name()
        ))),
    }
}

fn floor(
    _: &mut Thread,
    _: &Builtin,
    args: &[Value],
    kwargs: &[(Value, Value)],
) -> Result<Value, Error> {
    let mut x = Value::None;
    unpack_positional_args("floor", args, kwargs, 1, &mut [&mut x])?;
    match &x {
        Value::Int(_) => Ok(x),
        Value::Float(f) => Ok(Value::Int(number_to_int(&Value::Float(gomath::floor(*f)))?)),
        _ => Err(Error::new(format!(
            "got {}, want float or int",
            x.type_name()
        ))),
    }
}

fn degrees(x: f64) -> f64 {
    360.0 * x / (2.0 * gomath::PI)
}

fn radians(x: f64) -> f64 {
    2.0 * gomath::PI * x / 360.0
}
