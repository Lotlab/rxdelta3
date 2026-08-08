//! Always-on append-only file logger for the wrapper DLL.
//!
//! Logs go to `xdelta3_wrap.log` next to the host process's executable
//! (`std::env::current_exe()`), so they are visible even when the DLL is
//! loaded into a GUI process with no console. Timestamps are computed from
//! `SystemTime` epoch (UTC). A write failure silently degrades: logging must
//! never affect merge results.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_FILE_NAME: &str = "xdelta3_wrap.log";

static LOG_FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();
static HEADER_WRITTEN: AtomicBool = AtomicBool::new(false);

fn process_exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

fn log_file() -> Option<&'static Mutex<File>> {
    LOG_FILE
        .get_or_init(|| {
            let exe = process_exe_path();
            let dir = exe.parent()?;
            let path = dir.join(LOG_FILE_NAME);
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => Some(Mutex::new(f)),
                Err(_) => None,
            }
        })
        .as_ref()
}

/// Convert a NUL-terminated UTF-16 pointer into a lossy display string.
pub fn u16_to_lossy(p: *const u16) -> String {
    if p.is_null() {
        return "(null)".to_string();
    }
    let mut len = 0usize;
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
    }
    if len == 0 {
        return String::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(p, len) };
    String::from_utf16_lossy(slice)
}

/// Format epoch seconds + millis into `YYYY-MM-DD HH:MM:SS.mmm` (UTC).
fn format_epoch(secs: i64, millis: u32) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{millis:03}")
}

/// Days since 1970-01-01 → (year, month, day) civil date (proleptic Gregorian,
/// Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn timestamp() -> String {
    let now = SystemTime::now();
    let d = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    format_epoch(d.as_secs() as i64, d.subsec_millis())
}

fn write_header_to(f: &mut File) -> std::io::Result<()> {
    if HEADER_WRITTEN.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let path = process_exe_path();
    let path_s = if path.as_os_str().is_empty() {
        "(unknown)".to_string()
    } else {
        path.to_string_lossy().into_owned()
    };
    writeln!(f, "==== {path_s} PID={} start {ts} ====", std::process::id(), ts = timestamp())
}

fn write_log_line_to(f: &mut File, args: std::fmt::Arguments) -> std::io::Result<()> {
    let ts = timestamp();
    writeln!(f, "[wrap] {ts} {args}")?;
    f.flush()
}

/// Append one log line. Best-effort: any failure is swallowed.
pub fn log(args: std::fmt::Arguments) {
    if let Some(m) = log_file() {
        if let Ok(mut f) = m.lock() {
            let _ = write_header_to(&mut f);
            let _ = write_log_line_to(&mut f, args);
        }
    }
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::log::log(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_utc_known_values() {
        assert_eq!(format_epoch(0, 0), "1970-01-01 00:00:00.000");
        assert_eq!(format_epoch(951_782_400, 0), "2000-02-29 00:00:00.000");
        assert_eq!(format_epoch(1_735_689_600, 123), "2025-01-01 00:00:00.123");
        assert_eq!(format_epoch(0, 999), "1970-01-01 00:00:00.999");
    }

    #[test]
    fn epoch_negative_leap_year() {
        assert_eq!(format_epoch(-86400, 0), "1969-12-31 00:00:00.000");
        assert_eq!(format_epoch(-1, 0), "1969-12-31 23:59:59.000");
    }

    #[test]
    fn epoch_midday_and_leap_second_boundary() {
        assert_eq!(format_epoch(1_700_000_000, 500), "2023-11-14 22:13:20.500");
    }

    #[test]
    fn u16_lossy_null_and_empty() {
        assert_eq!(u16_to_lossy(std::ptr::null()), "(null)");
        assert_eq!(u16_to_lossy([0u16].as_ptr()), "");
    }

    #[test]
    fn u16_lossy_text() {
        let w = [b'A' as u16, b'B' as u16, 0];
        assert_eq!(u16_to_lossy(w.as_ptr()), "AB");
    }

    #[test]
    fn write_line_format() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let mut fh = f.reopen().unwrap();
        write_log_line_to(&mut fh, format_args!("hello {} {}", "world", 42)).unwrap();
        let s = std::fs::read(f.path()).unwrap();
        let s = String::from_utf8(s).unwrap();
        assert!(s.starts_with("[wrap] "), "missing prefix: {s}");
        assert!(s.contains("hello world 42"), "missing message: {s}");
    }
}
