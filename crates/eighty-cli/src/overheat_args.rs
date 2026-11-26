//! Parsing of `eighty overheat ...` words.
use eighty_core::proto::Request;

const USAGE: &str = "usage: eighty overheat on | off | <PAUSE °C> [RESUME °C]";

/// Word shape only; ranges are validated by the helper.
pub fn parse_overheat(words: &[String], testing: bool) -> Result<Request, String> {
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["on"] if !testing => Ok(Request::SetOverheat { on: true }),
        ["off"] if !testing => Ok(Request::SetOverheat { on: false }),
        [pause] => Ok(Request::SetOverheatThresholds { pause_c: temp(pause)?, resume_c: None, testing }),
        [pause, resume] => {
            Ok(Request::SetOverheatThresholds { pause_c: temp(pause)?, resume_c: Some(temp(resume)?), testing })
        }
        _ => Err(USAGE.to_string()),
    }
}

fn temp(n: &str) -> Result<u8, String> {
    n.parse().map_err(|_| USAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(w: &[&str], testing: bool) -> Result<Request, String> {
        parse_overheat(&w.iter().map(|s| s.to_string()).collect::<Vec<_>>(), testing)
    }

    #[test]
    fn parse_overheat_args() {
        assert_eq!(p(&["on"], false), Ok(Request::SetOverheat { on: true }));
        assert_eq!(p(&["off"], false), Ok(Request::SetOverheat { on: false }));
        assert_eq!(p(&["38"], false), Ok(Request::SetOverheatThresholds { pause_c: 38, resume_c: None, testing: false }));
        assert_eq!(p(&["38", "34"], false), Ok(Request::SetOverheatThresholds { pause_c: 38, resume_c: Some(34), testing: false }));
        assert_eq!(p(&["22", "20"], true), Ok(Request::SetOverheatThresholds { pause_c: 22, resume_c: Some(20), testing: true }));
        assert!(p(&["hot"], false).is_err());
        assert!(p(&[], false).is_err());
        assert!(p(&["1", "2", "3"], false).is_err());
        assert!(p(&["300"], false).is_err());
    }

    #[test]
    fn clap_wires_testing_flag() {
        use clap::Parser;
        #[derive(Parser)]
        struct C {
            #[arg(required = true)]
            words: Vec<String>,
            #[arg(long, hide = true)]
            testing: bool,
        }
        let c = C::parse_from(["x", "22", "20", "--testing"]);
        assert_eq!(
            parse_overheat(&c.words, c.testing),
            Ok(Request::SetOverheatThresholds { pause_c: 22, resume_c: Some(20), testing: true })
        );
    }
}
