// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Inverse hyperbolic cosine (acosh.go).
//!
//! The original C code, the long comment, and the constants below are
//! from FreeBSD's /usr/src/lib/msun/src/e_acosh.c and came with a notice to this
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

use super::{is_nan, log, log1p, nan, LN2};

/// Returns the inverse hyperbolic cosine of x.
///
/// Special cases are:
///
///   - acosh(+Inf) = +Inf
///   - acosh(x) = NaN if x < 1
///   - acosh(NaN) = NaN
pub fn acosh(x: f64) -> f64 {
    const LARGE: f64 = (1u64 << 28) as f64; // 2**28

    // first case is special case
    if x < 1.0 || is_nan(x) {
        return nan();
    } else if x == 1.0 {
        return 0.0;
    } else if x >= LARGE {
        return log(x) + LN2; // x > 2**28
    } else if x > 2.0 {
        return log(2.0 * x - 1.0 / (x + x.mul_add(x, -1.0).sqrt())); // 2**28 > x > 2
    }
    let t = x - 1.0;
    log1p(t + t.mul_add(t, 2.0 * t).sqrt()) // 2 >= x > 1
}
