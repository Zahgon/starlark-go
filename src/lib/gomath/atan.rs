// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point arctangent (atan.go).
//!
//! The original C code, the long comment, and the constants below were
//! from http://netlib.sandia.gov/cephes/cmath/atan.c, available from
//! http://www.netlib.org/cephes/cmath.tgz.
//! Cephes Math Library Release 2.8:  June, 2000
//! Cephes code by Stephen L. Moshier (1984, 1987, 1989, 1992, 2000).

use super::PI;

/// Evaluates a series valid in the range [0, 0.66].
fn xatan(x: f64) -> f64 {
    const P0: f64 = -8.750608600031904122785e-01;
    const P1: f64 = -1.615753718733365076637e+01;
    const P2: f64 = -7.500855792314704667340e+01;
    const P3: f64 = -1.228866684490136173410e+02;
    const P4: f64 = -6.485021904942025371773e+01;
    const Q0: f64 = 2.485846490142306297962e+01;
    const Q1: f64 = 1.650270098316988542046e+02;
    const Q2: f64 = 4.328810604912902668951e+02;
    const Q3: f64 = 4.853903996359136964868e+02;
    const Q4: f64 = 1.945506571482613964425e+02;
    let z = x * x;
    let num = z * P0
        .mul_add(z, P1)
        .mul_add(z, P2)
        .mul_add(z, P3)
        .mul_add(z, P4);
    // z+Q0 is fused with the product x*x.
    let den = x
        .mul_add(x, Q0)
        .mul_add(z, Q1)
        .mul_add(z, Q2)
        .mul_add(z, Q3)
        .mul_add(z, Q4);
    x.mul_add(num / den, x)
}

/// Reduces its argument (known to be positive) to the range [0, 0.66]
/// and calls xatan.
pub(super) fn satan(x: f64) -> f64 {
    const MOREBITS: f64 = 6.123233995736765886130e-17; // pi/2 = PIO2 + Morebits
    const TAN3PIO8: f64 = 2.41421356237309504880; // tan(3*pi/8)
    if x <= 0.66 {
        return xatan(x);
    }
    if x > TAN3PIO8 {
        return PI / 2.0 - xatan(1.0 / x) + MOREBITS;
    }
    PI / 4.0 + xatan((x - 1.0) / (x + 1.0)) + 0.5 * MOREBITS
}

/// Returns the arctangent, in radians, of x.
///
/// Special cases are:
///
///   - atan(±0) = ±0
///   - atan(±Inf) = ±Pi/2
pub fn atan(x: f64) -> f64 {
    if x == 0.0 {
        return x;
    }
    if x > 0.0 {
        return satan(x);
    }
    -satan(-x)
}
