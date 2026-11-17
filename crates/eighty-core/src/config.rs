//! User settings and the on-disk config file schema.
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub upper: u8,
    pub lower: u8,
    pub auto_discharge: bool,
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
        let lower = lower.unwrap_or_else(|| upper.saturating_sub(5).max(10));
        let s = Settings { upper, lower, ..self };
        s.validate()?;
        Ok(s)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub accept_other_limiters: bool,
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
        assert_eq!(c.settings, Settings { upper: 70, lower: 75, auto_discharge: true });
    }
}
