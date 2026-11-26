//! User settings and the on-disk config file schema.
use crate::schedule::CalibrationConfig;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(from = "RawSettings")]
pub struct Settings {
    pub upper: u8,
    pub lower: u8,
    pub auto_discharge: bool,
}

/// Wire form of `Settings`: a missing `lower` follows `upper - 5`, other gaps use defaults.
#[derive(Deserialize)]
#[serde(default)]
struct RawSettings {
    upper: u8,
    lower: Option<u8>,
    auto_discharge: bool,
}

impl Default for RawSettings {
    fn default() -> RawSettings {
        let d = Settings::default();
        RawSettings { upper: d.upper, lower: None, auto_discharge: d.auto_discharge }
    }
}

impl From<RawSettings> for Settings {
    fn from(r: RawSettings) -> Settings {
        Settings { upper: r.upper, lower: r.lower.unwrap_or_else(|| default_lower(r.upper)), auto_discharge: r.auto_discharge }
    }
}

fn default_lower(upper: u8) -> u8 {
    upper.saturating_sub(5).max(10)
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { upper: 80, lower: 75, auto_discharge: true }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(20..=100).contains(&self.upper) {
            return Err(format!("upper limit must be between 20 and 100 (got {})", self.upper));
        }
        if self.lower < 10 || self.lower > self.upper {
            return Err(format!(
                "lower limit must be between 10 and the upper limit {} (got {})",
                self.upper, self.lower
            ));
        }
        Ok(())
    }

    /// Returns a copy with new limits; `lower` defaults to `upper - 5` (at least 10).
    pub fn with_limits(self, upper: u8, lower: Option<u8>) -> Result<Settings, String> {
        let lower = lower.unwrap_or_else(|| default_lower(upper));
        let s = Settings { upper, lower, ..self };
        s.validate()?;
        Ok(s)
    }

    /// Returns a copy with a native macOS limit. `lower` is kept: macOS decides when charging resumes.
    pub fn with_native_limit(self, upper: u8, lower: Option<u8>, available: &[u8]) -> Result<Settings, String> {
        if !available.contains(&upper) {
            let names: Vec<String> = available.iter().map(u8::to_string).collect();
            let list = match names.split_last() {
                Some((last, rest)) if !rest.is_empty() => format!("{} or {}", rest.join(", "), last),
                _ => names.join(""),
            };
            return Err(format!("this Mac only supports a limit of {list}%"));
        }
        if lower.is_some() {
            return Err("macOS decides when charging resumes on this Mac, so there is no lower limit".into());
        }
        self.with_native_upper(upper)
    }

    /// Returns a copy with `upper` set to a native limit, whether or not it is one of
    /// the steps (e.g. one the user picked in System Settings). `lower` is unused in
    /// Native mode but is clamped to at most `upper`, so the result always validates
    /// and the config file stays loadable.
    pub fn with_native_upper(self, upper: u8) -> Result<Settings, String> {
        let s = Settings { upper, lower: self.lower.min(upper), ..self };
        s.validate()?;
        Ok(s)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub accept_other_limiters: bool,
    /// The macOS Charge Limit found before eighty first changed it.
    #[serde(default)]
    pub native_original: Option<u8>,
    #[serde(default)]
    pub calibration: CalibrationConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn defaults_are_80_75_auto() { assert_eq!(Settings::default(), Settings { upper: 80, lower: 75, auto_discharge: true }); }
    #[test] fn lower_defaults_to_upper_minus_5() { assert_eq!(Settings::default().with_limits(90, None).unwrap().lower, 85); }
    #[test] fn rejects_out_of_range() {
        let d = Settings::default();
        assert!(d.with_limits(19, None).is_err());
        assert!(d.with_limits(101, None).is_err());
        assert!(d.with_limits(80, Some(81)).is_err());
        assert!(d.with_limits(80, Some(9)).is_err());
        assert!(d.with_limits(80, Some(80)).is_ok());
    }
    #[test] fn config_toml_roundtrip_and_partial() {
        let c: Config = toml::from_str("[settings]\nupper = 70\nlower = 60\nauto_discharge = false\n").unwrap();
        assert_eq!(c.settings.upper, 70);
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }
    #[test] fn partial_settings_fill_defaults() {
        let c: Config = toml::from_str("[settings]\nupper = 70\n").unwrap();
        assert_eq!(c.settings, Settings { upper: 70, lower: 65, auto_discharge: true });
    }
    #[test] fn native_limits_only_take_macos_steps() {
        let steps = [80, 85, 90, 95, 100];
        assert_eq!(Settings::default().with_native_limit(85, None, &steps).unwrap().upper, 85);
        assert_eq!(Settings::default().with_native_limit(70, None, &steps).unwrap_err(),
            "this Mac only supports a limit of 80, 85, 90, 95 or 100%");
        assert_eq!(Settings::default().with_native_limit(80, Some(75), &steps).unwrap_err(),
            "macOS decides when charging resumes on this Mac, so there is no lower limit");
    }
    #[test] fn native_limit_below_a_hold_era_lower_clamps_lower() {
        let hold = Settings { upper: 90, lower: 85, auto_discharge: true };
        let s = hold.with_native_limit(80, None, &[80, 85, 90, 95, 100]).unwrap();
        assert_eq!((s.upper, s.lower), (80, 80));
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(hold.with_native_upper(82).unwrap().lower, 82);
        assert_eq!(hold.with_native_upper(95).unwrap().lower, 85); // kept when already valid
        assert!(hold.with_native_upper(5).is_err());
    }
    #[test] fn old_config_without_native_original_loads() {
        assert_eq!(toml::from_str::<Config>("[settings]\nupper = 80\n").unwrap().native_original, None);
    }
}
