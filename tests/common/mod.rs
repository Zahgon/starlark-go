// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Utilities for testing that source code errors are reported in the
//! appropriate places (ported from go.starlark.net/internal/chunkedfile).
//!
//! A chunked file consists of several chunks of input text separated by
//! "---" lines.  Each chunk is an input to the program under test, such
//! as an evaluator.  Lines containing "###" are interpreted as
//! expectations of failure: the following text is a Go string literal
//! denoting a regular expression that should match the failure message.

use std::collections::BTreeMap;

use regex::Regex;

/// A Chunk is a portion of a source file.
/// It contains a set of expected errors.
pub struct Chunk {
    pub source: String,
    filename: String,
    want_errs: BTreeMap<usize, Regex>,
}

/// Collects test failures.
#[derive(Default)]
pub struct Reporter {
    pub failures: Vec<String>,
}

impl Reporter {
    pub fn errorf(&mut self, msg: String) {
        self.failures.push(msg);
    }

    /// Panics if any failure was reported.
    pub fn check(&self) {
        assert!(self.failures.is_empty(), "{}", self.failures.join("\n"));
    }
}

/// Parses a chunked file and returns its chunks.
pub fn read(filename: &str, report: &mut Reporter) -> Vec<Chunk> {
    let data = match std::fs::read_to_string(filename) {
        Ok(d) => d,
        Err(e) => {
            report.errorf(format!("{filename}: {e}"));
            return Vec::new();
        }
    };
    let mut linenum = 1;
    let mut chunks = Vec::new();
    for chunk in data.split("\n---\n") {
        // Pad with newlines so the line numbers match the original file.
        let src = "\n".repeat(linenum - 1) + chunk;

        let mut want_errs = BTreeMap::new();

        // Parse comments of the form:
        // ### "expected error".
        for line in chunk.split('\n') {
            if let Some((_, after)) = line.split_once("###") {
                let rest = after.trim();
                match go_unquote(rest) {
                    None => report.errorf(format!(
                        "\n{filename}:{linenum}: not a quoted regexp: {rest}"
                    )),
                    Some(pattern) => match Regex::new(&pattern) {
                        Ok(rx) => {
                            want_errs.insert(linenum, rx);
                        }
                        Err(e) => report.errorf(format!("\n{filename}:{linenum}: {e}")),
                    },
                }
            }
            linenum += 1;
        }
        linenum += 1;

        chunks.push(Chunk {
            source: src,
            filename: filename.to_string(),
            want_errs,
        });
    }
    chunks
}

impl Chunk {
    /// Should be called by the client to report an error at a particular line.
    /// It reports unexpected errors to the reporter.
    pub fn got_error(&mut self, report: &mut Reporter, linenum: usize, msg: &str) {
        match self.want_errs.remove(&linenum) {
            Some(rx) => {
                if !rx.is_match(msg) {
                    report.errorf(format!(
                        "\n{}:{}: error {:?} does not match pattern {:?}",
                        self.filename,
                        linenum,
                        msg,
                        rx.as_str()
                    ));
                }
            }
            None => report.errorf(format!(
                "\n{}:{}: unexpected error: {}",
                self.filename, linenum, msg
            )),
        }
    }

    /// Should be called by the client to indicate that the chunk has no more errors.
    /// It reports expected errors that did not occur.
    pub fn done(&mut self, report: &mut Reporter) {
        for (linenum, rx) in std::mem::take(&mut self.want_errs) {
            report.errorf(format!(
                "\n{}:{}: expected error matching {:?}",
                self.filename,
                linenum,
                rx.as_str()
            ));
        }
    }
}

/// Interprets s as a Go string literal (like strconv.Unquote).
pub fn go_unquote(s: &str) -> Option<String> {
    if s.len() >= 2 && s.starts_with('`') && s.ends_with('`') {
        let inner = &s[1..s.len() - 1];
        if inner.contains('`') {
            return None;
        }
        return Some(inner.replace('\r', ""));
    }
    if !(s.len() >= 2 && s.starts_with('"') && s.ends_with('"')) {
        return None;
    }
    let mut out = String::new();
    let mut chars = s[1..s.len() - 1].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\n' => return None,
            '\\' => {
                let e = chars.next()?;
                match e {
                    'a' => out.push('\x07'),
                    'b' => out.push('\x08'),
                    'f' => out.push('\x0c'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'v' => out.push('\x0b'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    'x' | 'u' | 'U' => {
                        let n = match e {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        let hex: String = chars.by_ref().take(n).collect();
                        let v = u32::from_str_radix(&hex, 16).ok()?;
                        out.push(char::from_u32(v)?);
                    }
                    '0'..='7' => {
                        let rest: String = chars.by_ref().take(2).collect();
                        let v = u32::from_str_radix(&format!("{e}{rest}"), 8).ok()?;
                        out.push(char::from_u32(v)?);
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}
