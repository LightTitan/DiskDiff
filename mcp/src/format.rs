//! Text for sizes and times in tool results. Agents get exact integers too;
//! these strings are what they quote back to people.

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Human-readable size, base 1000 to match Finder and the app.
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

/// "+1.2 MB", "-340 KB", "0 B". ASCII minus: easier for agents to parse.
pub fn delta(d: i64) -> String {
    match d {
        0 => "0 B".into(),
        d if d > 0 => format!("+{}", bytes(d as u64)),
        d => format!("-{}", bytes(d.unsigned_abs())),
    }
}

/// ISO 8601 in UTC, e.g. "2026-11-09T14:05:00Z".
pub fn iso(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// algorithm), so no date crate is needed.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(12_400_000_000), "12.4 GB");
        assert_eq!(bytes(1_500_000), "1.50 MB");
        assert_eq!(delta(-340_000), "-340 KB");
        assert_eq!(delta(1_200_000), "+1.20 MB");
    }

    #[test]
    fn dates() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso(1_793_888_700), "2026-11-05T14:25:00Z");
    }
}
