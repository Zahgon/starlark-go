// Ported from the Go standard library: original work of The Go Authors
// (2010), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Formatting and parsing of times and durations (format.go and
//! format_rfc3339.go).
//!
//! The reference time used in layouts is the specific time stamp
//! `01/02 03:04:05PM '06 -0700` (January 2, 15:04:05, 2006, in time zone
//! seven hours west of GMT). Layouts are byte strings: text that is not
//! recognized as part of the reference time is echoed verbatim during
//! Format and expected to appear verbatim in the input to Parse.

use std::rc::Rc;

use super::zoneinfo::{fixed_zone, Location};
use super::{
    abs_clock, days_before, days_date, days_in, days_weekday, days_year_yday, is_leap, loc_lookup,
    month_string, weekday_string, Time, LONG_DAY_NAMES, LONG_MONTH_NAMES, SECONDS_PER_DAY,
    SHORT_DAY_NAMES, SHORT_MONTH_NAMES,
};

/// The RFC 3339 layout.
pub const RFC3339: &[u8] = b"2006-01-02T15:04:05Z07:00";
/// The RFC 3339 layout with nanoseconds.
const RFC3339_NANO: &[u8] = b"2006-01-02T15:04:05.999999999Z07:00";

const STD_NEED_DATE: i64 = 1 << 8; // need month, day, year
const STD_NEED_YDAY: i64 = 1 << 9; // need yday
const STD_NEED_CLOCK: i64 = 1 << 10; // need hour, minute, second
const STD_ARG_SHIFT: i64 = 16; // extra argument in high bits, above low stdArgShift
const STD_SEPARATOR_SHIFT: i64 = 28; // extra argument in high 4 bits for fractional second separators
const STD_MASK: i64 = (1 << STD_ARG_SHIFT) - 1; // mask out argument

const STD_LONG_MONTH: i64 = 1 + STD_NEED_DATE; // "January"
const STD_MONTH: i64 = 2 + STD_NEED_DATE; // "Jan"
const STD_NUM_MONTH: i64 = 3 + STD_NEED_DATE; // "1"
const STD_ZERO_MONTH: i64 = 4 + STD_NEED_DATE; // "01"
const STD_LONG_WEEK_DAY: i64 = 5 + STD_NEED_DATE; // "Monday"
const STD_WEEK_DAY: i64 = 6 + STD_NEED_DATE; // "Mon"
const STD_DAY: i64 = 7 + STD_NEED_DATE; // "2"
const STD_UNDER_DAY: i64 = 8 + STD_NEED_DATE; // "_2"
const STD_ZERO_DAY: i64 = 9 + STD_NEED_DATE; // "02"
const STD_UNDER_YEAR_DAY: i64 = 10 + STD_NEED_YDAY; // "__2"
const STD_ZERO_YEAR_DAY: i64 = 11 + STD_NEED_YDAY; // "002"
const STD_HOUR: i64 = 12 + STD_NEED_CLOCK; // "15"
const STD_HOUR12: i64 = 13 + STD_NEED_CLOCK; // "3"
const STD_ZERO_HOUR12: i64 = 14 + STD_NEED_CLOCK; // "03"
const STD_MINUTE: i64 = 15 + STD_NEED_CLOCK; // "4"
const STD_ZERO_MINUTE: i64 = 16 + STD_NEED_CLOCK; // "04"
const STD_SECOND: i64 = 17 + STD_NEED_CLOCK; // "5"
const STD_ZERO_SECOND: i64 = 18 + STD_NEED_CLOCK; // "05"
const STD_LONG_YEAR: i64 = 19 + STD_NEED_DATE; // "2006"
const STD_YEAR: i64 = 20 + STD_NEED_DATE; // "06"
const STD_PM: i64 = 21 + STD_NEED_CLOCK; // "PM"
const STD_PM_LOWER: i64 = 22 + STD_NEED_CLOCK; // "pm"
const STD_TZ: i64 = 23; // "MST"
const STD_ISO8601_TZ: i64 = 24; // "Z0700"  // prints Z for UTC
const STD_ISO8601_SECONDS_TZ: i64 = 25; // "Z070000"
const STD_ISO8601_SHORT_TZ: i64 = 26; // "Z07"
const STD_ISO8601_COLON_TZ: i64 = 27; // "Z07:00" // prints Z for UTC
const STD_ISO8601_COLON_SECONDS_TZ: i64 = 28; // "Z07:00:00"
const STD_NUM_TZ: i64 = 29; // "-0700"  // always numeric
const STD_NUM_SECONDS_TZ: i64 = 30; // "-070000"
const STD_NUM_SHORT_TZ: i64 = 31; // "-07"    // always numeric
const STD_NUM_COLON_TZ: i64 = 32; // "-07:00" // always numeric
const STD_NUM_COLON_SECONDS_TZ: i64 = 33; // "-07:00:00"
const STD_FRAC_SECOND0: i64 = 34; // ".0", ".00", ... , trailing zeros included
const STD_FRAC_SECOND9: i64 = 35; // ".9", ".99", ..., trailing zeros omitted

/// The std values for "01", "02", ..., "06".
const STD0X: [i64; 6] = [
    STD_ZERO_MONTH,
    STD_ZERO_DAY,
    STD_ZERO_HOUR12,
    STD_ZERO_MINUTE,
    STD_ZERO_SECOND,
    STD_YEAR,
];

/// Reports whether the string has a lower-case letter at the beginning.
/// Its purpose is to prevent matching strings like "Month" when looking
/// for "Mon".
fn starts_with_lower_case(s: &[u8]) -> bool {
    !s.is_empty() && s[0].is_ascii_lowercase()
}

/// Finds the first occurrence of a std string in layout and returns the
/// text before, the std string, and the text after.
fn next_std_chunk(layout: &[u8]) -> (&[u8], i64, &[u8]) {
    let n = layout.len();
    let at = |i: usize, s: &[u8]| n >= i + s.len() && &layout[i..i + s.len()] == s;
    for i in 0..n {
        let c = layout[i];
        match c {
            b'J' => {
                // January, Jan
                if at(i, b"Jan") {
                    if at(i, b"January") {
                        return (&layout[..i], STD_LONG_MONTH, &layout[i + 7..]);
                    }
                    if !starts_with_lower_case(&layout[i + 3..]) {
                        return (&layout[..i], STD_MONTH, &layout[i + 3..]);
                    }
                }
            }
            b'M' => {
                // Monday, Mon, MST
                if n >= i + 3 {
                    if &layout[i..i + 3] == b"Mon" {
                        if at(i, b"Monday") {
                            return (&layout[..i], STD_LONG_WEEK_DAY, &layout[i + 6..]);
                        }
                        if !starts_with_lower_case(&layout[i + 3..]) {
                            return (&layout[..i], STD_WEEK_DAY, &layout[i + 3..]);
                        }
                    }
                    if &layout[i..i + 3] == b"MST" {
                        return (&layout[..i], STD_TZ, &layout[i + 3..]);
                    }
                }
            }
            b'0' => {
                // 01, 02, 03, 04, 05, 06, 002
                if n >= i + 2 && (b'1'..=b'6').contains(&layout[i + 1]) {
                    return (
                        &layout[..i],
                        STD0X[(layout[i + 1] - b'1') as usize],
                        &layout[i + 2..],
                    );
                }
                if n >= i + 3 && layout[i + 1] == b'0' && layout[i + 2] == b'2' {
                    return (&layout[..i], STD_ZERO_YEAR_DAY, &layout[i + 3..]);
                }
            }
            b'1' => {
                // 15, 1
                if n >= i + 2 && layout[i + 1] == b'5' {
                    return (&layout[..i], STD_HOUR, &layout[i + 2..]);
                }
                return (&layout[..i], STD_NUM_MONTH, &layout[i + 1..]);
            }
            b'2' => {
                // 2006, 2
                if at(i, b"2006") {
                    return (&layout[..i], STD_LONG_YEAR, &layout[i + 4..]);
                }
                return (&layout[..i], STD_DAY, &layout[i + 1..]);
            }
            b'_' => {
                // _2, _2006, __2
                if n >= i + 2 && layout[i + 1] == b'2' {
                    // _2006 is really a literal _, followed by stdLongYear
                    if at(i + 1, b"2006") {
                        return (&layout[..i + 1], STD_LONG_YEAR, &layout[i + 5..]);
                    }
                    return (&layout[..i], STD_UNDER_DAY, &layout[i + 2..]);
                }
                if n >= i + 3 && layout[i + 1] == b'_' && layout[i + 2] == b'2' {
                    return (&layout[..i], STD_UNDER_YEAR_DAY, &layout[i + 3..]);
                }
            }
            b'3' => return (&layout[..i], STD_HOUR12, &layout[i + 1..]),
            b'4' => return (&layout[..i], STD_MINUTE, &layout[i + 1..]),
            b'5' => return (&layout[..i], STD_SECOND, &layout[i + 1..]),
            b'P' => {
                // PM
                if n >= i + 2 && layout[i + 1] == b'M' {
                    return (&layout[..i], STD_PM, &layout[i + 2..]);
                }
            }
            b'p' => {
                // pm
                if n >= i + 2 && layout[i + 1] == b'm' {
                    return (&layout[..i], STD_PM_LOWER, &layout[i + 2..]);
                }
            }
            b'-' => {
                // -070000, -07:00:00, -0700, -07:00, -07
                if at(i, b"-070000") {
                    return (&layout[..i], STD_NUM_SECONDS_TZ, &layout[i + 7..]);
                }
                if at(i, b"-07:00:00") {
                    return (&layout[..i], STD_NUM_COLON_SECONDS_TZ, &layout[i + 9..]);
                }
                if at(i, b"-0700") {
                    return (&layout[..i], STD_NUM_TZ, &layout[i + 5..]);
                }
                if at(i, b"-07:00") {
                    return (&layout[..i], STD_NUM_COLON_TZ, &layout[i + 6..]);
                }
                if at(i, b"-07") {
                    return (&layout[..i], STD_NUM_SHORT_TZ, &layout[i + 3..]);
                }
            }
            b'Z' => {
                // Z070000, Z07:00:00, Z0700, Z07:00,
                if at(i, b"Z070000") {
                    return (&layout[..i], STD_ISO8601_SECONDS_TZ, &layout[i + 7..]);
                }
                if at(i, b"Z07:00:00") {
                    return (&layout[..i], STD_ISO8601_COLON_SECONDS_TZ, &layout[i + 9..]);
                }
                if at(i, b"Z0700") {
                    return (&layout[..i], STD_ISO8601_TZ, &layout[i + 5..]);
                }
                if at(i, b"Z07:00") {
                    return (&layout[..i], STD_ISO8601_COLON_TZ, &layout[i + 6..]);
                }
                if at(i, b"Z07") {
                    return (&layout[..i], STD_ISO8601_SHORT_TZ, &layout[i + 3..]);
                }
            }
            // ,000, or .000, or ,999, or .999 - repeated digits for fractional seconds.
            b'.' | b',' if i + 1 < n && (layout[i + 1] == b'0' || layout[i + 1] == b'9') => {
                let ch = layout[i + 1];
                let mut j = i + 1;
                while j < n && layout[j] == ch {
                    j += 1;
                }
                // String of digits must end here - only fractional second is all digits.
                if !is_digit(layout, j) {
                    let code = if layout[i + 1] == b'9' {
                        STD_FRAC_SECOND9
                    } else {
                        STD_FRAC_SECOND0
                    };
                    let std = std_frac_second(code, (j - (i + 1)) as i64, c);
                    return (&layout[..i], std, &layout[j..]);
                }
            }
            _ => {}
        }
    }
    (layout, 0, b"")
}

/// Reports whether s1 and s2 match ignoring case.
/// It is assumed s1 and s2 are the same length.
fn match_fold(s1: &[u8], s2: &[u8]) -> bool {
    for (&a, &b) in s1.iter().zip(s2) {
        let (mut c1, mut c2) = (a, b);
        if c1 != c2 {
            // Switch to lower-case; 'a'-'A' is known to be a single bit.
            c1 |= b'a' - b'A';
            c2 |= b'a' - b'A';
            if c1 != c2 || !c1.is_ascii_lowercase() {
                return false;
            }
        }
    }
    true
}

/// Looks up the prefix of val in tab, returning the index and the rest.
fn lookup(tab: &[&str], val: &[u8]) -> Result<(i64, usize), ()> {
    for (i, v) in tab.iter().enumerate() {
        let v = v.as_bytes();
        if val.len() >= v.len() && match_fold(&val[..v.len()], v) {
            return Ok((i as i64, v.len()));
        }
    }
    Err(())
}

/// Appends the decimal form of x to b. If the decimal form (excluding
/// sign) is shorter than width, the result is padded with leading 0's.
fn append_int(b: &mut Vec<u8>, x: i64, width: usize) {
    let mut u = x as u64;
    if x < 0 {
        b.push(b'-');
        u = (x as u64).wrapping_neg();
    }
    let digits = u.to_string();
    for _ in digits.len()..width {
        b.push(b'0');
    }
    b.extend_from_slice(digits.as_bytes());
}

/// Parses a decimal integer with optional sign (Go's time.atoi).
fn atoi(s: &[u8]) -> Result<i64, ()> {
    let mut s = s;
    let mut neg = false;
    if !s.is_empty() && (s[0] == b'-' || s[0] == b'+') {
        neg = s[0] == b'-';
        s = &s[1..];
    }
    let (q, rem) = leading_int(s)?;
    let mut x = q as i64;
    if !rem.is_empty() {
        return Err(());
    }
    if neg {
        x = x.wrapping_neg();
    }
    Ok(x)
}

/// Packs the number of digits after the decimal and the separator
/// character (period or comma) into a std value.
fn std_frac_second(code: i64, n: i64, c: u8) -> i64 {
    // Use 0xfff to make the failure case even more absurd.
    if c == b'.' {
        return code | ((n & 0xfff) << STD_ARG_SHIFT);
    }
    code | ((n & 0xfff) << STD_ARG_SHIFT) | 1 << STD_SEPARATOR_SHIFT
}

fn digits_len(std: i64) -> usize {
    ((std >> STD_ARG_SHIFT) & 0xfff) as usize
}

fn separator(std: i64) -> u8 {
    if (std >> STD_SEPARATOR_SHIFT) == 0 {
        b'.'
    } else {
        b','
    }
}

/// Appends a fractional second, as nanoseconds, to b.
/// The nanosec must be within [0, 999999999].
fn append_nano(b: &mut Vec<u8>, nanosec: i64, std: i64) {
    let trim = std & STD_MASK == STD_FRAC_SECOND9;
    let n = digits_len(std);
    if trim && (n == 0 || nanosec == 0) {
        return;
    }
    let dot = separator(std);
    b.push(dot);
    append_int(b, nanosec, 9);
    if n < 9 {
        b.truncate(b.len() - 9 + n);
    }
    if trim {
        while b.last() == Some(&b'0') {
            b.pop();
        }
        if b.last() == Some(&dot) {
            b.pop();
        }
    }
}

impl Time {
    /// Returns a textual representation of the time value formatted
    /// according to the layout.
    pub fn format(&self, layout: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(layout.len() + 10);
        // Optimize for RFC3339 as it accounts for over half of all representations.
        if layout == RFC3339 {
            self.append_format_rfc3339(&mut b, false);
        } else if layout == RFC3339_NANO {
            self.append_format_rfc3339(&mut b, true);
        } else {
            self.append_format(&mut b, layout);
        }
        b
    }

    fn append_format(&self, b: &mut Vec<u8>, layout: &[u8]) {
        let (name, offset, abs) = self.locabs();
        let days = abs / SECONDS_PER_DAY as u64;

        let mut year: i64 = -1;
        let mut month: i64 = 0;
        let mut day: i64 = 0;
        let mut yday: i64 = -1;
        let mut hour: i64 = -1;
        let mut min: i64 = 0;
        let mut sec: i64 = 0;

        let mut layout = layout;
        // Each iteration generates one std value.
        while !layout.is_empty() {
            let (prefix, std, suffix) = next_std_chunk(layout);
            b.extend_from_slice(prefix);
            if std == 0 {
                break;
            }
            layout = suffix;

            // Compute year, month, day if needed.
            if year < 0 && std & STD_NEED_DATE != 0 {
                (year, month, day) = days_date(days);
            }
            if yday < 0 && std & STD_NEED_YDAY != 0 {
                yday = days_year_yday(days).1;
            }

            // Compute hour, minute, second if needed.
            if hour < 0 && std & STD_NEED_CLOCK != 0 {
                (hour, min, sec) = abs_clock(abs);
            }

            match std & STD_MASK {
                STD_YEAR => {
                    let y = if year < 0 { year.wrapping_neg() } else { year };
                    append_int(b, y % 100, 2);
                }
                STD_LONG_YEAR => append_int(b, year, 4),
                STD_MONTH => b.extend_from_slice(&month_string(month)[..3]),
                STD_LONG_MONTH => b.extend_from_slice(&month_string(month)),
                STD_NUM_MONTH => append_int(b, month, 0),
                STD_ZERO_MONTH => append_int(b, month, 2),
                STD_WEEK_DAY => b.extend_from_slice(&weekday_string(days_weekday(days))[..3]),
                STD_LONG_WEEK_DAY => b.extend_from_slice(&weekday_string(days_weekday(days))),
                STD_DAY => append_int(b, day, 0),
                STD_UNDER_DAY => {
                    if day < 10 {
                        b.push(b' ');
                    }
                    append_int(b, day, 0);
                }
                STD_ZERO_DAY => append_int(b, day, 2),
                STD_UNDER_YEAR_DAY => {
                    if yday < 100 {
                        b.push(b' ');
                        if yday < 10 {
                            b.push(b' ');
                        }
                    }
                    append_int(b, yday, 0);
                }
                STD_ZERO_YEAR_DAY => append_int(b, yday, 3),
                STD_HOUR => append_int(b, hour, 2),
                STD_HOUR12 | STD_ZERO_HOUR12 => {
                    // Noon is 12PM, midnight is 12AM.
                    let mut hr = hour % 12;
                    if hr == 0 {
                        hr = 12;
                    }
                    append_int(b, hr, if std == STD_HOUR12 { 0 } else { 2 });
                }
                STD_MINUTE => append_int(b, min, 0),
                STD_ZERO_MINUTE => append_int(b, min, 2),
                STD_SECOND => append_int(b, sec, 0),
                STD_ZERO_SECOND => append_int(b, sec, 2),
                STD_PM => b.extend_from_slice(if hour >= 12 { b"PM" } else { b"AM" }),
                STD_PM_LOWER => b.extend_from_slice(if hour >= 12 { b"pm" } else { b"am" }),
                STD_ISO8601_TZ
                | STD_ISO8601_COLON_TZ
                | STD_ISO8601_SECONDS_TZ
                | STD_ISO8601_SHORT_TZ
                | STD_ISO8601_COLON_SECONDS_TZ
                | STD_NUM_TZ
                | STD_NUM_COLON_TZ
                | STD_NUM_SECONDS_TZ
                | STD_NUM_SHORT_TZ
                | STD_NUM_COLON_SECONDS_TZ => {
                    // Ugly special case. We cheat and take the "Z" variants
                    // to mean "the time zone as formatted for ISO 8601".
                    if offset == 0
                        && matches!(
                            std,
                            STD_ISO8601_TZ
                                | STD_ISO8601_COLON_TZ
                                | STD_ISO8601_SECONDS_TZ
                                | STD_ISO8601_SHORT_TZ
                                | STD_ISO8601_COLON_SECONDS_TZ
                        )
                    {
                        b.push(b'Z');
                        continue;
                    }
                    let mut zone = offset / 60; // convert to minutes
                    let mut absoffset = offset;
                    if zone < 0 {
                        b.push(b'-');
                        zone = -zone;
                        absoffset = -absoffset;
                    } else {
                        b.push(b'+');
                    }
                    append_int(b, zone / 60, 2);
                    if matches!(
                        std,
                        STD_ISO8601_COLON_TZ
                            | STD_NUM_COLON_TZ
                            | STD_ISO8601_COLON_SECONDS_TZ
                            | STD_NUM_COLON_SECONDS_TZ
                    ) {
                        b.push(b':');
                    }
                    if std != STD_NUM_SHORT_TZ && std != STD_ISO8601_SHORT_TZ {
                        append_int(b, zone % 60, 2);
                    }

                    // append seconds if appropriate
                    if matches!(
                        std,
                        STD_ISO8601_SECONDS_TZ
                            | STD_NUM_SECONDS_TZ
                            | STD_NUM_COLON_SECONDS_TZ
                            | STD_ISO8601_COLON_SECONDS_TZ
                    ) {
                        if std == STD_NUM_COLON_SECONDS_TZ || std == STD_ISO8601_COLON_SECONDS_TZ {
                            b.push(b':');
                        }
                        append_int(b, absoffset % 60, 2);
                    }
                }
                STD_TZ => {
                    if !name.is_empty() {
                        b.extend_from_slice(&name);
                        continue;
                    }
                    // No time zone known for this time, but we must print one.
                    // Use the -0700 format.
                    let mut zone = offset / 60; // convert to minutes
                    if zone < 0 {
                        b.push(b'-');
                        zone = -zone;
                    } else {
                        b.push(b'+');
                    }
                    append_int(b, zone / 60, 2);
                    append_int(b, zone % 60, 2);
                }
                STD_FRAC_SECOND0 | STD_FRAC_SECOND9 => append_nano(b, self.nanosecond(), std),
                _ => {}
            }
        }
    }

    fn append_format_rfc3339(&self, b: &mut Vec<u8>, nanos: bool) {
        let (_, offset, abs) = self.locabs();

        // Format date.
        let (year, month, day) = days_date(abs / SECONDS_PER_DAY as u64);
        append_int(b, year, 4);
        b.push(b'-');
        append_int(b, month, 2);
        b.push(b'-');
        append_int(b, day, 2);

        b.push(b'T');

        // Format time.
        let (hour, min, sec) = abs_clock(abs);
        append_int(b, hour, 2);
        b.push(b':');
        append_int(b, min, 2);
        b.push(b':');
        append_int(b, sec, 2);

        if nanos {
            let std = std_frac_second(STD_FRAC_SECOND9, 9, b'.');
            append_nano(b, self.nanosecond(), std);
        }

        if offset == 0 {
            b.push(b'Z');
            return;
        }

        // Format zone.
        let mut zone = offset / 60; // convert to minutes
        if zone < 0 {
            b.push(b'-');
            zone = -zone;
        } else {
            b.push(b'+');
        }
        append_int(b, zone / 60, 2);
        b.push(b':');
        append_int(b, zone % 60, 2);
    }
}

/// Quotes a string like Go's time package (not strconv.Quote): non-ASCII
/// and control bytes are written as \x escapes.
pub fn quote(s: &[u8]) -> String {
    const LOWERHEX: &[u8] = b"0123456789abcdef";
    let mut buf = String::with_capacity(s.len() + 2);
    buf.push('"');
    let mut i = 0;
    while i < s.len() {
        let (c, size) = crate::syntax::utf8::decode_rune(&s[i..]);
        if c as u32 >= 0x80 || (c as u32) < (b' ' as u32) {
            let width = if c == '\u{FFFD}' {
                if i + 2 < s.len() && &s[i..i + 3] == "\u{FFFD}".as_bytes() {
                    3
                } else {
                    1
                }
            } else {
                c.len_utf8()
            };
            for j in 0..width {
                buf.push_str("\\x");
                buf.push(LOWERHEX[(s[i + j] >> 4) as usize] as char);
                buf.push(LOWERHEX[(s[i + j] & 0xF) as usize] as char);
            }
        } else {
            if c == '"' || c == '\\' {
                buf.push('\\');
            }
            buf.push(c);
        }
        i += size;
    }
    buf.push('"');
    buf
}

/// Describes a problem parsing a time string (Go's ParseError), rendered
/// as its Error() text.
fn parse_error(
    layout: &[u8],
    value: &[u8],
    layout_elem: &[u8],
    value_elem: &[u8],
    message: &str,
) -> String {
    if message.is_empty() {
        return format!(
            "parsing time {} as {}: cannot parse {} as {}",
            quote(value),
            quote(layout),
            quote(value_elem),
            quote(layout_elem)
        );
    }
    format!("parsing time {}{}", quote(value), message)
}

/// Reports whether s[i] is in range and is a decimal digit.
fn is_digit(s: &[u8], i: usize) -> bool {
    i < s.len() && s[i].is_ascii_digit()
}

/// Parses s[0:1] or s[0:2] (fixed forces s[0:2]) as a decimal integer
/// and returns the integer and the remainder of the string.
fn getnum(s: &[u8], fixed: bool) -> Result<(i64, &[u8]), ()> {
    if !is_digit(s, 0) {
        return Err(());
    }
    if !is_digit(s, 1) {
        if fixed {
            return Err(());
        }
        return Ok(((s[0] - b'0') as i64, &s[1..]));
    }
    Ok(((s[0] - b'0') as i64 * 10 + (s[1] - b'0') as i64, &s[2..]))
}

/// Parses s[0:1], s[0:2], or s[0:3] (fixed forces s[0:3]) as a decimal
/// integer and returns the integer and the remainder of the string.
fn getnum3(s: &[u8], fixed: bool) -> Result<(i64, &[u8]), ()> {
    let mut n = 0;
    let mut i = 0;
    while i < 3 && is_digit(s, i) {
        n = n * 10 + (s[i] - b'0') as i64;
        i += 1;
    }
    if i == 0 || fixed && i != 3 {
        return Err(());
    }
    Ok((n, &s[i..]))
}

fn cutspace(mut s: &[u8]) -> &[u8] {
    while !s.is_empty() && s[0] == b' ' {
        s = &s[1..];
    }
    s
}

/// Removes the given prefix from value, treating runs of space characters
/// as equivalent. On failure it returns the remaining value.
fn skip<'a>(mut value: &'a [u8], mut prefix: &[u8]) -> Result<&'a [u8], &'a [u8]> {
    while !prefix.is_empty() {
        if prefix[0] == b' ' {
            if !value.is_empty() && value[0] != b' ' {
                return Err(value);
            }
            prefix = cutspace(prefix);
            value = cutspace(value);
            continue;
        }
        if value.is_empty() || value[0] != prefix[0] {
            return Err(value);
        }
        prefix = &prefix[1..];
        value = &value[1..];
    }
    Ok(value)
}

/// Parses a formatted string and returns the time value it represents
/// (Go's time.Parse). In the absence of a time zone indicator, Parse
/// returns a time in UTC; zone offsets and abbreviations are matched
/// against the Local location.
pub fn parse(layout: &[u8], value: &[u8]) -> Result<Time, String> {
    let local = Some(super::local());
    // Optimize for RFC3339 as it accounts for over half of all representations.
    if layout == RFC3339 || layout == RFC3339_NANO {
        if let Some(t) = parse_rfc3339(value, &local) {
            return Ok(t);
        }
    }
    parse_impl(layout, value, None, &local)
}

/// Like parse but interprets a time without zone information in the given
/// location, and matches zone offsets and abbreviations against it
/// (Go's time.ParseInLocation).
pub fn parse_in_location(
    layout: &[u8],
    value: &[u8],
    loc: Option<Rc<Location>>,
) -> Result<Time, String> {
    // Optimize for RFC3339 as it accounts for over half of all representations.
    if layout == RFC3339 || layout == RFC3339_NANO {
        if let Some(t) = parse_rfc3339(value, &loc) {
            return Ok(t);
        }
    }
    parse_impl(layout, value, loc.clone(), &loc)
}

fn parse_impl(
    layout: &[u8],
    value: &[u8],
    default_location: Option<Rc<Location>>,
    local: &Option<Rc<Location>>,
) -> Result<Time, String> {
    let (alayout, avalue) = (layout, value);
    let mut layout = layout;
    let mut value = value;
    let mut range_err_string = ""; // set if a value is out of range
    let mut am_set = false; // do we need to subtract 12 from the hour for midnight?
    let mut pm_set = false; // do we need to add 12 to the hour?

    // Time being constructed.
    let mut year: i64 = 0;
    let mut month: i64 = -1;
    let mut day: i64 = -1;
    let mut yday: i64 = -1;
    let mut hour: i64 = 0;
    let mut min: i64 = 0;
    let mut sec: i64 = 0;
    let mut nsec: i64 = 0;
    let mut z: Option<Option<Rc<Location>>> = None;
    let mut zone_offset: i64 = -1;
    let mut zone_name: &[u8] = b"";

    // Each iteration processes one std value.
    loop {
        let (prefix, std, suffix) = next_std_chunk(layout);
        let stdstr = &layout[prefix.len()..layout.len() - suffix.len()];
        value = match skip(value, prefix) {
            Ok(v) => v,
            Err(v) => return Err(parse_error(alayout, avalue, prefix, v, "")),
        };
        if std == 0 {
            if !value.is_empty() {
                return Err(parse_error(
                    alayout,
                    avalue,
                    b"",
                    value,
                    &format!(": extra text: {}", quote(value)),
                ));
            }
            break;
        }
        layout = suffix;
        let hold = value;
        let mut err = false;
        match std & STD_MASK {
            STD_YEAR => {
                if value.len() < 2 {
                    err = true;
                } else {
                    let p = &value[..2];
                    value = &value[2..];
                    match atoi(p) {
                        Ok(y) => year = if y >= 69 { y + 1900 } else { y + 2000 },
                        Err(()) => {
                            year = 0;
                            err = true;
                        }
                    }
                }
            }
            STD_LONG_YEAR => {
                if value.len() < 4 || !is_digit(value, 0) {
                    err = true;
                } else {
                    let p = &value[..4];
                    value = &value[4..];
                    match atoi(p) {
                        Ok(y) => year = y,
                        Err(()) => {
                            year = 0;
                            err = true;
                        }
                    }
                }
            }
            STD_MONTH | STD_LONG_MONTH => {
                let tab: &[&str] = if std == STD_MONTH {
                    &SHORT_MONTH_NAMES
                } else {
                    &LONG_MONTH_NAMES
                };
                match lookup(tab, value) {
                    Ok((i, n)) => {
                        month = i + 1;
                        value = &value[n..];
                    }
                    Err(()) => {
                        month = 0;
                        err = true;
                    }
                }
            }
            STD_NUM_MONTH | STD_ZERO_MONTH => match getnum(value, std == STD_ZERO_MONTH) {
                Ok((m, rest)) => {
                    month = m;
                    value = rest;
                    if month <= 0 || 12 < month {
                        range_err_string = "month";
                    }
                }
                Err(()) => {
                    month = 0;
                    err = true;
                }
            },
            STD_WEEK_DAY | STD_LONG_WEEK_DAY => {
                // Ignore weekday except for error checking.
                let tab: &[&str] = if std == STD_WEEK_DAY {
                    &SHORT_DAY_NAMES
                } else {
                    &LONG_DAY_NAMES
                };
                match lookup(tab, value) {
                    Ok((_, n)) => value = &value[n..],
                    Err(()) => err = true,
                }
            }
            STD_DAY | STD_UNDER_DAY | STD_ZERO_DAY => {
                if std == STD_UNDER_DAY && !value.is_empty() && value[0] == b' ' {
                    value = &value[1..];
                }
                match getnum(value, std == STD_ZERO_DAY) {
                    Ok((d, rest)) => {
                        day = d;
                        value = rest;
                    }
                    Err(()) => {
                        day = 0;
                        err = true;
                    }
                }
                // Note that we allow any one- or two-digit day here.
                // The month, day, year combination is validated after we've completed parsing.
            }
            STD_UNDER_YEAR_DAY | STD_ZERO_YEAR_DAY => {
                for _ in 0..2 {
                    if std == STD_UNDER_YEAR_DAY && !value.is_empty() && value[0] == b' ' {
                        value = &value[1..];
                    }
                }
                match getnum3(value, std == STD_ZERO_YEAR_DAY) {
                    Ok((d, rest)) => {
                        yday = d;
                        value = rest;
                    }
                    Err(()) => {
                        yday = 0;
                        err = true;
                    }
                }
                // Note that we allow any one-, two-, or three-digit year-day here.
                // The year-day, year combination is validated after we've completed parsing.
            }
            STD_HOUR => {
                match getnum(value, false) {
                    Ok((h, rest)) => {
                        hour = h;
                        value = rest;
                    }
                    Err(()) => {
                        hour = 0;
                        err = true;
                    }
                }
                if !(0..24).contains(&hour) {
                    range_err_string = "hour";
                }
            }
            STD_HOUR12 | STD_ZERO_HOUR12 => {
                match getnum(value, std == STD_ZERO_HOUR12) {
                    Ok((h, rest)) => {
                        hour = h;
                        value = rest;
                    }
                    Err(()) => {
                        hour = 0;
                        err = true;
                    }
                }
                if !(0..=12).contains(&hour) {
                    range_err_string = "hour";
                }
            }
            STD_MINUTE | STD_ZERO_MINUTE => {
                match getnum(value, std == STD_ZERO_MINUTE) {
                    Ok((m, rest)) => {
                        min = m;
                        value = rest;
                    }
                    Err(()) => {
                        min = 0;
                        err = true;
                    }
                }
                if !(0..60).contains(&min) {
                    range_err_string = "minute";
                }
            }
            STD_SECOND | STD_ZERO_SECOND => match getnum(value, std == STD_ZERO_SECOND) {
                Err(()) => {
                    sec = 0;
                    err = true;
                }
                Ok((s, rest)) => {
                    sec = s;
                    value = rest;
                    if !(0..60).contains(&sec) {
                        range_err_string = "second";
                    } else if value.len() >= 2 && comma_or_period(value[0]) && is_digit(value, 1) {
                        // Special case: do we have a fractional second but no
                        // fractional second in the format?
                        let (_, next, _) = next_std_chunk(layout);
                        let next = next & STD_MASK;
                        if next != STD_FRAC_SECOND0 && next != STD_FRAC_SECOND9 {
                            // No fractional second in the layout but we have one in the input.
                            let mut n = 2;
                            while n < value.len() && is_digit(value, n) {
                                n += 1;
                            }
                            match parse_nanoseconds(value, n) {
                                Ok((ns, r)) => {
                                    nsec = ns;
                                    range_err_string = r;
                                }
                                Err(()) => {
                                    nsec = 0;
                                    err = true;
                                }
                            }
                            value = &value[n..];
                        }
                    }
                }
            },
            STD_PM | STD_PM_LOWER => {
                if value.len() < 2 {
                    err = true;
                } else {
                    let p = &value[..2];
                    value = &value[2..];
                    let (pm, am): (&[u8], &[u8]) = if std == STD_PM {
                        (b"PM", b"AM")
                    } else {
                        (b"pm", b"am")
                    };
                    if p == pm {
                        pm_set = true;
                    } else if p == am {
                        am_set = true;
                    } else {
                        err = true;
                    }
                }
            }
            STD_ISO8601_TZ
            | STD_ISO8601_SHORT_TZ
            | STD_ISO8601_COLON_TZ
            | STD_ISO8601_SECONDS_TZ
            | STD_ISO8601_COLON_SECONDS_TZ
            | STD_NUM_TZ
            | STD_NUM_SHORT_TZ
            | STD_NUM_COLON_TZ
            | STD_NUM_SECONDS_TZ
            | STD_NUM_COLON_SECONDS_TZ => {
                let iso = matches!(
                    std,
                    STD_ISO8601_TZ
                        | STD_ISO8601_SHORT_TZ
                        | STD_ISO8601_COLON_TZ
                        | STD_ISO8601_SECONDS_TZ
                        | STD_ISO8601_COLON_SECONDS_TZ
                );
                if iso && !value.is_empty() && value[0] == b'Z' {
                    value = &value[1..];
                    z = Some(None);
                } else {
                    match parse_zone_offset(std, value) {
                        Err(()) => err = true,
                        Ok((sign, hr, mm, ss, rest)) => {
                            value = rest;
                            let mut nums = (0, 0, 0);
                            match getnum(hr, true) {
                                Err(()) => err = true,
                                Ok((h, _)) => {
                                    nums.0 = h;
                                    match getnum(mm, true) {
                                        Err(()) => err = true,
                                        Ok((m, _)) => {
                                            nums.1 = m;
                                            match getnum(ss, true) {
                                                Err(()) => err = true,
                                                Ok((s, _)) => nums.2 = s,
                                            }
                                        }
                                    }
                                }
                            }
                            let (hr, mm, ss) = nums;

                            // The range test use > rather than >=,
                            // as some people do write offsets of 24 hours
                            // or 60 minutes or 60 seconds.
                            if hr > 24 {
                                range_err_string = "time zone offset hour";
                            }
                            if mm > 60 {
                                range_err_string = "time zone offset minute";
                            }
                            if ss > 60 {
                                range_err_string = "time zone offset second";
                            }

                            zone_offset = (hr * 60 + mm) * 60 + ss; // offset is in seconds
                            match sign {
                                b'+' => {}
                                b'-' => zone_offset = -zone_offset,
                                _ => err = true,
                            }
                        }
                    }
                }
            }
            STD_TZ => {
                // Does it look like a time zone?
                if value.len() >= 3 && &value[..3] == b"UTC" {
                    z = Some(None);
                    value = &value[3..];
                } else {
                    match parse_time_zone(value) {
                        Some(n) => {
                            zone_name = &value[..n];
                            value = &value[n..];
                        }
                        None => err = true,
                    }
                }
            }
            STD_FRAC_SECOND0 => {
                // stdFracSecond0 requires the exact number of digits as specified in
                // the layout.
                let ndigit = 1 + digits_len(std);
                if value.len() < ndigit {
                    err = true;
                } else {
                    match parse_nanoseconds(value, ndigit) {
                        Ok((ns, r)) => {
                            nsec = ns;
                            range_err_string = r;
                        }
                        Err(()) => {
                            nsec = 0;
                            err = true;
                        }
                    }
                    value = &value[ndigit..];
                }
            }
            STD_FRAC_SECOND9 => {
                if value.len() < 2 || !comma_or_period(value[0]) || !value[1].is_ascii_digit() {
                    // Fractional second omitted.
                } else {
                    // Take any number of digits, even more than asked for,
                    // because it is what the stdSecond case would do.
                    let mut i = 0;
                    while i + 1 < value.len() && value[i + 1].is_ascii_digit() {
                        i += 1;
                    }
                    match parse_nanoseconds(value, 1 + i) {
                        Ok((ns, r)) => {
                            nsec = ns;
                            range_err_string = r;
                        }
                        Err(()) => {
                            nsec = 0;
                            err = true;
                        }
                    }
                    value = &value[1 + i..];
                }
            }
            _ => {}
        }
        if !range_err_string.is_empty() {
            return Err(parse_error(
                alayout,
                avalue,
                stdstr,
                value,
                &format!(": {} out of range", range_err_string),
            ));
        }
        if err {
            return Err(parse_error(alayout, avalue, stdstr, hold, ""));
        }
    }
    if pm_set && hour < 12 {
        hour += 12;
    } else if am_set && hour == 12 {
        hour = 0;
    }

    // Convert yday to day, month.
    if yday >= 0 {
        let mut d = 0;
        let mut m = 0;
        if is_leap(year) {
            if yday == 31 + 29 {
                m = 2;
                d = 29;
            } else if yday > 31 + 29 {
                yday -= 1;
            }
        }
        if !(1..=365).contains(&yday) {
            return Err(parse_error(
                alayout,
                avalue,
                b"",
                value,
                ": day-of-year out of range",
            ));
        }
        if m == 0 {
            m = (yday - 1) / 31 + 1;
            if days_before(m + 1) < yday {
                m += 1;
            }
            d = yday - days_before(m);
        }
        // If month, day already seen, yday's m, d must match.
        // Otherwise, set them from m, d.
        if month >= 0 && month != m {
            return Err(parse_error(
                alayout,
                avalue,
                b"",
                value,
                ": day-of-year does not match month",
            ));
        }
        month = m;
        if day >= 0 && day != d {
            return Err(parse_error(
                alayout,
                avalue,
                b"",
                value,
                ": day-of-year does not match day",
            ));
        }
        day = d;
    } else {
        if month < 0 {
            month = 1;
        }
        if day < 0 {
            day = 1;
        }
    }

    // Validate the day of the month.
    if day < 1 || day > days_in(month, year) {
        return Err(parse_error(
            alayout,
            avalue,
            b"",
            value,
            ": day out of range",
        ));
    }

    if let Some(z) = z {
        return Ok(Time::date(year, month, day, hour, min, sec, nsec, z));
    }

    if zone_offset != -1 {
        let mut t = Time::date(year, month, day, hour, min, sec, nsec, None);
        t.add_sec(-zone_offset);

        // Look for local zone with the given offset.
        // If that zone was in effect at the given time, use it.
        let zl = loc_lookup(local, t.unix_sec());
        if zl.offset == zone_offset && (zone_name.is_empty() || zl.name == zone_name) {
            t.set_loc(local.clone());
            return Ok(t);
        }

        // Otherwise create fake zone to record offset.
        t.set_loc(Some(fixed_zone(zone_name, zone_offset)));
        return Ok(t);
    }

    if !zone_name.is_empty() {
        let mut t = Time::date(year, month, day, hour, min, sec, nsec, None);
        // Look for local zone with the given offset.
        // If that zone was in effect at the given time, use it.
        if let Some(l) = local {
            if let Some(offset) = l.lookup_name(zone_name, t.unix_sec()) {
                t.add_sec(-offset);
                t.set_loc(local.clone());
                return Ok(t);
            }
        }

        // Otherwise, create fake zone with unknown offset.
        let mut offset = 0;
        if zone_name.len() > 3 && &zone_name[..3] == b"GMT" {
            offset = atoi(&zone_name[3..]).unwrap_or(0); // Guaranteed OK by parseGMT.
            offset *= 3600;
        }
        t.set_loc(Some(fixed_zone(zone_name, offset)));
        return Ok(t);
    }

    // Otherwise, fall back to default.
    Ok(Time::date(
        year,
        month,
        day,
        hour,
        min,
        sec,
        nsec,
        default_location,
    ))
}

/// Splits a numeric zone offset off the front of value according to the
/// layout element std: (sign, hour, minute, second, rest).
#[allow(clippy::type_complexity)]
fn parse_zone_offset(std: i64, value: &[u8]) -> Result<(u8, &[u8], &[u8], &[u8], &[u8]), ()> {
    if std == STD_ISO8601_COLON_TZ || std == STD_NUM_COLON_TZ {
        if value.len() < 6 || value[3] != b':' {
            return Err(());
        }
        Ok((value[0], &value[1..3], &value[4..6], b"00", &value[6..]))
    } else if std == STD_NUM_SHORT_TZ || std == STD_ISO8601_SHORT_TZ {
        if value.len() < 3 {
            return Err(());
        }
        Ok((value[0], &value[1..3], b"00", b"00", &value[3..]))
    } else if std == STD_ISO8601_COLON_SECONDS_TZ || std == STD_NUM_COLON_SECONDS_TZ {
        if value.len() < 9 || value[3] != b':' || value[6] != b':' {
            return Err(());
        }
        Ok((
            value[0],
            &value[1..3],
            &value[4..6],
            &value[7..9],
            &value[9..],
        ))
    } else if std == STD_ISO8601_SECONDS_TZ || std == STD_NUM_SECONDS_TZ {
        if value.len() < 7 {
            return Err(());
        }
        Ok((
            value[0],
            &value[1..3],
            &value[3..5],
            &value[5..7],
            &value[7..],
        ))
    } else {
        if value.len() < 5 {
            return Err(());
        }
        Ok((value[0], &value[1..3], &value[3..5], b"00", &value[5..]))
    }
}

/// Parses a time zone string and returns its length. Time zones are
/// human-generated and unpredictable. We can't do precise error checking.
/// On the other hand, for a correct parse there must be a time zone at the
/// beginning of the string, so it's almost always true that there's one
/// there. We look at the beginning of the string for a run of upper-case
/// letters. If there are more than 5, it's an error. If there are 4 or 5
/// and the last is a T, it's a time zone. If there are 3, it's a time
/// zone. Otherwise, other than special cases, it's not a time zone. GMT is
/// special because it can have an hour offset.
fn parse_time_zone(value: &[u8]) -> Option<usize> {
    if value.len() < 3 {
        return None;
    }
    // Special case 1: ChST and MeST are the only zones with a lower-case letter.
    if value.len() >= 4 && (&value[..4] == b"ChST" || &value[..4] == b"MeST") {
        return Some(4);
    }
    // Special case 2: GMT may have an hour offset; treat it specially.
    if &value[..3] == b"GMT" {
        return Some(parse_gmt(value));
    }
    // Special Case 3: Some time zones are not named, but have +/-00 format
    if value[0] == b'+' || value[0] == b'-' {
        let length = parse_signed_offset(value);
        return if length > 0 { Some(length) } else { None };
    }
    // How many upper-case letters are there? Need at least three, at most five.
    let mut n_upper = 0;
    while n_upper < 6 {
        if n_upper >= value.len() || !value[n_upper].is_ascii_uppercase() {
            break;
        }
        n_upper += 1;
    }
    match n_upper {
        0 | 1 | 2 | 6 => None,
        // Must end in T to match.
        5 if value[4] == b'T' => Some(5),
        // Must end in T, except one special case.
        4 if value[3] == b'T' || &value[..4] == b"WITA" => Some(4),
        3 => Some(3),
        _ => None,
    }
}

/// Parses a GMT time zone. The input string is known to start "GMT". The
/// function checks whether that is followed by a sign and a number in the
/// range -23 through +23 excluding zero.
fn parse_gmt(value: &[u8]) -> usize {
    let value = &value[3..];
    if value.is_empty() {
        return 3;
    }
    3 + parse_signed_offset(value)
}

/// Parses a signed timezone offset (e.g. "+03" or "-04"). The function
/// checks for a signed number in the range -23 through +23 excluding zero.
/// Returns length of the found offset string or 0 otherwise.
fn parse_signed_offset(value: &[u8]) -> usize {
    let sign = value[0];
    if sign != b'-' && sign != b'+' {
        return 0;
    }
    match leading_int(&value[1..]) {
        // fail if nothing consumed by leadingInt
        Ok((x, rem)) if rem.len() != value.len() - 1 => {
            if x > 23 {
                return 0;
            }
            value.len() - rem.len()
        }
        _ => 0,
    }
}

fn comma_or_period(b: u8) -> bool {
    b == b'.' || b == b','
}

/// Parses the fractional second value[1:nbytes] (value[0] is the
/// separator), returning nanoseconds and a range error string.
fn parse_nanoseconds(value: &[u8], nbytes: usize) -> Result<(i64, &'static str), ()> {
    if !comma_or_period(value[0]) {
        return Err(());
    }
    let mut value = value;
    let mut nbytes = nbytes;
    if nbytes > 10 {
        value = &value[..10];
        nbytes = 10;
    }
    let mut ns = atoi(&value[1..nbytes])?;
    if ns < 0 {
        return Ok((ns, "fractional second"));
    }
    // We need nanoseconds, which means scaling by the number
    // of missing digits in the format, maximum length 10.
    let scale_digits = 10 - nbytes;
    for _ in 0..scale_digits {
        ns *= 10;
    }
    Ok((ns, ""))
}

/// Consumes the leading [0-9]* from s.
fn leading_int(s: &[u8]) -> Result<(u64, &[u8]), ()> {
    let mut x: u64 = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if !c.is_ascii_digit() {
            break;
        }
        if x > (1 << 63) / 10 {
            // overflow
            return Err(());
        }
        x = x * 10 + (c - b'0') as u64;
        if x > 1 << 63 {
            // overflow
            return Err(());
        }
        i += 1;
    }
    Ok((x, &s[i..]))
}

/// Consumes the leading [0-9]* from s. It is used only for fractions, so
/// does not return an error on overflow, it just stops accumulating
/// precision.
fn leading_fraction(s: &[u8]) -> (u64, f64, &[u8]) {
    let mut i = 0;
    let mut x: u64 = 0;
    let mut scale = 1.0;
    let mut overflow = false;
    while i < s.len() {
        let c = s[i];
        if !c.is_ascii_digit() {
            break;
        }
        i += 1;
        if overflow {
            continue;
        }
        if x > ((1u64 << 63) - 1) / 10 {
            // It's possible for overflow to give a positive number, so take care.
            overflow = true;
            continue;
        }
        let y = x * 10 + (c - b'0') as u64;
        if y > 1 << 63 {
            overflow = true;
            continue;
        }
        x = y;
        scale *= 10.0;
    }
    (x, scale, &s[i..])
}

/// Returns the number of nanoseconds in the duration unit u.
fn unit(u: &[u8]) -> Option<u64> {
    Some(match u {
        b"ns" => 1,
        b"us" => 1_000,
        b"\xc2\xb5s" => 1_000, // U+00B5 = micro symbol
        b"\xce\xbcs" => 1_000, // U+03BC = Greek letter mu
        b"ms" => 1_000_000,
        b"s" => 1_000_000_000,
        b"m" => 60_000_000_000,
        b"h" => 3_600_000_000_000,
        _ => return None,
    })
}

/// Parses a duration string. A duration string is a possibly signed
/// sequence of decimal numbers, each with optional fraction and a unit
/// suffix, such as "300ms", "-1.5h" or "2h45m". Valid time units are "ns",
/// "us" (or "µs"), "ms", "s", "m", "h".
pub fn parse_duration(s: &[u8]) -> Result<i64, String> {
    // [-+]?([0-9]*(\.[0-9]*)?[a-z]+)+
    let orig = s;
    let invalid = || format!("time: invalid duration {}", quote(orig));
    let mut s = s;
    let mut d: u64 = 0;
    let mut neg = false;

    // Consume [-+]?
    if !s.is_empty() {
        let c = s[0];
        if c == b'-' || c == b'+' {
            neg = c == b'-';
            s = &s[1..];
        }
    }
    // Special case: if all that is left is "0", this is zero.
    if s == b"0" {
        return Ok(0);
    }
    if s.is_empty() {
        return Err(invalid());
    }
    while !s.is_empty() {
        // The next character must be [0-9.]
        if !(s[0] == b'.' || s[0].is_ascii_digit()) {
            return Err(invalid());
        }
        // Consume [0-9]*
        let pl = s.len();
        let (mut v, rest) = leading_int(s).map_err(|_| invalid())?;
        s = rest;
        let pre = pl != s.len(); // whether we consumed anything before a period

        // Consume (\.[0-9]*)?
        let mut post = false;
        let mut f: u64 = 0;
        let mut scale = 1.0;
        if !s.is_empty() && s[0] == b'.' {
            s = &s[1..];
            let pl = s.len();
            let (ff, sc, rest) = leading_fraction(s);
            f = ff;
            scale = sc;
            s = rest;
            post = pl != s.len();
        }
        if !pre && !post {
            // no digits (e.g. ".s" or "-.s")
            return Err(invalid());
        }

        // Consume unit.
        let mut i = 0;
        while i < s.len() {
            let c = s[i];
            if c == b'.' || c.is_ascii_digit() {
                break;
            }
            i += 1;
        }
        if i == 0 {
            return Err(format!("time: missing unit in duration {}", quote(orig)));
        }
        let u = &s[..i];
        s = &s[i..];
        let unit = match unit(u) {
            Some(unit) => unit,
            None => {
                return Err(format!(
                    "time: unknown unit {} in duration {}",
                    quote(u),
                    quote(orig)
                ))
            }
        };
        if v > (1 << 63) / unit {
            // overflow
            return Err(invalid());
        }
        v *= unit;
        if f > 0 {
            // float64 is needed to be nanosecond accurate for fractions of hours.
            // v >= 0 && (f*unit/scale) <= 3.6e+12 (ns/h, h is the largest unit)
            v = v.wrapping_add((f as f64 * (unit as f64 / scale)) as u64);
            if v > 1 << 63 {
                // overflow
                return Err(invalid());
            }
        }
        d = d.wrapping_add(v);
        if d > 1 << 63 {
            return Err(invalid());
        }
    }
    if neg {
        return Ok((d as i64).wrapping_neg());
    }
    if d > (1 << 63) - 1 {
        return Err(invalid());
    }
    Ok(d as i64)
}

/// Parses an RFC 3339 time quickly, returning None if the input is not in
/// the canonical form (the general parser then reports the error).
fn parse_rfc3339(s: &[u8], local: &Option<Rc<Location>>) -> Option<Time> {
    // parseUint parses s as an unsigned decimal integer and
    // verifies that it is within some range.
    // If it is invalid or out-of-range,
    // it sets ok to false and returns the min value.
    let mut ok = true;
    fn parse_uint(ok: &mut bool, s: &[u8], min: i64, max: i64) -> i64 {
        let mut x: i64 = 0;
        for &c in s {
            if !c.is_ascii_digit() {
                *ok = false;
                return min;
            }
            x = x * 10 + (c - b'0') as i64;
        }
        if x < min || max < x {
            *ok = false;
            return min;
        }
        x
    }

    // Parse the date and time.
    if s.len() < b"2006-01-02T15:04:05".len() {
        return None;
    }
    let year = parse_uint(&mut ok, &s[0..4], 0, 9999); // e.g., 2006
    let month = parse_uint(&mut ok, &s[5..7], 1, 12); // e.g., 01
    let day = parse_uint(&mut ok, &s[8..10], 1, days_in(month, year)); // e.g., 02
    let hour = parse_uint(&mut ok, &s[11..13], 0, 23); // e.g., 15
    let min = parse_uint(&mut ok, &s[14..16], 0, 59); // e.g., 04
    let sec = parse_uint(&mut ok, &s[17..19], 0, 59); // e.g., 05
    if !(ok && s[4] == b'-' && s[7] == b'-' && s[10] == b'T' && s[13] == b':' && s[16] == b':') {
        return None;
    }
    let mut s = &s[19..];

    // Parse the fractional second.
    let mut nsec = 0;
    if s.len() >= 2 && s[0] == b'.' && is_digit(s, 1) {
        let mut n = 2;
        while n < s.len() && is_digit(s, n) {
            n += 1;
        }
        nsec = parse_nanoseconds(s, n).map(|(ns, _)| ns).unwrap_or(0);
        s = &s[n..];
    }

    // Parse the time zone.
    let mut t = Time::date(year, month, day, hour, min, sec, nsec, None);
    if s.len() != 1 || s[0] != b'Z' {
        if s.len() != b"-07:00".len() {
            return None;
        }
        let hr = parse_uint(&mut ok, &s[1..3], 0, 23); // e.g., 07
        let mm = parse_uint(&mut ok, &s[4..6], 0, 59); // e.g., 00
        if !ok || !((s[0] == b'-' || s[0] == b'+') && s[3] == b':') {
            return None;
        }
        let mut zone_offset = (hr * 60 + mm) * 60;
        if s[0] == b'-' {
            zone_offset *= -1;
        }
        t.add_sec(-zone_offset);

        // Use local zone with the given offset if possible.
        if loc_lookup(local, t.unix_sec()).offset == zone_offset {
            t.set_loc(local.clone());
        } else {
            t.set_loc(Some(fixed_zone(b"", zone_offset)));
        }
    }
    Some(t)
}
