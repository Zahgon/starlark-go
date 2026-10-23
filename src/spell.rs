// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! A simple spelling checker for use in attribute errors such as
//! "no such field .foo; did you mean .food?" (ported from
//! go.starlark.net/internal/spell).

use crate::unicode;

/// Returns the element of candidates nearest to x using the Levenshtein
/// metric, or "" if none were promising.
pub fn nearest<S: AsRef<str>>(x: &str, candidates: &[S]) -> String {
    // Ignore underscores and case when matching.
    let fold = |s: &str| -> String {
        s.chars()
            .filter(|&r| r != '_')
            .map(unicode::to_lower)
            .collect()
    };

    let x = fold(x);

    let mut best = "";
    let mut best_d = x.len().div_ceil(2); // allow up to 50% typos
    for c in candidates {
        let c = c.as_ref();
        let d = levenshtein(x.as_bytes(), fold(c).as_bytes(), best_d);
        if d < best_d {
            best_d = d;
            best = c;
        }
    }
    best.to_string()
}

/// Returns the non-negative Levenshtein edit distance between the byte
/// strings x and y.
///
/// If the computed distance exceeds max, the function may return early
/// with an approximate value > max.
fn levenshtein(x: &[u8], y: &[u8], max: usize) -> usize {
    // This implementation is derived from one by Laurent Le Brun in
    // Bazel that uses the single-row space efficiency trick
    // described at bitbucket.org/clearer/iosifovich.

    // Let x be the shorter string.
    let (mut x, mut y) = if x.len() > y.len() { (y, x) } else { (x, y) };

    // Remove common prefix.
    for i in 0..x.len() {
        if x[i] != y[i] {
            x = &x[i..];
            y = &y[i..];
            break;
        }
    }
    if x.is_empty() {
        return y.len();
    }

    let d = y.len() - x.len();
    if d > max {
        return d; // excessive length divergence
    }

    let mut row: Vec<usize> = (0..=y.len()).collect();

    for i in 1..=x.len() {
        row[0] = i;
        let mut best = i;
        let mut prev = i - 1;
        for j in 1..=y.len() {
            let a = prev + usize::from(x[i - 1] != y[j - 1]); // substitution
            let b = 1 + row[j - 1]; // deletion
            let c = 1 + row[j]; // insertion
            let k = a.min(b.min(c));
            prev = row[j];
            row[j] = k;
            best = best.min(k);
        }
        if best > max {
            return best;
        }
    }
    row[y.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nearest() {
        assert_eq!(nearest("food", &["foo", "bar"]), "foo");
        assert_eq!(nearest("fo", &["xyz"]), "");
        assert_eq!(nearest("Foo_Bar", &["foobar", "baz"]), "foobar");
        assert_eq!(levenshtein(b"kitten", b"sitting", 10), 3);
    }
}
