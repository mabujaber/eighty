//! CHIE soak: hold `CHIE=08` for a while so the owner can check the Mac stays
//! awake with the lid closed, then restore it.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eighty_hw::smc::{FourCC, SmcIo};

use crate::guard::RestoreGuard;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Makes SIGINT, SIGTERM and SIGHUP end the soak loop (checked each second)
/// instead of killing the process, so `CHIE` is restored.
pub fn install_stop_handler() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
}

/// Writes `CHIE=08`, calls `sample(elapsed)` at the start and then every `every`
/// until `total` has passed (or a stop signal arrives), then restores `CHIE`.
pub fn run_soak(
    smc: &mut dyn SmcIo,
    total: Duration,
    every: Duration,
    sample: &mut dyn FnMut(Duration),
) -> anyhow::Result<()> {
    let key = FourCC::new("CHIE")?;
    let mut guard = RestoreGuard::new(smc);
    guard.write_tracked(key, &[0x08])?;
    let start = Instant::now();
    let mut next = Duration::ZERO;
    loop {
        let elapsed = start.elapsed();
        if elapsed >= next {
            sample(elapsed);
            next = elapsed + every;
        }
        if elapsed >= total || STOP.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep((total - elapsed).min(Duration::from_secs(1)));
    }
    let failed: Vec<String> = guard
        .restore_all()
        .into_iter()
        .filter_map(|(k, r)| r.err().map(|e| format!("{k}: {e}")))
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("FAILED to restore {}. Re-run eighty-probe, or reboot to reset the SMC.", failed.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};

    fn soak(start: u8) -> FakeSmc {
        let smc = FakeSmc::new();
        smc.insert("CHIE", "ui8 ", &[start], WriteMode::Accept);
        let mut c = smc.clone();
        run_soak(&mut c, Duration::ZERO, Duration::ZERO, &mut |_| {}).unwrap();
        smc
    }

    #[test]
    fn soak_writes_08_then_restores() {
        let smc = soak(0);
        assert_eq!(smc.writes(), vec![("CHIE".to_string(), vec![8]), ("CHIE".to_string(), vec![0])]);
        assert_eq!(smc.value("CHIE"), Some(vec![0]));
    }

    #[test]
    fn soak_restores_original_nonzero() {
        assert_eq!(soak(0x10).value("CHIE"), Some(vec![0x10]));
    }
}
