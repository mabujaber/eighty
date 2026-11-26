//! On-disk config (`config.toml`) with atomic save and forgiving load.
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

use eighty_core::calibration::Calibration;
use eighty_core::config::Config;

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("config.toml")
    }

    /// Missing file: defaults, no warning. Unreadable or invalid: defaults plus a warning.
    pub fn load(&self) -> (Config, Option<String>) {
        let path = self.file();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Config::default(), None),
            Err(e) => return (Config::default(), Some(Self::warn(&path, &e.to_string()))),
        };
        let parsed = toml::from_str::<Config>(&text)
            .map_err(|e| e.to_string())
            .and_then(|c| c.settings.validate().and_then(|_| c.calibration.validate()).map(|_| c));
        match parsed {
            Ok(c) => (c, None),
            Err(reason) => (Config::default(), Some(Self::warn(&path, &reason))),
        }
    }

    fn warn(path: &std::path::Path, reason: &str) -> String {
        format!("ignoring {}: {}; using defaults", path.display(), reason.trim())
    }

    pub fn save(&self, c: &Config) -> io::Result<()> {
        let text = toml::to_string_pretty(c).map_err(io::Error::other)?;
        self.write_atomic("config.toml", &text)
    }

    fn write_atomic(&self, name: &str, text: &str) -> io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o755).create(&self.dir)?;
        let tmp = self.dir.join(format!("{name}.tmp"));
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, self.dir.join(name))
    }

    fn calibration_file(&self) -> PathBuf {
        self.dir.join("calibration.toml")
    }

    /// Missing: `(None, None)`. Unreadable or invalid: the file is deleted and a warning returned.
    pub fn load_calibration(&self) -> (Option<Calibration>, Option<String>) {
        let path = self.calibration_file();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (None, None),
            Err(e) => return (None, Some(Self::drop_warning(&path, &e.to_string()))),
        };
        match toml::from_str::<Calibration>(&text) {
            Ok(c) => (Some(c), None),
            Err(e) => {
                let _ = fs::remove_file(&path);
                (None, Some(Self::drop_warning(&path, &e.to_string())))
            }
        }
    }

    fn drop_warning(path: &std::path::Path, reason: &str) -> String {
        format!("ignoring {}: {}; calibration dropped", path.display(), reason.trim())
    }

    pub fn save_calibration(&self, c: &Calibration) -> io::Result<()> {
        let text = toml::to_string_pretty(c).map_err(io::Error::other)?;
        self.write_atomic("calibration.toml", &text)
    }

    pub fn clear_calibration(&self) {
        let _ = fs::remove_file(self.calibration_file());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_core::calibration::CalStep;
    use eighty_core::config::Settings;

    fn cal() -> Calibration {
        Calibration {
            step: CalStep::Charge, started_at: 100, scheduled: true, step_started_at: 200,
            held_secs: 30, last_progress_at: 250, best_percent: 14,
            last_tick: Some((260, true)), last_kick_at: None,
        }
    }
    #[test]
    fn calibration_save_load_clear() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::new(d.path().join("eighty"));
        assert_eq!(st.load_calibration(), (None, None));
        st.save_calibration(&cal()).unwrap();
        assert_eq!(st.load_calibration(), (Some(cal()), None));
        st.clear_calibration();
        assert_eq!(st.load_calibration(), (None, None));
        st.clear_calibration();
    }
    #[test]
    fn invalid_calibration_file_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::new(d.path().into());
        std::fs::write(d.path().join("calibration.toml"), "garbage [[[").unwrap();
        let (c, w) = st.load_calibration();
        assert_eq!(c, None);
        assert!(w.unwrap().contains("calibration.toml"));
        assert!(!d.path().join("calibration.toml").exists());
    }
    #[test]
    fn missing_config_is_defaults_without_warning() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Store::new(d.path().into()).load(), (Config::default(), None));
    }
    #[test]
    fn save_then_load() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::new(d.path().join("eighty"));
        let c = Config { settings: Settings { upper: 70, lower: 60, auto_discharge: false }, accept_other_limiters: true, native_original: Some(100), ..Config::default() };
        st.save(&c).unwrap();
        assert_eq!(st.load(), (c, None));
    }
    #[test]
    fn invalid_config_falls_back_with_warning() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::new(d.path().into());
        for bad in ["not toml [[[", "[settings]\nupper = 300\n", "[settings]\nupper = 80\nlower = 90\n"] {
            std::fs::write(d.path().join("config.toml"), bad).unwrap();
            let (c, w) = st.load();
            assert_eq!(c, Config::default());
            assert!(w.unwrap().contains("config.toml"));
        }
    }
    #[test]
    fn invalid_schedule_falls_back_with_warning() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "[calibration]\nevery_days = 3\n").unwrap();
        let (c, w) = Store::new(d.path().into()).load();
        assert_eq!(c, Config::default());
        assert!(w.unwrap().contains("every 7 to 90 days"));
    }
    #[test]
    fn partial_settings_load_with_lower_derived() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "[settings]\nupper = 70\n").unwrap();
        let (c, w) = Store::new(d.path().into()).load();
        assert_eq!(c.settings, Settings { upper: 70, lower: 65, auto_discharge: true });
        assert_eq!(w, None);
    }
}
