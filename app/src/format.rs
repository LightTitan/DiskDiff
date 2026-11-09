//! Text formatting shared by every view.

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Human-readable size, base 1000 to match Finder.
pub fn bytes(n: u64) -> String {
    if n == 0 {
        return "0 B".into();
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    let digits = if v >= 100.0 || u == 0 { 0 } else if v >= 10.0 { 1 } else { 2 };
    format!("{v:.digits$} {}", UNITS[u])
}

/// "+1.2 MB", "−340 KB", or "—" for no change.
pub fn delta(d: i64) -> String {
    match d {
        0 => "—".into(),
        d if d > 0 => format!("+{}", bytes(d as u64)),
        d => format!("−{}", bytes(d.unsigned_abs())),
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "Oct 31, 14:05" in local time.
pub fn date(unix: i64) -> String {
    let (y, mo, d, h, mi) = local(unix);
    let _ = y;
    format!("{} {d}, {h:02}:{mi:02}", MONTHS[mo as usize - 1])
}

/// "Oct 31, 2026 at 14:05".
pub fn long_date(unix: i64) -> String {
    let (y, mo, d, h, mi) = local(unix);
    format!("{} {d}, {y} at {h:02}:{mi:02}", MONTHS[mo as usize - 1])
}

/// "3 days", "5 hours"… between two instants.
pub fn span(secs: i64) -> String {
    let s = secs.unsigned_abs();
    let (n, unit) = if s < 3600 {
        ((s / 60).max(1), "minute")
    } else if s < 86_400 {
        (s / 3600, "hour")
    } else if s < 86_400 * 60 {
        (s / 86_400, "day")
    } else {
        (s / (86_400 * 30), "month")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn local(unix: i64) -> (i32, u32, u32, u32, u32) {
    #[cfg(unix)]
    {
        let t = unix as libc_time_t;
        let mut tm: Tm = unsafe { std::mem::zeroed() };
        unsafe { localtime_r(&t, &mut tm) };
        (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32)
    }
    #[cfg(windows)]
    {
        windows_local(unix).unwrap_or_else(|| civil(unix))
    }
    #[cfg(not(any(unix, windows)))]
    {
        civil(unix)
    }
}

/// Windows has no `localtime_r`; convert the UTC civil time with the
/// current time-zone rules (daylight saving included).
#[cfg(windows)]
fn windows_local(unix: i64) -> Option<(i32, u32, u32, u32, u32)> {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    unsafe extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            tz: *const std::ffi::c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }
    let (y, m, d, h, min) = civil(unix);
    let utc = SystemTime {
        year: u16::try_from(y).ok()?,
        month: m as u16,
        day: d as u16,
        hour: h as u16,
        minute: min as u16,
        ..Default::default()
    };
    let mut local = SystemTime::default();
    // SAFETY: both pointers are valid for the call; a null zone means the
    // currently active one.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return None;
    }
    Some((local.year.into(), local.month.into(), local.day.into(), local.hour.into(), local.minute.into()))
}

#[cfg(unix)]
#[allow(non_camel_case_types)]
type libc_time_t = i64;

#[cfg(unix)]
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

#[cfg(unix)]
unsafe extern "C" {
    fn localtime_r(t: *const libc_time_t, out: *mut Tm) -> *mut Tm;
}

/// UTC civil date, for platforms without `localtime_r`.
#[cfg_attr(unix, allow(dead_code))]
fn civil(unix: i64) -> (i32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d, (secs / 3600) as u32, ((secs % 3600) / 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_234), "1.23 KB");
        assert_eq!(bytes(12_345_678), "12.3 MB");
        assert_eq!(bytes(512_000_000_000), "512 GB");
        assert_eq!(delta(0), "—");
        assert_eq!(delta(-340_000), "−340 KB");
        assert_eq!(delta(1_200_000), "+1.20 MB");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(civil(0), (1970, 1, 1, 0, 0));
    }
}

/// The path with the home folder shortened to "~", as Finder's path bar does.
pub fn home_path(p: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    p.display().to_string()
}
