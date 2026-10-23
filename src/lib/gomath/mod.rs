// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! A port of the parts of Go's math package used by the Starlark math
//! module, reproducing the results of Go on linux/arm64 bit for bit.
//!
//! The Go compiler for arm64 fuses `x*y + z`, `z - x*y` and `x*y - z`
//! (whenever an operand of an addition or subtraction is a product) into
//! single-rounding FMADD/FMSUB/FNMSUB instructions; the ports therefore use
//! `f64::mul_add` at exactly the places where the Go binary uses fused
//! instructions. Exp is implemented in assembly on arm64 (exp_arm64.s), and
//! Floor, Ceil, Trunc, Modf and Sqrt use exact hardware instructions.
//!
//! This file corresponds to bits.go, abs.go, copysign.go, signbit.go,
//! frexp.go, ldexp.go, modf_arm64.s and floor.go.

// The constants are kept exactly as written in the Go sources.
#![allow(clippy::excessive_precision, clippy::approx_constant)]

mod acosh;
mod asin;
mod asinh;
mod atan;
mod atan2;
mod atanh;
mod exp;
mod fmod;
mod gamma;
mod hypot;
mod log;
mod log1p;
mod pow;
mod remainder;
mod sin;
mod sinh;
mod tan;
mod tanh;
mod trig_reduce;

pub use acosh::acosh;
pub use asin::{acos, asin};
pub use asinh::asinh;
pub use atan::atan;
pub use atan2::atan2;
pub use atanh::atanh;
pub use exp::exp;
pub use fmod::fmod;
pub use gamma::gamma;
pub use hypot::hypot;
pub use log::log;
pub use log1p::log1p;
pub use pow::pow;
pub use remainder::remainder;
pub use sin::{cos, sin};
pub use sinh::{cosh, sinh};
pub use tan::tan;
pub use tanh::tanh;

/// The mathematical constant e (Go's math.E).
pub const E: f64 = 2.71828182845904523536028747135266249775724709369995957496696763;
/// The mathematical constant pi (Go's math.Pi).
pub const PI: f64 = 3.14159265358979323846264338327950288419716939937510582097494459;
/// The square root of 2 (Go's math.Sqrt2).
pub const SQRT2: f64 = 1.41421356237309504880168872420969807856967187537694807317667974;
/// The natural logarithm of 2 (Go's math.Ln2).
pub const LN2: f64 = 0.693147180559945309417232121458176568075500134360255254120680009;

const UVNAN: u64 = 0x7FF8000000000001;
const UVINF: u64 = 0x7FF0000000000000;
const UVNEGINF: u64 = 0xFFF0000000000000;
const UVONE: u64 = 0x3FF0000000000000;
const MASK: u64 = 0x7FF;
const SHIFT: u32 = 64 - 11 - 1;
const BIAS: i64 = 1023;
const SIGN_MASK: u64 = 1 << 63;
const FRAC_MASK: u64 = (1 << SHIFT) - 1;

/// Returns positive infinity if sign >= 0, negative infinity if sign < 0.
pub fn inf(sign: i32) -> f64 {
    f64::from_bits(if sign >= 0 { UVINF } else { UVNEGINF })
}

/// Returns an IEEE 754 "not-a-number" value (with Go's bit pattern).
pub fn nan() -> f64 {
    f64::from_bits(UVNAN)
}

/// Reports whether f is an IEEE 754 "not-a-number" value.
fn is_nan(f: f64) -> bool {
    f.is_nan()
}

/// Reports whether f is an infinity, according to sign: positive
/// infinity if sign > 0, negative infinity if sign < 0, either if 0.
fn is_inf(f: f64, sign: i32) -> bool {
    sign >= 0 && f > f64::MAX || sign <= 0 && f < -f64::MAX
}

/// Returns a normal number y and exponent exp satisfying x == y × 2**exp.
/// It assumes x is finite and non-zero.
fn normalize(x: f64) -> (f64, i64) {
    const SMALLEST_NORMAL: f64 = 2.2250738585072014e-308; // 2**-1022
    if abs(x) < SMALLEST_NORMAL {
        return (x * (1u64 << 52) as f64, -52);
    }
    (x, 0)
}

/// Returns the absolute value of x.
pub fn abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1 << 63))
}

/// Returns a value with the magnitude of f and the sign of sign.
pub fn copysign(f: f64, sign: f64) -> f64 {
    const SIGN_BIT: u64 = 1 << 63;
    f64::from_bits(f.to_bits() & !SIGN_BIT | sign.to_bits() & SIGN_BIT)
}

/// Reports whether x is negative or negative zero.
fn signbit(x: f64) -> bool {
    x.to_bits() & (1 << 63) != 0
}

/// Breaks f into a normalized fraction and an integral power of two.
/// It returns frac and exp satisfying f == frac × 2**exp, with the
/// absolute value of frac in the interval [½, 1).
fn frexp(f: f64) -> (f64, i64) {
    if f == 0.0 {
        return (f, 0); // correctly return -0
    }
    if is_inf(f, 0) || is_nan(f) {
        return (f, 0);
    }
    let (f, mut exp) = normalize(f);
    let mut x = f.to_bits();
    exp += ((x >> SHIFT) & MASK) as i64 - BIAS + 1;
    x &= !(MASK << SHIFT);
    x |= ((-1 + BIAS) as u64) << SHIFT;
    (f64::from_bits(x), exp)
}

/// The inverse of frexp: returns frac × 2**exp.
fn ldexp(frac: f64, exp: i64) -> f64 {
    if frac == 0.0 {
        return frac; // correctly return -0
    }
    if is_inf(frac, 0) || is_nan(frac) {
        return frac;
    }
    let (frac, e) = normalize(frac);
    let mut exp = exp.wrapping_add(e);
    let mut x = frac.to_bits();
    exp = exp.wrapping_add(((x >> SHIFT) & MASK) as i64 - BIAS);
    if exp < -1075 {
        return copysign(0.0, frac); // underflow
    }
    if exp > 1023 {
        // overflow
        if frac < 0.0 {
            return inf(-1);
        }
        return inf(1);
    }
    let mut m: f64 = 1.0;
    if exp < -1022 {
        // denormal
        exp += 53;
        m = 1.0 / (1u64 << 53) as f64; // 2**-53
    }
    x &= !(MASK << SHIFT);
    x |= ((exp + BIAS) as u64) << SHIFT;
    m * f64::from_bits(x)
}

/// Returns integer and fractional floating-point numbers that sum to f.
/// Both values have the same sign as f (arm64 assembly semantics).
fn modf(f: f64) -> (f64, f64) {
    let int = f.trunc();
    let frac = (f - int).to_bits() | (f.to_bits() & (1 << 63));
    (int, f64::from_bits(frac))
}

/// Returns the greatest integer value less than or equal to x.
pub fn floor(x: f64) -> f64 {
    x.floor()
}

/// Returns the least integer value greater than or equal to x.
pub fn ceil(x: f64) -> f64 {
    x.ceil()
}

/// Returns the square root of x.
pub fn sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// Returns the nearest integer, rounding half away from zero.
pub fn round(x: f64) -> f64 {
    let mut bits = x.to_bits();
    let mut e = (bits >> SHIFT) & MASK;
    let bias = BIAS as u64;
    if e < bias {
        // Round abs(x) < 1 including denormals.
        bits &= SIGN_MASK; // +-0
        if e == bias - 1 {
            bits |= UVONE; // +-1
        }
    } else if e < bias + SHIFT as u64 {
        // Round any abs(x) >= 1 containing a fractional component [0,1).
        const HALF: u64 = 1 << (SHIFT - 1);
        e -= bias;
        bits = bits.wrapping_add(HALF >> e);
        bits &= !(FRAC_MASK >> e);
    }
    f64::from_bits(bits)
}

#[cfg(test)]
mod tests {
    /// Results computed by Go 1.25 on linux/arm64, as
    /// (function, x bits, y bits (binary functions only), result bits).
    const GOLDEN: &[(&str, u64, u64, u64)] = &[
        ("fabs", 0xc05d69e5a47f1735, 0, 0x405d69e5a47f1735),
        ("fabs", 0x3fe7963648d29a32, 0, 0x3fe7963648d29a32),
        ("fabs", 0x40701f6f3ba80e92, 0, 0x40701f6f3ba80e92),
        ("fabs", 0x3fd444151694afe4, 0, 0x3fd444151694afe4),
        ("fabs", 0x3ff0000000001f37, 0, 0x3ff0000000001f37),
        ("fabs", 0xfcbce8511c737a16, 0, 0x7cbce8511c737a16),
        ("fabs", 0xc058800000000000, 0, 0x4058800000000000),
        ("fabs", 0x4028000000000000, 0, 0x4028000000000000),
        ("round", 0xc04f000000000000, 0, 0xc04f000000000000),
        ("round", 0xc07e5867d0c5ec3d, 0, 0xc07e600000000000),
        ("round", 0x3dd8775a6ae9b3d3, 0, 0x0),
        ("round", 0x1a56e1fc2f8f359, 0, 0x0),
        ("round", 0x4044e8abf4c22bcd, 0, 0x4045000000000000),
        ("round", 0xc060400000000000, 0, 0xc060400000000000),
        ("round", 0xc061400000000000, 0, 0xc061400000000000),
        ("round", 0x4022000000000000, 0, 0x4022000000000000),
        ("exp", 0x67f0fff8b50be688, 0, 0x7ff0000000000000),
        ("exp", 0xbfeb809855887bfa, 0, 0x3fdb18eb0e278040),
        ("exp", 0xc0603d77e30023e6, 0, 0x3437a6775c6f53b2),
        ("exp", 0xbfcadc4c2cd0fe18, 0, 0x3fe9f150de4976c6),
        ("exp", 0xc08836fd18c6e26c, 0, 0x0),
        ("exp", 0x40659933fcaf986d, 0, 0x4f836cae2ca0bb86),
        ("exp", 0xc041000000000000, 0, 0x3cdee001eed62aa0),
        ("exp", 0x3feffffffb70ce87, 0, 0x4005bf0a87fb26c2),
        ("sqrt", 0xac5bee0e4f760, 0, 0x1ffa41d2af0e8db3),
        ("sqrt", 0xc4805609c982e486, 0, 0x7ff8000000000000),
        ("sqrt", 0xc08e79f784d935df, 0, 0x7ff8000000000000),
        ("sqrt", 0x7ff8000000000001, 0, 0x7ff8000000000001),
        ("sqrt", 0xbfed3caf7a4668c7, 0, 0x7ff8000000000000),
        ("sqrt", 0x4fc643fd156f3fa9, 0, 0x47dab152c0b494d2),
        ("sqrt", 0x3efe2724be242193, 0, 0x3f75f6f43a7f02ae),
        ("sqrt", 0x40822abcab621ab6, 0, 0x40381c6cf22f93b1),
        ("acos", 0xc0217852e7b86062, 0, 0x7ff8000000000001),
        ("acos", 0x40647e8bd853281d, 0, 0x7ff8000000000001),
        ("acos", 0x4060300000000000, 0, 0x7ff8000000000001),
        ("acos", 0x404871fe0a72df84, 0, 0x7ff8000000000001),
        ("acos", 0x3feffffffffffffa, 0, 0x3e63988e14000000),
        ("acos", 0xc017fddec8861146, 0, 0x7ff8000000000001),
        ("acos", 0xbfc6094d3117c23c, 0, 0x3ffbe6acd2a0ff02),
        ("acos", 0x446c5ac21e791, 0, 0x3ff921fb54442d18),
        ("asin", 0x401e676b20a1e40c, 0, 0x7ff8000000000001),
        ("asin", 0x3cca0b41bdc46f58, 0, 0x3cca0b41bdc46f58),
        ("asin", 0x403d000000000000, 0, 0x7ff8000000000001),
        ("asin", 0x800072201e4c9222, 0, 0x800072201e4c9222),
        ("asin", 0xc050be8f58c2b96a, 0, 0x7ff8000000000001),
        ("asin", 0x3f500df7b253a3ad, 0, 0x3f500df7dd6e6ecb),
        ("asin", 0x403b800000000000, 0, 0x7ff8000000000001),
        ("asin", 0x8a6b135a0f92b03c, 0, 0x8a6b135a0f92b03c),
        ("atan", 0x54f0a458fe232071, 0, 0x3ff921fb54442d18),
        ("atan", 0xc0164b37deba5af1, 0, 0xbff64ace8e08714a),
        ("atan", 0xf4118c59caf5fb78, 0, 0xbff921fb54442d18),
        ("atan", 0xeeb719f68c5420fa, 0, 0xbff921fb54442d18),
        ("atan", 0x407aff80fbee7149, 0, 0x3ff9187fe666303c),
        ("atan", 0x3ff00cc5639d3704, 0, 0x3fe92ebba07042fd),
        ("atan", 0xc067395f612dcd88, 0, 0xbff90bef9813a16c),
        ("atan", 0x404d800000000000, 0, 0x3ff8dc908e5fe988),
        ("cos", 0x800e7720c68b0bed, 0, 0x3ff0000000000000),
        ("cos", 0x800236e08e96f17f, 0, 0x3ff0000000000000),
        ("cos", 0x3feffffffffffce8, 0, 0x3fe14a280fb50926),
        ("cos", 0xc07fa7f544489a9d, 0, 0xbfe874816e0d03af),
        ("cos", 0xfce16bfb108db3f7, 0, 0x3fc6511e2312bcb4),
        ("cos", 0x48d3de07615a09e8, 0, 0xbfe195c39681d6bf),
        ("cos", 0x406398313acf69b6, 0, 0x3fee56bfb279ff2e),
        ("cos", 0xbf9f1067b6ddaf79, 0, 0x3feffc3b18c65613),
        ("sin", 0xc01670d58e8fd020, 0, 0x3fe3f254630464cf),
        ("sin", 0x3fee1507a4014370, 0, 0x3fe9d7d719bae909),
        ("sin", 0x3f953095f73c638e, 0, 0x3f953032dc22efef),
        ("sin", 0x4068100000000000, 0, 0xbfe84f719492c529),
        ("sin", 0xaedd2c87b427b190, 0, 0xaedd2c87b427b190),
        ("sin", 0x3fe4000000000000, 0, 0x3fe2b91dea88421e),
        ("sin", 0xc05566f004748368, 0, 0x3fe6a09df0925e0c),
        ("sin", 0x6d7ae42a86dfe, 0, 0x6d7ae42a86dfe),
        ("tan", 0x6cb22eb46255e11, 0, 0x6cb22eb46255e11),
        ("tan", 0x4085db6223cf3a8c, 0, 0xc001fbe786340332),
        ("tan", 0x4040800000000000, 0, 0xc052d4086f3a61e2),
        ("tan", 0x405d800000000000, 0, 0xc014c490dc79fea7),
        ("tan", 0x3fefffffffffffbf, 0, 0x3ff8eb245cbee336),
        ("tan", 0x3fefff91630c8880, 0, 0x3ff8ea66ec9add58),
        ("tan", 0x4002b8d58f631606, 0, 0xbff084bc63b46b96),
        ("tan", 0x10264b3a265b7557, 0, 0x10264b3a265b7557),
        ("acosh", 0xc085b89e767dcc5c, 0, 0x7ff8000000000001),
        ("acosh", 0xc04cbe0941920af7, 0, 0x7ff8000000000001),
        ("acosh", 0x408820483d32f8fa, 0, 0x401d5e63a5ca3448),
        ("acosh", 0x4052c00000000000, 0, 0x40140ad853b0a83d),
        ("acosh", 0xbfd63b86c7c8f088, 0, 0x7ff8000000000001),
        ("acosh", 0x520a9be64a475, 0, 0x7ff8000000000001),
        ("acosh", 0xc086abe354e0f447, 0, 0x7ff8000000000001),
        ("acosh", 0x4063100000000000, 0, 0x4016e19697a2b375),
        ("asinh", 0x590118d13de44, 0, 0x590118d13de44),
        ("asinh", 0x40614e2eda009857, 0, 0x40167e965f08124f),
        ("asinh", 0x8002c0111b82b149, 0, 0x8002c0111b82b149),
        ("asinh", 0x46fde76bfdefd, 0, 0x46fde76bfdefd),
        ("asinh", 0x4066f00000000000, 0, 0x40179f191f60795d),
        ("asinh", 0x1d0a6157341bb232, 0, 0x1d0a6157341bb232),
        ("asinh", 0x5bb0b162dbb7caf9, 0, 0x407347e2e1a8f09d),
        ("asinh", 0x3feb885784fc38ee, 0, 0x3fe8eeaf0e5fb9f8),
        ("atanh", 0x3e64f938a2e91c14, 0, 0x3e64f938a2e91c17),
        ("atanh", 0x404c000000000000, 0, 0x7ff8000000000001),
        ("atanh", 0x3fefd8e0346045dc, 0, 0x400823c44bdae521),
        ("atanh", 0xc03d000000000000, 0, 0x7ff8000000000001),
        ("atanh", 0xc06be8e5001396c5, 0, 0x7ff8000000000001),
        ("atanh", 0x405420a34f6ebf4a, 0, 0x7ff8000000000001),
        ("atanh", 0xbfe033b5de6fc257, 0, 0xbfe1d927fbe2f1ad),
        ("atanh", 0xc00d05d376aca3e8, 0, 0x7ff8000000000001),
        ("cosh", 0xc08d26ee71db7108, 0, 0x7ff0000000000000),
        ("cosh", 0xf0949d9ad235bee8, 0, 0x7ff0000000000000),
        ("cosh", 0x40786ad150e1f34c, 0, 0x6318b35c3c29455b),
        ("cosh", 0x43e0000000000000, 0, 0x7ff0000000000000),
        ("cosh", 0x4052359260919dfd, 0, 0x4670ee8ae62c479a),
        ("cosh", 0xc0799b48122491a7, 0, 0x64d0e812e7aee8e4),
        ("cosh", 0x3c3baa1f55f8cf26, 0, 0x3ff0000000000000),
        ("cosh", 0x40718986376c4ba0, 0, 0x592c1e1a979eaf7d),
        ("sinh", 0x3febea76d05c2f3c, 0, 0x3fef9808e1c2a76f),
        ("sinh", 0xc095b2cc90ddb612, 0, 0xfff0000000000000),
        ("sinh", 0x4067c41d9e242b77, 0, 0x5103aa6c35ad690d),
        ("sinh", 0xc07be1b2d1bae593, 0, 0xe8182b7f8877a3fe),
        ("sinh", 0x3feffffffffffff4, 0, 0x3ff2cd9fc44eb979),
        ("sinh", 0x625cfe4ef5caa00b, 0, 0x7ff0000000000000),
        ("sinh", 0xdd2b05ab183be0ac, 0, 0xfff0000000000000),
        ("sinh", 0xc0851d2c97916b40, 0, 0xfccaf35f677805cd),
        ("tanh", 0xae9aa5602b95ba79, 0, 0xae9aa5602b95ba79),
        ("tanh", 0xbfed4818fecc3878, 0, 0xbfe7274d9a4540f5),
        ("tanh", 0xc06dcade7aebd20f, 0, 0xbff0000000000000),
        ("tanh", 0x3ea5ea9ab405e, 0, 0x3ea5ea9ab405e),
        ("tanh", 0xbfece9d9768b95ba, 0, 0xbfe6fa055cadd666),
        ("tanh", 0xdf42d5f07b3a4ce8, 0, 0xbff0000000000000),
        ("tanh", 0xc0124b3b1eb822be, 0, 0xbfeffe41297beb59),
        ("tanh", 0x2c4fa9e30c2b8e06, 0, 0x2c4fa9e30c2b8e06),
        ("log", 0x3fed6fd72dce8930, 0, 0xbfb55f7304fa9a26),
        ("log", 0xbfdc7adcb785a824, 0, 0x7ff8000000000001),
        ("log", 0x404a400000000000, 0, 0x400fafbed0a7e6cf),
        ("log", 0x4829fbc34048ef14, 0, 0x4056d260b1536ae7),
        ("log", 0x226269f4461c644, 0, 0xc08569940431ccca),
        ("log", 0xc066c0321c3275ee, 0, 0x7ff8000000000001),
        ("log", 0xbfcdb75efba5c834, 0, 0x7ff8000000000001),
        ("log", 0xfff0000000000000, 0, 0x7ff8000000000001),
        ("gamma", 0x3fda7f2466124d50, 0, 0x400121ba86689a8b),
        ("gamma", 0xbfe0000000000000, 0, 0xc00c5bf891b4ef6a),
        ("gamma", 0x40679b30a5a0f2f4, 0, 0x7ff0000000000000),
        ("gamma", 0x4f0bfa2f1292847d, 0, 0x7ff0000000000000),
        ("gamma", 0x8002cc57a2163f2c, 0, 0xfff0000000000000),
        ("gamma", 0x3fda273012262d5c, 0, 0x40015c885b9b1e7a),
        ("gamma", 0x2c128ce1817aaa8e, 0, 0x53cb99c1735812b4),
        ("gamma", 0x3fbd80440935a49b, 0, 0x4020684dc70441c5),
        ("floor", 0x3d8763f7257c7dc9, 0, 0x0),
        ("floor", 0x40671a4ea1180a56, 0, 0x4067000000000000),
        ("floor", 0x4ee68e9a3985ad12, 0, 0x4ee68e9a3985ad12),
        ("floor", 0xc0159e40206488ce, 0, 0xc018000000000000),
        ("floor", 0xc55650339d487f30, 0, 0xc55650339d487f30),
        ("floor", 0xbfdbd05930f538c0, 0, 0xbff0000000000000),
        ("floor", 0x3dec57bbc1580a06, 0, 0x0),
        ("floor", 0xc053234b40e68d2a, 0, 0xc053400000000000),
        ("ceil", 0x402d000000000000, 0, 0x402e000000000000),
        ("ceil", 0x3fab68d2a7997120, 0, 0x3ff0000000000000),
        ("ceil", 0xc0624eaa97683270, 0, 0xc062400000000000),
        ("ceil", 0x456517b3650e3651, 0, 0x456517b3650e3651),
        ("ceil", 0x401a44995f679e5a, 0, 0x401c000000000000),
        ("ceil", 0x3d448c52d6f33f66, 0, 0x3ff0000000000000),
        ("ceil", 0x7fefffffffffffff, 0, 0x7fefffffffffffff),
        ("ceil", 0x407fd90db9837353, 0, 0x407fe00000000000),
        ("log1p", 0x4061e00000000000, 0, 0x4013e116bcd39e7d),
        ("log1p", 0xc08748cccccccccd, 0, 0x7ff8000000000001),
        ("log1p", 0xc084ef8e9bc473be, 0, 0x7ff8000000000001),
        ("log1p", 0x490c976ebb056dde, 0, 0x4059458f2a3bec39),
        ("log1p", 0x3e644644eb02f63d, 0, 0x3e644644e496be06),
        ("log1p", 0xc037800000000000, 0, 0x7ff8000000000001),
        ("log1p", 0x53db9f1d29211, 0, 0x53db9f1d29211),
        ("log1p", 0xc068b00000000000, 0, 0x7ff8000000000001),
        (
            "copysign",
            0x3ffc83ce863ffee0,
            0xd639609bf3974777,
            0xbffc83ce863ffee0,
        ),
        (
            "copysign",
            0x3cc996e031049581,
            0x7ff0000000000000,
            0x3cc996e031049581,
        ),
        (
            "copysign",
            0x3ff0000000000039,
            0xffefffffffffffff,
            0xbff0000000000039,
        ),
        (
            "copysign",
            0xc062800000000000,
            0xd1964965c2e94,
            0x4062800000000000,
        ),
        (
            "copysign",
            0x4010e1d1275207bb,
            0x80079f8001d03d97,
            0xc010e1d1275207bb,
        ),
        (
            "copysign",
            0x640583420c25f,
            0x3fe2363e0e4d6638,
            0x640583420c25f,
        ),
        (
            "copysign",
            0x3e58f471fd89dc10,
            0xc0214b046664d612,
            0xbe58f471fd89dc10,
        ),
        (
            "copysign",
            0xc012000000000000,
            0x3ff00000000000c2,
            0x4012000000000000,
        ),
        (
            "mod",
            0xf72356c6b7789e68,
            0xc030000000000000,
            0x8000000000000000,
        ),
        (
            "mod",
            0x405e000000000000,
            0x405ce00000000000,
            0x4012000000000000,
        ),
        (
            "mod",
            0x406587e63b4bfe07,
            0xc05fa0a0241b582b,
            0x4046de58a4f947c6,
        ),
        (
            "mod",
            0xc084d878c3467690,
            0xc0737cd96122d352,
            0xc045b9f6223a33e0,
        ),
        (
            "mod",
            0x3fef606183e7ceb0,
            0x7ff0000000000000,
            0x3fef606183e7ceb0,
        ),
        (
            "mod",
            0x3d50800722f7f085,
            0x401d8e615cb955f5,
            0x3d50800722f7f085,
        ),
        (
            "mod",
            0x41c0000000000000,
            0x3172ae3d82a85da6,
            0x315f9e731343a0d0,
        ),
        (
            "mod",
            0x1a56e1fc2f8f359,
            0x8008f7cda6e0e9e8,
            0x3ba7565da10d8,
        ),
        (
            "pow",
            0x4048400000000000,
            0x3feddbee33d9cb70,
            0x4042b42e2a88634d,
        ),
        (
            "pow",
            0x400a267904054904,
            0xc05439221f012d25,
            0x374b5d2bf70a168f,
        ),
        (
            "pow",
            0x4050a00000000000,
            0x4070cf72560394e0,
            0x7ff0000000000000,
        ),
        (
            "pow",
            0x4065800000000000,
            0x408179a0c0a01a42,
            0x7ff0000000000000,
        ),
        (
            "pow",
            0x407ac0ac077f54b7,
            0x3fefffffffffffd8,
            0x407ac0ac077f53ed,
        ),
        (
            "pow",
            0xbfbfb025b0c26398,
            0x3fefffffffe8c6ef,
            0x7ff8000000000001,
        ),
        (
            "pow",
            0x3ff364d3e53a9ff1,
            0x405c44483325c8c7,
            0x41e4cf6fbe10cb5f,
        ),
        (
            "pow",
            0xc002126b4e49d738,
            0x4062800000000000,
            0x4ad001ee1564a1e6,
        ),
        (
            "remainder",
            0x4054200000000000,
            0xc0803ab08ba10664,
            0x4054200000000000,
        ),
        (
            "remainder",
            0x800640fde92d1eda,
            0xbfd542a654e08308,
            0x800640fde92d1eda,
        ),
        (
            "remainder",
            0xfa116a479a9d927c,
            0xc052600000000000,
            0x4023000000000000,
        ),
        (
            "remainder",
            0x5a40b60853424141,
            0x6530037de8e12417,
            0x5a40b60853424141,
        ),
        (
            "remainder",
            0xc052400000000000,
            0xbfd4d9550b929090,
            0xbf9d595dfc182000,
        ),
        (
            "remainder",
            0x404b800000000000,
            0x4054600000000000,
            0xc03a800000000000,
        ),
        (
            "remainder",
            0xd4ffd81c01eeb,
            0x4008000000000000,
            0xd4ffd81c01eeb,
        ),
        (
            "remainder",
            0x4040c00000000000,
            0xffefffffffffffff,
            0x4040c00000000000,
        ),
        (
            "atan2",
            0x3feffd2d2f807147,
            0x4068700000000000,
            0x3f74f1aa2ca04c69,
        ),
        (
            "atan2",
            0x4066600000000000,
            0xc01f2044a419e198,
            0x3ff9d3ee7b6813bc,
        ),
        (
            "atan2",
            0x3d1ca646d04b8140,
            0x4040800000000000,
            0x3ccbc8069b700136,
        ),
        (
            "atan2",
            0xd0e660ea4b8af685,
            0xbfdd9d1817a67e18,
            0xbff921fb54442d18,
        ),
        (
            "atan2",
            0x4059600000000000,
            0x4fa7e52df26ff9fe,
            0x30a0fda77fde2b1a,
        ),
        (
            "atan2",
            0x4064600000000000,
            0x3dd6e2390bf45b11,
            0x3ff921fb5444241c,
        ),
        (
            "atan2",
            0x40225344167994ab,
            0xbfecd7173a1d6fe2,
            0x3ffab394d78a0b78,
        ),
        (
            "atan2",
            0x3ff00000001189e5,
            0x4047dc58c0119bd3,
            0x3f95746804f496a3,
        ),
        (
            "hypot",
            0xc056c00000000000,
            0xbfe0000000000000,
            0x4056c016815cf4cf,
        ),
        (
            "hypot",
            0x3fd47a686b2d38f8,
            0x8006774e4f20ece8,
            0x3fd47a686b2d38f8,
        ),
        (
            "hypot",
            0xc056e00000000000,
            0x4061c94f4a5f507e,
            0x4065257c02752b46,
        ),
        (
            "hypot",
            0x40229f373e7dcf2a,
            0x3fe0000000000000,
            0x4022a6159f5aac17,
        ),
        (
            "hypot",
            0xbfc4dae073c95d08,
            0xcf8f3ff922d23b1a,
            0x4f8f3ff922d23b1a,
        ),
        (
            "hypot",
            0x4056e00000000000,
            0xc0813034bc809614,
            0x40816cad3dea8c49,
        ),
        (
            "hypot",
            0xd1de5a0a9697a,
            0x405c05dd3fa82384,
            0x405c05dd3fa82384,
        ),
        (
            "hypot",
            0xbfede5ecbd4f878c,
            0x4067100000000000,
            0x40671013613a8d50,
        ),
    ];

    #[test]
    fn matches_go() {
        for &(name, x, y, want) in GOLDEN {
            let (x, y) = (f64::from_bits(x), f64::from_bits(y));
            let got = match name {
                "fabs" => super::abs(x),
                "round" => super::round(x),
                "exp" => super::exp(x),
                "sqrt" => super::sqrt(x),
                "acos" => super::acos(x),
                "asin" => super::asin(x),
                "atan" => super::atan(x),
                "cos" => super::cos(x),
                "sin" => super::sin(x),
                "tan" => super::tan(x),
                "acosh" => super::acosh(x),
                "asinh" => super::asinh(x),
                "atanh" => super::atanh(x),
                "cosh" => super::cosh(x),
                "sinh" => super::sinh(x),
                "tanh" => super::tanh(x),
                "log" => super::log(x),
                "gamma" => super::gamma(x),
                "floor" => super::floor(x),
                "ceil" => super::ceil(x),
                "log1p" => super::log1p(x),
                "copysign" => super::copysign(x, y),
                "mod" => super::fmod(x, y),
                "pow" => super::pow(x, y),
                "remainder" => super::remainder(x, y),
                "atan2" => super::atan2(x, y),
                "hypot" => super::hypot(x, y),
                _ => unreachable!("unknown function {name}"),
            };
            let want = f64::from_bits(want);
            if want.is_nan() {
                assert!(got.is_nan(), "{name}({x:e}, {y:e}) = {got:e}, want NaN");
            } else {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{name}({x:e}, {y:e}) = {got:e}, want {want:e}"
                );
            }
        }
    }
}
