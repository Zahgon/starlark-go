// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point hyperbolic tangent (tanh.go).
//!
//! The original C code, the long comment, and the constants below were
//! from http://netlib.sandia.gov/cephes/cmath/sin.c, available from
//! http://www.netlib.org/cephes/cmath.tgz.
//! Cephes Math Library Release 2.8:  June, 2000
//! Cephes code by Stephen L. Moshier (1984, 1987, 1989, 1992, 2000).

use super::{abs, exp};

const TANH_P: [f64; 3] = [
    -9.64399179425052238628e-1,
    -9.92877231001918586564e1,
    -1.61468768441708447952e3,
];
const TANH_Q: [f64; 3] = [
    1.12811678491632931402e2,
    2.23548839060100448583e3,
    4.84406305325125486048e3,
];

/// Returns the hyperbolic tangent of x.
///
/// Special cases are:
///
///   - tanh(±0) = ±0
///   - tanh(±Inf) = ±1
///   - tanh(NaN) = NaN
pub fn tanh(x: f64) -> f64 {
    const MAXLOG: f64 = 8.8029691931113054295988e+01; // log(2**127)
    let mut z = abs(x);
    if z > 0.5 * MAXLOG {
        if x < 0.0 {
            return -1.0;
        }
        return 1.0;
    } else if z >= 0.625 {
        let s = exp(2.0 * z);
        z = 1.0 - 2.0 / (s + 1.0);
        if x < 0.0 {
            z = -z;
        }
    } else {
        if x == 0.0 {
            return x;
        }
        let s = x * x;
        let num = (x * s) * TANH_P[0].mul_add(s, TANH_P[1]).mul_add(s, TANH_P[2]);
        // s+Q[0] is fused with the product x*x.
        let den = x
            .mul_add(x, TANH_Q[0])
            .mul_add(s, TANH_Q[1])
            .mul_add(s, TANH_Q[2]);
        z = x + num / den;
    }
    z
}
