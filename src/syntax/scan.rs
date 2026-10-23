// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! A lexical scanner for Starlark (ported from go.starlark.net/syntax/scan.go).

use std::fmt;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::Num;

use crate::syntax::quote::{quote_rune, unquote};
use crate::syntax::utf8;
use crate::syntax::{Position, Token};
use crate::unicode;

/// An Error describes the nature and position of a scanner or parser error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub pos: Position,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// A function that reads the next line of input (REPL only).
/// `Ok(None)` denotes end of input.
pub type Readline<'a> = &'a mut dyn FnMut() -> std::io::Result<Option<Vec<u8>>>;

/// TokenValue records the position and value associated with each token.
#[derive(Clone, Debug, Default)]
pub(crate) struct TokenValue {
    /// Raw text of token.
    pub raw: String,
    /// Decoded int.
    pub int: i64,
    /// Decoded integers > int64.
    pub big_int: Option<BigInt>,
    /// Decoded float.
    pub float: f64,
    /// Decoded string or bytes.
    pub string: Vec<u8>,
    /// Start position of token.
    pub pos: Position,
}

/// A FilePortion describes the content of a portion of a file.
/// Callers may provide a FilePortion to `parse_file_portion` when the
/// desired initial line and column numbers are not (1, 1), such as when
/// an expression is parsed from within larger file.
#[derive(Clone, Debug)]
pub struct FilePortion<'a> {
    pub content: &'a [u8],
    pub first_line: i32,
    pub first_col: i32,
}

/// A scanner represents a single input file being parsed.
pub(crate) struct Scanner<'a> {
    /// Current input buffer (in REPL, a line of input).
    buf: Vec<u8>,
    /// Offset of the rest of the input within `buf`.
    off: usize,
    /// Offset within `buf` of the token being scanned.
    token: usize,
    /// Current input position.
    pub pos: Position,
    /// Nesting of [ ] { } ( ).
    depth: usize,
    /// Stack of indentation levels.
    indentstk: Vec<usize>,
    /// Number of saved INDENT (>0) or OUTDENT (<0) tokens to return.
    dents: isize,
    /// After NEWLINE; convert spaces to indentation tokens.
    line_start: bool,
    /// Read next line of input (REPL only).
    readline: Option<Readline<'a>>,
}

impl<'a> Scanner<'a> {
    pub fn new(filename: &str, src: Vec<u8>, readline: Option<Readline<'a>>) -> Scanner<'a> {
        let file: Rc<str> = Rc::from(filename);
        let mut indentstk = Vec::with_capacity(10);
        indentstk.push(0);
        Scanner {
            buf: src,
            off: 0,
            token: 0,
            pos: Position::new(file, 1, 1),
            depth: 0,
            indentstk,
            dents: 0,
            line_start: true,
            readline,
        }
    }

    fn rest(&self) -> &[u8] {
        &self.buf[self.off..]
    }

    /// Returns an error at the specified position.
    pub fn error<T>(&self, pos: Position, msg: impl Into<String>) -> Result<T> {
        Err(Error {
            pos,
            msg: msg.into(),
        })
    }

    /// Reports whether the input has reached end of file.
    fn eof(&mut self) -> Result<bool> {
        Ok(self.off == self.buf.len() && !self.read_line()?)
    }

    /// Attempts to read another line of input.
    /// Precondition: the rest of the input is empty.
    fn read_line(&mut self) -> Result<bool> {
        let Some(readline) = self.readline.as_mut() else {
            return Ok(false);
        };
        match readline() {
            Ok(Some(line)) => {
                self.buf = line;
                self.off = 0;
                self.token = 0;
                Ok(!self.buf.is_empty())
            }
            Ok(None) => self.error(self.pos.clone(), "EOF"),
            Err(err) => self.error(self.pos.clone(), err.to_string()),
        }
    }

    /// Returns the next rune in the input without consuming it.
    /// Newlines in Unix, DOS, or Mac format are treated as one rune, '\n'.
    /// End of input is reported as '\0'.
    fn peek_rune(&mut self) -> Result<char> {
        if self.eof()? {
            return Ok('\0');
        }

        // fast path: ASCII
        let b = self.buf[self.off];
        if b < 0x80 {
            if b == b'\r' {
                return Ok('\n');
            }
            return Ok(char::from(b));
        }

        Ok(utf8::decode_rune(self.rest()).0)
    }

    /// Consumes and returns the next rune in the input.
    /// Newlines in Unix, DOS, or Mac format are treated as one rune, '\n'.
    fn read_rune(&mut self) -> Result<char> {
        if self.off == self.buf.len() {
            if !self.read_line()? {
                return self.error(self.pos.clone(), "internal scanner error: readRune at EOF");
            }
            if self.off == self.buf.len() {
                return Ok('\0');
            }
        }

        // fast path: ASCII
        let b = self.buf[self.off];
        if b < 0x80 {
            let mut r = char::from(b);
            self.off += 1;
            if r == '\r' {
                if self.off < self.buf.len() && self.buf[self.off] == b'\n' {
                    self.off += 1;
                }
                r = '\n';
            }
            if r == '\n' {
                self.pos.line += 1;
                self.pos.col = 1;
            } else {
                self.pos.col += 1;
            }
            return Ok(r);
        }

        let (r, size) = utf8::decode_rune(self.rest());
        self.off += size;
        self.pos.col += 1;
        Ok(r)
    }

    /// Marks the beginning of the next input token.
    /// It must be followed by a call to end_token once the token has
    /// been consumed using read_rune.
    fn start_token(&mut self, val: &mut TokenValue) {
        self.token = self.off;
        val.raw.clear();
        val.pos = self.pos.clone();
    }

    /// Marks the end of an input token.
    /// It records the actual token string in val.raw if the caller
    /// has not done that already.
    fn end_token(&mut self, val: &mut TokenValue) {
        if val.raw.is_empty() {
            val.raw = String::from_utf8_lossy(&self.buf[self.token..self.off]).into_owned();
        }
    }

    /// Called by the parser to obtain the next input token.
    /// It returns the token and sets val to the data associated with
    /// the token.
    ///
    /// For all our input tokens, the associated data is val.pos (the
    /// position where the token begins), val.raw (the input string
    /// corresponding to the token).  For string and int tokens, the string
    /// and int fields additionally contain the token's interpreted value.
    pub fn next_token(&mut self, val: &mut TokenValue) -> Result<Token> {
        'start: loop {
            let mut c;

            // Deal with leading spaces and indentation.
            let mut blank = false;
            let saved_line_start = self.line_start;
            if self.line_start {
                self.line_start = false;
                let mut col = 0;
                loop {
                    c = self.peek_rune()?;
                    if c == ' ' {
                        col += 1;
                        self.read_rune()?;
                    } else if c == '\t' {
                        const TAB: usize = 8;
                        col += TAB - (self.pos.col - 1) as usize % TAB;
                        self.read_rune()?;
                    } else {
                        break;
                    }
                }

                // The third clause matches EOF.
                if c == '#' || c == '\n' || c == '\0' {
                    blank = true;
                }

                // Compute indentation level for non-blank lines not
                // inside an expression.  This is not the common case.
                if !blank && self.depth == 0 {
                    let cur = *self.indentstk.last().unwrap_or(&0);
                    if col > cur {
                        // indent
                        self.dents += 1;
                        self.indentstk.push(col);
                    } else if col < cur {
                        // outdent(s)
                        while let Some(&top) = self.indentstk.last() {
                            if col >= top {
                                break;
                            }
                            self.dents -= 1;
                            self.indentstk.pop();
                        }
                        if self.indentstk.last() != Some(&col) {
                            return self.error(
                                self.pos.clone(),
                                "unindent does not match any outer indentation level",
                            );
                        }
                    }
                }
            }

            // Return saved indentation tokens.
            if self.dents != 0 {
                self.start_token(val);
                self.end_token(val);
                if self.dents < 0 {
                    self.dents += 1;
                    return Ok(Token::Outdent);
                } else {
                    self.dents -= 1;
                    return Ok(Token::Indent);
                }
            }

            // start of line proper
            c = self.peek_rune()?;

            // Skip spaces.
            while c == ' ' || c == '\t' {
                self.read_rune()?;
                c = self.peek_rune()?;
            }

            // comment
            if c == '#' {
                // Consume up to newline (included).
                while c != '\0' && c != '\n' {
                    self.read_rune()?;
                    c = self.peek_rune()?;
                }
            }

            // newline
            if c == '\n' {
                self.line_start = true;

                // Ignore newlines within expressions (common case).
                if self.depth > 0 {
                    self.read_rune()?;
                    continue 'start;
                }

                // Ignore blank lines, except in the REPL,
                // where they emit OUTDENTs and NEWLINE.
                if blank {
                    if self.readline.is_none() {
                        self.read_rune()?;
                        continue 'start;
                    } else if self.indentstk.len() > 1 {
                        self.dents = 1 - self.indentstk.len() as isize;
                        self.indentstk.truncate(1);
                        continue 'start;
                    }
                }

                // At top-level (not in an expression).
                self.start_token(val);
                self.read_rune()?;
                val.raw = "\n".to_string();
                return Ok(Token::Newline);
            }

            // end of file
            if c == '\0' {
                // Emit OUTDENTs for unfinished indentation,
                // preceded by a NEWLINE if we haven't just emitted one.
                if self.indentstk.len() > 1 {
                    if saved_line_start {
                        self.dents = 1 - self.indentstk.len() as isize;
                        self.indentstk.truncate(1);
                        continue 'start;
                    } else {
                        self.line_start = true;
                        self.start_token(val);
                        val.raw = "\n".to_string();
                        return Ok(Token::Newline);
                    }
                }

                self.start_token(val);
                self.end_token(val);
                return Ok(Token::Eof);
            }

            // line continuation
            if c == '\\' {
                self.read_rune()?;
                if self.peek_rune()? != '\n' {
                    return self.error(self.pos.clone(), "stray backslash in program");
                }
                self.read_rune()?;
                continue 'start;
            }

            // start of the next token
            self.start_token(val);

            // comma (common case)
            if c == ',' {
                self.read_rune()?;
                self.end_token(val);
                return Ok(Token::Comma);
            }

            // string literal
            if c == '"' || c == '\'' {
                return self.scan_string(val, c);
            }

            // identifier or keyword
            if is_ident_start(c) {
                let rest = self.rest();
                if (c == 'r' || c == 'b') && rest.len() > 1 && (rest[1] == b'"' || rest[1] == b'\'')
                {
                    //  r"..."
                    //  b"..."
                    self.read_rune()?;
                    c = self.peek_rune()?;
                    return self.scan_string(val, c);
                } else if c == 'r'
                    && rest.len() > 2
                    && rest[1] == b'b'
                    && (rest[2] == b'"' || rest[2] == b'\'')
                {
                    // rb"..."
                    self.read_rune()?;
                    self.read_rune()?;
                    c = self.peek_rune()?;
                    return self.scan_string(val, c);
                }

                while is_ident(c) {
                    self.read_rune()?;
                    c = self.peek_rune()?;
                }
                self.end_token(val);
                if let Some(k) = Token::keyword(val.raw.as_bytes()) {
                    return Ok(k);
                }

                return Ok(Token::Ident);
            }

            // brackets
            match c {
                '[' | '(' | '{' => {
                    self.depth += 1;
                    if self.depth > 1000 {
                        return self.error(self.pos.clone(), "excessive nesting");
                    }
                    self.read_rune()?;
                    self.end_token(val);
                    return Ok(match c {
                        '[' => Token::Lbrack,
                        '(' => Token::Lparen,
                        _ => Token::Lbrace,
                    });
                }
                ']' | ')' | '}' => {
                    if self.depth == 0 {
                        return self
                            .error(self.pos.clone(), format!("unexpected {}", quote_rune(c)));
                    } else {
                        self.depth -= 1;
                    }
                    self.read_rune()?;
                    self.end_token(val);
                    return Ok(match c {
                        ']' => Token::Rbrack,
                        ')' => Token::Rparen,
                        _ => Token::Rbrace,
                    });
                }
                _ => {}
            }

            // int or float literal, or period
            if c.is_ascii_digit() || c == '.' {
                return self.scan_number(val, c);
            }

            // other punctuation
            let tok = self.scan_punct(c);
            self.end_token(val);
            return tok;
        }
    }

    /// Scans an operator or punctuation token starting with `c`.
    fn scan_punct(&mut self, c: char) -> Result<Token> {
        match c {
            '=' | '<' | '>' | '!' | '+' | '-' | '%' | '/' | '&' | '|' | '^' => {
                // possibly followed by '='
                let start = self.pos.clone();
                self.read_rune()?;
                if self.peek_rune()? == '=' {
                    self.read_rune()?;
                    match c {
                        '<' => return Ok(Token::Le),
                        '>' => return Ok(Token::Ge),
                        '=' => return Ok(Token::Eql),
                        '!' => return Ok(Token::Neq),
                        '+' => return Ok(Token::PlusEq),
                        '-' => return Ok(Token::MinusEq),
                        '/' => return Ok(Token::SlashEq),
                        '%' => return Ok(Token::PercentEq),
                        '&' => return Ok(Token::AmpEq),
                        '|' => return Ok(Token::PipeEq),
                        '^' => return Ok(Token::CircumflexEq),
                        _ => {}
                    }
                }
                match c {
                    '=' => Ok(Token::Eq),
                    '<' => {
                        if self.peek_rune()? == '<' {
                            self.read_rune()?;
                            if self.peek_rune()? == '=' {
                                self.read_rune()?;
                                return Ok(Token::LtltEq);
                            } else {
                                return Ok(Token::Ltlt);
                            }
                        }
                        Ok(Token::Lt)
                    }
                    '>' => {
                        if self.peek_rune()? == '>' {
                            self.read_rune()?;
                            if self.peek_rune()? == '=' {
                                self.read_rune()?;
                                return Ok(Token::GtgtEq);
                            } else {
                                return Ok(Token::Gtgt);
                            }
                        }
                        Ok(Token::Gt)
                    }
                    '!' => self.error(start, "unexpected input character '!'"),
                    '+' => Ok(Token::Plus),
                    '-' => Ok(Token::Minus),
                    '/' => {
                        if self.peek_rune()? == '/' {
                            self.read_rune()?;
                            if self.peek_rune()? == '=' {
                                self.read_rune()?;
                                return Ok(Token::SlashslashEq);
                            } else {
                                return Ok(Token::Slashslash);
                            }
                        }
                        Ok(Token::Slash)
                    }
                    '%' => Ok(Token::Percent),
                    '&' => Ok(Token::Amp),
                    '|' => Ok(Token::Pipe),
                    _ => Ok(Token::Circumflex),
                }
            }

            ':' | ';' | '~' => {
                // single-char tokens (except comma)
                self.read_rune()?;
                Ok(match c {
                    ':' => Token::Colon,
                    ';' => Token::Semi,
                    _ => Token::Tilde,
                })
            }

            '*' => {
                // possibly followed by '*' or '='
                self.read_rune()?;
                match self.peek_rune()? {
                    '*' => {
                        self.read_rune()?;
                        Ok(Token::Starstar)
                    }
                    '=' => {
                        self.read_rune()?;
                        Ok(Token::StarEq)
                    }
                    _ => Ok(Token::Star),
                }
            }

            _ => self.error(
                self.pos.clone(),
                format!("unexpected input character {}", quote_rune(c)),
            ),
        }
    }

    fn scan_string(&mut self, val: &mut TokenValue, quote: char) -> Result<Token> {
        let start = self.pos.clone();
        let q = quote as u8;
        let rest = self.rest();
        let triple = rest.len() >= 3 && rest[0] == q && rest[1] == q && rest[2] == q;
        self.read_rune()?;

        // String literals may contain escaped or unescaped newlines,
        // causing them to span multiple lines (gulps) of REPL input;
        // they are the only such token. Thus we cannot call end_token,
        // as it assumes the input buffer is unchanged since start_token.
        // Instead, buffer the token here.
        let mut raw: Vec<u8> = Vec::new();

        // Copy the prefix, e.g. r' or " (see start_token).
        raw.extend_from_slice(&self.buf[self.token..self.off]);

        if !triple {
            // single-quoted string literal
            loop {
                if self.eof()? {
                    return self.error(val.pos.clone(), "unexpected EOF in string");
                }
                let mut c = self.read_rune()?;
                utf8::push_rune(&mut raw, c);
                if c == quote {
                    break;
                }
                if c == '\n' {
                    return self.error(val.pos.clone(), "unexpected newline in string");
                }
                if c == '\\' {
                    if self.eof()? {
                        return self.error(val.pos.clone(), "unexpected EOF in string");
                    }
                    c = self.read_rune()?;
                    utf8::push_rune(&mut raw, c);
                }
            }
        } else {
            // triple-quoted string literal
            self.read_rune()?;
            utf8::push_rune(&mut raw, quote);
            self.read_rune()?;
            utf8::push_rune(&mut raw, quote);

            let mut quote_count = 0;
            loop {
                if self.eof()? {
                    return self.error(val.pos.clone(), "unexpected EOF in string");
                }
                let mut c = self.read_rune()?;
                utf8::push_rune(&mut raw, c);
                if c == quote {
                    quote_count += 1;
                    if quote_count == 3 {
                        break;
                    }
                } else {
                    quote_count = 0;
                }
                if c == '\\' {
                    if self.eof()? {
                        return self.error(val.pos.clone(), "unexpected EOF in string");
                    }
                    c = self.read_rune()?;
                    utf8::push_rune(&mut raw, c);
                }
            }
        }

        let unquoted = match unquote(&raw) {
            Ok(u) => u,
            Err(msg) => return self.error(start, msg),
        };
        // raw consists of the ASCII prefix and pushed runes: valid UTF-8.
        val.raw = String::from_utf8_lossy(&raw).into_owned();
        val.string = unquoted.value;
        if unquoted.is_byte {
            Ok(Token::Bytes)
        } else {
            Ok(Token::String)
        }
    }

    fn scan_number(&mut self, val: &mut TokenValue, c: char) -> Result<Token> {
        // https://github.com/google/starlark-go/blob/master/doc/spec.md#lexical-elements
        //
        // Python features not supported:
        // - integer literals of >64 bits of precision
        // - 123L or 123l long suffix
        // - traditional octal: 0755
        // https://docs.python.org/2/reference/lexical_analysis.html#integer-and-long-integer-literals

        let start = self.pos.clone();
        let mut fraction = false;
        let mut exponent = false;
        let mut c = c;

        if c == '.' {
            // dot or start of fraction
            self.read_rune()?;
            c = self.peek_rune()?;
            if !c.is_ascii_digit() {
                self.end_token(val);
                return Ok(Token::Dot);
            }
            fraction = true;
        } else if c == '0' {
            // hex, octal, binary or float
            self.read_rune()?;
            c = self.peek_rune()?;

            if c == '.' {
                fraction = true;
            } else if c == 'x' || c == 'X' {
                // hex
                self.read_rune()?;
                c = self.peek_rune()?;
                if !c.is_ascii_hexdigit() {
                    return self.error(start, "invalid hex literal");
                }
                while c.is_ascii_hexdigit() {
                    self.read_rune()?;
                    c = self.peek_rune()?;
                }
            } else if c == 'o' || c == 'O' {
                // octal
                self.read_rune()?;
                c = self.peek_rune()?;
                if !is_odigit(c) {
                    return self.error(self.pos.clone(), "invalid octal literal");
                }
                while is_odigit(c) {
                    self.read_rune()?;
                    c = self.peek_rune()?;
                }
            } else if c == 'b' || c == 'B' {
                // binary
                self.read_rune()?;
                c = self.peek_rune()?;
                if !is_bdigit(c) {
                    return self.error(self.pos.clone(), "invalid binary literal");
                }
                while is_bdigit(c) {
                    self.read_rune()?;
                    c = self.peek_rune()?;
                }
            } else {
                // float (or obsolete octal "0755")
                let mut allzeros = true;
                let mut octal = true;
                while c.is_ascii_digit() {
                    if c != '0' {
                        allzeros = false;
                    }
                    if c > '7' {
                        octal = false;
                    }
                    self.read_rune()?;
                    c = self.peek_rune()?;
                }
                if c == '.' {
                    fraction = true;
                } else if c == 'e' || c == 'E' {
                    exponent = true;
                } else if octal && !allzeros {
                    self.end_token(val);
                    return self.error(
                        self.pos.clone(),
                        format!("obsolete form of octal literal; use 0o{}", &val.raw[1..]),
                    );
                }
            }
        } else {
            // decimal
            while c.is_ascii_digit() {
                self.read_rune()?;
                c = self.peek_rune()?;
            }

            if c == '.' {
                fraction = true;
            } else if c == 'e' || c == 'E' {
                exponent = true;
            }
        }

        if fraction {
            self.read_rune()?; // consume '.'
            c = self.peek_rune()?;
            while c.is_ascii_digit() {
                self.read_rune()?;
                c = self.peek_rune()?;
            }

            if c == 'e' || c == 'E' {
                exponent = true;
            }
        }

        if exponent {
            self.read_rune()?; // consume [eE]
            c = self.peek_rune()?;
            if c == '+' || c == '-' {
                self.read_rune()?;
                c = self.peek_rune()?;
                if !c.is_ascii_digit() {
                    return self.error(self.pos.clone(), "invalid float literal");
                }
            }
            while c.is_ascii_digit() {
                self.read_rune()?;
                c = self.peek_rune()?;
            }
        }

        self.end_token(val);
        if fraction || exponent {
            match val.raw.parse::<f64>() {
                Ok(f) if f.is_finite() => val.float = f,
                _ => return self.error(self.pos.clone(), "invalid float literal"),
            }
            Ok(Token::Float)
        } else {
            let s = val.raw.as_str();
            val.big_int = None;
            let ok = if s.len() > 2 && s.starts_with('0') && (s.as_bytes()[1] | 0x20) == b'o' {
                i64::from_str_radix(&s[2..], 8).map(|n| val.int = n).is_ok()
            } else if s.len() > 2 && s.starts_with('0') && (s.as_bytes()[1] | 0x20) == b'b' {
                i64::from_str_radix(&s[2..], 2).map(|n| val.int = n).is_ok()
            } else if let Some(n) = parse_int_base0(s) {
                val.int = n;
                true
            } else if let Some(n) = parse_big_int_base0(s) {
                val.big_int = Some(n);
                true
            } else {
                false
            };
            if !ok {
                return self.error(start, "invalid int literal");
            }
            Ok(Token::Int)
        }
    }
}

/// Splits an integer literal into its radix and digits, following the
/// base prefix rules of Go's strconv.ParseInt with base 0.
fn split_base0(s: &str) -> (u32, &str) {
    let b = s.as_bytes();
    if b.len() > 1 && b[0] == b'0' {
        match b[1] | 0x20 {
            b'x' => (16, &s[2..]),
            b'o' => (8, &s[2..]),
            b'b' => (2, &s[2..]),
            _ => (8, &s[1..]),
        }
    } else {
        (10, s)
    }
}

fn valid_digits(digits: &str, radix: u32) -> bool {
    !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix))
}

/// Parses an int64 like Go's strconv.ParseInt(s, 0, 64).
fn parse_int_base0(s: &str) -> Option<i64> {
    let (radix, digits) = split_base0(s);
    if !valid_digits(digits, radix) {
        return None;
    }
    i64::from_str_radix(digits, radix).ok()
}

/// Parses an arbitrary-precision integer like Go's big.Int.SetString(s, 0).
fn parse_big_int_base0(s: &str) -> Option<BigInt> {
    let (radix, digits) = split_base0(s);
    if !valid_digits(digits, radix) {
        return None;
    }
    BigInt::from_str_radix(digits, radix).ok()
}

/// Reports whether c is an identifier rune.
fn is_ident(c: char) -> bool {
    c.is_ascii_digit() || is_ident_start(c)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || unicode::is_letter(c)
}

fn is_odigit(c: char) -> bool {
    ('0'..='7').contains(&c)
}

fn is_bdigit(c: char) -> bool {
    c == '0' || c == '1'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::quote;

    fn scan(src: &str) -> std::result::Result<String, Error> {
        let mut sc = Scanner::new("foo.star", src.as_bytes().to_vec(), None);
        let mut buf = String::new();
        let mut val = TokenValue::default();
        loop {
            let tok = sc.next_token(&mut val)?;
            if !buf.is_empty() {
                buf.push(' ');
            }
            match tok {
                Token::Eof => buf.push_str("EOF"),
                Token::Ident => buf.push_str(&val.raw),
                Token::Int => match &val.big_int {
                    Some(b) => buf.push_str(&b.to_string()),
                    None => buf.push_str(&val.int.to_string()),
                },
                Token::Float => buf.push_str(&go_e(val.float)),
                Token::String | Token::Bytes => {
                    buf.push_str(&quote(&val.string, tok == Token::Bytes))
                }
                _ => buf.push_str(tok.name()),
            }
            if tok == Token::Eof {
                break;
            }
        }
        Ok(buf)
    }

    /// Formats f like Go's %e verb.
    fn go_e(f: f64) -> String {
        let s = format!("{f:.6e}");
        let (mant, exp) = s.split_once('e').unwrap();
        let exp: i32 = exp.parse().unwrap();
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    }

    #[test]
    fn test_scanner() {
        let tests: &[(&str, &str)] = &[
            ("", "EOF"),
            ("123", "123 EOF"),
            ("x.y", "x . y EOF"),
            ("chocolate.éclair", "chocolate . éclair EOF"),
            ("123 \"foo\" hello x.y", "123 \"foo\" hello x . y EOF"),
            ("print(x)", "print ( x ) EOF"),
            ("print(x); print(y)", "print ( x ) ; print ( y ) EOF"),
            ("\nprint(\n1\n)\n", "print ( 1 ) newline EOF"),
            ("/ // /= //= ///=", "/ // /= //= // /= EOF"),
            ("# hello\nprint(x)", "print ( x ) EOF"),
            (
                "# hello\nprint(1)\ncc_binary(name=\"foo\")\ndef f(x):\n\t\treturn x+1\nprint(1)\n",
                "print ( 1 ) newline cc_binary ( name = \"foo\" ) newline def f ( x ) : newline indent return x + 1 newline outdent print ( 1 ) newline EOF",
            ),
            ("def f(): pass", "def f ( ) : pass EOF"),
            ("def f():\n\tpass", "def f ( ) : newline indent pass newline outdent EOF"),
            ("def f():\n\tpass\n# oops", "def f ( ) : newline indent pass newline outdent EOF"),
            ("def f():\n\tpass \\\n", "def f ( ) : newline indent pass newline outdent EOF"),
            ("def f():\n\tpass\n", "def f ( ) : newline indent pass newline outdent EOF"),
            ("pass\n\n\npass", "pass newline pass EOF"),
            ("def f():\n    pass\n    ", "def f ( ) : newline indent pass newline outdent EOF"),
            ("def f():\n    pass\n    \n", "def f ( ) : newline indent pass newline outdent EOF"),
            ("pass", "pass EOF"),
            ("pass\n", "pass newline EOF"),
            ("pass\n ", "pass newline EOF"),
            ("pass\n \n", "pass newline EOF"),
            ("if x:\n  pass\n ", "if x : newline indent pass newline outdent EOF"),
            ("x = 1 + \\\n2", "x = 1 + 2 EOF"),
            ("x = 'a\\nb'", "x = \"a\\nb\" EOF"),
            ("x = r'a\\nb'", "x = \"a\\\\nb\" EOF"),
            ("x = 'a\\\nb'", "x = \"ab\" EOF"),
            ("x = '\\''", "x = \"'\" EOF"),
            ("x = \"\\\"\"", "x = \"\\\"\" EOF"),
            ("x = r'\\''", "x = \"\\\\'\" EOF"),
            ("x = '''\\''''", "x = \"'\" EOF"),
            ("x = r'''\\''''", "x = \"\\\\'\" EOF"),
            ("x = ''''a'b'c'''", "x = \"'a'b'c\" EOF"),
            ("x = '''a\nb'''", "x = \"a\\nb\" EOF"),
            ("x = '''a\rb'''", "x = \"a\\nb\" EOF"),
            ("x = '''a\r\nb'''", "x = \"a\\nb\" EOF"),
            ("x = '''a\n\rb'''", "x = \"a\\n\\nb\" EOF"),
            ("x = r'a\\\nb'", "x = \"a\\\\\\nb\" EOF"),
            ("x = r'a\\\rb'", "x = \"a\\\\\\nb\" EOF"),
            ("x = r'a\\\r\nb'", "x = \"a\\\\\\nb\" EOF"),
            ("a\rb", "a newline b EOF"),
            ("a\nb", "a newline b EOF"),
            ("a\r\nb", "a newline b EOF"),
            ("a\n\nb", "a newline b EOF"),
            // numbers
            ("0", "0 EOF"),
            ("00", "0 EOF"),
            ("0.", "0.000000e+00 EOF"),
            ("0.e1", "0.000000e+00 EOF"),
            (".0", "0.000000e+00 EOF"),
            ("0.0", "0.000000e+00 EOF"),
            (".e1", ". e1 EOF"),
            ("1", "1 EOF"),
            ("1.", "1.000000e+00 EOF"),
            (".1", "1.000000e-01 EOF"),
            (".1e1", "1.000000e+00 EOF"),
            (".1e+1", "1.000000e+00 EOF"),
            (".1e-1", "1.000000e-02 EOF"),
            ("1e1", "1.000000e+01 EOF"),
            ("1e+1", "1.000000e+01 EOF"),
            ("1e-1", "1.000000e-01 EOF"),
            ("123", "123 EOF"),
            ("123e45", "1.230000e+47 EOF"),
            (
                "999999999999999999999999999999999999999999999999999",
                "999999999999999999999999999999999999999999999999999 EOF",
            ),
            ("12345678901234567890", "12345678901234567890 EOF"),
            // hex
            ("0xA", "10 EOF"),
            ("0xAAG", "170 G EOF"),
            ("0xG", "foo.star:1:1: invalid hex literal"),
            ("0XA", "10 EOF"),
            ("0XG", "foo.star:1:1: invalid hex literal"),
            ("0xA.", "10 . EOF"),
            ("0xA.e1", "10 . e1 EOF"),
            ("0x12345678deadbeef12345678", "5634002672576678570168178296 EOF"),
            // binary
            ("0b1010", "10 EOF"),
            ("0B111101", "61 EOF"),
            ("0b3", "foo.star:1:3: invalid binary literal"),
            ("0b1010201", "10 201 EOF"),
            ("0b1010.01", "10 1.000000e-02 EOF"),
            ("0b0000", "0 EOF"),
            // octal
            ("0o123", "83 EOF"),
            ("0o12834", "10 834 EOF"),
            ("0o12934", "10 934 EOF"),
            ("0o12934.", "10 9.340000e+02 EOF"),
            ("0o12934.1", "10 9.341000e+02 EOF"),
            ("0o12934e1", "10 9.340000e+03 EOF"),
            ("0o123.", "83 . EOF"),
            ("0o123.1", "83 1.000000e-01 EOF"),
            ("0123", "foo.star:1:5: obsolete form of octal literal; use 0o123"),
            ("012834", "foo.star:1:1: invalid int literal"),
            ("012934", "foo.star:1:1: invalid int literal"),
            ("i = 012934", "foo.star:1:5: invalid int literal"),
            // octal escapes in string literals
            (r#""\037""#, r#""\x1f" EOF"#),
            (
                r#""\377""#,
                r#"foo.star:1:1: non-ASCII octal escape \377 (use \u00FF for the UTF-8 encoding of U+00FF)"#,
            ),
            (r#""\378""#, r#""\x1f8" EOF"#),
            (r#""\400""#, r#"foo.star:1:1: non-ASCII octal escape \400"#),
            // hex escapes
            (r#""\x00\x20\x09\x41\x7e\x7f""#, r#""\x00 \tA~\x7f" EOF"#),
            (r#""\x80""#, "foo.star:1:1: non-ASCII hex escape"),
            (r#""\xff""#, "foo.star:1:1: non-ASCII hex escape"),
            (r#""\xFf""#, "foo.star:1:1: non-ASCII hex escape"),
            (r#""\xF""#, r#"foo.star:1:1: truncated escape sequence \xF"#),
            (r#""\x""#, r#"foo.star:1:1: truncated escape sequence \x"#),
            (r#""\xfg""#, r#"foo.star:1:1: invalid escape sequence \xfg"#),
            // Unicode escapes
            (r#""\u0400""#, r#""Ѐ" EOF"#),
            (r#""\u100""#, r#"foo.star:1:1: truncated escape sequence \u100"#),
            (r#""\u04000""#, r#""Ѐ0" EOF"#),
            (r#""\u100g""#, r#"foo.star:1:1: invalid escape sequence \u100g"#),
            (r#""\u4E16""#, r#""世" EOF"#),
            (r#""\udc00""#, "foo.star:1:1: invalid Unicode code point U+DC00"),
            (r#""\U00000400""#, r#""Ѐ" EOF"#),
            (r#""\U0000400""#, r#"foo.star:1:1: truncated escape sequence \U0000400"#),
            (r#""\U000004000""#, r#""Ѐ0" EOF"#),
            (r#""\U1000000g""#, r#"foo.star:1:1: invalid escape sequence \U1000000g"#),
            (r#""\U0010FFFF""#, r#""\U0010ffff" EOF"#),
            (
                r#""\U00110000""#,
                r#"foo.star:1:1: code point out of range: \U00110000 (max \U00110000)"#,
            ),
            (r#""\U0001F63F""#, r#""😿" EOF"#),
            (r#""\U0000dc00""#, "foo.star:1:1: invalid Unicode code point U+DC00"),
            // backslash escapes
            (r#""foo\(bar""#, r#"foo.star:1:1: invalid escape sequence \("#),
            (r#""\+""#, r#"foo.star:1:1: invalid escape sequence \+"#),
            (r#""\w""#, r#"foo.star:1:1: invalid escape sequence \w"#),
            (r#""\"""#, r#""\"" EOF"#),
            (r#""\'""#, r#""'" EOF"#),
            (r#"'\w'"#, r#"foo.star:1:1: invalid escape sequence \w"#),
            (r#"'\''"#, r#""'" EOF"#),
            (r#"'\"'"#, r#""\"" EOF"#),
            (r#""""\w""""#, r#"foo.star:1:1: invalid escape sequence \w"#),
            (r#""""\"""""#, r#""\"" EOF"#),
            (r#""""\'""""#, r#""'" EOF"#),
            (r#"'''\w'''"#, r#"foo.star:1:1: invalid escape sequence \w"#),
            (r#"'''\''''"#, r#""'" EOF"#),
            (r#"'''\"'''"#, r#""\"" EOF"#),
            (r#"r"\w""#, r#""\\w" EOF"#),
            (r#"r"\"""#, r#""\\\"" EOF"#),
            (r#"r"\'""#, r#""\\'" EOF"#),
            (r#"r'\w'"#, r#""\\w" EOF"#),
            (r#"r'\''"#, r#""\\'" EOF"#),
            (r#"r'\"'"#, r#""\\\"" EOF"#),
            (r#"'a\zb'"#, r#"foo.star:1:1: invalid escape sequence \z"#),
            (r#""\o123""#, r#"foo.star:1:1: invalid escape sequence \o"#),
            // bytes literals (where they differ from text strings)
            (r#"b"AЀ世😿""#, r#"b"AЀ世😿"#),
            (r#"b"\x41\u0400\u4e16\U0001F63F""#, r#"b"AЀ世😿""#),
            (r#"b"\377\378\x80\xff\xFf""#, r#"b"\xff\x1f8\x80\xff\xff" EOF"#),
            (r#"b"\400""#, r#"foo.star:1:2: invalid escape sequence \400"#),
            (r#"b"\udc00""#, "foo.star:1:2: invalid Unicode code point U+DC00"),
            // floats starting with octal digits
            ("012934.", "1.293400e+04 EOF"),
            ("012934.1", "1.293410e+04 EOF"),
            ("012934e1", "1.293400e+05 EOF"),
            ("0123.", "1.230000e+02 EOF"),
            ("0123.1", "1.231000e+02 EOF"),
            // github.com/google/skylark/issues/16
            ("x ! 0", "foo.star:1:3: unexpected input character '!'"),
            // github.com/google/starlark-go/issues/80
            ("([{<>}])", "( [ { < > } ] ) EOF"),
            ("f();", "f ( ) ; EOF"),
            // github.com/google/starlark-go/issues/104
            (
                "def f():\n  if x:\n    pass\n  ",
                "def f ( ) : newline indent if x : newline indent pass newline outdent outdent EOF",
            ),
            ("while cond: pass", "while cond : pass EOF"),
            // github.com/google/starlark-go/issues/107
            ("~= ~= 5", "~ = ~ = 5 EOF"),
            ("0in", "0 in EOF"),
            ("0or", "foo.star:1:3: invalid octal literal"),
            ("6in", "6 in EOF"),
            ("6or", "6 or EOF"),
        ];
        let mut failures = Vec::new();
        for &(input, want) in tests {
            let got = match scan(input) {
                Ok(s) => s,
                Err(e) => e.to_string(),
            };
            // Prefix match allows us to truncate errors in expectations.
            // Success cases all end in EOF.
            if !got.starts_with(want) {
                failures.push(format!("scan `{input}` = [{got}], want [{want}]"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
