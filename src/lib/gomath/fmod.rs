// Ported from the Go standard library: original work of The Go Authors
// (2009-2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point mod function (mod.go).

use super::{abs, frexp, is_inf, is_nan, ldexp, nan};

/// Returns the floating-point remainder of x/y. The magnitude of the
/// result is less than y and its sign agrees with that of x.
///
/// Special cases are:
///
///   - mod(±Inf, y) = NaN
///   - mod(NaN, y) = NaN
///   - mod(x, 0) = NaN
///   - mod(x, ±Inf) = x
///   - mod(x, NaN) = NaN
pub fn fmod(x: f64, y: f64) -> f64 {
    if y == 0.0 || is_inf(x, 0) || is_nan(x) || is_nan(y) {
        return nan();
    }
    let y = abs(y);

    let (yfr, yexp) = frexp(y);
    let mut r = x;
    if x < 0.0 {
        r = -x;
    }

    while r >= y {
        let (rfr, mut rexp) = frexp(r);
        if rfr < yfr {
            rexp -= 1;
        }
        r -= ldexp(y, rexp - yexp);
    }
    if x < 0.0 {
        r = -r;
    }
    r
}
