// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! A port of the parts of Go's time package used by the Starlark time
//! module: instants, durations, calendrical computations, formatting and
//! parsing (format.go), and time zones (zoneinfo*.go).
//!
//! The calendrical calculations always assume a Gregorian calendar, with
//! no leap seconds. All arithmetic wraps on overflow exactly as Go's
//! int64/uint64 arithmetic does.

mod format;
mod zoneinfo;
mod zoneinfo_read;

use std::rc::Rc;

pub use format::{parse, parse_duration, parse_in_location, RFC3339};
pub use zoneinfo::{load_location, local, Location};

/// A Time represents an instant in time with nanosecond precision.
///
/// Unlike Go's time.Time, it carries no monotonic clock reading: the
/// Starlark module never makes one observable.
#[derive(Clone)]
pub struct Time {
    /// Seconds since January 1, year 1 00:00:00 UTC (Go's "internal" time).
    sec: i64,
    /// Nanoseconds within the second, in the range [0, 999999999].
    nsec: i32,
    /// The location used to compute the presentation form. None means UTC.
    loc: Option<Rc<Location>>,
}

// Computations on Times
//
// The calendar runs on an exact 400 year cycle. It simplifies date
// computations to choose the cycle boundaries so that the exceptional
// years are always delayed as long as possible: March 1, year 0 is such a
// day. The "absolute zero instant" is March 1 of the year -292277022400;
// times measured as a uint64 seconds since then are absolute times.
// Times measured as an int64 seconds since the year 1 are internal times;
// seconds since 1970 are Unix times.
//
// The date calculations use the Euclidean affine functions of Cassio Neri
// and Lorenz Schneider, "Euclidean affine functions and their application
// to calendar algorithms," SP&E 2023.

pub const SECONDS_PER_MINUTE: i64 = 60;
pub const SECONDS_PER_HOUR: i64 = 60 * SECONDS_PER_MINUTE;
pub const SECONDS_PER_DAY: i64 = 24 * SECONDS_PER_HOUR;

/// Days from March 1 through end of year.
const MARCH_THRU_DECEMBER: u32 = 31 + 30 + 31 + 30 + 31 + 31 + 30 + 31 + 30 + 31;

/// The number of years we subtract from internal time to get absolute time.
const ABSOLUTE_YEARS: u64 = 292277022400;

const ABSOLUTE_TO_INTERNAL: i64 =
    -((ABSOLUTE_YEARS as i64 / 400 * 146097 + MARCH_THRU_DECEMBER as i64) * SECONDS_PER_DAY);
const INTERNAL_TO_ABSOLUTE: i64 = -ABSOLUTE_TO_INTERNAL;

const UNIX_TO_INTERNAL: i64 = (1969 * 365 + 1969 / 4 - 1969 / 100 + 1969 / 400) * SECONDS_PER_DAY;
const INTERNAL_TO_UNIX: i64 = -UNIX_TO_INTERNAL;

const ABSOLUTE_TO_UNIX: i64 = ABSOLUTE_TO_INTERNAL + INTERNAL_TO_UNIX;
const UNIX_TO_ABSOLUTE: i64 = UNIX_TO_INTERNAL + INTERNAL_TO_ABSOLUTE;

/// math.MinInt64, the start of all time for zone lookups.
pub const ALPHA: i64 = i64::MIN;
/// math.MaxInt64, the end of all time for zone lookups.
pub const OMEGA: i64 = i64::MAX;

pub const LONG_DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
pub const SHORT_DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
pub const SHORT_MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub const LONG_MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Returns the English name of the month (Go's Month.String).
fn month_string(m: i64) -> Vec<u8> {
    if (1..=12).contains(&m) {
        return LONG_MONTH_NAMES[(m - 1) as usize].as_bytes().to_vec();
    }
    // Go formats the month as an unsigned value here.
    format!("%!Month({})", m as u64).into_bytes()
}

/// Returns the English name of the day (Go's Weekday.String).
fn weekday_string(d: i64) -> Vec<u8> {
    if (0..=6).contains(&d) {
        return LONG_DAY_NAMES[d as usize].as_bytes().to_vec();
    }
    format!("%!Weekday({})", d as u64).into_bytes()
}

/// Returns the number of days from the absolute epoch to the given
/// standard year/month/day. The day argument can be out of range.
fn date_to_abs_days(year: i64, month: i64, day: i64) -> u64 {
    let mut amonth = month as u32;
    let mut jan_feb: u32 = 0;
    if amonth < 3 {
        jan_feb = 1;
    }
    amonth = amonth.wrapping_add(12 * jan_feb);
    let y = (year as u64)
        .wrapping_sub(jan_feb as u64)
        .wrapping_add(ABSOLUTE_YEARS);

    let ayday = 979u32.wrapping_mul(amonth).wrapping_sub(2919) >> 5;

    let century = y / 100;
    let cyear = (y % 100) as u32;
    let cday = 1461 * cyear / 4;
    let centurydays = 146097u64.wrapping_mul(century) / 4;

    let rest = (cday.wrapping_add(ayday) as i64)
        .wrapping_add(day)
        .wrapping_sub(1);
    centurydays.wrapping_add(rest as u64)
}

/// Splits absolute days into century, cyear, ayday.
fn split_days(days: u64) -> (u64, u64, u32) {
    let d = 4u64.wrapping_mul(days).wrapping_add(3);
    let century = d / 146097;
    let cd = (d % 146097) as u32 | 3;
    let prod = 2939745u64 * cd as u64;
    let hi = (prod >> 32) as u32;
    let lo = prod as u32;
    (century, hi as u64, lo / 2939745 / 4)
}

/// Splits ayday into absolute month and standard (1-based) day-in-month.
fn split_ayday(ayday: u32) -> (i64, i64) {
    let d = 2141u32.wrapping_mul(ayday).wrapping_add(197913);
    ((d >> 16) as i64, 1 + ((d & 0xFFFF) / 2141) as i64)
}

/// Returns 1 if the March 1-based ayday is in January or February.
fn jan_feb(ayday: u32) -> i64 {
    if ayday >= MARCH_THRU_DECEMBER {
        1
    } else {
        0
    }
}

/// Returns 1 if (century, cyear) is a leap year, 0 otherwise.
fn leap(century: u64, cyear: u64) -> i64 {
    let y4ok = cyear.is_multiple_of(4) as i64;
    let y100ok = (cyear != 0) as i64;
    let y400ok = century.is_multiple_of(4) as i64;
    y4ok & (y100ok | y400ok)
}

/// Returns the standard year for (century, cyear, janFeb).
fn century_year(century: u64, cyear: u64, jf: i64) -> i64 {
    (century.wrapping_mul(100).wrapping_sub(ABSOLUTE_YEARS) as i64)
        .wrapping_add(cyear as i64)
        .wrapping_add(jf)
}

/// Converts absolute days into standard year, month, day.
fn days_date(days: u64) -> (i64, i64, i64) {
    let (century, cyear, ayday) = split_days(days);
    let (amonth, day) = split_ayday(ayday);
    let jf = jan_feb(ayday);
    (century_year(century, cyear, jf), amonth - jf * 12, day)
}

/// Converts absolute days into the standard year and 1-based yday.
fn days_year_yday(days: u64) -> (i64, i64) {
    let (century, cyear, ayday) = split_days(days);
    let jf = jan_feb(ayday);
    let year = century_year(century, cyear, jf);
    let yday = ayday as i64 + (1 + 31 + 28) + (leap(century, cyear) & !jf) - 365 * jf;
    (year, yday)
}

/// Returns the day of the week specified by absolute days.
fn days_weekday(days: u64) -> i64 {
    // March 1 of the absolute year, like March 1 of 2000, was a Wednesday.
    (days.wrapping_add(3) % 7) as i64
}

/// Returns the hour, minute, and second within the day of absolute seconds.
fn abs_clock(abs: u64) -> (i64, i64, i64) {
    let mut sec = (abs % SECONDS_PER_DAY as u64) as i64;
    let hour = sec / SECONDS_PER_HOUR;
    sec -= hour * SECONDS_PER_HOUR;
    let min = sec / SECONDS_PER_MINUTE;
    sec -= min * SECONDS_PER_MINUTE;
    (hour, min, sec)
}

/// Reports whether year is a leap year.
pub fn is_leap(year: i64) -> bool {
    let mask = if year % 25 != 0 { 3 } else { 0xf };
    year & mask == 0
}

/// Returns the number of days in a non-leap year before month m.
/// days_before(13) returns 365.
fn days_before(m: i64) -> i64 {
    let adj = if m >= 3 { -2 } else { 0 };
    (214i64.wrapping_mul(m).wrapping_sub(211)) / 7 + adj
}

/// Returns the number of days in month m of the given year.
fn days_in(m: i64, year: i64) -> i64 {
    if m == 2 {
        if is_leap(year) {
            return 29;
        }
        return 28;
    }
    30 + ((m.wrapping_add(m >> 3)) & 1)
}

/// Returns nhi, nlo such that hi * base + lo == nhi * base + nlo and
/// 0 <= nlo < base.
fn norm(mut hi: i64, mut lo: i64, base: i64) -> (i64, i64) {
    if lo < 0 {
        let n = (lo.wrapping_neg().wrapping_sub(1)) / base + 1;
        hi = hi.wrapping_sub(n);
        lo = lo.wrapping_add(n.wrapping_mul(base));
    }
    if lo >= base {
        let n = lo / base;
        hi = hi.wrapping_add(n);
        lo = lo.wrapping_sub(n.wrapping_mul(base));
    }
    (hi, lo)
}

/// Returns the zone lookup for loc (None = UTC) at the given Unix second.
fn loc_lookup(loc: &Option<Rc<Location>>, sec: i64) -> zoneinfo::ZoneLookup {
    match loc {
        Some(l) => l.lookup(sec),
        None => zoneinfo::ZoneLookup::utc(),
    }
}

impl Time {
    /// Returns the Time corresponding to yyyy-mm-dd hh:mm:ss + nsec
    /// nanoseconds in the appropriate zone for that time in the given
    /// location (None = UTC). Out-of-range values are normalized.
    #[allow(clippy::too_many_arguments)]
    pub fn date(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        min: i64,
        sec: i64,
        nsec: i64,
        loc: Option<Rc<Location>>,
    ) -> Time {
        // Normalize month, overflowing into year.
        let m = month.wrapping_sub(1);
        let (year, m) = norm(year, m, 12);
        let month = m + 1;

        // Normalize nsec, sec, min, hour, overflowing into day.
        let (sec, nsec) = norm(sec, nsec, 1_000_000_000);
        let (min, sec) = norm(min, sec, 60);
        let (hour, min) = norm(hour, min, 60);
        let (day, hour) = norm(day, hour, 24);

        // Convert to absolute time and then Unix time.
        let mut unix = (date_to_abs_days(year, month, day) as i64)
            .wrapping_mul(SECONDS_PER_DAY)
            .wrapping_add(
                hour.wrapping_mul(SECONDS_PER_HOUR)
                    .wrapping_add(min.wrapping_mul(SECONDS_PER_MINUTE))
                    .wrapping_add(sec),
            )
            .wrapping_add(ABSOLUTE_TO_UNIX);

        // Look for zone offset for expected time, so we can adjust to UTC.
        let z = loc_lookup(&loc, unix);
        let mut offset = z.offset;
        if offset != 0 {
            let utc = unix.wrapping_sub(offset);
            // If utc is valid for the time zone we found, then we have the
            // right offset. If not, we get the correct offset by looking up
            // utc in the location.
            if utc < z.start || utc >= z.end {
                offset = loc_lookup(&loc, utc).offset;
            }
            unix = unix.wrapping_sub(offset);
        }

        let mut t = Time::unix_time(unix, nsec as i32);
        t.set_loc(loc);
        t
    }

    /// Returns the local Time corresponding to the given Unix time
    /// (Go's unixTime).
    fn unix_time(sec: i64, nsec: i32) -> Time {
        Time {
            sec: sec.wrapping_add(UNIX_TO_INTERNAL),
            nsec,
            loc: Some(local()),
        }
    }

    /// Returns the local Time corresponding to the given Unix time, sec
    /// seconds and nsec nanoseconds since January 1, 1970 UTC.
    pub fn unix(mut sec: i64, mut nsec: i64) -> Time {
        if !(0..1_000_000_000).contains(&nsec) {
            let n = nsec / 1_000_000_000;
            sec = sec.wrapping_add(n);
            nsec -= n * 1_000_000_000;
            if nsec < 0 {
                nsec += 1_000_000_000;
                sec = sec.wrapping_sub(1);
            }
        }
        Time::unix_time(sec, nsec as i32)
    }

    /// Returns the current local time.
    pub fn now() -> Time {
        let (sec, nsec) = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, d.subsec_nanos() as i64),
            Err(e) => {
                let d = e.duration();
                (-(d.as_secs() as i64), -(d.subsec_nanos() as i64))
            }
        };
        Time::unix(sec, nsec)
    }

    /// Sets the location associated with the time.
    fn set_loc(&mut self, loc: Option<Rc<Location>>) {
        self.loc = loc;
    }

    /// Returns a copy of t with its location set to loc.
    pub fn in_location(&self, loc: Option<Rc<Location>>) -> Time {
        let mut t = self.clone();
        t.set_loc(loc);
        t
    }

    /// Returns the time's seconds since Jan 1 year 1.
    fn internal_sec(&self) -> i64 {
        self.sec
    }

    /// Returns the time's seconds since Jan 1 1970 (Unix time).
    fn unix_sec(&self) -> i64 {
        self.sec.wrapping_add(INTERNAL_TO_UNIX)
    }

    /// Adds d seconds to the time, saturating on overflow.
    fn add_sec(&mut self, d: i64) {
        let sum = self.sec.wrapping_add(d);
        if (sum > self.sec) == (d > 0) {
            self.sec = sum;
        } else if d > 0 {
            self.sec = i64::MAX;
        } else {
            self.sec = -i64::MAX;
        }
    }

    /// Reports whether t represents the zero time instant,
    /// January 1, year 1, 00:00:00 UTC.
    pub fn is_zero(&self) -> bool {
        self.internal_sec() == 0 && self.nsec == 0
    }

    /// Reports whether the time instant t is after u.
    pub fn after(&self, u: &Time) -> bool {
        let (ts, us) = (self.internal_sec(), u.internal_sec());
        ts > us || ts == us && self.nsec > u.nsec
    }

    /// Reports whether the time instant t is before u.
    pub fn before(&self, u: &Time) -> bool {
        let (ts, us) = (self.internal_sec(), u.internal_sec());
        ts < us || ts == us && self.nsec < u.nsec
    }

    /// Reports whether t and u represent the same time instant.
    pub fn equal(&self, u: &Time) -> bool {
        self.internal_sec() == u.internal_sec() && self.nsec == u.nsec
    }

    /// Returns the time t as an absolute seconds, adjusted by the zone
    /// offset, together with the zone name and offset.
    fn locabs(&self) -> (Vec<u8>, i64, u64) {
        let mut sec = self.unix_sec();
        let (name, offset) = match &self.loc {
            Some(l) => {
                let z = l.lookup(sec);
                sec = sec.wrapping_add(z.offset);
                (z.name, z.offset)
            }
            _ => (b"UTC".to_vec(), 0),
        };
        (name, offset, sec.wrapping_add(UNIX_TO_ABSOLUTE) as u64)
    }

    /// Returns the time t as an absolute seconds, adjusted by the zone offset.
    fn abs_sec(&self) -> u64 {
        self.locabs().2
    }

    /// Returns the year in which t occurs.
    pub fn year(&self) -> i64 {
        days_date(self.abs_sec() / SECONDS_PER_DAY as u64).0
    }

    /// Returns the month of the year (1-12) in which t occurs.
    pub fn month(&self) -> i64 {
        days_date(self.abs_sec() / SECONDS_PER_DAY as u64).1
    }

    /// Returns the day of the month in which t occurs.
    pub fn day(&self) -> i64 {
        days_date(self.abs_sec() / SECONDS_PER_DAY as u64).2
    }

    /// Returns the hour within the day, in the range [0, 23].
    pub fn hour(&self) -> i64 {
        abs_clock(self.abs_sec()).0
    }

    /// Returns the minute offset within the hour, in the range [0, 59].
    pub fn minute(&self) -> i64 {
        abs_clock(self.abs_sec()).1
    }

    /// Returns the second offset within the minute, in the range [0, 59].
    pub fn second(&self) -> i64 {
        abs_clock(self.abs_sec()).2
    }

    /// Returns the nanosecond offset within the second.
    pub fn nanosecond(&self) -> i64 {
        self.nsec as i64
    }

    /// Returns t as a Unix time, the number of seconds elapsed since
    /// January 1, 1970 UTC.
    pub fn unix_seconds(&self) -> i64 {
        self.unix_sec()
    }

    /// Returns t as a Unix time in nanoseconds (wrapping on overflow).
    pub fn unix_nano(&self) -> i64 {
        self.unix_sec()
            .wrapping_mul(1_000_000_000)
            .wrapping_add(self.nsec as i64)
    }

    /// Returns the time t+d.
    pub fn add(&self, d: i64) -> Time {
        let mut t = self.clone();
        let mut dsec = d / 1_000_000_000;
        let mut nsec = t.nsec + (d % 1_000_000_000) as i32;
        if nsec >= 1_000_000_000 {
            dsec += 1;
            nsec -= 1_000_000_000;
        } else if nsec < 0 {
            dsec -= 1;
            nsec += 1_000_000_000;
        }
        t.nsec = nsec;
        t.add_sec(dsec);
        t
    }

    /// Returns the duration t-u, saturating at the minimum or maximum
    /// duration.
    pub fn sub(&self, u: &Time) -> i64 {
        let d = (self.internal_sec().wrapping_sub(u.internal_sec()))
            .wrapping_mul(1_000_000_000)
            .wrapping_add((self.nsec - u.nsec) as i64);
        if u.add(d).equal(self) {
            d
        } else if self.before(u) {
            i64::MIN
        } else {
            i64::MAX
        }
    }
}

/// Formats a duration like Go's Duration.String, e.g. "72h3m0.5s".
/// Leading zero units are omitted; durations less than one second use a
/// smaller unit (milli-, micro-, or nanoseconds). The zero duration
/// formats as 0s.
pub fn duration_string(d: i64) -> String {
    let mut buf = [0u8; 32];
    let mut w = buf.len();

    let mut u = d as u64;
    let neg = d < 0;
    if neg {
        u = u.wrapping_neg();
    }

    if u < 1_000_000_000 {
        // Special case: if duration is smaller than a second,
        // use smaller units, like 1.2ms
        let prec;
        w -= 1;
        buf[w] = b's';
        w -= 1;
        if u == 0 {
            buf[w] = b'0';
            return String::from_utf8_lossy(&buf[w..]).into_owned();
        } else if u < 1000 {
            // print nanoseconds
            prec = 0;
            buf[w] = b'n';
        } else if u < 1_000_000 {
            // print microseconds
            prec = 3;
            // U+00B5 'µ' micro sign == 0xC2 0xB5
            w -= 1;
            buf[w] = 0xC2;
            buf[w + 1] = 0xB5;
        } else {
            // print milliseconds
            prec = 6;
            buf[w] = b'm';
        }
        let (nw, nu) = fmt_frac(&mut buf[..w], u, prec);
        w = fmt_int(&mut buf[..nw], nu);
    } else {
        w -= 1;
        buf[w] = b's';

        let (nw, nu) = fmt_frac(&mut buf[..w], u, 9);
        u = nu;

        // u is now integer seconds
        w = fmt_int(&mut buf[..nw], u % 60);
        u /= 60;

        // u is now integer minutes
        if u > 0 {
            w -= 1;
            buf[w] = b'm';
            w = fmt_int(&mut buf[..w], u % 60);
            u /= 60;

            // u is now integer hours
            if u > 0 {
                w -= 1;
                buf[w] = b'h';
                w = fmt_int(&mut buf[..w], u);
            }
        }
    }

    if neg {
        w -= 1;
        buf[w] = b'-';
    }

    String::from_utf8_lossy(&buf[w..]).into_owned()
}

/// Formats the fraction of v/10**prec (e.g., ".12345") into the tail of
/// buf, omitting trailing zeros and the decimal point when the fraction is
/// 0. Returns the index where the output bytes begin and v/10**prec.
fn fmt_frac(buf: &mut [u8], mut v: u64, prec: usize) -> (usize, u64) {
    let mut w = buf.len();
    let mut print = false;
    for _ in 0..prec {
        let digit = v % 10;
        print = print || digit != 0;
        if print {
            w -= 1;
            buf[w] = digit as u8 + b'0';
        }
        v /= 10;
    }
    if print {
        w -= 1;
        buf[w] = b'.';
    }
    (w, v)
}

/// Formats v into the tail of buf, returning the index where the output
/// begins.
fn fmt_int(buf: &mut [u8], mut v: u64) -> usize {
    let mut w = buf.len();
    if v == 0 {
        w -= 1;
        buf[w] = b'0';
    } else {
        while v > 0 {
            w -= 1;
            buf[w] = (v % 10) as u8 + b'0';
            v /= 10;
        }
    }
    w
}

/// Returns the duration as a floating point number of hours.
pub fn duration_hours(d: i64) -> f64 {
    let hour = d / 3_600_000_000_000;
    let nsec = d % 3_600_000_000_000;
    hour as f64 + nsec as f64 / (60.0 * 60.0 * 1e9)
}

/// Returns the duration as a floating point number of minutes.
pub fn duration_minutes(d: i64) -> f64 {
    let min = d / 60_000_000_000;
    let nsec = d % 60_000_000_000;
    min as f64 + nsec as f64 / (60.0 * 1e9)
}

/// Returns the duration as a floating point number of seconds.
pub fn duration_seconds(d: i64) -> f64 {
    let sec = d / 1_000_000_000;
    let nsec = d % 1_000_000_000;
    sec as f64 + nsec as f64 / 1e9
}
