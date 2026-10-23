// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Tests of the hash table (ported from starlark/hashtable_test.go).

use std::collections::HashMap;

use starlark::starlark::{call, iterate, universe_get, HashTable, Int, Thread, Value};

const TEST_ITERS: usize = 10000;

/// A small deterministic pseudo-random source (splitmix64). The Go test
/// uses math/rand seeded with 0; the exact sequence does not matter, as
/// the table is checked against a HashMap.
struct Rand(u64);

impl Rand {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Returns a float64 in [0, 1).
    fn float64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Generates Zipf distributed variates (Go's rand.Zipf).
struct Zipf {
    r: Rand,
    imax: f64,
    v: f64,
    q: f64,
    s: f64,
    one_minus_q: f64,
    one_minus_q_inv: f64,
    hxm: f64,
    hx0_minus_hxm: f64,
}

impl Zipf {
    fn h(&self, x: f64) -> f64 {
        (self.one_minus_q * (self.v + x).ln()).exp() * self.one_minus_q_inv
    }

    fn hinv(&self, x: f64) -> f64 {
        (self.one_minus_q_inv * (self.one_minus_q * x).ln()).exp() - self.v
    }

    /// Returns a Zipf variate generator: values k in [0, imax] such that
    /// P(k) is proportional to (v + k) ** (-s). Requires s > 1 and v >= 1.
    fn new(r: Rand, s: f64, v: f64, imax: u64) -> Zipf {
        let mut z = Zipf {
            r,
            imax: imax as f64,
            v,
            q: s,
            s: 0.0,
            one_minus_q: 1.0 - s,
            one_minus_q_inv: 1.0 / (1.0 - s),
            hxm: 0.0,
            hx0_minus_hxm: 0.0,
        };
        z.hxm = z.h(z.imax + 0.5);
        z.hx0_minus_hxm = z.h(0.5) - (z.v.ln() * (-z.q)).exp() - z.hxm;
        z.s = 1.0 - z.hinv(z.h(1.5) - (-z.q * (z.v + 1.0).ln()).exp());
        z
    }

    fn uint64(&mut self) -> u64 {
        let mut k;
        loop {
            let r = self.r.float64(); // r on [0,1]
            let ur = self.hxm + r * self.hx0_minus_hxm;
            let x = self.hinv(ur);
            k = (x + 0.5).floor();
            if k - x <= self.s {
                break;
            }
            if ur >= self.h(k + 0.5) - (-(k + self.v).ln() * self.q).exp() {
                break;
            }
        }
        k as u64
    }
}

/// Returns a zipf-distributed array of Ints and corresponding ints.
fn make_test_ints() -> Vec<(Int, i64)> {
    let mut zipf = Zipf::new(Rand(0), 1.1, 1.0, 1000);
    (0..3 * TEST_ITERS)
        .map(|_| {
            let r = zipf.uint64() as i64;
            (Int::from_i64(r), r)
        })
        .collect()
}

/// Tests the hash table against the semantics of a HashMap.
fn check_hashtable(test_ints: &[(Int, i64)], sane: &mut HashMap<i64, bool>) {
    let mut i = 0; // index into test_ints

    let ht = HashTable::new();

    // Insert 10000 random ints into the map.
    for _ in 0..TEST_ITERS {
        let k = &test_ints[i];
        i += 1;
        ht.insert(Value::Int(k.0.clone()), Value::None)
            .unwrap_or_else(|err| panic!("{}", err));
        sane.insert(k.1, true);
    }

    // Do 10000 random lookups in the map.
    for _ in 0..TEST_ITERS {
        let k = &test_ints[i];
        i += 1;
        let found = ht
            .lookup(&Value::Int(k.0.clone()))
            .unwrap_or_else(|err| panic!("{}", err))
            .is_some();
        let found2 = sane.contains_key(&k.1);
        assert_eq!(found, found2, "sanity check failed");
    }

    // Do 10000 random deletes from the map.
    for _ in 0..TEST_ITERS {
        let k = &test_ints[i];
        i += 1;
        let found = ht
            .delete(&Value::Int(k.0.clone()))
            .unwrap_or_else(|err| panic!("{}", err))
            .is_some();
        let found2 = sane.contains_key(&k.1);
        assert_eq!(found, found2, "sanity check failed");
        sane.remove(&k.1);
    }

    assert_eq!(ht.len(), sane.len(), "sanity check failed");
}

#[test]
fn test_hashtable() {
    check_hashtable(&make_test_ints(), &mut HashMap::new());
}

#[test]
fn test_hashtable_count() {
    let count = 1000;
    let ht = HashTable::new();
    for i in 0..count {
        ht.insert(Value::int(i), Value::None).unwrap();
    }

    let range = universe_get("range").expect("range");
    let r = call(&mut Thread::new(""), &range, &[Value::int(count)], &[]).unwrap();
    let mut it = iterate(&r).expect("iterable");
    match ht.count(&mut *it) {
        Err(err) => panic!("{}", err),
        Ok(c) => assert_eq!(
            c, count as usize,
            "count doesn't match: expected {} got {}",
            count, c
        ),
    }
}
