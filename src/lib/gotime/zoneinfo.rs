// Ported from the Go standard library: original work of The Go Authors
// (2011), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Time zones: Location, zone lookup, the POSIX TZ rule evaluator (tzset)
//! and LoadLocation (zoneinfo.go and zoneinfo_unix.go).

use std::rc::Rc;

use super::zoneinfo_read::{
    load_location_from_sources, load_tzinfo_from_dir_or_zip, location_from_tzdata, LoadError,
};
use super::{
    days_before, days_in, days_year_yday, is_leap, ALPHA, OMEGA, SECONDS_PER_DAY, SECONDS_PER_HOUR,
    SECONDS_PER_MINUTE, UNIX_TO_ABSOLUTE,
};

/// A Location maps time instants to the zone in use at that time.
/// Typically, the Location represents the collection of time offsets in
/// use in a geographical area. For many Locations the time offset varies
/// depending on whether daylight savings time is in use at the time
/// instant.
pub struct Location {
    pub(super) name: Vec<u8>,
    pub(super) zone: Vec<Zone>,
    pub(super) tx: Vec<ZoneTrans>,
    /// The extend field contains an optional POSIX TZ string for
    /// describing how to compute a time after the last transition.
    pub(super) extend: Vec<u8>,
}

/// A zone represents a single time zone such as CET.
#[derive(Clone)]
pub(super) struct Zone {
    /// abbreviated name, "CET"
    pub(super) name: Vec<u8>,
    /// seconds east of UTC
    pub(super) offset: i64,
    /// is this zone Daylight Savings Time?
    pub(super) is_dst: bool,
}

/// A zoneTrans represents a single time zone transition.
#[derive(Clone)]
pub(super) struct ZoneTrans {
    /// transition time, in seconds since 1970 GMT
    pub(super) when: i64,
    /// the index of the zone that goes into effect at that time
    pub(super) index: u8,
}

/// The result of a zone lookup: the zone in use at a given instant and
/// the bounds [start, end) of the period during which it applies.
pub struct ZoneLookup {
    pub name: Vec<u8>,
    pub offset: i64,
    pub start: i64,
    pub end: i64,
}

impl ZoneLookup {
    /// The lookup result for UTC.
    pub fn utc() -> ZoneLookup {
        ZoneLookup {
            name: b"UTC".to_vec(),
            offset: 0,
            start: ALPHA,
            end: OMEGA,
        }
    }
}

thread_local! {
    static LOCAL_LOC: Rc<Location> = Rc::new(init_local());
}

/// Returns the system's local time zone (Go's time.Local), initialized on
/// first use from the TZ environment variable as Go does.
pub fn local() -> Rc<Location> {
    LOCAL_LOC.with(Rc::clone)
}

/// Go's initLocal for Unix systems.
fn init_local() -> Location {
    // consult $TZ to find the time zone to use.
    // no $TZ means use the system default /etc/localtime.
    // $TZ="" means use UTC.
    // $TZ="foo" or $TZ=":foo" if foo is an absolute path, then the file pointed
    // by foo will be used to initialize timezone; otherwise, file
    // /usr/share/zoneinfo/foo will be used.
    use std::os::unix::ffi::OsStrExt;
    let tz = std::env::var_os("TZ");
    match tz {
        None => {
            if let Ok(mut z) = load_location_from_sources(b"localtime", &[b"/etc"]) {
                z.name = b"Local".to_vec();
                return z;
            }
        }
        Some(tz) if !tz.is_empty() => {
            let mut tz = tz.as_bytes();
            if tz[0] == b':' {
                tz = &tz[1..];
            }
            if !tz.is_empty() && tz[0] == b'/' {
                if let Ok(mut z) = load_location_from_sources(tz, &[b""]) {
                    z.name = if tz == b"/etc/localtime" {
                        b"Local".to_vec()
                    } else {
                        tz.to_vec()
                    };
                    return z;
                }
            } else if !tz.is_empty() && tz != b"UTC" {
                if let Ok(z) = load_location_from_sources(tz, PLATFORM_ZONE_SOURCES) {
                    return z;
                }
            }
        }
        Some(_) => {}
    }
    // Fall back to UTC.
    Location {
        name: b"UTC".to_vec(),
        zone: Vec::new(),
        tx: Vec::new(),
        extend: Vec::new(),
    }
}

/// Many systems use /usr/share/zoneinfo, Solaris 2 has
/// /usr/share/lib/zoneinfo, IRIX 6 has /usr/lib/locale/TZ,
/// NixOS has /etc/zoneinfo.
pub(super) const PLATFORM_ZONE_SOURCES: &[&[u8]] = &[
    b"/usr/share/zoneinfo/",
    b"/usr/share/lib/zoneinfo/",
    b"/usr/lib/locale/TZ/",
    b"/etc/zoneinfo",
];

/// Returns a Location that always uses the given zone name and offset
/// (seconds east of UTC).
pub fn fixed_zone(name: &[u8], offset: i64) -> Rc<Location> {
    Rc::new(Location {
        name: name.to_vec(),
        zone: vec![Zone {
            name: name.to_vec(),
            offset,
            is_dst: false,
        }],
        tx: vec![ZoneTrans {
            when: ALPHA,
            index: 0,
        }],
        extend: Vec::new(),
    })
}

impl Location {
    /// Creates a location from its parts (used by the TZif reader).
    pub(super) fn new(
        name: Vec<u8>,
        zone: Vec<Zone>,
        tx: Vec<ZoneTrans>,
        extend: Vec<u8>,
    ) -> Location {
        Location {
            name,
            zone,
            tx,
            extend,
        }
    }

    /// Returns information about the time zone in use at an instant in
    /// time expressed as seconds since January 1, 1970 00:00:00 UTC.
    ///
    /// The returned information gives the name of the zone (such as
    /// "CET"), the start and end times bracketing sec when that zone is in
    /// effect, the offset in seconds east of UTC (such as -5*60*60), and
    /// whether the daylight savings is being observed at that time.
    pub fn lookup(&self, sec: i64) -> ZoneLookup {
        if self.zone.is_empty() {
            return ZoneLookup::utc();
        }

        if self.tx.is_empty() || sec < self.tx[0].when {
            let zone = &self.zone[self.lookup_first_zone()];
            let end = if !self.tx.is_empty() {
                self.tx[0].when
            } else {
                OMEGA
            };
            return ZoneLookup {
                name: zone.name.clone(),
                offset: zone.offset,
                start: ALPHA,
                end,
            };
        }

        // Binary search for entry with largest time <= sec.
        // Not using sort.Search to avoid dependencies.
        let tx = &self.tx;
        let mut end = OMEGA;
        let mut lo = 0;
        let mut hi = tx.len();
        while hi - lo > 1 {
            let m = (lo + hi) >> 1;
            let lim = tx[m].when;
            if sec < lim {
                end = lim;
                hi = m;
            } else {
                lo = m;
            }
        }
        let zone = &self.zone[tx[lo].index as usize];
        let start = tx[lo].when;

        // If we're at the end of the known zone transitions,
        // try the extend string.
        if lo == tx.len() - 1 && !self.extend.is_empty() {
            if let Some(z) = tzset(&self.extend, start, sec) {
                return z;
            }
        }

        ZoneLookup {
            name: zone.name.clone(),
            offset: zone.offset,
            start,
            end,
        }
    }

    /// Returns the index of the time zone to use for times before the
    /// first transition time, or when there are no transition times.
    ///
    /// The reference implementation in localtime.c from
    /// https://www.iana.org/time-zones/repository/releases/tzcode2013g.tar.gz
    /// implements the following algorithm for these cases:
    ///  1. If the first zone is unused by the transitions, use it.
    ///  2. Otherwise, if there are transition times, and the first
    ///     transition is to a zone in daylight time, find the first
    ///     non-daylight-time zone before and closest to the first transition
    ///     zone.
    ///  3. Otherwise, use the first zone that is not daylight time, if
    ///     there is one.
    ///  4. Otherwise, use the first zone.
    fn lookup_first_zone(&self) -> usize {
        // Case 1.
        if !self.first_zone_used() {
            return 0;
        }

        // Case 2.
        if !self.tx.is_empty() && self.zone[self.tx[0].index as usize].is_dst {
            let mut zi = self.tx[0].index as isize - 1;
            while zi >= 0 {
                if !self.zone[zi as usize].is_dst {
                    return zi as usize;
                }
                zi -= 1;
            }
        }

        // Case 3.
        for (zi, z) in self.zone.iter().enumerate() {
            if !z.is_dst {
                return zi;
            }
        }

        // Case 4.
        0
    }

    /// Reports whether the first zone is used by some transition.
    fn first_zone_used(&self) -> bool {
        self.tx.iter().any(|tx| tx.index == 0)
    }

    /// Returns the offset of the zone with the given abbreviation that is
    /// in effect near the given Unix time, if any.
    ///
    /// First try for a zone with the right name that was actually in effect
    /// at the given time. (In Sydney, Australia, both standard and daylight
    /// savings time are abbreviated "EST". Using the offset helps us pick
    /// the right one for the given time. It's not perfect: during the
    /// backward transition we might pick either one.) Otherwise fall back
    /// to an ordinary name match.
    pub fn lookup_name(&self, name: &[u8], unix: i64) -> Option<i64> {
        for zone in &self.zone {
            if zone.name == name {
                let z = self.lookup(unix.wrapping_sub(zone.offset));
                if z.name == zone.name {
                    return Some(z.offset);
                }
            }
        }
        for zone in &self.zone {
            if zone.name == name {
                return Some(zone.offset);
            }
        }
        None
    }
}

/// Takes a timezone string like the one found in the TZ environment
/// variable, the time of the last time zone transition expressed as
/// seconds since January 1, 1970 00:00:00 UTC, and a time expressed the
/// same way. We call this a tzset string since in C the function tzset
/// reads TZ. The return values are as for lookup, or None if the TZ
/// string could not be parsed.
pub(super) fn tzset(s: &[u8], last_tx_sec: i64, sec: i64) -> Option<ZoneLookup> {
    // The format of the TZ string is described in POSIX.

    let (mut std_name, s) = tzset_name(s)?;
    let (std_offset, s) = tzset_offset(s)?;

    // The numbers in the tzset string are added to local time to get UTC,
    // but our offsets are added to UTC to get local time,
    // so we negate the number we see here.
    let mut std_offset = -std_offset;

    if s.is_empty() || s[0] == b',' {
        // No daylight savings time.
        return Some(ZoneLookup {
            name: std_name.to_vec(),
            offset: std_offset,
            start: last_tx_sec,
            end: OMEGA,
        });
    }

    let (mut dst_name, mut s) = tzset_name(s)?;
    let mut dst_offset;
    if s.is_empty() || s[0] == b',' {
        dst_offset = std_offset + SECONDS_PER_HOUR;
    } else {
        let (off, rest) = tzset_offset(s)?;
        dst_offset = -off; // as with stdOffset, above
        s = rest;
    }

    if s.is_empty() {
        // Default DST rules per tzcode.
        s = b",M3.2.0,M11.1.0";
    }
    // The TZ definition does not mention ';' here but tzcode accepts it.
    if s[0] != b',' && s[0] != b';' {
        return None;
    }
    s = &s[1..];

    let (start_rule, rest) = tzset_rule(s)?;
    s = rest;
    if s.is_empty() || s[0] != b',' {
        return None;
    }
    s = &s[1..];
    let (end_rule, rest) = tzset_rule(s)?;
    if !rest.is_empty() {
        return None;
    }

    let (year, yday) =
        days_year_yday(sec.wrapping_add(UNIX_TO_ABSOLUTE) as u64 / SECONDS_PER_DAY as u64);

    let ysec = (yday - 1) * SECONDS_PER_DAY + sec % SECONDS_PER_DAY;

    // Compute start of year in seconds since Unix epoch.
    let ystart = sec.wrapping_sub(ysec);

    let mut start_sec = tzrule_time(year, &start_rule, std_offset);
    let mut end_sec = tzrule_time(year, &end_rule, dst_offset);
    // Note: this is a flipping of "DST" and "STD" while retaining the labels.
    // This happens in southern hemispheres. The labelling here thus is a little
    // inconsistent with the goal.
    if end_sec < start_sec {
        std::mem::swap(&mut start_sec, &mut end_sec);
        std::mem::swap(&mut std_name, &mut dst_name);
        std::mem::swap(&mut std_offset, &mut dst_offset);
    }

    // The start and end values that we return are accurate
    // close to a daylight savings transition, but are otherwise
    // just the start and end of the year. That suffices for
    // the only caller that cares, which is Date.
    Some(if ysec < start_sec {
        ZoneLookup {
            name: std_name.to_vec(),
            offset: std_offset,
            start: ystart,
            end: start_sec.wrapping_add(ystart),
        }
    } else if ysec >= end_sec {
        ZoneLookup {
            name: std_name.to_vec(),
            offset: std_offset,
            start: end_sec.wrapping_add(ystart),
            end: ystart.wrapping_add(365 * SECONDS_PER_DAY),
        }
    } else {
        ZoneLookup {
            name: dst_name.to_vec(),
            offset: dst_offset,
            start: start_sec.wrapping_add(ystart),
            end: end_sec.wrapping_add(ystart),
        }
    })
}

/// Returns the timezone name at the start of the tzset string s, and the
/// remainder of s.
fn tzset_name(s: &[u8]) -> Option<(&[u8], &[u8])> {
    if s.is_empty() {
        return None;
    }
    if s[0] != b'<' {
        for (i, &r) in s.iter().enumerate() {
            match r {
                b'0'..=b'9' | b',' | b'-' | b'+' => {
                    if i < 3 {
                        return None;
                    }
                    return Some((&s[..i], &s[i..]));
                }
                _ => {}
            }
        }
        if s.len() < 3 {
            return None;
        }
        Some((s, b""))
    } else {
        for (i, &r) in s.iter().enumerate() {
            if r == b'>' {
                return Some((&s[1..i], &s[i + 1..]));
            }
        }
        None
    }
}

/// Returns the timezone offset at the start of the tzset string s, and
/// the remainder of s. The offset is in seconds.
fn tzset_offset(s: &[u8]) -> Option<(i64, &[u8])> {
    if s.is_empty() {
        return None;
    }
    let mut s = s;
    let mut neg = false;
    if s[0] == b'+' {
        s = &s[1..];
    } else if s[0] == b'-' {
        s = &s[1..];
        neg = true;
    }

    // The tzdata code permits values up to 24 * 7 here,
    // although POSIX does not.
    let (hours, rest) = tzset_num(s, 0, 24 * 7)?;
    s = rest;
    let mut off = hours * SECONDS_PER_HOUR;
    if s.is_empty() || s[0] != b':' {
        if neg {
            off = -off;
        }
        return Some((off, s));
    }

    let (mins, rest) = tzset_num(&s[1..], 0, 59)?;
    s = rest;
    off += mins * SECONDS_PER_MINUTE;
    if s.is_empty() || s[0] != b':' {
        if neg {
            off = -off;
        }
        return Some((off, s));
    }

    let (secs, rest) = tzset_num(&s[1..], 0, 59)?;
    s = rest;
    off += secs;

    if neg {
        off = -off;
    }
    Some((off, s))
}

/// The kinds of tzset rules.
#[derive(Clone, Copy, PartialEq)]
enum RuleKind {
    Julian,
    Doy,
    MonthWeekDay,
}

/// A rule read from a tzset string.
struct Rule {
    kind: RuleKind,
    day: i64,
    week: i64,
    mon: i64,
    /// transition time
    time: i64,
}

/// Parses a rule from a tzset string, returning the rule and the
/// remainder of the string.
fn tzset_rule(s: &[u8]) -> Option<(Rule, &[u8])> {
    let mut r = Rule {
        kind: RuleKind::Julian,
        day: 0,
        week: 0,
        mon: 0,
        time: 0,
    };
    if s.is_empty() {
        return None;
    }
    let mut s = s;
    if s[0] == b'J' {
        let (jday, rest) = tzset_num(&s[1..], 1, 365)?;
        s = rest;
        r.kind = RuleKind::Julian;
        r.day = jday;
    } else if s[0] == b'M' {
        let (mon, rest) = tzset_num(&s[1..], 1, 12)?;
        if rest.is_empty() || rest[0] != b'.' {
            return None;
        }
        let (week, rest) = tzset_num(&rest[1..], 1, 5)?;
        if rest.is_empty() || rest[0] != b'.' {
            return None;
        }
        let (day, rest) = tzset_num(&rest[1..], 0, 6)?;
        s = rest;
        r.kind = RuleKind::MonthWeekDay;
        r.day = day;
        r.week = week;
        r.mon = mon;
    } else {
        let (day, rest) = tzset_num(s, 0, 365)?;
        s = rest;
        r.kind = RuleKind::Doy;
        r.day = day;
    }

    if s.is_empty() || s[0] != b'/' {
        r.time = 2 * SECONDS_PER_HOUR; // 2am is the default
        return Some((r, s));
    }

    let (offset, rest) = tzset_offset(&s[1..])?;
    r.time = offset;

    Some((r, rest))
}

/// Parses a number from a tzset string. It returns the number and the
/// remainder of the string. It checks that the number is in the range
/// [min, max].
fn tzset_num(s: &[u8], min: i64, max: i64) -> Option<(i64, &[u8])> {
    if s.is_empty() {
        return None;
    }
    let mut num: i64 = 0;
    for (i, &r) in s.iter().enumerate() {
        if !r.is_ascii_digit() {
            if i == 0 || num < min {
                return None;
            }
            return Some((num, &s[i..]));
        }
        num *= 10;
        num += (r - b'0') as i64;
        if num > max {
            return None;
        }
    }
    if num < min {
        return None;
    }
    Some((num, b""))
}

/// Takes a year, a rule, and a timezone offset, and returns the number of
/// seconds since the start of the year that the rule takes effect.
fn tzrule_time(year: i64, r: &Rule, off: i64) -> i64 {
    let s = match r.kind {
        RuleKind::Julian => {
            let mut s = (r.day - 1) * SECONDS_PER_DAY;
            if is_leap(year) && r.day >= 60 {
                s += SECONDS_PER_DAY;
            }
            s
        }
        RuleKind::Doy => r.day * SECONDS_PER_DAY,
        RuleKind::MonthWeekDay => {
            // Zeller's Congruence.
            let m1 = (r.mon + 9) % 12 + 1;
            let mut yy0 = year;
            if r.mon <= 2 {
                yy0 = yy0.wrapping_sub(1);
            }
            let yy1 = yy0 / 100;
            let yy2 = yy0 % 100;
            let mut dow = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4)
                .wrapping_sub(yy1.wrapping_mul(2))
                % 7;
            if dow < 0 {
                dow += 7;
            }
            // Now dow is the day-of-week of the first day of r.mon.
            // Get the day-of-month of the first "dow" day.
            let mut d = r.day - dow;
            if d < 0 {
                d += 7;
            }
            for _ in 1..r.week {
                if d + 7 >= days_in(r.mon, year) {
                    break;
                }
                d += 7;
            }
            d += days_before(r.mon);
            if is_leap(year) && r.mon > 2 {
                d += 1;
            }
            d * SECONDS_PER_DAY
        }
    };

    s + r.time - off
}

/// An error from LoadLocation, rendered as Go's err.Error().
pub struct LocationError(pub String);

/// Returns the Location with the given name (None means UTC).
///
/// If the name is "" or "UTC", LoadLocation returns UTC.
/// If the name is "Local", LoadLocation returns Local.
///
/// Otherwise, the name is taken to be a location name corresponding to a
/// file in the IANA Time Zone database, such as "America/New_York".
///
/// LoadLocation looks for the IANA Time Zone database in the following
/// locations in order:
///
///   - the directory or uncompressed zip file named by the ZONEINFO
///     environment variable
///   - on a Unix system, the system standard installation location
///   - $GOROOT/lib/time/zoneinfo.zip
pub fn load_location(name: &[u8]) -> Result<Option<Rc<Location>>, LocationError> {
    if name.is_empty() || name == b"UTC" {
        return Ok(None);
    }
    if name == b"Local" {
        return Ok(Some(local()));
    }
    if contains_dot_dot(name) || name[0] == b'/' || name[0] == b'\\' {
        // No valid IANA Time Zone name contains a single dot,
        // much less dot dot. Likewise, none begin with a slash.
        return Err(LocationError("time: invalid location name".to_string()));
    }
    let mut first_err: Option<LoadError> = None;
    if let Some(zoneinfo) = std::env::var_os("ZONEINFO") {
        use std::os::unix::ffi::OsStrExt;
        let zoneinfo = zoneinfo.as_bytes();
        if !zoneinfo.is_empty() {
            match load_tzinfo_from_dir_or_zip(zoneinfo, name) {
                Ok(data) => match location_from_tzdata(name, &data) {
                    Ok(z) => return Ok(Some(Rc::new(z))),
                    Err(e) => first_err = Some(e),
                },
                Err(e) if !e.is_not_exist() => first_err = Some(e),
                // A zone missing from $ZONEINFO falls through to the platform sources, as in Go.
                Err(_) => {}
            }
        }
    }
    match load_location_from_sources(name, PLATFORM_ZONE_SOURCES) {
        Ok(z) => Ok(Some(Rc::new(z))),
        Err(e) => Err(LocationError(first_err.unwrap_or(e).to_string())),
    }
}

/// Reports whether s contains "..".
fn contains_dot_dot(s: &[u8]) -> bool {
    s.windows(2).any(|w| w == b"..")
}
