// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Inverse hyperbolic sine (asinh.go).
//!
//! The original C code, the long comment, and the constants below are
//! from FreeBSD's /usr/src/lib/msun/src/s_asinh.c and came with a notice to this
//! effect:
//!
//! ====================================================
//! Original work of Sun Microsystems, Inc. (1993), all rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ====================================================

use super::{is_inf, is_nan, log, log1p};

/// Returns the inverse hyperbolic sine of x.
///
/// Special cases are:
///
///   - asinh(±0) = ±0
///   - asinh(±Inf) = ±Inf
///   - asinh(NaN) = NaN
pub fn asinh(x: f64) -> f64 {
    const LN2: f64 = 6.93147180559945286227e-01; // 0x3FE62E42FEFA39EF
    const NEAR_ZERO: f64 = 1.0 / (1u64 << 28) as f64; // 2**-28
    const LARGE: f64 = (1u64 << 28) as f64; // 2**28

    // special cases
    if is_nan(x) || is_inf(x, 0) {
        return x;
    }
    let mut x = x;
    let mut sign = false;
    if x < 0.0 {
        x = -x;
        sign = true;
    }
    let mut temp = if x > LARGE {
        log(x) + LN2 // |x| > 2**28
    } else if x > 2.0 {
        log(2.0 * x + 1.0 / (x.mul_add(x, 1.0).sqrt() + x)) // 2**28 > |x| > 2.0
    } else if x < NEAR_ZERO {
        x // |x| < 2**-28
    } else {
        log1p(x + x * x / (1.0 + x.mul_add(x, 1.0).sqrt())) // 2.0 > |x| > 2**-28
    };
    if sign {
        temp = -temp;
    }
    temp
}
