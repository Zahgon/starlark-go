// Ported from the Go standard library: original work of The Go Authors
// (2011), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Floating-point sine and cosine (sin.go).
//!
//! The original C code, the long comment, and the constants below were
//! from http://netlib.sandia.gov/cephes/cmath/sin.c, available from
//! http://www.netlib.org/cephes/cmath.tgz.
//! Cephes Math Library Release 2.8:  June, 2000
//! Cephes code by Stephen L. Moshier (1984, 1987, 1989, 1992, 2000).

use super::trig_reduce::{trig_reduce, REDUCE_THRESHOLD};
use super::{abs, is_inf, is_nan, nan, PI};

// sin coefficients
const SIN: [f64; 6] = [
    1.58962301576546568060e-10, // 0x3de5d8fd1fd19ccd
    -2.50507477628578072866e-8, // 0xbe5ae5e5a9291f5d
    2.75573136213857245213e-6,  // 0x3ec71de3567d48a1
    -1.98412698295895385996e-4, // 0xbf2a01a019bfdf03
    8.33333333332211858878e-3,  // 0x3f8111111110f7d0
    -1.66666666666666307295e-1, // 0xbfc5555555555548
];

// cos coefficients
const COS: [f64; 6] = [
    -1.13585365213876817300e-11, // 0xbda8fa49a0861a9b
    2.08757008419747316778e-9,   // 0x3e21ee9d7b4e3f05
    -2.75573141792967388112e-7,  // 0xbe927e4f7eac4bc6
    2.48015872888517045348e-5,   // 0x3efa01a019c844f5
    -1.38888888888730564116e-3,  // 0xbf56c16c16c14f91
    4.16666666666665929218e-2,   // 0x3fa555555555554b
];

// Pi/4 split into three parts
pub(super) const PI4A: f64 = 7.85398125648498535156e-1; // 0x3fe921fb40000000
pub(super) const PI4B: f64 = 3.77489470793079817668e-8; // 0x3e64442d00000000
pub(super) const PI4C: f64 = 2.69515142907905952645e-15; // 0x3ce8469898cc5170

/// Reduces x (0 <= x < reduceThreshold) to the octant j and the
/// remainder z, using Cody-Waite extended precision arithmetic.
/// The zeros and singularities are mapped to the origin.
pub(super) fn reduce(x: f64) -> (u64, f64) {
    let mut j = (x * (4.0 / PI)) as u64; // integer part of x/(Pi/4), as integer for tests on the phase angle
    let mut y = j as f64; // integer part of x/(Pi/4), as float
                          // map zeros to origin
    if j & 1 == 1 {
        j += 1;
        y += 1.0;
    }
    // Extended precision modular arithmetic
    let z = (-y).mul_add(PI4C, (-y).mul_add(PI4B, (-y).mul_add(PI4A, x)));
    (j, z)
}

/// The sine polynomial: z + z*zz*P(zz).
fn sin_poly(z: f64, zz: f64) -> f64 {
    let p = SIN[0]
        .mul_add(zz, SIN[1])
        .mul_add(zz, SIN[2])
        .mul_add(zz, SIN[3])
        .mul_add(zz, SIN[4])
        .mul_add(zz, SIN[5]);
    (z * zz).mul_add(p, z)
}

/// The cosine polynomial: 1 - 0.5*zz + zz*zz*Q(zz).
fn cos_poly(zz: f64) -> f64 {
    let q = COS[0]
        .mul_add(zz, COS[1])
        .mul_add(zz, COS[2])
        .mul_add(zz, COS[3])
        .mul_add(zz, COS[4])
        .mul_add(zz, COS[5]);
    (zz * zz).mul_add(q, (-0.5f64).mul_add(zz, 1.0))
}

/// Returns the cosine of the radian argument x.
///
/// Special cases are:
///
///   - cos(±Inf) = NaN
///   - cos(NaN) = NaN
pub fn cos(x: f64) -> f64 {
    // special cases
    if is_nan(x) || is_inf(x, 0) {
        return nan();
    }

    // make argument positive
    let mut sign = false;
    let x = abs(x);

    let (mut j, z) = if x >= REDUCE_THRESHOLD {
        trig_reduce(x)
    } else {
        let (j, z) = reduce(x);
        (j & 7, z) // octant modulo 2Pi radians (360 degrees)
    };

    // reflect in x axis
    if j > 3 {
        j -= 4;
        sign = !sign;
    }
    if j > 1 {
        sign = !sign;
    }

    let zz = z * z;
    let y = if j == 1 || j == 2 {
        sin_poly(z, zz)
    } else {
        cos_poly(zz)
    };
    if sign {
        -y
    } else {
        y
    }
}

/// Returns the sine of the radian argument x.
///
/// Special cases are:
///
///   - sin(±0) = ±0
///   - sin(±Inf) = NaN
///   - sin(NaN) = NaN
pub fn sin(x: f64) -> f64 {
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

    let (mut j, z) = if x >= REDUCE_THRESHOLD {
        trig_reduce(x)
    } else {
        let (j, z) = reduce(x);
        (j & 7, z) // octant modulo 2Pi radians (360 degrees)
    };

    // reflect in x axis
    if j > 3 {
        sign = !sign;
        j -= 4;
    }

    let zz = z * z;
    let y = if j == 1 || j == 2 {
        cos_poly(zz)
    } else {
        sin_poly(z, zz)
    };
    if sign {
        -y
    } else {
        y
    }
}
