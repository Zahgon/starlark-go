// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Atan2 (atan2.go).

use super::{atan, copysign, is_inf, is_nan, nan, signbit, PI};

/// Returns the arc tangent of y/x, using the signs of the two to
/// determine the quadrant of the return value.
///
/// Special cases are (in order):
///
///   - atan2(y, NaN) = NaN
///   - atan2(NaN, x) = NaN
///   - atan2(+0, x>=0) = +0
///   - atan2(-0, x>=0) = -0
///   - atan2(+0, x<=-0) = +Pi
///   - atan2(-0, x<=-0) = -Pi
///   - atan2(y>0, 0) = +Pi/2
///   - atan2(y<0, 0) = -Pi/2
///   - atan2(+Inf, +Inf) = +Pi/4
///   - atan2(-Inf, +Inf) = -Pi/4
///   - atan2(+Inf, -Inf) = 3Pi/4
///   - atan2(-Inf, -Inf) = -3Pi/4
///   - atan2(y, +Inf) = 0
///   - atan2(y>0, -Inf) = +Pi
///   - atan2(y<0, -Inf) = -Pi
///   - atan2(+Inf, x) = +Pi/2
///   - atan2(-Inf, x) = -Pi/2
pub fn atan2(y: f64, x: f64) -> f64 {
    // special cases
    if is_nan(y) || is_nan(x) {
        return nan();
    } else if y == 0.0 {
        if x >= 0.0 && !signbit(x) {
            return copysign(0.0, y);
        }
        return copysign(PI, y);
    } else if x == 0.0 {
        return copysign(PI / 2.0, y);
    } else if is_inf(x, 0) {
        if is_inf(x, 1) {
            if is_inf(y, 0) {
                return copysign(PI / 4.0, y);
            }
            return copysign(0.0, y);
        }
        if is_inf(y, 0) {
            return copysign(3.0 * PI / 4.0, y);
        }
        return copysign(PI, y);
    } else if is_inf(y, 0) {
        return copysign(PI / 2.0, y);
    }

    // Call atan and determine the quadrant.
    let q = atan(y / x);
    if x < 0.0 {
        if q <= 0.0 {
            return q + PI;
        }
        return q - PI;
    }
    q
}
