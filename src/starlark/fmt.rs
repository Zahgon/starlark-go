// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Helpers that reproduce the output of Go's strconv and fmt packages,
//! on which the original implementation's messages and value
//! representations depend.

use crate::syntax::utf8;
use crate::unicode;

/// A decimal digit string `d` (no leading zeros; empty for zero) whose
/// decimal point is at position `dp`, i.e. the value is 0.d × 10^dp.
struct Decimal {
    d: Vec<u8>,
    dp: i32,
}

/// Parses the output of Rust's `{:e}` formatting of a non-negative finite
/// float ("1.2345e-5") into a Decimal.
fn parse_exp(s: &str) -> Decimal {
    let (mant, exp) = s.split_once('e').unwrap_or((s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut d: Vec<u8> = mant.bytes().filter(|&c| c != b'.').collect();
    while d.last() == Some(&b'0') {
        d.pop();
    }
    if d.is_empty() {
        return Decimal { d, dp: 0 };
    }
    Decimal { d, dp: exp + 1 }
}

/// Shortest decimal that round-trips to `f` (non-negative, finite).
fn shortest(f: f64) -> Decimal {
    let d = parse_exp(&format!("{:e}", f));
    if d.d.is_empty() {
        return d;
    }
    // Rust finds the shortest digit count n, but when two n-digit
    // candidates are equally close it may pick either; Go picks the even
    // one. The correctly rounded (half-even) n-digit form is at least as
    // close, so it also round-trips: use it.
    significant(f, d.d.len())
}

/// The decimal for `f` (non-negative, finite) rounded to `n` >= 1
/// significant digits.
fn significant(f: f64, n: usize) -> Decimal {
    let mut dec = parse_exp(&format!("{:.*e}", n - 1, f));
    if dec.d.is_empty() {
        dec.dp = 0;
    }
    dec
}

/// Formats a float like Go's strconv.FormatFloat(f, fmt, prec, 64), for
/// fmt in 'e', 'E', 'f', 'F', 'g', 'G'. A negative precision means the
/// smallest number of digits necessary to represent the value uniquely.
pub fn format_float(f: f64, fmt: u8, prec: i32) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.to_string();
    }
    if !matches!(fmt, b'e' | b'E' | b'f' | b'g' | b'G') {
        // Go appends '%' and the verb, without a sign, for an unknown format.
        return format!("%{}", fmt as char);
    }
    let neg = f.is_sign_negative();
    let a = f.abs();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    let shortest_mode = prec < 0;
    match fmt {
        b'e' | b'E' => {
            let (digs, p) = if shortest_mode {
                let d = shortest(a);
                let p = (d.d.len() as i32 - 1).max(0);
                (d, p)
            } else {
                (significant(a, prec as usize + 1), prec)
            };
            fmt_e(&mut out, &digs, p, fmt);
        }
        b'f' => {
            if shortest_mode {
                let d = shortest(a);
                let p = (d.d.len() as i32 - d.dp).max(0);
                fmt_f(&mut out, &d, p);
            } else {
                out.push_str(&format!("{:.*}", prec as usize, a));
            }
        }
        b'g' | b'G' => {
            let mut prec = prec;
            let digs = if shortest_mode {
                let d = shortest(a);
                prec = d.d.len() as i32;
                d
            } else {
                if prec == 0 {
                    prec = 1;
                }
                significant(a, prec as usize)
            };
            let mut eprec = prec;
            let nd = digs.d.len() as i32;
            if eprec > nd && nd >= digs.dp {
                eprec = nd;
            }
            // %e is used if the exponent from the conversion
            // is less than -4 or greater than or equal to the precision.
            // if precision was the shortest possible, use precision 6 for this decision.
            if shortest_mode {
                eprec = 6;
            }
            let exp = digs.dp - 1;
            if exp < -4 || exp >= eprec {
                if prec > nd {
                    prec = nd;
                }
                fmt_e(&mut out, &digs, prec - 1, fmt + b'e' - b'g');
            } else {
                if prec > digs.dp {
                    prec = nd;
                }
                fmt_f(&mut out, &digs, (prec - digs.dp).max(0));
            }
        }
        _ => {
            out.push('%');
            out.push(fmt as char);
        }
    }
    out
}

/// %e: -d.ddddde±dd
fn fmt_e(out: &mut String, d: &Decimal, prec: i32, fmt: u8) {
    let nd = d.d.len() as i32;
    // first digit
    out.push(if nd == 0 { '0' } else { d.d[0] as char });
    // .moredigits
    if prec > 0 {
        out.push('.');
        let mut i = 1;
        let m = nd.min(prec + 1);
        if i < m {
            for &c in &d.d[i as usize..m as usize] {
                out.push(c as char);
            }
            i = m;
        }
        while i <= prec {
            out.push('0');
            i += 1;
        }
    }
    // e±
    out.push(fmt as char);
    let mut exp = d.dp - 1;
    if nd == 0 {
        // special case: 0 has exponent 0
        exp = 0;
    }
    if exp < 0 {
        out.push('-');
        exp = -exp;
    } else {
        out.push('+');
    }
    // dd or ddd
    if exp < 10 {
        out.push('0');
    }
    out.push_str(&exp.to_string());
}

/// %f: -ddddddd.ddddd
fn fmt_f(out: &mut String, d: &Decimal, prec: i32) {
    let nd = d.d.len() as i32;
    // integer, padded with zeros as needed.
    if d.dp > 0 {
        let m = nd.min(d.dp);
        for &c in &d.d[..m as usize] {
            out.push(c as char);
        }
        for _ in m..d.dp {
            out.push('0');
        }
    } else {
        out.push('0');
    }
    // fraction
    if prec > 0 {
        out.push('.');
        for i in 1..=prec {
            let j = d.dp + i - 1;
            let ch = if 0 <= j && j < nd {
                d.d[j as usize] as char
            } else {
                '0'
            };
            out.push(ch);
        }
    }
}

/// Formats a float like Go's fmt `%v` verb (shortest %g, but with
/// exponent thresholds -4 and 21).
pub fn format_float_v(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.to_string();
    }
    let a = f.abs();
    let d = shortest(a);
    let exp = d.dp - 1;
    let mut out = String::new();
    if f.is_sign_negative() {
        out.push('-');
    }
    if !(-4..21).contains(&exp) {
        let p = (d.d.len() as i32 - 1).max(0);
        fmt_e(&mut out, &d, p, b'e');
    } else {
        let p = (d.d.len() as i32 - d.dp).max(0);
        fmt_f(&mut out, &d, p);
    }
    out
}

/// Quotes a string like Go's strconv.Quote (used by `%q`): the result
/// uses Go escape sequences for non-printable characters and invalid
/// UTF-8 bytes.
pub fn go_quote(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut i = 0;
    while i < s.len() {
        let (r, width) = utf8::decode_rune(&s[i..]);
        if width == 1 && r == utf8::RUNE_ERROR {
            out.push_str(&format!("\\x{:02x}", s[i]));
            i += 1;
            continue;
        }
        append_escaped_rune(&mut out, r);
        i += width;
    }
    out.push('"');
    out
}

fn append_escaped_rune(out: &mut String, r: char) {
    if r == '"' || r == '\\' {
        out.push('\\');
        out.push(r);
        return;
    }
    if unicode::is_print(r) {
        out.push(r);
        return;
    }
    match r {
        '\x07' => out.push_str("\\a"),
        '\x08' => out.push_str("\\b"),
        '\x0c' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\x0b' => out.push_str("\\v"),
        _ => {
            let c = r as u32;
            if c < ' ' as u32 || c == 0x7f {
                out.push_str(&format!("\\x{:02x}", c));
            } else if c < 0x10000 {
                out.push_str(&format!("\\u{:04x}", c));
            } else {
                out.push_str(&format!("\\U{:08x}", c));
            }
        }
    }
}

/// Parses a decimal floating-point literal like Go's strconv.ParseFloat
/// (bitSize 64). Returns the value and whether the syntax was valid; for
/// out-of-range values the result is ±Inf with a valid syntax.
pub fn parse_float(s: &[u8]) -> Option<f64> {
    let st = std::str::from_utf8(s).ok()?;
    if st.is_empty() {
        return None;
    }
    let body = st.strip_prefix(['+', '-']).unwrap_or(st);
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" || lower == "nan" {
        let v = if lower == "nan" {
            f64::NAN
        } else {
            f64::INFINITY
        };
        return Some(if st.starts_with('-') { -v } else { v });
    }
    if st.contains('_') {
        // Go accepts underscores between digits (strconv's underscoreOK) and ignores them.
        if !underscore_ok(st.as_bytes()) {
            return None;
        }
        return parse_float(st.replace('_', "").as_bytes());
    }
    if lower.starts_with("0x") {
        return parse_hex_float(st);
    }
    // Validate the decimal syntax: digits [. digits] [e [+-] digits].
    let b = body.as_bytes();
    let mut i = 0;
    let mut ndig = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        ndig += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            ndig += 1;
        }
    }
    if ndig == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    st.parse::<f64>().ok()
}

/// Reports whether the underscores in a number are digit separators, as in
/// Go's strconv.underscoreOK: each must sit between digits (or after a base
/// prefix).
fn underscore_ok(s: &[u8]) -> bool {
    let mut s = s;
    if let [b'+' | b'-', rest @ ..] = s {
        s = rest;
    }
    let mut saw = b'^';
    let mut i = 0;
    let mut hex = false;
    if s.len() >= 2 && s[0] == b'0' && matches!(s[1].to_ascii_lowercase(), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = s[1].eq_ignore_ascii_case(&b'x');
    }
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_digit() || (hex && c.is_ascii_hexdigit()) {
            saw = b'0';
        } else if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
        } else {
            if saw == b'_' {
                return false;
            }
            saw = b'!';
        }
        i += 1;
    }
    saw != b'_'
}

/// Parses a hexadecimal floating-point literal (0x1.8p3) as Go does.
fn parse_hex_float(st: &str) -> Option<f64> {
    let neg = st.starts_with('-');
    let body = st.strip_prefix(['+', '-']).unwrap_or(st);
    let b = &body.as_bytes()[2..];
    let mut i = 0;
    let mut mant: u128 = 0;
    let mut exp: i64 = 0;
    let mut ndig = 0;
    let mut seen_dot = false;
    let mut overflow_digits = false;
    while i < b.len() {
        let c = b[i];
        if c == b'_' {
            i += 1;
            continue;
        }
        if c == b'.' {
            if seen_dot {
                return None;
            }
            seen_dot = true;
            i += 1;
            continue;
        }
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => break,
        };
        ndig += 1;
        if mant >> 120 == 0 {
            mant = mant * 16 + d as u128;
            if seen_dot {
                exp -= 4;
            }
        } else {
            if d != 0 {
                overflow_digits = true;
            }
            if !seen_dot {
                exp += 4;
            }
        }
        i += 1;
    }
    if ndig == 0 {
        return None;
    }
    // Go requires a 'p' exponent for hex floats.
    if i >= b.len() || (b[i] != b'p' && b[i] != b'P') {
        return None;
    }
    i += 1;
    let mut esign = 1i64;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            esign = -1;
        }
        i += 1;
    }
    let start = i;
    let mut e: i64 = 0;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        if b[i] != b'_' && e < 100_000 {
            e = e * 10 + (b[i] - b'0') as i64;
        }
        i += 1;
    }
    if i == start || i != b.len() {
        return None;
    }
    exp += esign * e;
    if overflow_digits {
        mant |= 1;
    }
    let v = hex_to_f64(mant, exp);
    Some(if neg { -v } else { v })
}

/// Computes mant × 2^exp rounded to the nearest f64.
fn hex_to_f64(mant: u128, exp: i64) -> f64 {
    if mant == 0 {
        return 0.0;
    }
    let bits = 128 - mant.leading_zeros() as i64;
    // Normalize to 54 bits (53 + guard) with a sticky bit.
    let mut m = mant;
    let mut e = exp;
    let mut sticky = false;
    if bits > 55 {
        let sh = bits - 55;
        sticky = m & ((1u128 << sh) - 1) != 0;
        m >>= sh;
        e += sh;
    }
    let mut mf = m as u64;
    if sticky {
        mf |= 1;
    }
    // Use exact scaling in two steps to avoid premature overflow.
    let f = mf as f64;
    let half = (e / 2) as i32;
    let rest = (e - e / 2) as i32;
    f * 2f64.powi(half) * 2f64.powi(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::excessive_precision)] // exact halfway cases
    fn go_g() {
        let g = |f: f64| format_float(f, b'g', -1);
        assert_eq!(g(0.0), "0");
        assert_eq!(g(1.0), "1");
        assert_eq!(g(123456.0), "123456");
        assert_eq!(g(1234567.0), "1.234567e+06");
        assert_eq!(g(1e21), "1e+21");
        assert_eq!(g(0.0001), "0.0001");
        assert_eq!(g(0.00001), "1e-05");
        assert_eq!(g(1.5), "1.5");
        assert_eq!(g(-2.25), "-2.25");
        assert_eq!(g(1e100), "1e+100");
        assert_eq!(g(f64::MAX), "1.7976931348623157e+308");
        assert_eq!(g(5e-324), "5e-324");
        assert_eq!(g(2001439366031453.25), "2.0014393660314532e+15");
        assert_eq!(g(7.242857142857142e+14), "7.242857142857142e+14");
    }

    #[test]
    fn go_e_f() {
        assert_eq!(format_float(0.0, b'e', 6), "0.000000e+00");
        assert_eq!(format_float(123.456, b'e', 6), "1.234560e+02");
        assert_eq!(format_float(123.456, b'f', 6), "123.456000");
        assert_eq!(format_float(-0.0, b'f', 6), "-0.000000");
        assert_eq!(format_float(1.0, b'F', 6), "%F");
        assert_eq!(format_float(1e-7, b'E', 6), "1.000000E-07");
        assert_eq!(format_float(2.5, b'f', 0), "2");
        assert_eq!(format_float(1.0, b'g', 3), "1");
        assert_eq!(format_float(1234.5678, b'g', 3), "1.23e+03");
        assert_eq!(format_float_v(1e20), "100000000000000000000");
        assert_eq!(format_float_v(1e21), "1e+21");
    }

    #[test]
    fn quote() {
        assert_eq!(go_quote(b"a\"b\\c\n"), r#""a\"b\\c\n""#);
        assert_eq!(go_quote(b"\xff\x01"), r#""\xff\x01""#);
        assert_eq!(go_quote("é".as_bytes()), "\"é\"");
    }

    #[test]
    fn parse() {
        assert_eq!(parse_float(b"1.5"), Some(1.5));
        assert_eq!(parse_float(b"1e400"), Some(f64::INFINITY));
        assert_eq!(parse_float(b".5"), Some(0.5));
        assert_eq!(parse_float(b"5."), Some(5.0));
        assert_eq!(parse_float(b"1_0"), Some(10.0));
        assert_eq!(parse_float(b"1__0"), None);
        assert_eq!(parse_float(b"_1"), None);
        assert_eq!(parse_float(b"0x1p3"), Some(8.0));
        assert_eq!(parse_float(b"abc"), None);
        assert_eq!(parse_float(b"1e"), None);
    }
}
