// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point hyperbolic sine and cosine (sinh.go).
//!
//! The exponential func is called for arguments greater in magnitude
//! than 0.5. A series is used for arguments smaller in magnitude than 0.5.
//! Cosh(x) is computed from the exponential func for all arguments.

use super::{abs, exp};

/// Returns the hyperbolic sine of x.
///
/// Special cases are:
///
///   - sinh(±0) = ±0
///   - sinh(±Inf) = ±Inf
///   - sinh(NaN) = NaN
pub fn sinh(x: f64) -> f64 {
    // The coefficients are #2029 from Hart & Cheney. (20.36D)
    const P0: f64 = -0.6307673640497716991184787251e+6;
    const P1: f64 = -0.8991272022039509355398013511e+5;
    const P2: f64 = -0.2894211355989563807284660366e+4;
    const P3: f64 = -0.2630563213397497062819489e+2;
    const Q0: f64 = -0.6307673640497716991212077277e+6;
    const Q1: f64 = 0.1521517378790019070696485176e+5;
    const Q2: f64 = -0.173678953558233699533450911e+3;

    let mut x = x;
    let mut sign = false;
    if x < 0.0 {
        x = -x;
        sign = true;
    }

    let mut temp = if x > 21.0 {
        exp(x) * 0.5
    } else if x > 0.5 {
        let ex = exp(x);
        (ex - 1.0 / ex) * 0.5
    } else {
        let sq = x * x;
        let num = P3.mul_add(sq, P2).mul_add(sq, P1).mul_add(sq, P0) * x;
        // sq+Q2 is fused with the product x*x.
        num / x.mul_add(x, Q2).mul_add(sq, Q1).mul_add(sq, Q0)
    };

    if sign {
        temp = -temp;
    }
    temp
}

/// Returns the hyperbolic cosine of x.
///
/// Special cases are:
///
///   - cosh(±0) = 1
///   - cosh(±Inf) = +Inf
///   - cosh(NaN) = NaN
pub fn cosh(x: f64) -> f64 {
    let x = abs(x);
    if x > 21.0 {
        return exp(x) * 0.5;
    }
    let ex = exp(x);
    (ex + 1.0 / ex) * 0.5
}
