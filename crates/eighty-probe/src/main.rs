//! eighty-probe: report which SMC keys control charging on this Mac.
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use eighty_hw::battery::{BatterySource, IoRegBattery};
use eighty_hw::smc::SmcConnection;
use eighty_probe::catalog::CANDIDATES;
use eighty_hw::machine::MachineInfo;
use eighty_probe::reads::probe_reads;
use eighty_probe::report::{render_text, ProbeReport};
use eighty_probe::writes::{any_restore_failed, run_write_tests, WriteResult, WriteTestConfig};

/// Report which SMC keys stop charging, force discharge and set the MagSafe
/// light. Read-only unless --write-test is given (needs sudo).
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Try each candidate key (needs sudo). Every value is restored afterwards.
    #[arg(long)]
    write_test: bool,
    /// Seconds after each write before a battery reading counts. macOS only
    /// refreshes readings about once a minute, so each check may wait longer.
    #[arg(long, default_value_t = 15)]
    settle_secs: u64,
    /// Also write the report as JSON to this path.
    #[arg(long)]
    json: Option<PathBuf>,
}

fn check_write_permission(write_test: bool, euid: u32) -> Result<(), String> {
    if write_test && euid != 0 {
        Err("--write-test changes SMC keys and needs root: run `sudo eighty-probe --write-test`".into())
    } else {
        Ok(())
    }
}

/// Ctrl-C, closing the terminal (SIGHUP) and SIGTERM all stop the run
/// safely so values get restored. Ctrl-\\ and Ctrl-Z are ignored, because they
/// would kill or suspend the probe with a key still written.
fn install_signal_handlers(cancel: Arc<AtomicBool>) -> anyhow::Result<()> {
    ctrlc::set_handler(move || {
        if !cancel.swap(true, Ordering::SeqCst) {
            eprintln!("\neighty-probe: stopping — restoring SMC keys, please wait…");
        }
    })
    .context("installing signal handler")?;
    // SAFETY: setting the disposition of two signals to SIG_IGN.
    unsafe {
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        libc::signal(libc::SIGTSTP, libc::SIG_IGN);
    }
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// 3 when any key could not be restored, else 0.
fn exit_code(writes: Option<&[WriteResult]>) -> u8 {
    match writes {
        Some(w) if any_restore_failed(w) => 3,
        _ => 0,
    }
}

fn run(args: Args) -> anyhow::Result<u8> {
    let mut smc = SmcConnection::open().context("opening AppleSMC")?;
    let mut battery = IoRegBattery;
    let reads = probe_reads(&mut smc);
    let writes = if args.write_test {
        let cancel = Arc::new(AtomicBool::new(false));
        install_signal_handlers(cancel.clone())?;
        let cfg = WriteTestConfig { settle: Duration::from_secs(args.settle_secs), ..Default::default() };
        eprintln!(
            "Testing {} keys, up to ~3 min each (macOS refreshes battery readings once a minute). \
             Ctrl-C stops safely and restores values.",
            CANDIDATES.len()
        );
        eprintln!(
            "At the end, watch the MagSafe light: GREEN for {0} s, then ORANGE for {0} s.",
            args.settle_secs
        );
        Some(run_write_tests(&mut smc, &mut battery, &cfg, &cancel, &mut std::thread::sleep, &unix_now))
    } else {
        None
    };
    let (battery, battery_error) = match battery.snapshot() {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let report = ProbeReport {
        tool_version: env!("CARGO_PKG_VERSION").into(),
        machine: MachineInfo::current(),
        battery,
        battery_error,
        reads,
        writes,
    };
    print!("{}", render_text(&report));
    if let Some(path) = args.json {
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let code = exit_code(report.writes.as_deref());
    if code == 3 {
        eprintln!(
            "\neighty-probe: WARNING — at least one SMC key could NOT be restored (see RestoreFailed above, \
             which shows the original value). Charging may be affected. Re-run `sudo eighty-probe --write-test` \
             or reboot to reset the SMC."
        );
    }
    Ok(code)
}

fn main() -> ExitCode {
    let args = Args::parse();
    // SAFETY: geteuid has no preconditions.
    if let Err(e) = check_write_permission(args.write_test, unsafe { libc::geteuid() }) {
        eprintln!("eighty-probe: {e}");
        return ExitCode::from(2);
    }
    match run(args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("eighty-probe: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_test_requires_root() {
        let e = check_write_permission(true, 501).unwrap_err();
        assert!(e.contains("sudo eighty-probe --write-test"));
        assert!(check_write_permission(true, 0).is_ok());
        assert!(check_write_permission(false, 501).is_ok());
    }

    #[test]
    fn termination_signals_cancel_and_stop_signals_are_ignored() {
        let cancel = Arc::new(AtomicBool::new(false));
        install_signal_handlers(cancel.clone()).unwrap();
        // SAFETY: raising signals in-process; handlers are installed above.
        unsafe {
            libc::raise(libc::SIGQUIT);
            libc::raise(libc::SIGTSTP);
            libc::raise(libc::SIGTERM);
        }
        for _ in 0..100 {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn exit_code_3_on_restore_failure() {
        use eighty_probe::catalog::Role;
        use eighty_probe::writes::{Verdict, WriteResult};
        let w = |v| WriteResult { backend: "tahoe".into(), key: "CHTE".into(), role: Role::InhibitCharging, verdict: v, before: None, after: None };
        assert_eq!(exit_code(Some(&[w(Verdict::Verified)])), 0);
        assert_eq!(exit_code(Some(&[w(Verdict::RestoreFailed("x".into()))])), 3);
        assert_eq!(exit_code(None), 0);
    }
}
