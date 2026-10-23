// Ported from the Go standard library: original work of The Go Authors
// (2018), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Payne-Hanek argument reduction for large trigonometric arguments
//! (trig_reduce.go).

use super::{BIAS, MASK, PI, SHIFT};

/// The minimum value of x for which the Payne-Hanek reduction is used.
/// This value is chosen to avoid inaccuracies in the Cody-Waite reduction.
pub const REDUCE_THRESHOLD: f64 = (1u64 << 29) as f64;

/// Go's shift semantics: shifting by 64 or more yields 0.
fn shl(x: u64, n: u32) -> u64 {
    x.checked_shl(n).unwrap_or(0)
}

fn shr(x: u64, n: u32) -> u64 {
    x.checked_shr(n).unwrap_or(0)
}

/// Implements Payne-Hanek range reduction by Pi/4 for x > 0. It returns
/// the integer part mod 8 (j) and the fractional part (z) of x / (Pi/4).
/// The implementation is based on "ARGUMENT REDUCTION FOR HUGE ARGUMENTS:
/// Good to the Last Bit", K. C. Ng et al, March 24, 1992. The simulated
/// multi-precision calculation of x*B uses 64-bit integer arithmetic.
pub fn trig_reduce(x: f64) -> (u64, f64) {
    const PI4: f64 = PI / 4.0;
    if x < PI4 {
        return (0, x);
    }
    // Extract out the integer and exponent such that, x = ix * 2 ** exp.
    let mut ix = x.to_bits();
    let exp = ((ix >> SHIFT) & MASK) as i64 - BIAS - SHIFT as i64;
    ix &= !(MASK << SHIFT);
    ix |= 1 << SHIFT;
    // Use the exponent to extract the 3 appropriate uint64 digits from
    // mPi4, B ~ (z0, z1, z2), such that the product leading digit has the
    // exponent -61. Note, exp >= -53 since x >= PI4 and exp < 971 for
    // maximum float64.
    let digit = ((exp + 61) as u64 / 64) as usize;
    let bitshift = ((exp + 61) as u64 % 64) as u32;
    let z0 = shl(M_PI4[digit], bitshift) | shr(M_PI4[digit + 1], 64 - bitshift);
    let z1 = shl(M_PI4[digit + 1], bitshift) | shr(M_PI4[digit + 2], 64 - bitshift);
    let z2 = shl(M_PI4[digit + 2], bitshift) | shr(M_PI4[digit + 3], 64 - bitshift);
    // Multiply mantissa by the digits and extract the upper two digits (hi, lo).
    let z2hi = ((z2 as u128 * ix as u128) >> 64) as u64;
    let p1 = z1 as u128 * ix as u128;
    let (z1hi, z1lo) = ((p1 >> 64) as u64, p1 as u64);
    let z0lo = z0.wrapping_mul(ix);
    let (lo, c) = z1lo.overflowing_add(z2hi);
    let mut hi = z0lo.wrapping_add(z1hi).wrapping_add(c as u64);
    // The top 3 bits of hi give j.
    let mut j = hi >> 61;
    // Use the remaining bits of hi and the top bits of lo to form z.
    hi = hi << 3 | lo >> 61;
    let lz = hi.leading_zeros();
    let e = (BIAS as u64).wrapping_sub(lz as u64 + 1);
    // Clear implicit mantissa bit and shift into place.
    hi = shl(hi, lz + 1) | shr(lo, 64 - (lz + 1));
    hi >>= 64 - SHIFT;
    // Include the exponent and convert to a float.
    hi |= e << SHIFT;
    let mut z = f64::from_bits(hi);
    // Map zeros to origin.
    if j & 1 == 1 {
        j += 1;
        j &= 7;
        z -= 1.0;
    }
    // Multiply the fractional part by pi/4.
    (j, z * PI4)
}

/// The binary digits of 4/pi as a uint64 array, that is, 4/pi = Sum
/// mPi4[i]*2^(-64*i). 19 64-bit digits and the leading one bit give 1217
/// bits of precision to handle the largest possible float64 exponent.
const M_PI4: [u64; 20] = [
    0x0000000000000001,
    0x45f306dc9c882a53,
    0xf84eafa3ea69bb81,
    0xb6c52b3278872083,
    0xfca2c757bd778ac3,
    0x6e48dc74849ba5c0,
    0x0c925dd413a32439,
    0xfc3bd63962534e7d,
    0xd1046bea5d768909,
    0xd338e04d68befc82,
    0x7323ac7306a673e9,
    0x3908bf177bf25076,
    0x3ff12fffbc0b301f,
    0xde5e2316b414da3e,
    0xda6cfd9e4f96136e,
    0x9e8c7ecd3cbfd45a,
    0xea4f758fd7cbe2f6,
    0x7a0e73ef14a525d4,
    0xd7f6bf623f1aba10,
    0xac06608df8f6d757,
];
