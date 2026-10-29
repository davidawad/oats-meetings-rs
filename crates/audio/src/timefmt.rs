//! UTC timestamps without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// (year, month, day) from days since 1970-01-01 (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parts(secs: i64) -> (i64, u32, u32, i64, i64, i64) {
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    (y, m, d, s / 3600, (s / 60) % 60, s % 60)
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `2026-10-27T23:05:39Z`
pub fn iso(secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = parts(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// OpenOats session id shape: `session_2026-10-27_23-05-39`.
pub fn session_id(secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = parts(secs);
    format!("session_{y:04}-{m:02}-{d:02}_{hh:02}-{mm:02}-{ss:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_782_601_539), "2026-06-27T23:05:39Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(session_id(1_700_000_000), "session_2023-11-14_22-13-20");
    }
}
