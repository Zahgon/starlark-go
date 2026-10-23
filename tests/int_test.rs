// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Tests of Int arithmetic (ported from starlark/int_test.go).

use num_bigint::BigInt;

use starlark::starlark::Int;

/// Exercises integer arithmetic, especially at the boundaries.
#[test]
fn test_int_opts() {
    let f = Int::from_i64;
    let (left, right) = (BigInt::from(i32::MIN), BigInt::from(i32::MAX));

    let tests = [
        // Add
        (f(i32::MAX as i64).add(&f(1)), "80000000"),
        (f(i32::MIN as i64).add(&f(-1)), "-80000001"),
        // Mul
        (
            f(i32::MAX as i64).mul(&f(i32::MAX as i64)),
            "3fffffff00000001",
        ),
        (
            f(i32::MIN as i64).mul(&f(i32::MIN as i64)),
            "4000000000000000",
        ),
        (
            f(u32::MAX as i64).mul(&f(u32::MAX as i64)),
            "fffffffe00000001",
        ),
        (f(i32::MIN as i64).mul(&f(-1)), "80000000"),
        // Div
        (f(i32::MIN as i64).div(&f(-1)), "80000000"),
        (f(1 << 31).div(&f(2)), "40000000"),
        // And
        (f(i32::MAX as i64).and(&f(i32::MAX as i64)), "7fffffff"),
        (f(i32::MIN as i64).and(&f(i32::MIN as i64)), "-80000000"),
        (f(1 << 33).and(&f(1 << 32)), "0"),
        // Mod
        (f(1 << 32).modulo(&f(2)), "0"),
        // Or
        (f(1 << 32).or(&f(0)), "100000000"),
        (f(i32::MAX as i64).or(&f(0)), "7fffffff"),
        (f(u32::MAX as i64).or(&f(0)), "ffffffff"),
        (f(i32::MIN as i64).or(&f(i32::MIN as i64)), "-80000000"),
        // Xor
        (f(i32::MIN as i64).xor(&f(-1)), "7fffffff"),
        // Not
        (f(i32::MIN as i64).not(), "7fffffff"),
        (f(i32::MAX as i64).not(), "-80000000"),
        // Shift
        (f(1).lsh(31), "80000000"),
        (f(1).lsh(32), "100000000"),
        (f(i32::MAX as i64 + 1).rsh(1), "40000000"),
        (f(i32::MIN as i64 * 2).rsh(1), "-80000000"),
    ];
    for (i, (val, want)) in tests.iter().enumerate() {
        let got = val.text(16);
        assert_eq!(got, *want, "{} equals {}, want {}", i, got, want);
        // The Go test inspects the (small, big) representation.
        match val {
            Int::Small(small) => {
                assert!(
                    i32::MIN as i64 <= *small && *small <= i32::MAX as i64,
                    "expected big, {} {}",
                    i,
                    val
                );
            }
            Int::Big(big) => {
                assert!(
                    !(**big >= left && **big <= right),
                    "expected small, {} {}",
                    i,
                    val
                );
            }
        }
    }
}

#[test]
fn test_immutability_make_big_int() {
    // use max int64 for the test
    let expect = i64::MAX;

    let mut mutint = BigInt::from(expect);
    let value = Int::from_bigint(mutint.clone());
    mutint.clone_from(&BigInt::from(1));
    assert_eq!(mutint, BigInt::from(1));

    let got = value.to_i64().unwrap_or_default();
    assert_eq!(got, expect, "expected {}, got {}", expect, got);
}

#[test]
fn test_immutability_big_int() {
    // use 1 and max int64 for the test
    for expect in [1, i64::MAX] {
        let value = Int::from_bigint(BigInt::from(expect));

        let mut bigint = value.to_bigint();
        bigint.clone_from(&BigInt::from(2));
        assert_eq!(bigint, BigInt::from(2));

        let got = value.to_i64().unwrap_or_default();
        assert_eq!(got, expect, "expected {}, got {}", expect, got);
    }
}

// TestIntFallback is not ported: it checks the fallback of Go's POSIX
// 64-bit Int optimization (an address-space reservation) under ulimit -v;
// the Rust Int has no such optimization.
