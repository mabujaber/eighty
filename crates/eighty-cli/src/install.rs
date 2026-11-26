//! Install and uninstall the launchd helper. Uninstall always tries to restore charging.
use crate::client::call;
use crate::render::render_status;
use anyhow::{anyhow, bail, Context, Result};
use eighty_core::paths::{CALIBRATION_FILE, CONFIG_DIR, HELPER_INSTALL_PATH, LABEL, LOG_PATH, PLIST_PATH, SOCKET_PATH};
use eighty_core::proto::{Request, Response};
use std::fs;
use std::os::unix::fs::{chown, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const LAUNCHCTL: &str = "/bin/launchctl";
/// Files uninstall always removes (besides the helper binary, which may be kept).
const UNINSTALL_FILES: &[&str] = &[PLIST_PATH, SOCKET_PATH, CALIBRATION_FILE];
const HELPER_NAME: &str = "eighty-helper";

/// The launchd plist for the helper daemon.
pub fn plist(helper_path: &str, log_path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\
\t<string>{LABEL}</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>{helper_path}</string>\n\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
\t<key>KeepAlive</key>\n\
\t<true/>\n\
\t<key>StandardErrorPath</key>\n\
\t<string>{log_path}</string>\n\
\t<key>StandardOutPath</key>\n\
\t<string>{log_path}</string>\n\
\t<key>ProcessType</key>\n\
\t<string>Interactive</string>\n\
</dict>\n\
</plist>\n"
    )
}

fn require_root() -> Result<()> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        Ok(())
    } else {
        bail!("run with sudo")
    }
}

fn sibling_helper() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("can't locate the eighty binary")?;
    let dir = exe.parent().ok_or_else(|| anyhow!("can't locate the eighty binary's directory"))?;
    Ok(dir.join(HELPER_NAME))
}

fn bootout() -> std::io::Result<std::process::ExitStatus> {
    Command::new(LAUNCHCTL).arg("bootout").arg(format!("system/{LABEL}")).status()
}

/// Poll until `launchctl print system/<LABEL>` fails (job gone), up to 5 s.
fn wait_until_unloaded() -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let loaded = Command::new(LAUNCHCTL)
            .arg("print")
            .arg(format!("system/{LABEL}"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !loaded {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn write_root_file(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("can't set mode on {}", path.display()))?;
    chown(path, Some(0), Some(0)).with_context(|| format!("can't chown {}", path.display()))
}

pub fn install() -> Result<()> {
    require_root()?;
    let src = sibling_helper()?;
    if !src.exists() {
        bail!("can't find {HELPER_NAME} next to the eighty binary ({})", src.display());
    }
    if Path::new(PLIST_PATH).exists() {
        let _ = bootout(); // may not be loaded; ignore
        if !wait_until_unloaded() {
            bail!("the old helper is still loaded; run `sudo launchctl bootout system/{LABEL}` and retry");
        }
    }
    if let Some(dir) = Path::new(HELPER_INSTALL_PATH).parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    // Copy to a temp file and rename: rewriting a signed binary in place leaves a stale kernel
    // signature cache and the new binary gets killed at launch.
    let tmp = format!("{HELPER_INSTALL_PATH}.new");
    let _ = fs::remove_file(&tmp);
    fs::copy(&src, &tmp).with_context(|| format!("can't copy helper to {tmp}"))?;
    write_root_file(Path::new(&tmp), 0o755)?;
    fs::rename(&tmp, HELPER_INSTALL_PATH).with_context(|| format!("can't move helper into {HELPER_INSTALL_PATH}"))?;

    if let Some(dir) = Path::new(LOG_PATH).parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    fs::create_dir_all(CONFIG_DIR).with_context(|| format!("can't create {CONFIG_DIR}"))?;

    fs::write(PLIST_PATH, plist(HELPER_INSTALL_PATH, LOG_PATH)).with_context(|| format!("can't write {PLIST_PATH}"))?;
    write_root_file(Path::new(PLIST_PATH), 0o644)?;

    let out = Command::new(LAUNCHCTL)
        .args(["bootstrap", "system", PLIST_PATH])
        .output()
        .context("can't run launchctl")?;
    if !out.status.success() {
        bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Response::Status(s)) = call(Path::new(SOCKET_PATH), &Request::Status) {
            print!("{}", render_status(&s));
            println!("installed");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("the helper didn't start; see {LOG_PATH}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Which binary to run `--restore` with: the installed helper, else the sibling.
fn restore_binary(installed_exists: bool, sibling: Option<PathBuf>) -> Option<PathBuf> {
    if installed_exists {
        Some(PathBuf::from(HELPER_INSTALL_PATH))
    } else {
        sibling.filter(|p| p.exists())
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    restored: bool,
    keep_helper: bool,
    warning: Option<String>,
}

/// Decide the uninstall result. `restore_ok` is None when the restore wasn't run.
fn decide(unloaded: bool, restore_ok: Option<bool>, bin: Option<&Path>) -> Outcome {
    let restored = unloaded && restore_ok == Some(true);
    if restored {
        return Outcome { restored, keep_helper: false, warning: None };
    }
    let keep_helper = bin == Some(Path::new(HELPER_INSTALL_PATH));
    let cmd = bin.map_or_else(|| HELPER_NAME.to_string(), |p| p.display().to_string());
    let why = if unloaded { "" } else { " (the helper is still loaded)" };
    Outcome {
        restored,
        keep_helper,
        warning: Some(format!(
            "WARNING: couldn't confirm charging was restored{why}; run `sudo {cmd} --restore`"
        )),
    }
}

fn remove_file(path: &str) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("couldn't remove {path}: {e}"),
    }
}

fn remove_dir(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("couldn't remove {}: {e}", path.display()),
    }
}

pub fn uninstall(purge: bool) -> Result<()> {
    require_root()?;
    println!("stopping helper…");
    match bootout() {
        Ok(st) if st.success() => {}
        Ok(st) => eprintln!("launchctl bootout exited with {st}"),
        Err(e) => eprintln!("couldn't run launchctl: {e}"),
    }
    let unloaded = wait_until_unloaded();
    let bin = restore_binary(Path::new(HELPER_INSTALL_PATH).exists(), sibling_helper().ok());
    // Only restore once the helper is gone, or it would rewrite the inhibit key.
    let restore_ok = match (&bin, unloaded) {
        (Some(b), true) => {
            println!("restoring charging…");
            Some(matches!(Command::new(b).arg("--restore").status(), Ok(s) if s.success()))
        }
        _ => None,
    };
    let outcome = decide(unloaded, restore_ok, bin.as_deref());
    if let Some(w) = &outcome.warning {
        eprintln!("{w}");
    }
    println!("removing files…");
    for f in UNINSTALL_FILES {
        remove_file(f);
    }
    if !outcome.keep_helper {
        remove_file(HELPER_INSTALL_PATH);
    }
    if purge {
        remove_dir(Path::new(CONFIG_DIR));
        if let Some(dir) = Path::new(LOG_PATH).parent() {
            remove_dir(dir);
        }
    }
    if outcome.restored {
        println!("charging restored; eighty uninstalled");
        Ok(())
    } else {
        bail!("uninstalled, but couldn't confirm charging was restored")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_paths_include_calibration_file() {
        assert!(UNINSTALL_FILES.contains(&CALIBRATION_FILE));
    }

    #[test]
    fn plist_has_label_program_and_keepalive() {
        let p = plist("/Library/PrivilegedHelperTools/x", "/Library/Logs/eighty/helper.log");
        for needle in [
            "<string>io.github.mabujaber.eighty.helper</string>",
            "<string>/Library/PrivilegedHelperTools/x</string>",
            "<key>KeepAlive</key>\n\t<true/>",
            "<key>RunAtLoad</key>\n\t<true/>",
            "<string>/Library/Logs/eighty/helper.log</string>",
        ] {
            assert!(p.contains(needle), "{needle}");
        }
    }

    #[test]
    fn decide_branches() {
        let inst = Path::new(HELPER_INSTALL_PATH);
        let sib = Path::new("/x/eighty-helper");
        let ok = decide(true, Some(true), Some(inst));
        assert!(ok.restored && !ok.keep_helper && ok.warning.is_none());

        let failed = decide(true, Some(false), Some(inst));
        assert!(!failed.restored && failed.keep_helper);
        assert!(failed.warning.unwrap().contains(&format!("sudo {HELPER_INSTALL_PATH} --restore")));

        let loaded = decide(false, None, Some(inst));
        assert!(!loaded.restored && loaded.keep_helper);
        assert!(loaded.warning.unwrap().contains("still loaded"));

        let sibling = decide(true, Some(false), Some(sib));
        assert!(!sibling.keep_helper);
        assert!(sibling.warning.unwrap().contains("sudo /x/eighty-helper --restore"));

        let none = decide(true, None, None);
        assert!(!none.restored && !none.keep_helper && none.warning.is_some());
    }

    #[test]
    fn restore_binary_prefers_installed() {
        assert_eq!(restore_binary(true, None), Some(PathBuf::from(HELPER_INSTALL_PATH)));
        assert_eq!(restore_binary(false, Some(PathBuf::from("/nonexistent/h"))), None);
    }
}
