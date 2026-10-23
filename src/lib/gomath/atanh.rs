// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Inverse hyperbolic tangent (atanh.go).
//!
//! The original C code, the long comment, and the constants below are
//! from FreeBSD's /usr/src/lib/msun/src/e_atanh.c and came with a notice to this
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

use super::{inf, is_nan, log1p, nan};

/// Returns the inverse hyperbolic tangent of x.
///
/// Special cases are:
///
///   - atanh(1) = +Inf
///   - atanh(±0) = ±0
///   - atanh(-1) = -Inf
///   - atanh(x) = NaN if x < -1 or x > 1
///   - atanh(NaN) = NaN
pub fn atanh(x: f64) -> f64 {
    const NEAR_ZERO: f64 = 1.0 / (1u64 << 28) as f64; // 2**-28

    // special cases
    if !(-1.0..=1.0).contains(&x) || is_nan(x) {
        return nan();
    } else if x == 1.0 {
        return inf(1);
    } else if x == -1.0 {
        return inf(-1);
    }
    let mut x = x;
    let mut sign = false;
    if x < 0.0 {
        x = -x;
        sign = true;
    }
    let mut temp = if x < NEAR_ZERO {
        x
    } else if x < 0.5 {
        let t = x + x;
        0.5 * log1p(t + t * x / (1.0 - x))
    } else {
        0.5 * log1p((x + x) / (1.0 - x))
    };
    if sign {
        temp = -temp;
    }
    temp
}
