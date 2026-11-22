//! Unix seconds to and from the Mac's local wall-clock time, for the calibration schedule.
use chrono::{DateTime, Local, LocalResult, NaiveDateTime, TimeDelta, TimeZone};

/// Unix seconds as local wall-clock time.
pub(crate) fn to_local(t: u64) -> NaiveDateTime {
    let utc = DateTime::from_timestamp(i64::try_from(t).unwrap_or(i64::MAX), 0).unwrap_or_default();
    utc.with_timezone(&Local).naive_local()
}

/// Local wall-clock time as Unix seconds. Never panics: see `resolve`.
pub(crate) fn from_local(n: NaiveDateTime) -> u64 {
    resolve(n, |n| Local.from_local_datetime(&n).map(|d| d.timestamp()))
}

/// Resolve a local time with `lookup`. An ambiguous time (clocks going back) takes the
/// earlier instant; a time that doesn't exist (clocks going forward) moves on an hour at a
/// time until it does, so 02:30 in a 02:00-03:00 gap becomes 03:30.
fn resolve(n: NaiveDateTime, lookup: impl Fn(NaiveDateTime) -> LocalResult<i64>) -> u64 {
    let mut t = n;
    for _ in 0..24 {
        match lookup(t) {
            LocalResult::Single(s) => return s.max(0) as u64,
            LocalResult::Ambiguous(a, b) => return a.min(b).max(0) as u64,
            LocalResult::None => t += TimeDelta::hours(1),
        }
    }
    // No time zone has a day-long gap; treat it as UTC rather than fail.
    n.and_utc().timestamp().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 3, 8).unwrap().and_hms_opt(h, m, 0).unwrap()
    }
    /// UTC-5 until 02:00 local, UTC-4 from 03:00; 02:00-03:00 doesn't exist.
    fn spring_forward(n: NaiveDateTime) -> LocalResult<i64> {
        let utc = n.and_utc().timestamp();
        if n < at(2, 0) {
            LocalResult::Single(utc + 5 * 3600)
        } else if n < at(3, 0) {
            LocalResult::None
        } else {
            LocalResult::Single(utc + 4 * 3600)
        }
    }

    #[test]
    fn gap_moves_forward() {
        assert_eq!(resolve(at(2, 30), spring_forward), (at(3, 30).and_utc().timestamp() + 4 * 3600) as u64);
        assert_eq!(resolve(at(10, 0), spring_forward), (at(10, 0).and_utc().timestamp() + 4 * 3600) as u64);
    }
    #[test]
    fn ambiguous_takes_the_earlier() {
        assert_eq!(resolve(at(1, 30), |_| LocalResult::Ambiguous(7_200, 3_600)), 3_600);
    }
    #[test]
    fn never_resolving_falls_back_to_utc() {
        assert_eq!(resolve(at(10, 0), |_| LocalResult::None), at(10, 0).and_utc().timestamp() as u64);
    }
    #[test]
    fn local_round_trips_an_ordinary_time() {
        // 2026-11-22 12:00 UTC: not near a clock change in any zone eighty runs in.
        let t = 1_795_348_800;
        assert_eq!(from_local(to_local(t)), t);
    }
}
