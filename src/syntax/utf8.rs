// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! UTF-8 helpers with Go's semantics for invalid input.
//!
//! Starlark strings are arbitrary byte sequences. Like Go's unicode/utf8
//! package, decoding an invalid or truncated sequence yields U+FFFD with a
//! width of one byte.

/// The replacement character returned for invalid input.
pub const RUNE_ERROR: char = '\u{FFFD}';

/// Decodes the first UTF-8 sequence in `s`, returning the rune and its width
/// in bytes. An empty input yields (RUNE_ERROR, 0); an invalid sequence
/// yields (RUNE_ERROR, 1).
pub fn decode_rune(s: &[u8]) -> (char, usize) {
    let Some(&b0) = s.first() else {
        return (RUNE_ERROR, 0);
    };
    if b0 < 0x80 {
        return (b0 as char, 1);
    }
    let (len, min, init) = match b0 {
        0xC2..=0xDF => (2, 0x80, (b0 & 0x1F) as u32),
        0xE0..=0xEF => (3, 0x800, (b0 & 0x0F) as u32),
        0xF0..=0xF4 => (4, 0x10000, (b0 & 0x07) as u32),
        _ => return (RUNE_ERROR, 1),
    };
    if s.len() < len {
        return (RUNE_ERROR, 1);
    }
    let mut r = init;
    for &b in &s[1..len] {
        if b & 0xC0 != 0x80 {
            return (RUNE_ERROR, 1);
        }
        r = (r << 6) | (b & 0x3F) as u32;
    }
    if r < min || r > 0x10FFFF || (0xD800..=0xDFFF).contains(&r) {
        return (RUNE_ERROR, 1);
    }
    (char::from_u32(r).unwrap_or(RUNE_ERROR), len)
}

/// Decodes the last UTF-8 sequence in `s` (Go's utf8.DecodeLastRune).
pub fn decode_last_rune(s: &[u8]) -> (char, usize) {
    if s.is_empty() {
        return (RUNE_ERROR, 0);
    }
    let end = s.len();
    let lim = end.saturating_sub(4);
    let mut start = end - 1;
    while start > lim && s[start] & 0xC0 == 0x80 {
        start -= 1;
    }
    let (r, size) = decode_rune(&s[start..end]);
    if start + size != end {
        return (RUNE_ERROR, 1);
    }
    (r, size)
}

/// Appends the UTF-8 encoding of `r` to `buf`. Surrogates and out-of-range
/// values cannot occur in a Rust `char`.
pub fn push_rune(buf: &mut Vec<u8>, r: char) {
    let mut tmp = [0u8; 4];
    buf.extend_from_slice(r.encode_utf8(&mut tmp).as_bytes());
}

/// Reports whether `s` is entirely valid UTF-8.
pub fn valid(s: &[u8]) -> bool {
    std::str::from_utf8(s).is_ok()
}
