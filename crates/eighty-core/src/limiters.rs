//! Detection of other charge limiters that would fight eighty over the SMC.
use std::path::{Path, PathBuf};

pub struct KnownLimiter {
    pub name: &'static str,
    /// Files whose presence marks the limiter. `~/` paths are checked in every home directory.
    pub paths: &'static [&'static str],
    /// launchd labels that mark the limiter when loaded.
    pub labels: &'static [&'static str],
}

pub const KNOWN_LIMITERS: &[KnownLimiter] = &[
    KnownLimiter {
        name: "battery",
        paths: &[
            "~/Library/LaunchAgents/battery.plist",
            "/usr/local/co.palokaj.battery",
            "/etc/sudoers.d/battery",
        ],
        labels: &["com.battery.app"],
    },
    KnownLimiter {
        name: "batt",
        paths: &["/Library/LaunchDaemons/cc.chlc.batt.plist"],
        labels: &["cc.chlc.batt"],
    },
    // BattOpt (js4jiang5/BattOpt) ships a prebuilt binary whose `setup` creates its
    // plists; the repo and install script don't name them, so they are unverified
    // and left empty until confirmed on a machine with BattOpt installed.
    KnownLimiter {
        name: "BattOpt",
        paths: &[],
        labels: &[],
    },
];

/// Names of installed limiters, in catalog order. `root` prefixes absolute paths.
pub fn find_other_limiters(
    root: &Path,
    homes: &[PathBuf],
    label_loaded: &mut dyn FnMut(&str) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for l in KNOWN_LIMITERS {
        let by_path = l.paths.iter().any(|p| match p.strip_prefix("~/") {
            Some(rest) => homes.iter().any(|h| h.join(rest).exists()),
            None => root.join(p.trim_start_matches('/')).exists(),
        });
        if by_path || l.labels.iter().any(|lb| label_loaded(lb)) {
            found.push(l.name.to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_battery_leftovers_by_file_and_label() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("Users/a");
        std::fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        std::fs::write(home.join("Library/LaunchAgents/battery.plist"), "").unwrap();
        assert_eq!(
            find_other_limiters(root.path(), std::slice::from_ref(&home), &mut |_| false),
            vec!["battery"]
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            find_other_limiters(empty.path(), &[], &mut |l| l == "cc.chlc.batt"),
            vec!["batt"]
        );
        assert!(find_other_limiters(empty.path(), &[], &mut |_| false).is_empty());
    }
}
