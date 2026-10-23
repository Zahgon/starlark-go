// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Hypot -- sqrt(p*p + q*q), but overflows only if the result does
//! (hypot.go).

use super::{abs, inf, is_inf, is_nan, nan};

/// Returns Sqrt(p*p + q*q), taking care to avoid unnecessary overflow
/// and underflow.
///
/// Special cases are:
///
///   - hypot(±Inf, q) = +Inf
///   - hypot(p, ±Inf) = +Inf
///   - hypot(NaN, q) = NaN
///   - hypot(p, NaN) = NaN
pub fn hypot(p: f64, q: f64) -> f64 {
    let (mut p, mut q) = (abs(p), abs(q));
    // special cases
    if is_inf(p, 1) || is_inf(q, 1) {
        return inf(1);
    }
    if is_nan(p) || is_nan(q) {
        return nan();
    }
    if p < q {
        std::mem::swap(&mut p, &mut q);
    }
    if p == 0.0 {
        return 0.0;
    }
    q /= p;
    p * q.mul_add(q, 1.0).sqrt()
}
