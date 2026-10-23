// Ported from the Go standard library: original work of The Go Authors
// (2011), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point tangent (tan.go).
//!
//! The original C code, the long comment, and the constants below were
//! from http://netlib.sandia.gov/cephes/cmath/sin.c, available from
//! http://www.netlib.org/cephes/cmath.tgz.
//! Cephes Math Library Release 2.8:  June, 2000
//! Cephes code by Stephen L. Moshier (1984, 1987, 1989, 1992, 2000).

use super::sin::reduce;
use super::trig_reduce::{trig_reduce, REDUCE_THRESHOLD};
use super::{is_inf, is_nan, nan};

// tan coefficients
const TAN_P: [f64; 3] = [
    -1.30936939181383777646e4, // 0xc0c992d8d24f3f38
    1.15351664838587416140e6,  // 0x413199eca5fc9ddd
    -1.79565251976484877988e7, // 0xc1711fead3299176
];
const TAN_Q: [f64; 5] = [
    1.00000000000000000000e0,
    1.36812963470692954678e4,  // 0x40cab8a5eeb36572
    -1.32089234440210967447e6, // 0xc13427bc582abc96
    2.50083801823357915839e7,  // 0x4177d98fc2ead8ef
    -5.38695755929454629881e7, // 0xc189afe03cbe5a31
];

/// Returns the tangent of the radian argument x.
///
/// Special cases are:
///
///   - tan(±0) = ±0
///   - tan(±Inf) = NaN
///   - tan(NaN) = NaN
pub fn tan(x: f64) -> f64 {
    // special cases
    if x == 0.0 || is_nan(x) {
        return x; // return ±0 || NaN()
    }
    if is_inf(x, 0) {
        return nan();
    }

    // make argument positive but save the sign
    let mut sign = false;
    let mut x = x;
    if x < 0.0 {
        x = -x;
        sign = true;
    }
    let (j, z) = if x >= REDUCE_THRESHOLD {
        trig_reduce(x)
    } else {
        reduce(x)
    };

    let zz = z * z;
    let mut y = if zz > 1e-14 {
        let num = zz * TAN_P[0].mul_add(zz, TAN_P[1]).mul_add(zz, TAN_P[2]);
        // zz+Q[1] is fused with the product z*z.
        let den = z
            .mul_add(z, TAN_Q[1])
            .mul_add(zz, TAN_Q[2])
            .mul_add(zz, TAN_Q[3])
            .mul_add(zz, TAN_Q[4]);
        z.mul_add(num / den, z)
    } else {
        z
    };
    if j & 2 == 2 {
        y = -1.0 / y;
    }
    if sign {
        y = -y;
    }
    y
}
