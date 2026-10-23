// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Starlark quoted string utilities (ported from
//! go.starlark.net/syntax/quote.go).

use crate::syntax::utf8;
use crate::unicode;

/// Maps single-letter chars following \ to their actual values.
fn unesc(c: u8) -> u8 {
    match c {
        b'a' => 0x07,
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'v' => 0x0b,
        _ => c, // '\\', '\'', '"'
    }
}

/// The result of a successful [`unquote`].
pub(crate) struct Unquoted {
    /// The actual string value.
    pub value: Vec<u8>,
    /// Whether the original was triple-quoted.
    #[cfg_attr(not(test), allow(dead_code))]
    pub triple: bool,
    /// Whether it was a byte string.
    pub is_byte: bool,
}

fn parse_hex(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for &c in s {
        let d = (c as char).to_digit(16)?;
        n = n * 16 + u64::from(d);
    }
    Some(n)
}

fn lossy(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// Unquotes the quoted string, returning the actual string value, whether
/// the original was triple-quoted, whether it was a byte string, or an
/// error describing invalid input.
pub(crate) fn unquote(quoted: &[u8]) -> Result<Unquoted, String> {
    let mut quoted = quoted;
    // Check for raw prefix: means don't interpret the inner \.
    let mut raw = false;
    if quoted.starts_with(b"r") {
        raw = true;
        quoted = &quoted[1..];
    }
    // Check for bytes prefix.
    let mut is_byte = false;
    if quoted.starts_with(b"b") {
        is_byte = true;
        quoted = &quoted[1..];
    }

    if quoted.len() < 2 {
        return Err("string literal too short".to_string());
    }

    if quoted[0] != b'"' && quoted[0] != b'\'' || quoted[0] != quoted[quoted.len() - 1] {
        return Err("string literal has invalid quotes".to_string());
    }

    // Check for triple quoted string.
    let quote = quoted[0];
    let mut triple = false;
    if quoted.len() >= 6
        && quoted[1] == quote
        && quoted[2] == quote
        && quoted[..3] == quoted[quoted.len() - 3..]
    {
        triple = true;
        quoted = &quoted[3..quoted.len() - 3];
    } else {
        quoted = &quoted[1..quoted.len() - 1];
    }

    // Now quoted is the quoted data, but no quotes.
    // If we're in raw mode or there are no escapes or
    // carriage returns, we're done.
    let is_special = |c: &u8| *c == b'\r' || (!raw && *c == b'\\');
    if !quoted.iter().any(is_special) {
        return Ok(Unquoted {
            value: quoted.to_vec(),
            triple,
            is_byte,
        });
    }

    // Otherwise process quoted string.
    // Each iteration processes one escape sequence along with the
    // plain text leading up to it.
    let mut buf = Vec::with_capacity(quoted.len());
    loop {
        // Remove prefix before escape sequence.
        let i = quoted.iter().position(is_special).unwrap_or(quoted.len());
        buf.extend_from_slice(&quoted[..i]);
        quoted = &quoted[i..];

        if quoted.is_empty() {
            break;
        }

        // Process carriage return.
        if quoted[0] == b'\r' {
            buf.push(b'\n');
            if quoted.len() > 1 && quoted[1] == b'\n' {
                quoted = &quoted[2..];
            } else {
                quoted = &quoted[1..];
            }
            continue;
        }

        // Process escape sequence.
        if quoted.len() == 1 {
            return Err("truncated escape sequence \\".to_string());
        }

        match quoted[1] {
            b'\n' => {
                // Ignore the escape and the line break.
                quoted = &quoted[2..];
            }
            b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v' | b'\\' | b'\'' | b'"' => {
                // One-char escape.
                // Escapes are allowed for both kinds of quotation
                // mark, not just the kind in use.
                buf.push(unesc(quoted[1]));
                quoted = &quoted[2..];
            }
            b'0'..=b'7' => {
                // Octal escape, up to 3 digits, \OOO.
                let mut n = u32::from(quoted[1] - b'0');
                quoted = &quoted[2..];
                for _ in 1..3 {
                    if quoted.is_empty() || !(b'0'..=b'7').contains(&quoted[0]) {
                        break;
                    }
                    n = n * 8 + u32::from(quoted[0] - b'0');
                    quoted = &quoted[1..];
                }
                if !is_byte && n > 127 {
                    return Err(format!(
                        "non-ASCII octal escape \\{n:o} (use \\u{n:04X} for the UTF-8 encoding of U+{n:04X})"
                    ));
                }
                if n >= 256 {
                    // NOTE: Python silently discards the high bit,
                    // so that '\541' == '\141' == 'a'.
                    // Let's see if we can avoid doing that in BUILD files.
                    return Err(format!("invalid escape sequence \\{n:03o}"));
                }
                buf.push(n as u8);
            }
            b'x' => {
                // Hexadecimal escape, exactly 2 digits, \xXX. [0-127]
                if quoted.len() < 4 {
                    return Err(format!("truncated escape sequence {}", lossy(quoted)));
                }
                let Some(n) = parse_hex(&quoted[2..4]) else {
                    return Err(format!("invalid escape sequence {}", lossy(&quoted[..4])));
                };
                if !is_byte && n > 127 {
                    return Err(format!(
                        "non-ASCII hex escape {} (use \\u{n:04X} for the UTF-8 encoding of U+{n:04X})",
                        lossy(&quoted[..4])
                    ));
                }
                buf.push(n as u8);
                quoted = &quoted[4..];
            }
            b'u' | b'U' => {
                // Unicode code point, 4 (\uXXXX) or 8 (\UXXXXXXXX) hex digits.
                let sz = if quoted[1] == b'U' { 10 } else { 6 };
                if quoted.len() < sz {
                    return Err(format!("truncated escape sequence {}", lossy(quoted)));
                }
                let Some(n) = parse_hex(&quoted[2..sz]) else {
                    return Err(format!("invalid escape sequence {}", lossy(&quoted[..sz])));
                };
                if n > 0x10FFFF {
                    return Err(format!(
                        "code point out of range: {} (max \\U{n:08x})",
                        lossy(&quoted[..sz])
                    ));
                }
                // As in Go, surrogates are disallowed.
                if (0xD800..0xE000).contains(&n) {
                    return Err(format!("invalid Unicode code point U+{n:04X}"));
                }
                let r = char::from_u32(n as u32).unwrap_or(utf8::RUNE_ERROR);
                utf8::push_rune(&mut buf, r);
                quoted = &quoted[sz..];
            }
            c => {
                // In Starlark, like Go, a backslash must escape something.
                // (Python still treats unnecessary backslashes literally,
                // but since 3.6 has emitted a deprecation warning.)
                return Err(format!("invalid escape sequence \\{}", char::from(c)));
            }
        }
    }

    Ok(Unquoted {
        value: buf,
        triple,
        is_byte,
    })
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Returns a Starlark literal that denotes s.
/// If `bytes`, it returns a bytes literal.
pub fn quote(s: &[u8], bytes: bool) -> String {
    let mut buf: Vec<u8> = Vec::with_capacity(3 * s.len() / 2 + 3);
    if bytes {
        buf.push(b'b');
    }
    buf.push(b'"');
    let mut s = s;
    while !s.is_empty() {
        let (r, width) = utf8::decode_rune(s);
        if width == 1 && r == utf8::RUNE_ERROR {
            // String (!bytes) literals accept \xXX escapes only for ASCII,
            // but we must use them here to represent invalid bytes.
            // The result is not a legal literal.
            buf.extend_from_slice(b"\\x");
            buf.push(HEX[usize::from(s[0] >> 4)]);
            buf.push(HEX[usize::from(s[0] & 0xF)]);
            s = &s[1..];
            continue;
        }
        s = &s[width..];
        if r == '"' || r == '\\' {
            // always backslashed
            buf.push(b'\\');
            buf.push(r as u8);
            continue;
        }
        if unicode::is_print(r) {
            utf8::push_rune(&mut buf, r);
            continue;
        }
        push_escaped(&mut buf, r);
    }
    buf.push(b'"');
    // All non-ASCII output was produced by push_rune, so this is valid UTF-8.
    String::from_utf8(buf).unwrap_or_default()
}

/// Appends the escape sequence for the non-printable rune `r`.
fn push_escaped(buf: &mut Vec<u8>, r: char) {
    match r {
        '\x07' => buf.extend_from_slice(b"\\a"),
        '\x08' => buf.extend_from_slice(b"\\b"),
        '\x0c' => buf.extend_from_slice(b"\\f"),
        '\n' => buf.extend_from_slice(b"\\n"),
        '\r' => buf.extend_from_slice(b"\\r"),
        '\t' => buf.extend_from_slice(b"\\t"),
        '\x0b' => buf.extend_from_slice(b"\\v"),
        _ => {
            let c = r as u32;
            if c < u32::from(b' ') || c == 0x7f {
                buf.extend_from_slice(b"\\x");
                buf.push(HEX[(c >> 4) as usize & 0xF]);
                buf.push(HEX[c as usize & 0xF]);
            } else if c < 0x10000 {
                buf.extend_from_slice(b"\\u");
                for shift in (0..=12).rev().step_by(4) {
                    buf.push(HEX[(c >> shift) as usize & 0xF]);
                }
            } else {
                buf.extend_from_slice(b"\\U");
                for shift in (0..=28).rev().step_by(4) {
                    buf.push(HEX[(c >> shift) as usize & 0xF]);
                }
            }
        }
    }
}

/// Returns a single-quoted Go character literal for `r`, like Go's
/// strconv.QuoteRune (as used by the `%q` verb on runes).
pub(crate) fn quote_rune(r: char) -> String {
    let mut buf = vec![b'\''];
    if r == '\'' || r == '\\' {
        buf.push(b'\\');
        buf.push(r as u8);
    } else if unicode::is_print(r) {
        utf8::push_rune(&mut buf, r);
    } else {
        push_escaped(&mut buf, r);
    }
    buf.push(b'\'');
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // (quoted, unquoted, quoted is the standard form)
    const QUOTE_TESTS: &[(&str, &str, bool)] = &[
        (r#""""#, "", true),
        (r#"''"#, "", false),
        (r#""hello""#, "hello", true),
        (r#"'hello'"#, "hello", false),
        (r#""quote\"here""#, "quote\"here", true),
        (r#"'quote"here'"#, "quote\"here", false),
        (r#""quote'here""#, "quote'here", true),
        (r#"'quote\'here'"#, "quote'here", false),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F",
            true,
        ),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F",
            false,
        ),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F",
            false,
        ),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f\"'\\\x03""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F\"'\\\x03",
            true,
        ),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f\"'\\\x03""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F\"'\\\x03",
            false,
        ),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f\"'\\\x03""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F\"'\\\x03",
            false,
        ),
        (
            r#""\a\b\f\n\r\t\v\x00\x7f\"\\\x03""#,
            "\x07\x08\x0c\n\r\t\x0b\x00\x7F\"\\\x03",
            false,
        ),
        (
            r#""cat $(SRCS) | grep '\\s*ip_block:' | sed -e 's/\\s*ip_block: \"\\([^ ]*\\)\"/    \x27\\1\x27,/g' >> $@; ""#,
            r#"cat $(SRCS) | grep '\s*ip_block:' | sed -e 's/\s*ip_block: "\([^ ]*\)"/    '\1',/g' >> $@; "#,
            false,
        ),
        (
            r#""cat $(SRCS) | grep '\\s*ip_block:' | sed -e 's/\\s*ip_block: \"\\([^ ]*\\)\"/    '\\1',/g' >> $@; ""#,
            r#"cat $(SRCS) | grep '\s*ip_block:' | sed -e 's/\s*ip_block: "\([^ ]*\)"/    '\1',/g' >> $@; "#,
            true,
        ),
    ];

    #[test]
    fn test_quote() {
        for &(q, s, std) in QUOTE_TESTS {
            if std {
                assert_eq!(quote(s.as_bytes(), false), q, "quote({s:?})");
            }
        }
    }

    #[test]
    fn test_unquote() {
        for &(q, s, _) in QUOTE_TESTS {
            let u = unquote(q.as_bytes()).unwrap_or_else(|e| panic!("unquote({q}): {e}"));
            let want_triple = q.starts_with("\"\"\"") || q.starts_with("'''");
            assert_eq!(u.value, s.as_bytes(), "unquote({q})");
            assert_eq!(u.triple, want_triple, "unquote({q})");
        }
    }

    #[test]
    fn test_quote_rune() {
        assert_eq!(quote_rune('$'), "'$'");
        assert_eq!(quote_rune('\''), "'\\''");
        assert_eq!(quote_rune('\x01'), "'\\x01'");
        assert_eq!(quote_rune('\u{200b}'), "'\\u200b'");
    }
}
