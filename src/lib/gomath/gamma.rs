// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! The Gamma function (gamma.go).
//!
//! The original C code, the long comment, and the constants below are
//! from http://netlib.sandia.gov/cephes/cprob/gamma.c.
//! Cephes Math Library Release 2.8:  June, 2000
//! Cephes code by Stephen L. Moshier (1984, 1987, 1989, 1992, 2000).

use super::{abs, exp, inf, is_inf, is_nan, modf, nan, pow, signbit, sin, PI};

const GAM_P: [f64; 7] = [
    1.60119522476751861407e-04,
    1.19135147006586384913e-03,
    1.04213797561761569935e-02,
    4.76367800457137231464e-02,
    2.07448227648435975150e-01,
    4.94214826801497100753e-01,
    9.99999999999999996796e-01,
];
const GAM_Q: [f64; 8] = [
    -2.31581873324120129819e-05,
    5.39605580493303397842e-04,
    -4.45641913851797240494e-03,
    1.18139785222060435552e-02,
    3.58236398605498653373e-02,
    -2.34591795718243348568e-01,
    7.14304917030273074085e-02,
    1.00000000000000000320e+00,
];
const GAM_S: [f64; 5] = [
    7.87311395793093628397e-04,
    -2.29549961613378126380e-04,
    -2.68132617805781232825e-03,
    3.47222221605458667310e-03,
    8.33333333333482257126e-02,
];

/// Gamma function computed by Stirling's formula. The pair of results
/// must be multiplied together to get the actual answer. The
/// multiplication is left to the caller so that, if careful, the caller
/// can avoid infinity for 172 <= x <= 180. The polynomial is valid for
/// 33 <= x <= 172; larger values are only used in reciprocal and produce
/// denormalized floats. The lower precision there masks any imprecision
/// in the polynomial.
fn stirling(x: f64) -> (f64, f64) {
    if x > 200.0 {
        return (inf(1), 1.0);
    }
    const SQRT_TWO_PI: f64 = 2.506628274631000502417;
    const MAX_STIRLING: f64 = 143.01608;
    let mut w = 1.0 / x;
    w = w.mul_add(
        GAM_S[0]
            .mul_add(w, GAM_S[1])
            .mul_add(w, GAM_S[2])
            .mul_add(w, GAM_S[3])
            .mul_add(w, GAM_S[4]),
        1.0,
    );
    let mut y1 = exp(x);
    let mut y2 = 1.0;
    if x > MAX_STIRLING {
        // avoid Pow() overflow
        let v = pow(x, 0.5f64.mul_add(x, -0.25));
        y2 = v / y1;
        y1 = v;
    } else {
        y1 = pow(x, x - 0.5) / y1;
    }
    (y1, SQRT_TWO_PI * w * y2)
}

/// Returns the Gamma function of x.
///
/// Special cases are:
///
///   - gamma(+Inf) = +Inf
///   - gamma(+0) = +Inf
///   - gamma(-0) = -Inf
///   - gamma(x) = NaN for integer x < 0
///   - gamma(-Inf) = NaN
///   - gamma(NaN) = NaN
pub fn gamma(x: f64) -> f64 {
    const EULER: f64 = 0.57721566490153286060651209008240243104215933593992; // A001620

    // special cases
    if is_neg_int(x) || is_inf(x, -1) || is_nan(x) {
        return nan();
    } else if is_inf(x, 1) {
        return inf(1);
    } else if x == 0.0 {
        if signbit(x) {
            return inf(-1);
        }
        return inf(1);
    }
    let mut x = x;
    let mut q = abs(x);
    let mut p = q.floor();
    if q > 33.0 {
        if x >= 0.0 {
            let (y1, y2) = stirling(x);
            return y1 * y2;
        }
        // Note: x is negative but (checked above) not a negative integer,
        // so x must be small enough to be in range for conversion to int64.
        // If |x| were >= 2⁶³ it would have to be an integer.
        let mut signgam = 1;
        let ip = p as i64;
        if ip & 1 == 0 {
            signgam = -1;
        }
        let mut z = q - p;
        if z > 0.5 {
            p += 1.0;
            z = q - p;
        }
        z = q * sin(PI * z);
        if z == 0.0 {
            return inf(signgam);
        }
        let (sq1, sq2) = stirling(q);
        let absz = abs(z);
        let d = absz * sq1 * sq2;
        if is_inf(d, 0) {
            z = PI / absz / sq1 / sq2;
        } else {
            z = PI / d;
        }
        return signgam as f64 * z;
    }

    // Reduce argument
    let mut z = 1.0;
    while x >= 3.0 {
        x -= 1.0;
        z *= x;
    }
    let small = loop {
        if x >= 0.0 {
            break false;
        }
        if x > -1e-09 {
            break true;
        }
        z /= x;
        x += 1.0;
    };
    let small = small
        || loop {
            if x >= 2.0 {
                break false;
            }
            if x < 1e-09 {
                break true;
            }
            z /= x;
            x += 1.0;
        };
    if small {
        if x == 0.0 {
            return inf(1);
        }
        return z / (EULER.mul_add(x, 1.0) * x);
    }

    if x == 2.0 {
        return z;
    }

    x -= 2.0;
    p = x
        .mul_add(GAM_P[0], GAM_P[1])
        .mul_add(x, GAM_P[2])
        .mul_add(x, GAM_P[3])
        .mul_add(x, GAM_P[4])
        .mul_add(x, GAM_P[5])
        .mul_add(x, GAM_P[6]);
    q = x
        .mul_add(GAM_Q[0], GAM_Q[1])
        .mul_add(x, GAM_Q[2])
        .mul_add(x, GAM_Q[3])
        .mul_add(x, GAM_Q[4])
        .mul_add(x, GAM_Q[5])
        .mul_add(x, GAM_Q[6])
        .mul_add(x, GAM_Q[7]);
    z * p / q
}

fn is_neg_int(x: f64) -> bool {
    if x < 0.0 {
        let (_, xf) = modf(x);
        return xf == 0.0;
    }
    false
}
