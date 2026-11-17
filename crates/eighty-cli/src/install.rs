//! Install and uninstall the launchd helper. Uninstall always tries to restore charging.
use crate::client::call;
use crate::render::render_status;
use anyhow::{anyhow, bail, Context, Result};
use eighty_core::paths::{CONFIG_DIR, HELPER_INSTALL_PATH, LABEL, LOG_PATH, PLIST_PATH, SOCKET_PATH};
use eighty_core::proto::{Request, Response};
use std::fs;
use std::os::unix::fs::{chown, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const LAUNCHCTL: &str = "/bin/launchctl";
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
    }
    if let Some(dir) = Path::new(HELPER_INSTALL_PATH).parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    fs::copy(&src, HELPER_INSTALL_PATH).with_context(|| format!("can't copy helper to {HELPER_INSTALL_PATH}"))?;
    write_root_file(Path::new(HELPER_INSTALL_PATH), 0o755)?;

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

fn restore_charging() -> bool {
    let installed = PathBuf::from(HELPER_INSTALL_PATH);
    let bin = if installed.exists() {
        installed
    } else {
        match sibling_helper() {
            Ok(p) if p.exists() => p,
            _ => return false,
        }
    };
    matches!(Command::new(&bin).arg("--restore").status(), Ok(s) if s.success())
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
    if let Err(e) = bootout() {
        eprintln!("couldn't run launchctl: {e}");
    }
    println!("restoring charging…");
    let restored = restore_charging();
    if !restored {
        eprintln!("WARNING: couldn't confirm charging was restored; run `sudo eighty-helper --restore`");
    }
    println!("removing files…");
    remove_file(PLIST_PATH);
    remove_file(HELPER_INSTALL_PATH);
    remove_file(SOCKET_PATH);
    if purge {
        remove_dir(Path::new(CONFIG_DIR));
        if let Some(dir) = Path::new(LOG_PATH).parent() {
            remove_dir(dir);
        }
    }
    if restored {
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
}
