//! Parsing of `eighty calibrate schedule ...` words.
use eighty_core::proto::Request;

const USAGE: &str = "usage: eighty calibrate schedule every <N> days [at HH:MM] | off";

/// Word shape only; ranges for N and HH:MM are validated by the helper.
pub fn parse_schedule(words: &[String]) -> Result<Request, String> {
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["off"] => Ok(Request::SetCalibrationSchedule { every_days: None, at: None }),
        ["every", n, "days"] => Ok(Request::SetCalibrationSchedule { every_days: Some(days(n)?), at: None }),
        ["every", n, "days", "at", t] => {
            Ok(Request::SetCalibrationSchedule { every_days: Some(days(n)?), at: Some((*t).to_string()) })
        }
        _ => Err(USAGE.to_string()),
    }
}

fn days(n: &str) -> Result<u16, String> {
    n.parse().map_err(|_| USAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(w: &[&str]) -> Result<Request, String> {
        parse_schedule(&w.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parse_schedule_forms() {
        assert_eq!(p(&["every", "30", "days"]), Ok(Request::SetCalibrationSchedule { every_days: Some(30), at: None }));
        assert_eq!(
            p(&["every", "30", "days", "at", "21:30"]),
            Ok(Request::SetCalibrationSchedule { every_days: Some(30), at: Some("21:30".into()) })
        );
        assert_eq!(p(&["off"]), Ok(Request::SetCalibrationSchedule { every_days: None, at: None }));
        let usage = "usage: eighty calibrate schedule every <N> days [at HH:MM] | off";
        assert_eq!(p(&["every", "30"]), Err(usage.to_string()));
        assert_eq!(p(&["every", "x", "days"]), Err(usage.to_string()));
        assert_eq!(p(&[]), Err(usage.to_string()));
    }
}
