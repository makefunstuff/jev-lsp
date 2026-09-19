//! RFC 3339 timestamps without a date library.
//!
//! Artifacts are stamped with `created` (PROTOCOL.md §7) and the log lines want a readable
//! clock. Pulling in `chrono` for two format strings would be the wrong trade, so this is
//! the standard civil-from-days conversion, tested against known instants.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days from 1970-01-01 to `y-m-d`, from Howard Hinnant's `days_from_civil`, inverted.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format a Unix timestamp (seconds) as RFC 3339 in UTC.
pub fn rfc3339_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// The current instant, RFC 3339 in UTC. Falls back to the epoch if the clock is before it,
/// which is the only case `SystemTime` can fail here.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants_format_correctly() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1), "1970-01-01T00:00:01Z");
        // 2000-03-01 is a leap-year boundary case for the conversion.
        assert_eq!(rfc3339_from_unix(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(rfc3339_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn the_current_time_looks_like_a_timestamp() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'));
        assert_eq!(now.as_bytes()[4], b'-');
        assert_eq!(now.as_bytes()[10], b'T');
        // Sanity: this crate was written after 2026.
        assert!(now.as_str() > "2026-01-01");
    }
}
