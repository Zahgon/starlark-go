// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Pow (pow.go).

use super::{abs, exp, frexp, inf, is_inf, is_nan, ldexp, log, modf, nan, signbit};

fn is_odd_int(x: f64) -> bool {
    if abs(x) >= (1u64 << 53) as f64 {
        // 1 << 53 is the largest exact integer in the float64 format.
        // Any number outside this range will be truncated before the
        // decimal point and therefore will always be an even integer.
        // Without this check and if x overflows int64 the int64(xi)
        // conversion below may produce incorrect results on some
        // architectures (and does so on arm64). See issue #57465.
        return false;
    }

    let (xi, xf) = modf(x);
    xf == 0.0 && (xi as i64) & 1 == 1
}

/// Returns x**y, the base-x exponential of y.
///
/// Special cases are (in order):
///
///   - pow(x, ±0) = 1 for any x
///   - pow(1, y) = 1 for any y
///   - pow(x, 1) = x for any x
///   - pow(NaN, y) = NaN
///   - pow(x, NaN) = NaN
///   - pow(±0, y) = ±Inf for y an odd integer < 0
///   - pow(±0, -Inf) = +Inf
///   - pow(±0, +Inf) = +0
///   - pow(±0, y) = +Inf for finite y < 0 and not an odd integer
///   - pow(±0, y) = ±0 for y an odd integer > 0
///   - pow(±0, y) = +0 for finite y > 0 and not an odd integer
///   - pow(-1, ±Inf) = 1
///   - pow(x, +Inf) = +Inf for |x| > 1
///   - pow(x, -Inf) = +0 for |x| > 1
///   - pow(x, +Inf) = +0 for |x| < 1
///   - pow(x, -Inf) = +Inf for |x| < 1
///   - pow(+Inf, y) = +Inf for y > 0
///   - pow(+Inf, y) = +0 for y < 0
///   - pow(-Inf, y) = pow(-0, -y)
///   - pow(x, y) = NaN for finite x < 0 and finite non-integer y
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    } else if y == 1.0 {
        return x;
    } else if is_nan(x) || is_nan(y) {
        return nan();
    } else if x == 0.0 {
        if y < 0.0 {
            if signbit(x) && is_odd_int(y) {
                return inf(-1);
            }
            return inf(1);
        } else if y > 0.0 {
            if signbit(x) && is_odd_int(y) {
                return x;
            }
            return 0.0;
        }
    } else if is_inf(y, 0) {
        if x == -1.0 {
            return 1.0;
        } else if (abs(x) < 1.0) == is_inf(y, 1) {
            return 0.0;
        } else {
            return inf(1);
        }
    } else if is_inf(x, 0) {
        if is_inf(x, -1) {
            return pow(1.0 / x, -y); // Pow(-0, -y)
        }
        if y < 0.0 {
            return 0.0;
        } else if y > 0.0 {
            return inf(1);
        }
    } else if y == 0.5 {
        return x.sqrt();
    } else if y == -0.5 {
        return 1.0 / x.sqrt();
    }

    let (mut yi, mut yf) = modf(abs(y));
    if yf != 0.0 && x < 0.0 {
        return nan();
    }
    if yi >= 9223372036854775808.0 {
        // yi is a large even int that will lead to overflow (or underflow to 0)
        // for all x except -1 (x == 1 was handled earlier)
        if x == -1.0 {
            return 1.0;
        } else if (abs(x) < 1.0) == (y > 0.0) {
            return 0.0;
        } else {
            return inf(1);
        }
    }

    // ans = a1 * 2**ae (= 1 for now).
    let mut a1 = 1.0;
    let mut ae: i64 = 0;

    // ans *= x**yf
    if yf != 0.0 {
        if yf > 0.5 {
            yf -= 1.0;
            yi += 1.0;
        }
        a1 = exp(yf * log(x));
    }

    // ans *= x**yi
    //   by repeated squaring and multiplying by bits of yi.
    let (mut x1, mut xe) = frexp(x);
    let mut i = yi as i64;
    while i != 0 {
        if !(-(1 << 12)..=1 << 12).contains(&xe) {
            // catastrophic overflow - let Ldexp handle it.
            ae = ae.wrapping_add(xe);
            break;
        }
        if i & 1 == 1 {
            a1 *= x1;
            ae = ae.wrapping_add(xe);
        }
        let sq = x1 * x1;
        xe <<= 1;
        if sq < 0.5 {
            // x1 += x1, where both operands are the product x1*x1 (fused).
            x1 = x1.mul_add(x1, sq);
            xe -= 1;
        } else {
            x1 = sq;
        }
        i >>= 1;
    }

    // ans = a1 * 2**ae
    // if y < 0 { ans = 1 / ans }
    // but in the opposite order
    if y < 0.0 {
        a1 = 1.0 / a1;
        ae = ae.wrapping_neg();
    }
    ldexp(a1, ae)
}
