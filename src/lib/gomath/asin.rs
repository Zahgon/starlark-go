// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point arcsine and arccosine (asin.go).
//!
//! They are implemented by computing the arctangent after appropriate
//! range reduction.

use super::atan::satan;
use super::{nan, PI};

/// Returns the arcsine, in radians, of x.
///
/// Special cases are:
///
///   - asin(±0) = ±0
///   - asin(x) = NaN if x < -1 or x > 1
pub fn asin(x: f64) -> f64 {
    if x == 0.0 {
        return x; // special case
    }
    let mut x = x;
    let mut sign = false;
    if x < 0.0 {
        x = -x;
        sign = true;
    }
    if x > 1.0 {
        return nan(); // special case
    }

    let mut temp = (-x).mul_add(x, 1.0).sqrt();
    if x > 0.7 {
        temp = PI / 2.0 - satan(temp / x);
    } else {
        temp = satan(x / temp);
    }

    if sign {
        temp = -temp;
    }
    temp
}

/// Returns the arccosine, in radians, of x.
///
/// Special case is:
///
///   - acos(x) = NaN if x < -1 or x > 1
pub fn acos(x: f64) -> f64 {
    PI / 2.0 - asin(x)
}
