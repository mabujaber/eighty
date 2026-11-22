//! Optional calibration schedule: `every N days [at HH:MM]`, and its next due time.
use chrono::{Days, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CalibrationConfig {
    pub every_days: Option<u16>,
    /// "HH:MM"; None = 10:00.
    pub at: Option<String>,
    /// When the schedule was set (Unix s).
    pub set_at: Option<u64>,
    /// When a calibration last finished (Unix s).
    pub last_calibrated: Option<u64>,
    /// The due time a scheduled start consumed (Unix s).
    pub last_scheduled_due: Option<u64>,
}

impl CalibrationConfig {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(n) = self.every_days {
            if !(7..=90).contains(&n) {
                return Err(format!("calibration schedule must be every 7 to 90 days (got {n})"));
            }
        }
        if let Some(s) = &self.at {
            parse_at(s)?;
        }
        Ok(())
    }
}

pub fn parse_at(s: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s, "%H:%M").map_err(|_| format!("time must be HH:MM, e.g. 10:00 (got {s})"))
}

/// The next scheduled calibration time (Unix s), or None when no schedule is set.
pub fn next_due(
    c: &CalibrationConfig,
    to_local: impl Fn(u64) -> NaiveDateTime,
    from_local: impl Fn(NaiveDateTime) -> u64,
) -> Option<u64> {
    let n = u64::from(c.every_days?);
    let at = match &c.at {
        Some(s) => parse_at(s).ok()?,
        None => NaiveTime::from_hms_opt(10, 0, 0)?,
    };
    let anchor = c.last_calibrated.or(c.set_at)?;
    let mut date = to_local(anchor).date().checked_add_days(Days::new(n))?;
    let mut due = from_local(date.and_time(at));
    while c.last_scheduled_due.is_some_and(|d| due <= d) {
        date = date.checked_add_days(Days::new(n))?;
        due = from_local(date.and_time(at));
    }
    Some(due)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, NaiveDate};

    fn loc(t: u64) -> NaiveDateTime { DateTime::from_timestamp(t as i64, 0).unwrap().naive_utc() }
    fn utc(n: NaiveDateTime) -> u64 { n.and_utc().timestamp() as u64 }
    fn ts(y: i32, m: u32, d: u32, h: u32, mi: u32) -> u64 {
        utc(NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap())
    }
    fn cfg() -> CalibrationConfig {
        CalibrationConfig { every_days: Some(30), set_at: Some(ts(2026, 11, 22, 15, 0)), ..Default::default() }
    }

    #[test] fn first_due_is_n_days_after_set() {
        assert_eq!(next_due(&cfg(), loc, utc), Some(ts(2026, 12, 22, 10, 0)));
    }
    #[test] fn after_a_calibration_counts_from_it() {
        let c = CalibrationConfig { last_calibrated: Some(ts(2026, 12, 1, 18, 0)), ..cfg() };
        assert_eq!(next_due(&c, loc, utc), Some(ts(2026, 12, 31, 10, 0)));
    }
    #[test] fn consumed_due_is_skipped() {
        let c = CalibrationConfig { last_scheduled_due: Some(ts(2026, 12, 22, 10, 0)), ..cfg() };
        assert_eq!(next_due(&c, loc, utc), Some(ts(2027, 1, 21, 10, 0)));
    }
    #[test] fn off_means_no_due() {
        assert_eq!(next_due(&CalibrationConfig { every_days: None, ..cfg() }, loc, utc), None);
    }
    #[test] fn custom_at_is_used() {
        let c = CalibrationConfig { at: Some("07:30".into()), ..cfg() };
        assert_eq!(next_due(&c, loc, utc), Some(ts(2026, 12, 22, 7, 30)));
    }
    #[test] fn validate_ranges() {
        let v = |n| CalibrationConfig { every_days: Some(n), ..Default::default() }.validate();
        assert_eq!(v(6), Err("calibration schedule must be every 7 to 90 days (got 6)".into()));
        assert!(v(91).is_err());
        assert!(v(7).is_ok());
        assert!(v(90).is_ok());
        let bad = CalibrationConfig { at: Some("25:00".into()), ..Default::default() };
        assert_eq!(bad.validate(), Err("time must be HH:MM, e.g. 10:00 (got 25:00)".into()));
    }
}
