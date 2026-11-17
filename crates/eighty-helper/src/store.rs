//! On-disk config (`config.toml`) with atomic save and forgiving load.
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

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
            .and_then(|c| c.settings.validate().map(|_| c));
        match parsed {
            Ok(c) => (c, None),
            Err(reason) => (Config::default(), Some(Self::warn(&path, &reason))),
        }
    }

    fn warn(path: &std::path::Path, reason: &str) -> String {
        format!("ignoring {}: {}; using defaults", path.display(), reason.trim())
    }

    pub fn save(&self, c: &Config) -> io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o755).create(&self.dir)?;
        let text = toml::to_string_pretty(c).map_err(io::Error::other)?;
        let tmp = self.dir.join("config.toml.tmp");
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, self.file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_core::config::Settings;

    #[test]
    fn missing_config_is_defaults_without_warning() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Store::new(d.path().into()).load(), (Config::default(), None));
    }
    #[test]
    fn save_then_load() {
        let d = tempfile::tempdir().unwrap();
        let st = Store::new(d.path().join("eighty"));
        let c = Config { settings: Settings { upper: 70, lower: 60, auto_discharge: false }, accept_other_limiters: true };
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
}
