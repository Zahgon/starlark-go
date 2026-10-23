// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Source positions (ported from go.starlark.net/syntax/scan.go).

use std::fmt;
use std::rc::Rc;

/// A Position describes the location of a rune of input.
///
/// `line` and `col` are 1-based; 0 means unknown. Columns count runes
/// (Unicode code points), not bytes. A position without a file is invalid
/// and prints its file name as `<invalid>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Position {
    file: Option<Rc<str>>,
    pub line: i32,
    pub col: i32,
}

impl Position {
    /// Returns the position with the specified components.
    pub fn new(file: Rc<str>, line: i32, col: i32) -> Position {
        Position {
            file: Some(file),
            line,
            col,
        }
    }

    /// Reports whether the position is valid (has a file name).
    pub fn is_valid(&self) -> bool {
        self.file.is_some()
    }

    /// Returns the name of the file containing this position.
    pub fn filename(&self) -> &str {
        self.file.as_deref().unwrap_or("<invalid>")
    }

    /// Returns the shared file name, if any.
    pub fn file(&self) -> Option<&Rc<str>> {
        self.file.as_ref()
    }

    /// Returns the same file with a different line and column.
    pub fn with_line_col(&self, line: i32, col: i32) -> Position {
        Position {
            file: self.file.clone(),
            line,
            col,
        }
    }

    /// Returns the position at the end of `s`, assuming it starts at `self`.
    pub fn add(&self, s: &[u8]) -> Position {
        let mut p = self.clone();
        let mut s = s;
        let n = s.iter().filter(|&&b| b == b'\n').count();
        if n > 0 {
            p.line += n as i32;
            let last = s.iter().rposition(|&b| b == b'\n').unwrap();
            s = &s[last + 1..];
            p.col = 1;
        }
        p.col += rune_count(s) as i32;
        p
    }

    /// Reports whether `self` is strictly before `q` (same file assumed).
    pub fn is_before(&self, q: &Position) -> bool {
        if self.line != q.line {
            return self.line < q.line;
        }
        self.col < q.col
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = self.filename();
        if self.line > 0 {
            if self.col > 0 {
                return write!(f, "{}:{}:{}", file, self.line, self.col);
            }
            return write!(f, "{}:{}", file, self.line);
        }
        f.write_str(file)
    }
}

/// Counts the runes in `s` the way Go's utf8.RuneCount does: each invalid
/// byte counts as one rune.
pub fn rune_count(s: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < s.len() {
        let (_, size) = crate::syntax::utf8::decode_rune(&s[i..]);
        i += size;
        n += 1;
    }
    n
}
