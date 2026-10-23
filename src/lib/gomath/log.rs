// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point logarithm (log.go).
//!
//! The original C code, the long comment, and the constants below are
//! from FreeBSD's /usr/src/lib/msun/src/e_log.c and came with a notice to this effect:
//!
//! ====================================================
//! Original work of Sun Microsystems, Inc. (1993), all rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ====================================================

use super::{frexp, inf, is_inf, is_nan, nan, SQRT2};

/// Returns the natural logarithm of x.
///
/// Special cases are:
///
///   - log(+Inf) = +Inf
///   - log(0) = -Inf
///   - log(x < 0) = NaN
///   - log(NaN) = NaN
pub fn log(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01; // 3fe62e42 fee00000
    const LN2_LO: f64 = 1.90821492927058770002e-10; // 3dea39ef 35793c76
    const L1: f64 = 6.666666666666735130e-01; // 3FE55555 55555593
    const L2: f64 = 3.999999999940941908e-01; // 3FD99999 9997FA04
    const L3: f64 = 2.857142874366239149e-01; // 3FD24924 94229359
    const L4: f64 = 2.222219843214978396e-01; // 3FCC71C5 1D8E78AF
    const L5: f64 = 1.818357216161805012e-01; // 3FC74664 96CB03DE
    const L6: f64 = 1.531383769920937332e-01; // 3FC39A09 D078C69F
    const L7: f64 = 1.479819860511658591e-01; // 3FC2F112 DF3E5244

    // special cases
    if is_nan(x) || is_inf(x, 1) {
        return x;
    }
    if x < 0.0 {
        return nan();
    }
    if x == 0.0 {
        return inf(-1);
    }

    // reduce
    let (mut f1, mut ki) = frexp(x);
    if f1 < SQRT2 / 2.0 {
        f1 *= 2.0;
        ki -= 1;
    }
    let f = f1 - 1.0;
    let k = ki as f64;

    // compute
    let s = f / (2.0 + f);
    let s2 = s * s;
    let s4 = s2 * s2;
    // t1 := s2 * p1; t2 := s4 * p2; R := t1 + t2 (the product t1 is fused)
    let p1 = s4.mul_add(s4.mul_add(s4.mul_add(L7, L5), L3), L1);
    let t2 = s4 * s4.mul_add(s4.mul_add(L6, L4), L2);
    let r = s2.mul_add(p1, t2);
    // hfsq := 0.5 * f * f
    let hf = 0.5 * f;
    let a = hf.mul_add(f, r); // hfsq + R
    let b = s.mul_add(a, k * LN2_LO); // s*(hfsq+R) + k*Ln2Lo
    let c = hf.mul_add(f, -b); // hfsq - (...)
    k.mul_add(LN2_HI, -(c - f))
}
