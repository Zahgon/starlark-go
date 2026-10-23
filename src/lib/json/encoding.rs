// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! The parts of Go's encoding/json package used by the json module:
//! the JSON scanner (scanner.go), Indent (indent.go), string encoding
//! (encode.go's appendString) and string unquoting (decode.go's
//! unquoteBytes, as used by json.Unmarshal into a string).

use crate::starlark::fmt::go_quote;
use crate::syntax::utf8::{decode_rune, push_rune, RUNE_ERROR};

const HEX: &[u8; 16] = b"0123456789abcdef";

/// The result of a scanner step (scanner.go's scan* constants).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scan {
    Continue,     // uninteresting byte
    BeginLiteral, // end implied by next result != Continue
    BeginObject,  // begin object
    ObjectKey,    // just finished object key (string)
    ObjectValue,  // just finished non-last object value
    EndObject,    // end object (implies ObjectValue if possible)
    BeginArray,   // begin array
    ArrayValue,   // just finished array value
    EndArray,     // end array (implies ArrayValue if possible)
    SkipSpace,    // space byte; can skip; known to be last "continue" result
    End,          // top-level value ended *before* this byte
    Error,        // hit an error, scanner.err
}

/// What the scanner is parsing in an enclosing value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parse {
    ObjectKey,   // parsing object key (before colon)
    ObjectValue, // parsing object value (after colon)
    ArrayValue,  // parsing array value
}

/// The scanner's next step function (scanner.go's state* functions).
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    BeginValueOrEmpty,
    BeginValue,
    BeginStringOrEmpty,
    BeginString,
    EndValue,
    EndTop,
    InString,
    InStringEsc,
    InStringEscU,
    InStringEscU1,
    InStringEscU12,
    InStringEscU123,
    Neg,
    One,
    Zero,
    Dot,
    Dot0,
    E,
    ESign,
    E0,
    T,
    Tr,
    Tru,
    F,
    Fa,
    Fal,
    Fals,
    N,
    Nu,
    Nul,
    Error,
}

const MAX_NESTING_DEPTH: usize = 10000;

/// A JSON scanning state machine (Go's encoding/json scanner).
struct Scanner {
    step: State,
    end_top: bool,
    parse_state: Vec<Parse>,
    err: Option<String>,
}

fn is_space(c: u8) -> bool {
    c <= b' ' && (c == b' ' || c == b'\t' || c == b'\r' || c == b'\n')
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// Formats c as a quoted character literal (scanner.go's quoteChar).
fn quote_char(c: u8) -> String {
    if c == b'\'' {
        return "'\\''".to_string();
    }
    if c == b'"' {
        return "'\"'".to_string();
    }
    let mut buf = Vec::new();
    push_rune(&mut buf, char::from(c));
    let s = go_quote(&buf);
    format!("'{}'", &s[1..s.len() - 1])
}

impl Scanner {
    fn new() -> Scanner {
        Scanner {
            step: State::BeginValue,
            end_top: false,
            parse_state: Vec::new(),
            err: None,
        }
    }

    /// Called when there is no more input; reports whether the input
    /// was complete.
    fn eof(&mut self) -> Scan {
        if self.err.is_some() {
            return Scan::Error;
        }
        if self.end_top {
            return Scan::End;
        }
        self.step(b' ');
        if self.end_top {
            return Scan::End;
        }
        if self.err.is_none() {
            self.err = Some("unexpected end of JSON input".to_string());
        }
        Scan::Error
    }

    fn push_parse_state(&mut self, c: u8, new_parse_state: Parse, success: Scan) -> Scan {
        self.parse_state.push(new_parse_state);
        if self.parse_state.len() <= MAX_NESTING_DEPTH {
            return success;
        }
        self.error(c, "exceeded max depth")
    }

    fn pop_parse_state(&mut self) {
        self.parse_state.pop();
        if self.parse_state.is_empty() {
            self.step = State::EndTop;
            self.end_top = true;
        } else {
            self.step = State::EndValue;
        }
    }

    fn error(&mut self, c: u8, context: &str) -> Scan {
        self.step = State::Error;
        self.err = Some(format!("invalid character {} {}", quote_char(c), context));
        Scan::Error
    }

    /// Processes the next byte of input.
    fn step(&mut self, c: u8) -> Scan {
        match self.step {
            State::BeginValueOrEmpty => {
                if is_space(c) {
                    return Scan::SkipSpace;
                }
                if c == b']' {
                    return self.end_value(c);
                }
                self.begin_value(c)
            }
            State::BeginValue => self.begin_value(c),
            State::BeginStringOrEmpty => {
                if is_space(c) {
                    return Scan::SkipSpace;
                }
                if c == b'}' {
                    if let Some(last) = self.parse_state.last_mut() {
                        *last = Parse::ObjectValue;
                    }
                    return self.end_value(c);
                }
                self.begin_string(c)
            }
            State::BeginString => self.begin_string(c),
            State::EndValue => self.end_value(c),
            State::EndTop => self.end_top_step(c),
            State::InString => {
                if c == b'"' {
                    self.step = State::EndValue;
                    return Scan::Continue;
                }
                if c == b'\\' {
                    self.step = State::InStringEsc;
                    return Scan::Continue;
                }
                if c < 0x20 {
                    return self.error(c, "in string literal");
                }
                Scan::Continue
            }
            State::InStringEsc => match c {
                b'b' | b'f' | b'n' | b'r' | b't' | b'\\' | b'/' | b'"' => {
                    self.step = State::InString;
                    Scan::Continue
                }
                b'u' => {
                    self.step = State::InStringEscU;
                    Scan::Continue
                }
                _ => self.error(c, "in string escape code"),
            },
            State::InStringEscU
            | State::InStringEscU1
            | State::InStringEscU12
            | State::InStringEscU123 => {
                if is_hex(c) {
                    self.step = match self.step {
                        State::InStringEscU => State::InStringEscU1,
                        State::InStringEscU1 => State::InStringEscU12,
                        State::InStringEscU12 => State::InStringEscU123,
                        _ => State::InString,
                    };
                    return Scan::Continue;
                }
                self.error(c, "in \\u hexadecimal character escape")
            }
            State::Neg => {
                if c == b'0' {
                    self.step = State::Zero;
                    return Scan::Continue;
                }
                if (b'1'..=b'9').contains(&c) {
                    self.step = State::One;
                    return Scan::Continue;
                }
                self.error(c, "in numeric literal")
            }
            State::One => {
                if c.is_ascii_digit() {
                    self.step = State::One;
                    return Scan::Continue;
                }
                self.zero(c)
            }
            State::Zero => self.zero(c),
            State::Dot => {
                if c.is_ascii_digit() {
                    self.step = State::Dot0;
                    return Scan::Continue;
                }
                self.error(c, "after decimal point in numeric literal")
            }
            State::Dot0 => {
                if c.is_ascii_digit() {
                    return Scan::Continue;
                }
                if c == b'e' || c == b'E' {
                    self.step = State::E;
                    return Scan::Continue;
                }
                self.end_value(c)
            }
            State::E => {
                if c == b'+' || c == b'-' {
                    self.step = State::ESign;
                    return Scan::Continue;
                }
                self.e_sign(c)
            }
            State::ESign => self.e_sign(c),
            State::E0 => {
                if c.is_ascii_digit() {
                    return Scan::Continue;
                }
                self.end_value(c)
            }
            State::T => self.literal(c, b'r', State::Tr, "in literal true (expecting 'r')"),
            State::Tr => self.literal(c, b'u', State::Tru, "in literal true (expecting 'u')"),
            State::Tru => self.literal(c, b'e', State::EndValue, "in literal true (expecting 'e')"),
            State::F => self.literal(c, b'a', State::Fa, "in literal false (expecting 'a')"),
            State::Fa => self.literal(c, b'l', State::Fal, "in literal false (expecting 'l')"),
            State::Fal => self.literal(c, b's', State::Fals, "in literal false (expecting 's')"),
            State::Fals => {
                self.literal(c, b'e', State::EndValue, "in literal false (expecting 'e')")
            }
            State::N => self.literal(c, b'u', State::Nu, "in literal null (expecting 'u')"),
            State::Nu => self.literal(c, b'l', State::Nul, "in literal null (expecting 'l')"),
            State::Nul => self.literal(c, b'l', State::EndValue, "in literal null (expecting 'l')"),
            State::Error => Scan::Error,
        }
    }

    fn literal(&mut self, c: u8, want: u8, next: State, context: &str) -> Scan {
        if c == want {
            self.step = next;
            return Scan::Continue;
        }
        self.error(c, context)
    }

    fn begin_value(&mut self, c: u8) -> Scan {
        if is_space(c) {
            return Scan::SkipSpace;
        }
        let next = match c {
            b'{' => {
                self.step = State::BeginStringOrEmpty;
                return self.push_parse_state(c, Parse::ObjectKey, Scan::BeginObject);
            }
            b'[' => {
                self.step = State::BeginValueOrEmpty;
                return self.push_parse_state(c, Parse::ArrayValue, Scan::BeginArray);
            }
            b'"' => State::InString,
            b'-' => State::Neg,
            b'0' => State::Zero,
            b't' => State::T,
            b'f' => State::F,
            b'n' => State::N,
            b'1'..=b'9' => State::One,
            _ => return self.error(c, "looking for beginning of value"),
        };
        self.step = next;
        Scan::BeginLiteral
    }

    fn begin_string(&mut self, c: u8) -> Scan {
        if is_space(c) {
            return Scan::SkipSpace;
        }
        if c == b'"' {
            self.step = State::InString;
            return Scan::BeginLiteral;
        }
        self.error(c, "looking for beginning of object key string")
    }

    fn end_value(&mut self, c: u8) -> Scan {
        let Some(&ps) = self.parse_state.last() else {
            self.step = State::EndTop;
            self.end_top = true;
            return self.end_top_step(c);
        };
        if is_space(c) {
            self.step = State::EndValue;
            return Scan::SkipSpace;
        }
        let n = self.parse_state.len();
        match ps {
            Parse::ObjectKey => {
                if c == b':' {
                    self.parse_state[n - 1] = Parse::ObjectValue;
                    self.step = State::BeginValue;
                    return Scan::ObjectKey;
                }
                self.error(c, "after object key")
            }
            Parse::ObjectValue => {
                if c == b',' {
                    self.parse_state[n - 1] = Parse::ObjectKey;
                    self.step = State::BeginString;
                    return Scan::ObjectValue;
                }
                if c == b'}' {
                    self.pop_parse_state();
                    return Scan::EndObject;
                }
                self.error(c, "after object key:value pair")
            }
            Parse::ArrayValue => {
                if c == b',' {
                    self.step = State::BeginValue;
                    return Scan::ArrayValue;
                }
                if c == b']' {
                    self.pop_parse_state();
                    return Scan::EndArray;
                }
                self.error(c, "after array element")
            }
        }
    }

    fn end_top_step(&mut self, c: u8) -> Scan {
        if !is_space(c) {
            self.error(c, "after top-level value");
        }
        Scan::End
    }

    fn zero(&mut self, c: u8) -> Scan {
        if c == b'.' {
            self.step = State::Dot;
            return Scan::Continue;
        }
        if c == b'e' || c == b'E' {
            self.step = State::E;
            return Scan::Continue;
        }
        self.end_value(c)
    }

    fn e_sign(&mut self, c: u8) -> Scan {
        if c.is_ascii_digit() {
            self.step = State::E0;
            return Scan::Continue;
        }
        self.error(c, "in exponent of numeric literal")
    }

    fn take_err(&mut self) -> String {
        self.err.take().unwrap_or_default()
    }
}

/// Checks that data is a valid JSON encoding, returning the scanner's
/// syntax error message otherwise (Go's checkValid).
fn check_valid(data: &[u8]) -> Result<(), String> {
    let mut scan = Scanner::new();
    for &c in data {
        if scan.step(c) == Scan::Error {
            return Err(scan.take_err());
        }
    }
    if scan.eof() == Scan::Error {
        return Err(scan.take_err());
    }
    Ok(())
}

fn append_newline(dst: &mut Vec<u8>, prefix: &[u8], indent: &[u8], depth: usize) {
    dst.push(b'\n');
    dst.extend_from_slice(prefix);
    for _ in 0..depth {
        dst.extend_from_slice(indent);
    }
}

/// Returns an indented form of the JSON-encoded src (Go's json.Indent).
/// Each element in a JSON object or array begins on a new, indented line
/// beginning with prefix followed by one or more copies of indent
/// according to the indentation nesting. Leading space characters are
/// dropped; trailing ones are preserved.
pub fn indent(src: &[u8], prefix: &[u8], indent: &[u8]) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity(2 * src.len());
    let mut scan = Scanner::new();
    let mut need_indent = false;
    let mut depth: usize = 0;
    for &c in src {
        let v = scan.step(c);
        if v == Scan::SkipSpace {
            continue;
        }
        if v == Scan::Error {
            break;
        }
        if need_indent && v != Scan::EndObject && v != Scan::EndArray {
            need_indent = false;
            depth += 1;
            append_newline(&mut dst, prefix, indent, depth);
        }

        // Emit semantically uninteresting bytes
        // (in particular, punctuation in strings) unmodified.
        if v == Scan::Continue {
            dst.push(c);
            continue;
        }

        // Add spacing around real punctuation.
        match c {
            b'{' | b'[' => {
                // delay indent so that empty object and array are formatted as {} and [].
                need_indent = true;
                dst.push(c);
            }
            b',' => {
                dst.push(c);
                append_newline(&mut dst, prefix, indent, depth);
            }
            b':' => {
                dst.push(c);
                dst.push(b' ');
            }
            b'}' | b']' => {
                if need_indent {
                    // suppress indent in empty object/array
                    need_indent = false;
                } else {
                    depth = depth.saturating_sub(1);
                    append_newline(&mut dst, prefix, indent, depth);
                }
                dst.push(c);
            }
            _ => dst.push(c),
        }
    }
    if scan.eof() == Scan::Error {
        return Err(scan.take_err());
    }
    Ok(dst)
}

/// Reports whether b may appear unescaped in a JSON string when HTML
/// escaping is enabled (Go's htmlSafeSet).
fn html_safe(b: u8) -> bool {
    (0x20..0x80).contains(&b) && !matches!(b, b'"' | b'\\' | b'<' | b'>' | b'&')
}

/// Appends the JSON encoding of the string src, with HTML escaping, as
/// done by json.Marshal (Go's appendString with escapeHTML=true).
pub fn append_string(dst: &mut Vec<u8>, src: &[u8]) {
    dst.push(b'"');
    let mut start = 0;
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b < 0x80 {
            if html_safe(b) {
                i += 1;
                continue;
            }
            dst.extend_from_slice(&src[start..i]);
            match b {
                b'\\' | b'"' => dst.extend_from_slice(&[b'\\', b]),
                0x08 => dst.extend_from_slice(b"\\b"),
                0x0c => dst.extend_from_slice(b"\\f"),
                b'\n' => dst.extend_from_slice(b"\\n"),
                b'\r' => dst.extend_from_slice(b"\\r"),
                b'\t' => dst.extend_from_slice(b"\\t"),
                _ => dst.extend_from_slice(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[(b >> 4) as usize],
                    HEX[(b & 0xF) as usize],
                ]),
            }
            i += 1;
            start = i;
            continue;
        }
        let (c, size) = decode_rune(&src[i..]);
        if c == RUNE_ERROR && size == 1 {
            dst.extend_from_slice(&src[start..i]);
            dst.extend_from_slice(b"\\ufffd");
            i += size;
            start = i;
            continue;
        }
        // U+2028 is LINE SEPARATOR and U+2029 is PARAGRAPH SEPARATOR;
        // they are escaped unconditionally.
        if c == '\u{2028}' || c == '\u{2029}' {
            dst.extend_from_slice(&src[start..i]);
            dst.extend_from_slice(&[b'\\', b'u', b'2', b'0', b'2', HEX[(c as usize) & 0xF]]);
            i += size;
            start = i;
            continue;
        }
        i += size;
    }
    dst.extend_from_slice(&src[start..]);
    dst.push(b'"');
}

/// Decodes \uXXXX from the beginning of s, returning -1 on failure
/// (Go's getu4).
fn getu4(s: &[u8]) -> i32 {
    if s.len() < 6 || s[0] != b'\\' || s[1] != b'u' {
        return -1;
    }
    let mut r: i32 = 0;
    for &c in &s[2..6] {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return -1,
        };
        r = r * 16 + i32::from(d);
    }
    r
}

fn is_surrogate(r: i32) -> bool {
    (0xD800..0xE000).contains(&r)
}

/// Decodes a UTF-16 surrogate pair, or returns U+FFFD.
fn decode_surrogates(r1: i32, r2: i32) -> char {
    if (0xD800..0xDC00).contains(&r1) && (0xDC00..0xE000).contains(&r2) {
        let r = (((r1 - 0xD800) << 10) | (r2 - 0xDC00)) + 0x10000;
        return char::from_u32(r as u32).unwrap_or(RUNE_ERROR);
    }
    RUNE_ERROR
}

/// Converts a quoted JSON string literal s into the bytes it denotes
/// (Go's unquoteBytes). Returns None if s is malformed.
fn unquote_bytes(s: &[u8]) -> Option<Vec<u8>> {
    if s.len() < 2 || s[0] != b'"' || s[s.len() - 1] != b'"' {
        return None;
    }
    let s = &s[1..s.len() - 1];
    let mut b = Vec::with_capacity(s.len() + 8);
    let mut r = 0;
    while r < s.len() {
        let c = s[r];
        if c == b'\\' {
            r += 1;
            if r >= s.len() {
                return None;
            }
            match s[r] {
                b'"' | b'\\' | b'/' | b'\'' => b.push(s[r]),
                b'b' => b.push(0x08),
                b'f' => b.push(0x0c),
                b'n' => b.push(b'\n'),
                b'r' => b.push(b'\r'),
                b't' => b.push(b'\t'),
                b'u' => {
                    r -= 1;
                    let rr = getu4(&s[r..]);
                    if rr < 0 {
                        return None;
                    }
                    r += 6;
                    let ch = if is_surrogate(rr) {
                        let rr1 = getu4(&s[r..]);
                        let dec = decode_surrogates(rr, rr1);
                        if dec != RUNE_ERROR {
                            // A valid pair; consume.
                            r += 6;
                        }
                        dec
                    } else {
                        char::from_u32(rr as u32).unwrap_or(RUNE_ERROR)
                    };
                    push_rune(&mut b, ch);
                    continue;
                }
                _ => return None,
            }
            r += 1;
        } else if c == b'"' || c < b' ' {
            // Quote, control characters are invalid.
            return None;
        } else if c < 0x80 {
            b.push(c);
            r += 1;
        } else {
            // Coerce to well-formed UTF-8.
            let (rr, size) = decode_rune(&s[r..]);
            r += size;
            push_rune(&mut b, rr);
        }
    }
    Some(b)
}

/// Decodes the JSON string literal data like json.Unmarshal into a Go
/// string: the whole input is validated first, so errors are the
/// scanner's syntax errors.
pub fn unmarshal_string(data: &[u8]) -> Result<Vec<u8>, String> {
    check_valid(data)?;
    unquote_bytes(data)
        .ok_or_else(|| "JSON decoder out of sync - data changing underfoot?".to_string())
}
