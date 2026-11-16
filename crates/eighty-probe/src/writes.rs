//! Write tests: does each candidate key actually change charging?
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eighty_hw::battery::{BatterySnapshot, BatterySource};
use eighty_hw::smc::{FourCC, SmcError, SmcIo};
use serde::Serialize;

use crate::catalog::{Candidate, Role, CANDIDATES};
use crate::guard::RestoreGuard;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Verdict {
    Verified,
    NoEffect,
    WriteAccepted,
    Blocked,
    Missing,
    SizeMismatch { expected: u32, got: usize },
    CannotVerify(String),
    WriteFailed(String),
    RestoreFailed(String),
}

#[derive(Debug, Serialize)]
pub struct WriteResult {
    pub backend: String,
    pub key: String,
    pub role: Role,
    pub verdict: Verdict,
    pub before: Option<BatterySnapshot>,
    pub after: Option<BatterySnapshot>,
}

#[derive(Debug, Clone)]
pub struct WriteTestConfig {
    /// How long after a write (or restore) a battery reading must be taken
    /// before it counts.
    pub settle: Duration,
    pub min_discharge_percent: u8,
    /// Above this, macOS slows charging itself, so "charging stopped" proves nothing.
    pub max_inhibit_percent: u8,
    /// Longest extra wait for macOS to publish a reading taken `settle` after
    /// a write. macOS refreshes battery readings about once a minute.
    pub refresh_timeout: Duration,
}

impl Default for WriteTestConfig {
    fn default() -> Self {
        WriteTestConfig {
            settle: Duration::from_secs(15),
            min_discharge_percent: 20,
            max_inhibit_percent: 95,
            refresh_timeout: Duration::from_secs(120),
        }
    }
}

const CHUNK: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_secs(1);

/// Waits `total` in chunks; returns false if cancelled.
fn settle(total: Duration, cancel: &AtomicBool, sleep: &mut dyn FnMut(Duration)) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        let d = left.min(CHUNK);
        sleep(d);
        left -= d;
    }
    !cancel.load(Ordering::SeqCst)
}

fn precondition(c: &Candidate, b: &BatterySnapshot, cfg: &WriteTestConfig) -> Result<(), String> {
    if !b.external_connected {
        return Err("not plugged in".into());
    }
    match c.role {
        Role::InhibitCharging if !b.is_charging => Err("battery is not charging".into()),
        Role::InhibitCharging if b.percent > cfg.max_inhibit_percent => Err(format!(
            "charge {}% above {}% ceiling (macOS slows charging near full)",
            b.percent, cfg.max_inhibit_percent
        )),
        Role::ForceDischarge if b.percent < cfg.min_discharge_percent => {
            Err(format!("charge {}% below {}% floor", b.percent, cfg.min_discharge_percent))
        }
        Role::ForceDischarge if b.amperage_ma < 0 => Err("battery is already discharging".into()),
        _ => Ok(()),
    }
}

enum Fresh {
    Got(BatterySnapshot),
    Stale,
    Cancelled,
    Failed(String),
}

/// Polls until macOS publishes a battery reading taken at least `cfg.settle`
/// after `since` (Unix seconds). A reading without `UpdateTime` counts as fresh.
fn fresh_snapshot(
    battery: &mut dyn BatterySource,
    since: u64,
    cfg: &WriteTestConfig,
    cancel: &AtomicBool,
    sleep: &mut dyn FnMut(Duration),
) -> Fresh {
    let wanted = since + cfg.settle.as_secs();
    let mut waited = Duration::ZERO;
    loop {
        let s = match battery.snapshot() {
            Ok(s) => s,
            Err(e) => return Fresh::Failed(format!("battery: {e}")),
        };
        if s.update_time.is_none_or(|t| t >= wanted) {
            return Fresh::Got(s);
        }
        if waited >= cfg.refresh_timeout {
            return Fresh::Stale;
        }
        if !settle(POLL, cancel, sleep) {
            return Fresh::Cancelled;
        }
        waited += POLL;
    }
}

struct Outcome {
    verdict: Verdict,
    before: Option<BatterySnapshot>,
    after: Option<BatterySnapshot>,
    /// Stop the run: cancelled, or a restore failed.
    stop: bool,
}

fn test_one(
    c: &Candidate,
    smc: &mut dyn SmcIo,
    battery: &mut dyn BatterySource,
    cfg: &WriteTestConfig,
    cancel: &AtomicBool,
    sleep: &mut dyn FnMut(Duration),
    now: &dyn Fn() -> u64,
) -> Outcome {
    let done = |verdict, before, after| Outcome { verdict, before, after, stop: false };
    let key = match FourCC::new(c.key) {
        Ok(k) => k,
        Err(e) => return done(Verdict::WriteFailed(e.to_string()), None, None),
    };
    match smc.key_info(key) {
        Err(SmcError::KeyNotFound(_)) => return done(Verdict::Missing, None, None),
        Err(SmcError::NotPrivileged) => return done(Verdict::Blocked, None, None),
        Err(e) => return done(Verdict::WriteFailed(e.to_string()), None, None),
        Ok(info) if info.size as usize != c.on.len() => {
            return done(Verdict::SizeMismatch { expected: info.size, got: c.on.len() }, None, None)
        }
        Ok(_) => {}
    }
    let before = match battery.snapshot() {
        Ok(b) => b,
        Err(e) => return done(Verdict::CannotVerify(format!("battery: {e}")), None, None),
    };
    if let Err(reason) = precondition(c, &before, cfg) {
        return done(Verdict::CannotVerify(reason), Some(before), None);
    }

    let mut guard = RestoreGuard::new(smc);
    let written_at = now();
    let mut verdict = match guard.write_tracked(key, c.on) {
        Err(SmcError::NotPrivileged) => Some(Verdict::Blocked),
        Err(e) => Some(Verdict::WriteFailed(e.to_string())),
        Ok(()) => None,
    };
    let mut after = None;
    let mut cancelled = false;
    let wrote = verdict.is_none();
    if verdict.is_none() {
        let mut ok = settle(cfg.settle, cancel, sleep);
        if ok && c.role == Role::Light {
            ok = match guard.write_tracked(key, c.off) {
                Ok(()) => settle(cfg.settle, cancel, sleep),
                Err(e) => {
                    verdict = Some(Verdict::WriteFailed(e.to_string()));
                    true
                }
            };
        }
        if !ok {
            cancelled = true;
            verdict = Some(Verdict::CannotVerify("cancelled".into()));
        }
    }
    if verdict.is_none() && c.role == Role::Light {
        // Only the owner's eyes can verify the light.
        verdict = Some(Verdict::WriteAccepted);
        after = battery.snapshot().ok();
    }
    if verdict.is_none() {
        verdict = Some(match fresh_snapshot(battery, written_at, cfg, cancel, sleep) {
            Fresh::Got(a) => {
                let v = match c.role {
                    Role::InhibitCharging if !a.is_charging => Verdict::Verified,
                    Role::ForceDischarge if a.amperage_ma < 0 => Verdict::Verified,
                    _ => Verdict::NoEffect,
                };
                after = Some(a);
                v
            }
            Fresh::Stale => Verdict::CannotVerify("macOS did not refresh the battery reading".into()),
            Fresh::Cancelled => {
                cancelled = true;
                Verdict::CannotVerify("cancelled".into())
            }
            Fresh::Failed(e) => Verdict::CannotVerify(e),
        });
    }
    let failures: Vec<String> = guard
        .restore_all()
        .into_iter()
        .filter_map(|(k, r)| r.err().map(|e| format!("{k}: {e}")))
        .collect();
    drop(guard);
    let restored_at = now();
    let restore_failed = !failures.is_empty();
    if restore_failed {
        verdict = Some(Verdict::RestoreFailed(failures.join("; ")));
    } else if wrote && !cancelled {
        // Let the battery return to its pre-test state, and wait until macOS
        // reports it, so the next candidate's "before" reading is accurate.
        if settle(cfg.settle, cancel, sleep) && c.role != Role::Light {
            let _ = fresh_snapshot(battery, restored_at, cfg, cancel, sleep);
        }
    }
    Outcome {
        verdict: verdict.expect("verdict set"),
        before: Some(before),
        after,
        stop: cancelled || restore_failed,
    }
}

/// Runs every catalog candidate in order, restoring each before the next.
/// Stops early when `cancel` is set or a restore fails.
pub fn run_write_tests(
    smc: &mut dyn SmcIo,
    battery: &mut dyn BatterySource,
    cfg: &WriteTestConfig,
    cancel: &AtomicBool,
    sleep: &mut dyn FnMut(Duration),
    now: &dyn Fn() -> u64,
) -> Vec<WriteResult> {
    let mut results = Vec::new();
    for c in CANDIDATES {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let o = test_one(c, smc, battery, cfg, cancel, sleep, now);
        results.push(WriteResult {
            backend: c.backend.to_string(),
            key: c.key.to_string(),
            role: c.role,
            verdict: o.verdict,
            before: o.before,
            after: o.after,
        });
        if o.stop {
            break;
        }
    }
    results
}

/// True if any key could not be put back; the caller must alert the user.
pub fn any_restore_failed(results: &[WriteResult]) -> bool {
    results.iter().any(|r| matches!(r.verdict, Verdict::RestoreFailed(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_hw::battery::fake::FakeBattery;
    use eighty_hw::battery::BatterySnapshot;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn snap(percent: u8, charging: bool, ext: bool, amp: i32) -> BatterySnapshot {
        BatterySnapshot {
            percent,
            is_charging: charging,
            external_connected: ext,
            amperage_ma: amp,
            temperature_c: 30.0,
            cycle_count: 1,
            update_time: None,
        }
    }

    fn cfg() -> WriteTestConfig {
        WriteTestConfig { settle: Duration::from_secs(1), ..Default::default() }
    }

    fn run(smc: &FakeSmc, battery: &mut FakeBattery) -> Vec<WriteResult> {
        let cancel = AtomicBool::new(false);
        run_write_tests(&mut smc.clone(), battery, &cfg(), &cancel, &mut |_| {}, &|| 0)
    }

    fn verdict<'a>(r: &'a [WriteResult], key: &str) -> &'a Verdict {
        &r.iter().find(|w| w.key == key).unwrap().verdict
    }

    #[test]
    fn inhibit_verified_when_charging_stops() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let s = smc.clone();
        let mut b = FakeBattery::new(move || {
            if s.value("CHTE") == Some(vec![1, 0, 0, 0]) { snap(70, false, true, 0) } else { snap(70, true, true, 1500) }
        });
        let r = run(&smc, &mut b);
        assert_eq!(verdict(&r, "CHTE"), &Verdict::Verified);
        assert_eq!(verdict(&r, "CH0B"), &Verdict::Missing);
        assert_eq!(smc.value("CHTE"), Some(vec![0, 0, 0, 0]));
    }

    #[test]
    fn inhibit_no_effect_when_battery_unchanged() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        assert_eq!(verdict(&run(&smc, &mut b), "CHTE"), &Verdict::NoEffect);
    }

    #[test]
    fn inhibit_cannot_verify_when_not_charging() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(100, false, true, 0));
        assert!(matches!(verdict(&run(&smc, &mut b), "CHTE"), Verdict::CannotVerify(_)));
        assert!(smc.writes().iter().all(|(k, _)| k != "CHTE"));
    }

    #[test]
    fn discharge_skipped_below_floor() {
        let smc = FakeSmc::new();
        smc.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(15, true, true, 1000));
        assert!(matches!(verdict(&run(&smc, &mut b), "CHIE"), Verdict::CannotVerify(_)));
        assert!(smc.writes().is_empty());
    }

    #[test]
    fn discharge_verified_on_negative_amperage() {
        let smc = FakeSmc::new();
        smc.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
        let s = smc.clone();
        let mut b = FakeBattery::new(move || {
            if s.value("CHIE") == Some(vec![8]) { snap(70, false, true, -900) } else { snap(70, true, true, 1500) }
        });
        assert_eq!(verdict(&run(&smc, &mut b), "CHIE"), &Verdict::Verified);
        assert_eq!(smc.value("CHIE"), Some(vec![0]));
    }

    #[test]
    fn size_mismatch_skips_write() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui8 ", &[0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        assert_eq!(verdict(&run(&smc, &mut b), "CHTE"), &Verdict::SizeMismatch { expected: 1, got: 4 });
        assert!(smc.writes().is_empty());
    }

    #[test]
    fn not_privileged_is_blocked() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::NotPrivileged);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        assert_eq!(verdict(&run(&smc, &mut b), "CHTE"), &Verdict::Blocked);
        assert_eq!(smc.value("CHTE"), Some(vec![0, 0, 0, 0]));
    }

    #[test]
    fn light_writes_green_then_orange_then_restores() {
        let smc = FakeSmc::new();
        smc.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        let r = run(&smc, &mut b);
        assert_eq!(verdict(&r, "ACLC"), &Verdict::WriteAccepted);
        assert_eq!(
            smc.writes(),
            vec![("ACLC".to_string(), vec![3]), ("ACLC".to_string(), vec![4]), ("ACLC".to_string(), vec![0])]
        );
    }

    #[test]
    fn cancel_mid_settle_restores() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        smc.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        let cancel = AtomicBool::new(false);
        let r = run_write_tests(&mut smc.clone(), &mut b, &cfg(), &cancel, &mut |_| {
            cancel.store(true, Ordering::SeqCst)
        }, &|| 0);
        assert_eq!(smc.value("CHTE"), Some(vec![0, 0, 0, 0]));
        assert_eq!(r.last().unwrap().key, "CHTE");
        assert!(smc.writes().iter().all(|(k, _)| k != "ACLC"));
    }

    #[test]
    fn info_not_privileged_is_blocked() {
        let smc = FakeSmc::new();
        smc.insert("CH0J", "ui8 ", &[0], WriteMode::InfoNotPrivileged);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        assert_eq!(verdict(&run(&smc, &mut b), "CH0J"), &Verdict::Blocked);
    }

    #[test]
    fn discharge_cannot_verify_when_already_discharging() {
        let smc = FakeSmc::new();
        smc.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(70, false, true, -500));
        assert!(matches!(verdict(&run(&smc, &mut b), "CHIE"), Verdict::CannotVerify(_)));
        assert!(smc.writes().is_empty());
    }

    #[test]
    fn settles_after_restore_before_next_candidate() {
        let smc = FakeSmc::new();
        smc.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
        let s = smc.clone();
        let mut b = FakeBattery::new(move || {
            if s.value("CHIE") == Some(vec![8]) { snap(70, false, true, -900) } else { snap(70, true, true, 1500) }
        });
        let cancel = AtomicBool::new(false);
        let mut total = Duration::ZERO;
        run_write_tests(&mut smc.clone(), &mut b, &cfg(), &cancel, &mut |d| total += d, &|| 0);
        assert_eq!(total, Duration::from_secs(2));
    }

    #[test]
    fn restore_failure_stops_run_and_names_original() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::AcceptOnce);
        smc.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(70, true, true, 1500));
        let r = run(&smc, &mut b);
        assert_eq!(r.last().unwrap().key, "CHTE");
        assert!(matches!(&r.last().unwrap().verdict, Verdict::RestoreFailed(m) if m.contains("00000000")));
        assert!(smc.writes().iter().all(|(k, _)| k != "ACLC"));
        assert!(any_restore_failed(&r));
    }

    /// A battery whose readings only refresh every 60 virtual seconds, driven
    /// by a virtual millisecond clock that `sleep` advances.
    fn slow_refresh(smc: &FakeSmc, clock: std::rc::Rc<std::cell::Cell<u64>>) -> FakeBattery {
        let s = smc.clone();
        let mut published = (0u64, snap(70, true, true, 1500));
        FakeBattery::new(move || {
            let t = clock.get() / 1000 / 60 * 60;
            if t != published.0 {
                let live = if s.value("CHTE") == Some(vec![1, 0, 0, 0]) {
                    snap(70, false, true, 0)
                } else {
                    snap(70, true, true, 1500)
                };
                published = (t, live);
            }
            BatterySnapshot { update_time: Some(published.0), ..published.1.clone() }
        })
    }

    #[test]
    fn waits_for_fresh_reading_before_judging() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let clock = std::rc::Rc::new(std::cell::Cell::new(10_000u64));
        let mut b = slow_refresh(&smc, clock.clone());
        let cfg = WriteTestConfig { settle: Duration::from_secs(15), ..Default::default() };
        let c2 = clock.clone();
        let r = run_write_tests(
            &mut smc.clone(),
            &mut b,
            &cfg,
            &AtomicBool::new(false),
            &mut |d| clock.set(clock.get() + d.as_millis() as u64),
            &move || c2.get() / 1000,
        );
        assert_eq!(verdict(&r, "CHTE"), &Verdict::Verified);
        assert_eq!(smc.value("CHTE"), Some(vec![0, 0, 0, 0]));
    }

    #[test]
    fn stale_reading_cannot_verify() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| BatterySnapshot { update_time: Some(0), ..snap(70, true, true, 1500) });
        let cfg = WriteTestConfig { settle: Duration::from_secs(1), refresh_timeout: Duration::from_secs(3), ..Default::default() };
        let r = run_write_tests(&mut smc.clone(), &mut b, &cfg, &AtomicBool::new(false), &mut |_| {}, &|| 100);
        assert!(matches!(verdict(&r, "CHTE"), Verdict::CannotVerify(m) if m.contains("refresh")));
        assert_eq!(smc.value("CHTE"), Some(vec![0, 0, 0, 0]));
    }

    #[test]
    fn inhibit_skipped_above_ceiling() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let mut b = FakeBattery::new(|| snap(98, true, true, 1400));
        assert!(matches!(verdict(&run(&smc, &mut b), "CHTE"), Verdict::CannotVerify(m) if m.contains("95%")));
        assert!(smc.writes().is_empty());
    }
}
