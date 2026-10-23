// Ported from the Go standard library: original work of The Go Authors
// (2017), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Exp, ported from Go's arm64 assembly implementation (exp_arm64.s),
//! which is the one used on linux/arm64.

const LN2_HI: f64 = 6.93147180369123816490e-01;
const LN2_LO: f64 = 1.90821492927058770002e-10;
const LOG2E: f64 = 1.44269504088896338700e+00;
const OVERFLOW: f64 = 7.09782712893383973096e+02;
const UNDERFLOW: f64 = -7.45133219101941108420e+02;
const NEAR_ZERO: u64 = 0x3e30000000000000; // 2**-28
const POS_INF: u64 = 0x7ff0000000000000;
const FRAC_MASK: u64 = 0x000fffffffffffff;
const C1: u64 = 0x3cb0000000000000; // 2**-52
const P1: f64 = 1.66666666666666657415e-01; // 0x3FC55555; 0x55555555
const P2: f64 = -2.77777777770155933842e-03; // 0xBF66C16C; 0x16BEBD93
const P3: f64 = 6.61375632143793436117e-05; // 0x3F11566A; 0xAF25DE2C
const P4: f64 = -1.65339022054652515390e-06; // 0xBEBBBD41; 0xC5D26BF1
const P5: f64 = 4.13813679705723846039e-08; // 0x3E663769; 0x72BEA4D0

/// Returns e**x, the base-e exponential of x.
///
/// Special cases are:
///
///   - exp(+Inf) = +Inf
///   - exp(NaN) = NaN
///
/// Very large values overflow to 0 or +Inf.
/// Very small values underflow to 1.
pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x; // x = NaN, return NaN
    }
    if x > OVERFLOW {
        return f64::from_bits(POS_INF); // x > Overflow, return PosInf
    }
    if x < UNDERFLOW {
        return 0.0; // x < Underflow, return 0
    }
    if super::abs(x) < f64::from_bits(NEAR_ZERO) {
        return x + 1.0; // fabs(x) < NearZero, return 1 + x
    }
    // k = int(Log2e*x -/+ 0.5)
    let k = if x < 0.0 {
        LOG2E.mul_add(x, -0.5)
    } else {
        LOG2E.mul_add(x, 0.5)
    };
    let ki = k as i64;
    let kf = ki as f64;
    let hi = (-kf).mul_add(LN2_HI, x); // x - float64(int(k))*Ln2Hi
    let lo = kf * LN2_LO;
    let r = hi - lo;
    let t = r * r;
    let mut c = t.mul_add(P5, P4);
    c = c.mul_add(t, P3);
    c = c.mul_add(t, P2);
    c = c.mul_add(t, P1);
    c = (-c).mul_add(t, r); // r - t*(P1+t*(P2+t*(P3+t*(P4+t*P5))))
    let y = 1.0 - ((lo - (r * c) / (2.0 - c)) - hi);
    // Scale y by 2**k, as the assembly does on the bit pattern.
    let bits = y.to_bits();
    let frac = bits & FRAC_MASK;
    let mut e = (bits >> 52).wrapping_add(ki as u64);
    let mut m = 1.0;
    if (e as i64) < 1 {
        // denormal
        e = e.wrapping_add(52);
        m = f64::from_bits(C1); // m = 2**-52
    }
    let r = f64::from_bits((e << 52) | frac);
    r * m
}
