// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Remainder (remainder.go).
//!
//! The original C code and the comment below are from
//! FreeBSD's /usr/src/lib/msun/src/e_remainder.c and came
//! with this notice:
//!
//! ====================================================
//! Original work of Sun Microsystems, Inc. (1993), all rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ====================================================

use super::{fmod, is_inf, is_nan, nan};

/// Returns the IEEE 754 floating-point remainder of x/y.
///
/// Special cases are:
///
///   - remainder(±Inf, y) = NaN
///   - remainder(NaN, y) = NaN
///   - remainder(x, 0) = NaN
///   - remainder(x, ±Inf) = x
///   - remainder(x, NaN) = NaN
pub fn remainder(x: f64, y: f64) -> f64 {
    const TINY: f64 = 4.45014771701440276618e-308; // 0x0020000000000000
    const HALF_MAX: f64 = f64::MAX / 2.0;

    // special cases
    if is_nan(x) || is_nan(y) || is_inf(x, 0) || y == 0.0 {
        return nan();
    }
    if is_inf(y, 0) {
        return x;
    }

    let mut x = x;
    let mut y = y;
    let mut sign = false;
    if x < 0.0 {
        x = -x;
        sign = true;
    }
    if y < 0.0 {
        y = -y;
    }
    if x == y {
        if sign {
            return -0.0;
        }
        return 0.0;
    }
    if y <= HALF_MAX {
        x = fmod(x, y + y); // now x < 2y
    }
    if y < TINY {
        if x + x > y {
            x -= y;
            if x + x >= y {
                x -= y;
            }
        }
    } else {
        let y_half = 0.5 * y;
        if x > y_half {
            x -= y;
            if x >= y_half {
                x -= y;
            }
        }
    }
    if sign {
        x = -x;
    }
    x
}
