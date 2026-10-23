// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! The Starlark int type (ported from int.go, int_generic.go).

use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{ToPrimitive, Zero};

use super::value::{Error, Value};
use crate::syntax::Token;

/// Int is the type of a Starlark int.
///
/// Values in the signed 32-bit range are held inline (so that small+small
/// and small*small never overflow an i64); all others are big.
#[derive(Clone, Debug)]
pub enum Int {
    /// Invariant: i32::MIN <= value <= i32::MAX.
    Small(i64),
    /// Invariant: the value is not representable as an i32.
    Big(Rc<BigInt>),
}

impl Int {
    /// Returns a Starlark int for the specified signed integer.
    pub fn from_i64(x: i64) -> Int {
        if i32::MIN as i64 <= x && x <= i32::MAX as i64 {
            return Int::Small(x);
        }
        Int::Big(Rc::new(BigInt::from(x)))
    }

    /// Returns a Starlark int for the specified unsigned integer.
    pub fn from_u64(x: u64) -> Int {
        if x <= i32::MAX as u64 {
            return Int::Small(x as i64);
        }
        Int::Big(Rc::new(BigInt::from(x)))
    }

    /// Returns a Starlark int for the specified big integer.
    pub fn from_bigint(x: BigInt) -> Int {
        if let Some(v) = x.to_i32() {
            return Int::Small(v as i64);
        }
        Int::Big(Rc::new(x))
    }

    /// The int 0.
    pub fn zero() -> Int {
        Int::Small(0)
    }

    /// Returns the value as an i64, if it is exactly representable.
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Int::Small(x) => Some(*x),
            Int::Big(b) => b.to_i64(),
        }
    }

    /// Returns the value as a u64, if it is exactly representable.
    pub fn to_u64(&self) -> Option<u64> {
        match self {
            Int::Small(x) => {
                if *x < 0 {
                    None
                } else {
                    Some(*x as u64)
                }
            }
            Int::Big(b) => b.to_u64(),
        }
    }

    /// Returns a new BigInt with the same value as the Int.
    pub fn to_bigint(&self) -> BigInt {
        match self {
            Int::Small(x) => BigInt::from(*x),
            Int::Big(b) => (**b).clone(),
        }
    }

    /// Returns the value as an i32, if it is small (Go's AsInt32 core).
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Int::Small(x) => Some(*x as i32),
            Int::Big(_) => None,
        }
    }

    /// Returns -1, 0, or +1 according to the sign of the value.
    pub fn sign(&self) -> i32 {
        match self {
            Int::Small(x) => x.signum() as i32,
            Int::Big(b) => match b.sign() {
                Sign::Minus => -1,
                Sign::NoSign => 0,
                Sign::Plus => 1,
            },
        }
    }

    /// Returns the hash of the int, consistent with Float's hash.
    pub fn hash(&self) -> u32 {
        let lo: u64 = match self {
            Int::Small(x) => *x as u64,
            Int::Big(b) => b.magnitude().iter_u64_digits().next().unwrap_or(0),
        };
        12582917u32.wrapping_mul(lo.wrapping_add(3) as u32)
    }

    /// Returns the float value nearest the int (possibly infinite).
    pub fn to_f64(&self) -> f64 {
        match self {
            Int::Small(x) => *x as f64,
            Int::Big(b) => bigint_to_f64(b),
        }
    }

    /// Returns the finite float value nearest the int, or an error if the
    /// magnitude is too large.
    pub fn finite_float(&self) -> Result<f64, Error> {
        let f = self.to_f64();
        if f.is_infinite() {
            return Err(Error::new("int too large to convert to float"));
        }
        Ok(f)
    }

    fn binop(
        &self,
        y: &Int,
        small: impl Fn(i64, i64) -> i64,
        big: impl Fn(&BigInt, &BigInt) -> BigInt,
    ) -> Int {
        match (self, y) {
            (Int::Small(a), Int::Small(b)) => Int::from_i64(small(*a, *b)),
            _ => Int::from_bigint(big(&self.to_bigint(), &y.to_bigint())),
        }
    }

    pub fn add(&self, y: &Int) -> Int {
        self.binop(y, |a, b| a + b, |a, b| a + b)
    }

    pub fn sub(&self, y: &Int) -> Int {
        self.binop(y, |a, b| a - b, |a, b| a - b)
    }

    pub fn mul(&self, y: &Int) -> Int {
        self.binop(y, |a, b| a * b, |a, b| a * b)
    }

    pub fn or(&self, y: &Int) -> Int {
        self.binop(y, |a, b| a | b, |a, b| a | b)
    }

    pub fn and(&self, y: &Int) -> Int {
        self.binop(y, |a, b| a & b, |a, b| a & b)
    }

    pub fn xor(&self, y: &Int) -> Int {
        self.binop(y, |a, b| a ^ b, |a, b| a ^ b)
    }

    pub fn not(&self) -> Int {
        match self {
            Int::Small(x) => Int::Small(!*x),
            Int::Big(b) => Int::from_bigint(!(**b).clone()),
        }
    }

    pub fn neg(&self) -> Int {
        Int::zero().sub(self)
    }

    pub fn lsh(&self, y: u32) -> Int {
        Int::from_bigint(self.to_bigint() << y)
    }

    /// Arithmetic right shift (rounds towards negative infinity, like Go).
    pub fn rsh(&self, y: u32) -> Int {
        Int::from_bigint(self.to_bigint() >> y)
    }

    /// Floored division. Precondition: y is nonzero.
    pub fn div(&self, y: &Int) -> Int {
        match (self, y) {
            (Int::Small(x), Int::Small(y)) => {
                let mut quo = x / y;
                let rem = x % y;
                if (*x < 0) != (*y < 0) && rem != 0 {
                    quo -= 1;
                }
                Int::from_i64(quo)
            }
            _ => {
                let (xb, yb) = (self.to_bigint(), y.to_bigint());
                Int::from_bigint(xb.div_floor(&yb))
            }
        }
    }

    /// Floored modulo. Precondition: y is nonzero.
    pub fn modulo(&self, y: &Int) -> Int {
        match (self, y) {
            (Int::Small(x), Int::Small(y)) => {
                let mut rem = x % y;
                if (*x < 0) != (*y < 0) && rem != 0 {
                    rem += y;
                }
                Int::Small(rem)
            }
            _ => {
                let (xb, yb) = (self.to_bigint(), y.to_bigint());
                Int::from_bigint(xb.mod_floor(&yb))
            }
        }
    }

    /// Implements the unary operations +int, -int, and ~int.
    pub fn unary(&self, op: Token) -> Option<Value> {
        match op {
            Token::Minus => Some(Value::Int(self.neg())),
            Token::Plus => Some(Value::Int(self.clone())),
            Token::Tilde => Some(Value::Int(self.not())),
            _ => None,
        }
    }

    /// Formats the int in the given radix (lower-case digits), with a
    /// leading '-' for negative values, like Go's big.Int.Text.
    pub fn text(&self, radix: u32) -> String {
        match self {
            Int::Small(x) => {
                if radix == 10 {
                    x.to_string()
                } else {
                    BigInt::from(*x).to_str_radix(radix)
                }
            }
            Int::Big(b) => b.to_str_radix(radix),
        }
    }
}

impl fmt::Display for Int {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text(10))
    }
}

impl PartialEq for Int {
    fn eq(&self, other: &Int) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Int {}

impl PartialOrd for Int {
    fn partial_cmp(&self, other: &Int) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Int {
    /// Compares two ints.
    fn cmp(&self, y: &Int) -> Ordering {
        match (self, y) {
            (Int::Small(a), Int::Small(b)) => a.cmp(b),
            _ => self.to_bigint().cmp(&y.to_bigint()),
        }
    }
}

/// Converts a big integer to the nearest f64 (round half to even), or an
/// infinity if it is too large.
pub fn bigint_to_f64(b: &BigInt) -> f64 {
    let neg = b.sign() == Sign::Minus;
    let mag = b.magnitude();
    let bits = mag.bits();
    let f = if bits <= 64 {
        mag.to_u64().unwrap_or(0) as f64
    } else if bits > 1024 {
        f64::INFINITY
    } else {
        // Keep the top 64 bits, with a sticky bit for the remainder,
        // so that the u64->f64 conversion rounds correctly.
        let shift = bits - 64;
        let top = (mag >> shift).to_u64().unwrap_or(0);
        let rest_nonzero = mag.trailing_zeros().is_some_and(|tz| tz < shift);
        let top = if rest_nonzero { top | 1 } else { top };
        let f = top as f64;
        // f * 2^shift; shift <= 960 so powi is exact.
        let r = f * 2f64.powi(shift as i32);
        if r.is_infinite() {
            f64::INFINITY
        } else {
            r
        }
    };
    if neg {
        -f
    } else {
        f
    }
}

/// Converts a finite float to an Int, truncating towards zero.
pub fn finite_float_to_int(f: f64) -> Int {
    // We avoid '<= MaxInt64' so that both constants are exactly representable as floats.
    if (-9223372036854775808.0..9223372036854775808.0).contains(&f) {
        return Int::from_i64(f as i64);
    }
    Int::from_bigint(float_to_bigint(f))
}

/// Returns the exact integer value of a finite float whose magnitude is
/// at least 2^53 (so it is integral), or truncates otherwise.
fn float_to_bigint(f: f64) -> BigInt {
    let f = f.trunc();
    let bits = f.to_bits();
    let neg = bits >> 63 != 0;
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    if exp == 0 {
        return BigInt::zero();
    }
    let mant = frac | (1u64 << 52);
    let e = exp - 1075;
    let mut r = BigInt::from(mant);
    if e >= 0 {
        r <<= e as u32;
    } else {
        r >>= (-e) as u32;
    }
    if neg {
        -r
    } else {
        r
    }
}

/// Converts a number to an integer value: an int is returned unchanged, a
/// float is truncated towards zero. Reports an error for all other values.
pub fn number_to_int(x: &Value) -> Result<Int, Error> {
    match x {
        Value::Int(i) => Ok(i.clone()),
        Value::Float(f) => {
            if f.is_infinite() {
                Err(Error::new("cannot convert float infinity to integer"))
            } else if f.is_nan() {
                Err(Error::new("cannot convert float NaN to integer"))
            } else {
                Ok(finite_float_to_int(*f))
            }
        }
        _ => Err(Error::new(format!(
            "cannot convert {} to int",
            x.type_name()
        ))),
    }
}

/// Returns the value of x if is representable as an int32.
pub fn as_int32(x: &Value) -> Result<i32, Error> {
    match x {
        Value::Int(i) => match i.as_i32() {
            Some(v) => Ok(v),
            None => Err(Error::new(format!("{} out of range", i))),
        },
        _ => Err(Error::new(format!("got {}, want int", x.type_name()))),
    }
}

/// Returns the value of Starlark int x as an i64 if it lies in the signed
/// range of the given bit size (Go's AsInt for *int8..*int64).
pub fn as_signed(x: &Value, bits: u32) -> Result<i64, Error> {
    let Value::Int(xint) = x else {
        return Err(Error::new(format!("got {}, want int", x.type_name())));
    };
    match xint.to_i64() {
        Some(i) if bits >= 64 || (-(1i64 << (bits - 1)) <= i && i < (1i64 << (bits - 1))) => Ok(i),
        _ => Err(Error::new(format!(
            "{} out of range (want value in signed {}-bit range)",
            xint, bits
        ))),
    }
}

/// Returns the value of Starlark int x as a u64 if it lies in the unsigned
/// range of the given bit size (Go's AsInt for *uint8..*uint64).
pub fn as_unsigned(x: &Value, bits: u32) -> Result<u64, Error> {
    let Value::Int(xint) = x else {
        return Err(Error::new(format!("got {}, want int", x.type_name())));
    };
    match xint.to_u64() {
        Some(i) if bits >= 64 || i < (1u64 << bits) => Ok(i),
        _ => Err(Error::new(format!(
            "{} out of range (want value in unsigned {}-bit range)",
            xint, bits
        ))),
    }
}

/// Parses a string of digits in the given base (2..=36), with no sign,
/// prefix or underscores, like Go's big.Int.SetString for base != 0.
pub fn parse_digits(s: &[u8], base: u32) -> Option<BigInt> {
    if s.is_empty() {
        return None;
    }
    let mut digits = Vec::with_capacity(s.len());
    for &c in s {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 10,
            b'A'..=b'Z' => (c - b'A') as u32 + 10,
            _ => return None,
        };
        if d >= base {
            return None;
        }
        digits.push(d as u8);
    }
    BigInt::from_radix_be(Sign::Plus, &digits, base)
}

/// Compares an int with a finite float exactly, returning -1, 0 or +1.
pub fn cmp_int_float(x: &Int, f: f64) -> i32 {
    let fl = f.floor();
    let flint = float_to_bigint(fl);
    let xb = x.to_bigint();
    match xb.cmp(&flint) {
        Ordering::Less => -1,
        Ordering::Equal => {
            if fl == f {
                0
            } else {
                -1
            }
        }
        Ordering::Greater => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_big_boundary() {
        assert!(matches!(Int::from_i64(i32::MAX as i64), Int::Small(_)));
        assert!(matches!(Int::from_i64(i32::MAX as i64 + 1), Int::Big(_)));
        assert!(matches!(Int::from_i64(i32::MIN as i64), Int::Small(_)));
        let x = Int::from_i64(i32::MAX as i64).add(&Int::Small(1));
        assert_eq!(x.to_string(), "2147483648");
        assert!(matches!(x.sub(&Int::Small(1)), Int::Small(_)));
    }

    #[test]
    fn floor_div_mod() {
        let d = |a: i64, b: i64| Int::from_i64(a).div(&Int::from_i64(b)).to_string();
        let m = |a: i64, b: i64| Int::from_i64(a).modulo(&Int::from_i64(b)).to_string();
        assert_eq!(d(-7, 2), "-4");
        assert_eq!(m(-7, 2), "1");
        assert_eq!(m(7, -2), "-1");
        let big = Int::from_bigint(BigInt::from(1) << 100);
        assert_eq!(
            big.neg().div(&Int::Small(3)).to_string(),
            "-422550200076076467165567735126"
        );
    }

    #[test]
    fn to_float() {
        let b = BigInt::from(1u64 << 63) * BigInt::from(4) + BigInt::from(1);
        assert_eq!(bigint_to_f64(&b), 3.6893488147419103e19);
        let huge = BigInt::from(1) << 1100;
        assert!(bigint_to_f64(&huge).is_infinite());
        assert_eq!(
            finite_float_to_int(1e20).to_string(),
            "100000000000000000000"
        );
        assert_eq!(cmp_int_float(&Int::Small(1), 1.5), -1);
        assert_eq!(cmp_int_float(&Int::Small(2), 1.5), 1);
        assert_eq!(cmp_int_float(&Int::Small(-2), -1.5), -1);
        assert_eq!(cmp_int_float(&Int::Small(-1), -1.5), 1);
        assert_eq!(cmp_int_float(&Int::Small(3), 3.0), 0);
    }
}
