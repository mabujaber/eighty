//! Timestamped log lines on stderr.
use std::time::{SystemTime, UNIX_EPOCH};

/// Write `<UTC timestamp> <msg>` to stderr.
pub fn log(msg: &str) {
    #[cfg(test)]
    CAPTURED.with(|c| c.borrow_mut().push(msg.to_string()));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    eprintln!("{} {msg}", utc(now));
}

#[cfg(test)]
thread_local! {
    /// Every line logged on this thread (tests run one per thread).
    static CAPTURED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The lines logged on this thread since the last call, without timestamps.
#[cfg(test)]
pub fn take_captured() -> Vec<String> {
    CAPTURED.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(4_102_444_799), "2099-12-31T23:59:59Z");
    }
}
