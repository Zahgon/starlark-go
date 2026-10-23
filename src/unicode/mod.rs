// Copyright 2017 The Bazel Authors of google/starlark-go. All rights reserved.
// Use of this source code is governed by a BSD-style
// license that can be found in the LICENSE file.

//! Character classes and case mappings with the semantics of Go's
//! `unicode` package and `strconv.IsPrint`, which the original
//! implementation's string methods, scanner and quoting rely on.
//!
//! The Rust standard library follows a newer Unicode version and different
//! definitions (for example `char::is_alphabetic` is the Alphabetic
//! property, not general category L, and `to_uppercase` applies special
//! casing), so the tables are carried here instead.

mod tables;

pub use tables::VERSION;

fn in_ranges(table: &[(u32, u32)], r: char) -> bool {
    let c = r as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn mapped(table: &[(u32, u32)], r: char) -> char {
    match table.binary_search_by_key(&(r as u32), |&(from, _)| from) {
        Ok(i) => char::from_u32(table[i].1).unwrap_or(r),
        Err(_) => r,
    }
}

/// Reports whether `r` is a letter (category L), like Go's unicode.IsLetter.
pub fn is_letter(r: char) -> bool {
    if (r as u32) < 0x80 {
        return r.is_ascii_alphabetic();
    }
    in_ranges(&tables::LETTER, r)
}

/// Reports whether `r` is a decimal digit (category Nd), like unicode.IsDigit.
pub fn is_digit(r: char) -> bool {
    if (r as u32) < 0x80 {
        return r.is_ascii_digit();
    }
    in_ranges(&tables::DIGIT, r)
}

/// Reports whether `r` is white space, like unicode.IsSpace.
pub fn is_space(r: char) -> bool {
    in_ranges(&tables::SPACE, r)
}

/// Reports whether `r` is an upper case letter, like unicode.IsUpper.
pub fn is_upper(r: char) -> bool {
    in_ranges(&tables::UPPER, r)
}

/// Reports whether `r` is a lower case letter, like unicode.IsLower.
pub fn is_lower(r: char) -> bool {
    in_ranges(&tables::LOWER, r)
}

/// Reports whether `r` is a title case letter, like unicode.IsTitle.
pub fn is_title(r: char) -> bool {
    in_ranges(&tables::TITLE, r)
}

/// Reports whether `r` is printable, like strconv.IsPrint.
pub fn is_print(r: char) -> bool {
    in_ranges(&tables::PRINT, r)
}

/// Maps `r` to upper case, like unicode.ToUpper (a single code point).
pub fn to_upper(r: char) -> char {
    mapped(&tables::TO_UPPER, r)
}

/// Maps `r` to lower case, like unicode.ToLower (a single code point).
pub fn to_lower(r: char) -> char {
    mapped(&tables::TO_LOWER, r)
}

/// Maps `r` to title case, like unicode.ToTitle (a single code point).
pub fn to_title(r: char) -> char {
    mapped(&tables::TO_TITLE, r)
}

/// Returns the next code point in the simple case-folding orbit of `r`,
/// like unicode.SimpleFold.
pub fn simple_fold(r: char) -> char {
    mapped(&tables::SIMPLE_FOLD, r)
}
