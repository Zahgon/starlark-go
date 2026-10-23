// Ported from the Go standard library: original work of The Go Authors
// (2009), all rights reserved. Use of this source code is governed by
// a BSD-style license that can be found in the LICENSE file of the Go distribution
// (https://go.dev/LICENSE).

//! Parse "zoneinfo" time zone file. This is a fairly standard file format
//! used on OS X, Linux, BSD, Sun, and others. See tzfile(5),
//! https://en.wikipedia.org/wiki/Zoneinfo, and ftp://munnari.oz.au/pub/oldtz/
//!
//! It is important that we not depend on external packages to parse
//! these files, as we want them to work the same everywhere.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;

use super::zoneinfo::{Location, Zone, ZoneTrans};
use super::ALPHA;

/// Maximum size of a time zone file.
const MAX_FILE_SIZE: usize = 10 << 20;

/// The GOROOT of the Go toolchain whose zoneinfo.zip is the last resort
/// (Go's runtime.GOROOT(): $GOROOT, or the toolchain's build-time root).
const DEFAULT_GOROOT: &[u8] = b"/usr/local/go";

/// An error encountered while loading time zone data.
pub enum LoadError {
    /// A system call error (Go's syscall.Errno), by number.
    Errno(i32),
    /// The time zone data is malformed (Go's errBadData).
    BadData,
    /// The file is too large (Go's fileSizeError).
    FileSize(Vec<u8>),
    /// Another error, with its message.
    Other(String),
}

impl LoadError {
    /// Reports whether the error is ENOENT, which LoadLocation ignores in
    /// favour of trying other sources.
    pub fn is_not_exist(&self) -> bool {
        matches!(self, LoadError::Errno(2))
    }

    fn from_io(e: &std::io::Error) -> LoadError {
        match e.raw_os_error() {
            Some(n) => LoadError::Errno(n),
            // A path containing NUL: Go's syscall.ByteSliceFromString fails
            // with EINVAL.
            None if e.kind() == std::io::ErrorKind::InvalidInput => LoadError::Errno(22),
            None => LoadError::Other(e.to_string()),
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Errno(n) => f.write_str(&errno_string(*n)),
            LoadError::BadData => f.write_str("malformed time zone information"),
            LoadError::FileSize(name) => write!(
                f,
                "time: file {} is too large",
                String::from_utf8_lossy(name)
            ),
            LoadError::Other(s) => f.write_str(s),
        }
    }
}

/// Returns the text of a Linux errno value, as Go's syscall.Errno.Error.
fn errno_string(n: i32) -> String {
    const ERRORS: [&str; 41] = [
        "",
        "operation not permitted",
        "no such file or directory",
        "no such process",
        "interrupted system call",
        "input/output error",
        "no such device or address",
        "argument list too long",
        "exec format error",
        "bad file descriptor",
        "no child processes",
        "resource temporarily unavailable",
        "cannot allocate memory",
        "permission denied",
        "bad address",
        "block device required",
        "device or resource busy",
        "file exists",
        "invalid cross-device link",
        "no such device",
        "not a directory",
        "is a directory",
        "invalid argument",
        "too many open files in system",
        "too many open files",
        "inappropriate ioctl for device",
        "text file busy",
        "file too large",
        "no space left on device",
        "illegal seek",
        "read-only file system",
        "too many links",
        "broken pipe",
        "numerical argument out of domain",
        "numerical result out of range",
        "resource deadlock avoided",
        "file name too long",
        "no locks available",
        "function not implemented",
        "directory not empty",
        "too many levels of symbolic links",
    ];
    match ERRORS.get(n as usize) {
        Some(s) if n > 0 => s.to_string(),
        _ => format!("errno {}", n),
    }
}

/// Simple I/O interface to binary blob of data.
struct DataIO<'a> {
    p: &'a [u8],
    error: bool,
}

impl<'a> DataIO<'a> {
    fn read(&mut self, n: usize) -> &'a [u8] {
        if self.p.len() < n {
            self.p = &[];
            self.error = true;
            return &[];
        }
        let (p, rest) = self.p.split_at(n);
        self.p = rest;
        p
    }

    fn big4(&mut self) -> Option<u32> {
        let p = self.read(4);
        if p.len() < 4 {
            self.error = true;
            return None;
        }
        Some(u32::from_be_bytes([p[0], p[1], p[2], p[3]]))
    }

    fn big8(&mut self) -> Option<u64> {
        let n1 = self.big4();
        let n2 = self.big4();
        match (n1, n2) {
            (Some(a), Some(b)) => Some(((a as u64) << 32) | b as u64),
            _ => {
                self.error = true;
                None
            }
        }
    }

    fn byte(&mut self) -> Option<u8> {
        let p = self.read(1);
        if p.is_empty() {
            self.error = true;
            return None;
        }
        Some(p[0])
    }

    /// Returns the rest of the data in the buffer.
    fn rest(&mut self) -> &'a [u8] {
        let r = self.p;
        self.p = &[];
        r
    }
}

/// Makes a string from the bytes of p up to the first NUL.
fn byte_string(p: &[u8]) -> Vec<u8> {
    match p.iter().position(|&b| b == 0) {
        Some(i) => p[..i].to_vec(),
        None => p.to_vec(),
    }
}

/// Returns a Location with the given name from IANA Time Zone database-
/// formatted data. The data should be in the format of a standard IANA
/// time zone file (for example, the content of /etc/localtime on Unix
/// systems).
pub fn location_from_tzdata(name: &[u8], data: &[u8]) -> Result<Location, LoadError> {
    let mut d = DataIO {
        p: data,
        error: false,
    };

    // 4-byte magic "TZif"
    if d.read(4) != b"TZif" {
        return Err(LoadError::BadData);
    }

    // 1-byte version, then 15 bytes of padding
    let p = d.read(16);
    if p.len() != 16 {
        return Err(LoadError::BadData);
    }
    let version = match p[0] {
        0 => 1,
        b'2' => 2,
        b'3' => 3,
        _ => return Err(LoadError::BadData),
    };

    // six big-endian 32-bit integers:
    //	number of UTC/local indicators
    //	number of standard/wall indicators
    //	number of leap seconds
    //	number of transition times
    //	number of local time zones
    //	number of characters of time zone abbrev strings
    const NUTC_LOCAL: usize = 0;
    const NSTD_WALL: usize = 1;
    const NLEAP: usize = 2;
    const NTIME: usize = 3;
    const NZONE: usize = 4;
    const NCHAR: usize = 5;
    let mut n = [0usize; 6];
    for v in n.iter_mut() {
        *v = d.big4().ok_or(LoadError::BadData)? as usize;
    }

    // If we have version 2 or 3, then the data is first written out
    // in a 32-bit format, then written out again in a 64-bit format.
    // Skip the 32-bit format and read the 64-bit one, as it can
    // describe a broader range of dates.

    let mut is64 = false;
    if version > 1 {
        // Skip the 32-bit data.
        let skip = n[NTIME] * 4
            + n[NTIME]
            + n[NZONE] * 6
            + n[NCHAR]
            + n[NLEAP] * 8
            + n[NSTD_WALL]
            + n[NUTC_LOCAL];
        // Skip the version 2 header that we just read.
        d.read(skip + 4 + 16);

        is64 = true;

        // Read the counts again, they can differ.
        for v in n.iter_mut() {
            *v = d.big4().ok_or(LoadError::BadData)? as usize;
        }
    }

    let size = if is64 { 8 } else { 4 };

    // Transition times.
    let mut txtimes = DataIO {
        p: d.read(n[NTIME] * size),
        error: false,
    };

    // Time zone indices for transition times.
    let txzones = d.read(n[NTIME]);

    // Zone info structures
    let mut zonedata = DataIO {
        p: d.read(n[NZONE] * 6),
        error: false,
    };

    // Time zone abbreviations.
    let abbrev = d.read(n[NCHAR]);

    // Leap-second time pairs
    d.read(n[NLEAP] * (size + 4));

    // Whether tx times associated with local time types
    // are specified as standard time or wall time.
    d.read(n[NSTD_WALL]);

    // Whether tx times associated with local time types
    // are specified as UTC or local time.
    d.read(n[NUTC_LOCAL]);

    if d.error {
        // ran out of data
        return Err(LoadError::BadData);
    }

    let mut extend = Vec::new();
    let rest = d.rest();
    if rest.len() > 2 && rest[0] == b'\n' && rest[rest.len() - 1] == b'\n' {
        extend = rest[1..rest.len() - 1].to_vec();
    }

    // Now we can build up a useful data structure.
    // First the zone information.
    //	utcoff[4] isdst[1] nameindex[1]
    let nzone = n[NZONE];
    if nzone == 0 {
        // Reject tzdata files with no zones. There's nothing useful in them.
        // This also avoids a panic later when we add and then use a fake transition (golang.org/issue/29437).
        return Err(LoadError::BadData);
    }
    let mut zones = Vec::with_capacity(nzone);
    for _ in 0..nzone {
        let n = zonedata.big4().ok_or(LoadError::BadData)?;
        let offset = n as i32 as i64;
        let b = zonedata.byte().ok_or(LoadError::BadData)?;
        let is_dst = b != 0;
        let b = zonedata.byte().ok_or(LoadError::BadData)?;
        if b as usize >= abbrev.len() {
            return Err(LoadError::BadData);
        }
        zones.push(Zone {
            name: byte_string(&abbrev[b as usize..]),
            offset,
            is_dst,
        });
    }

    // Now the transition time info.
    let mut tx = Vec::with_capacity(n[NTIME]);
    for &zi in txzones.iter() {
        let when = if !is64 {
            txtimes.big4().ok_or(LoadError::BadData)? as i32 as i64
        } else {
            txtimes.big8().ok_or(LoadError::BadData)? as i64
        };
        if zi as usize >= zones.len() {
            return Err(LoadError::BadData);
        }
        tx.push(ZoneTrans { when, index: zi });
    }

    if tx.is_empty() {
        // Build fake transition to cover all time.
        // This happens in fixed locations like "Etc/GMT0".
        tx.push(ZoneTrans {
            when: ALPHA,
            index: 0,
        });
    }

    Ok(Location::new(name.to_vec(), zones, tx, extend))
}

/// Loads the tzdata file with the given name from a directory, or from an
/// uncompressed zip file if dir ends in ".zip".
pub fn load_tzinfo_from_dir_or_zip(dir: &[u8], name: &[u8]) -> Result<Vec<u8>, LoadError> {
    if dir.len() > 4 && &dir[dir.len() - 4..] == b".zip" {
        return load_tzinfo_from_zip(dir, name);
    }
    let mut path = Vec::new();
    if !dir.is_empty() {
        path.extend_from_slice(dir);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    read_file(&path)
}

fn open(name: &[u8]) -> Result<File, LoadError> {
    File::open(std::ffi::OsStr::from_bytes(name)).map_err(|e| LoadError::from_io(&e))
}

/// Reads a file like Go's zoneinfo readFile: in 4096-byte chunks, failing
/// when the file exceeds the maximum size.
fn read_file(name: &[u8]) -> Result<Vec<u8>, LoadError> {
    let mut f = open(name)?;
    let mut buf = [0u8; 4096];
    let mut ret = Vec::new();
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => ret.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(LoadError::from_io(&e)),
        }
        if ret.len() > MAX_FILE_SIZE {
            return Err(LoadError::FileSize(name.to_vec()));
        }
    }
    Ok(ret)
}

/// Reads exactly buf.len() bytes at offset off (from the end if off < 0).
fn preadn(f: &mut File, buf: &mut [u8], off: i64) -> Result<(), String> {
    let pos = if off < 0 {
        SeekFrom::End(off)
    } else {
        SeekFrom::Start(off as u64)
    };
    f.seek(pos).map_err(|e| e.to_string())?;
    f.read_exact(buf).map_err(|e| e.to_string())
}

/// Little-endian 32-bit and 16-bit readers (0 if b is too short).
fn get4(b: &[u8]) -> usize {
    if b.len() < 4 {
        return 0;
    }
    b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16 | (b[3] as usize) << 24
}

fn get2(b: &[u8]) -> usize {
    if b.len() < 2 {
        return 0;
    }
    b[0] as usize | (b[1] as usize) << 8
}

/// Returns the contents of the file with the given name in the given
/// uncompressed zip file.
fn load_tzinfo_from_zip(zipfile: &[u8], name: &[u8]) -> Result<Vec<u8>, LoadError> {
    let mut fd = open(zipfile)?;
    let corrupt = || {
        LoadError::Other(format!(
            "corrupt zip file {}",
            String::from_utf8_lossy(zipfile)
        ))
    };

    const ZECHEADER: usize = 0x06054b50;
    const ZCHEADER: usize = 0x02014b50;
    const ZTAILSIZE: usize = 22;
    const ZHEADERSIZE: usize = 30;
    const ZHEADER: usize = 0x04034b50;

    let mut buf = vec![0u8; ZTAILSIZE];
    if preadn(&mut fd, &mut buf, -(ZTAILSIZE as i64)).is_err() || get4(&buf) != ZECHEADER {
        return Err(corrupt());
    }
    let n = get2(&buf[10..]);
    let size = get4(&buf[12..]);
    let off = get4(&buf[16..]);

    let mut dir = vec![0u8; size];
    if preadn(&mut fd, &mut dir, off as i64).is_err() {
        return Err(corrupt());
    }
    let mut buf: &[u8] = &dir;

    for _ in 0..n {
        // zip entry layout:
        //	0	off	signature
        //	10	2	method
        //	24	4	size
        //	28	2	name length
        //	30	2	extra field length
        //	32	2	file comment length
        //	42	4	local header offset
        //	46	namelen	name
        if get4(buf) != ZCHEADER || buf.len() < 46 {
            break;
        }
        let meth = get2(&buf[10..]);
        let size = get4(&buf[24..]);
        let namelen = get2(&buf[28..]);
        let xlen = get2(&buf[30..]);
        let fclen = get2(&buf[32..]);
        let off = get4(&buf[42..]);
        let end = 46 + namelen;
        if buf.len() < end + xlen + fclen {
            return Err(corrupt());
        }
        let zname = &buf[46..end];
        buf = &buf[end + xlen + fclen..];
        if zname != name {
            continue;
        }
        if meth != 0 {
            return Err(LoadError::Other(format!(
                "unsupported compression for {} in {}",
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(zipfile)
            )));
        }

        // zip per-file header layout:
        //	0	4	signature
        //	8	2	method
        //	26	2	name length
        //	28	2	extra field length
        //	30	namelen	name
        let mut hdr = vec![0u8; ZHEADERSIZE + namelen];
        if preadn(&mut fd, &mut hdr, off as i64).is_err()
            || get4(&hdr) != ZHEADER
            || get2(&hdr[8..]) != meth
            || get2(&hdr[26..]) != namelen
            || &hdr[30..30 + namelen] != name
        {
            return Err(corrupt());
        }
        let xlen = get2(&hdr[28..]);

        let mut data = vec![0u8; size];
        if preadn(&mut fd, &mut data, (off + 30 + namelen + xlen) as i64).is_err() {
            return Err(corrupt());
        }
        return Ok(data);
    }

    Err(LoadError::Errno(2))
}

/// Returns the Location with the given name from one of the specified
/// sources, falling back to $GOROOT/lib/time/zoneinfo.zip. Returns the
/// first error that is not ENOENT, or "unknown time zone".
pub fn load_location_from_sources(name: &[u8], sources: &[&[u8]]) -> Result<Location, LoadError> {
    let mut first_err: Option<LoadError> = None;
    let try_source = |source: &[u8], first_err: &mut Option<LoadError>| -> Option<Location> {
        let err = match load_tzinfo_from_dir_or_zip(source, name) {
            Ok(data) => match location_from_tzdata(name, &data) {
                Ok(z) => return Some(z),
                Err(e) => e,
            },
            Err(e) => e,
        };
        if first_err.is_none() && !err.is_not_exist() {
            *first_err = Some(err);
        }
        None
    };
    for source in sources {
        if let Some(z) = try_source(source, &mut first_err) {
            return Ok(z);
        }
    }
    let goroot = match std::env::var_os("GOROOT") {
        Some(g) if !g.is_empty() => g.as_bytes().to_vec(),
        _ => DEFAULT_GOROOT.to_vec(),
    };
    let mut zip = goroot;
    zip.extend_from_slice(b"/lib/time/zoneinfo.zip");
    if let Some(z) = try_source(&zip, &mut first_err) {
        return Ok(z);
    }
    match first_err {
        Some(e) => Err(e),
        None => Err(LoadError::Other(format!(
            "unknown time zone {}",
            String::from_utf8_lossy(name)
        ))),
    }
}
