// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! log1p (log1p.go).
//!
//! The original C code, the long comment, and the constants below are
//! from FreeBSD's /usr/src/lib/msun/src/s_log1p.c and came with a notice to this effect:
//!
//! ====================================================
//! Original work of Sun Microsystems, Inc. (1993), all rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ====================================================

use super::{abs, inf, is_inf, is_nan, nan};

/// Returns the natural logarithm of 1 plus its argument x. It is more
/// accurate than log(1 + x) when x is near zero.
///
/// Special cases are:
///
///   - log1p(+Inf) = +Inf
///   - log1p(±0) = ±0
///   - log1p(-1) = -Inf
///   - log1p(x < -1) = NaN
///   - log1p(NaN) = NaN
pub fn log1p(x: f64) -> f64 {
    const SQRT2_M1: f64 = 4.142135623730950488017e-01; // Sqrt(2)-1 = 0x3fda827999fcef34
    const SQRT2_HALF_M1: f64 = -2.928932188134524755992e-01; // Sqrt(2)/2-1 = 0xbfd2bec333018866
    const SMALL: f64 = 1.0 / (1u64 << 29) as f64; // 2**-29 = 0x3e20000000000000
    const TINY: f64 = 1.0 / (1u64 << 54) as f64; // 2**-54
    const TWO53: f64 = (1u64 << 53) as f64; // 2**53
    const LN2_HI: f64 = 6.93147180369123816490e-01; // 3fe62e42fee00000
    const LN2_LO: f64 = 1.90821492927058770002e-10; // 3dea39ef35793c76
    const LP1: f64 = 6.666666666666735130e-01; // 3FE5555555555593
    const LP2: f64 = 3.999999999940941908e-01; // 3FD999999997FA04
    const LP3: f64 = 2.857142874366239149e-01; // 3FD2492494229359
    const LP4: f64 = 2.222219843214978396e-01; // 3FCC71C51D8E78AF
    const LP5: f64 = 1.818357216161805012e-01; // 3FC7466496CB03DE
    const LP6: f64 = 1.531383769920937332e-01; // 3FC39A09D078C69F
    const LP7: f64 = 1.479819860511658591e-01; // 3FC2F112DF3E5244

    // special cases
    if x < -1.0 || is_nan(x) {
        // includes -Inf
        return nan();
    }
    if x == -1.0 {
        return inf(-1);
    }
    if is_inf(x, 1) {
        return inf(1);
    }

    let absx = abs(x);

    let mut f = 0.0;
    let mut iu: u64 = 0;
    let mut k: i64 = 1;
    if absx < SQRT2_M1 {
        //  |x| < Sqrt(2)-1
        if absx < SMALL {
            // |x| < 2**-29
            if absx < TINY {
                // |x| < 2**-54
                return x;
            }
            return (-(x * x)).mul_add(0.5, x);
        }
        if x > SQRT2_HALF_M1 {
            // Sqrt(2)/2-1 < x
            // (Sqrt(2)/2-1) < x < (Sqrt(2)-1)
            k = 0;
            f = x;
            iu = 1;
        }
    }
    let mut c = 0.0;
    if k != 0 {
        let mut u;
        if absx < TWO53 {
            // 1<<53
            u = 1.0 + x;
            iu = u.to_bits();
            k = ((iu >> 52).wrapping_sub(1023)) as i64;
            // correction term
            if k > 0 {
                c = 1.0 - (u - x);
            } else {
                c = x - (u - 1.0);
            }
            c /= u;
        } else {
            u = x;
            iu = u.to_bits();
            k = ((iu >> 52).wrapping_sub(1023)) as i64;
            c = 0.0;
        }
        iu &= 0x000fffffffffffff;
        if iu < 0x0006a09e667f3bcd {
            // mantissa of Sqrt(2)
            u = f64::from_bits(iu | 0x3ff0000000000000); // normalize u
        } else {
            k += 1;
            u = f64::from_bits(iu | 0x3fe0000000000000); // normalize u/2
            iu = (0x0010000000000000 - iu) >> 2;
        }
        f = u - 1.0; // Sqrt(2)/2 < u < Sqrt(2)
    }
    // hfsq := 0.5 * f * f
    let hf = 0.5 * f;
    let hfsq = hf * f;
    let kf = k as f64;
    if iu == 0 {
        // |f| < 2**-20
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            c = kf.mul_add(LN2_LO, c);
            return kf.mul_add(LN2_HI, c);
        }
        // R = hfsq * (1.0 - 0.66666666666666666*f), avoiding division
        let t = (-0.66666666666666666f64).mul_add(f, 1.0);
        if k == 0 {
            return (-hfsq).mul_add(t, f); // f - R
        }
        let a = kf.mul_add(LN2_LO, c);
        let b = hfsq.mul_add(t, -a); // R - (k*Ln2Lo + c)
        return kf.mul_add(LN2_HI, -(b - f));
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let p = z.mul_add(
        z.mul_add(
            z.mul_add(z.mul_add(z.mul_add(z.mul_add(LP7, LP6), LP5), LP4), LP3),
            LP2,
        ),
        LP1,
    );
    let r = z * p;
    let a = hf.mul_add(f, r); // hfsq + R
    if k == 0 {
        return f - (-a).mul_add(s, hfsq); // f - (hfsq - s*(hfsq+R))
    }
    let cc = kf.mul_add(LN2_LO, c);
    let b = s.mul_add(a, cc); // s*(hfsq+R) + (k*Ln2Lo + c)
    let d = hf.mul_add(f, -b); // hfsq - (...)
    kf.mul_add(LN2_HI, -(d - f))
}
