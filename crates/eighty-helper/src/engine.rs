//! The control engine: startup recovery, periodic ticks, health and shutdown restore.
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use eighty_core::config::{Config, Settings};
use eighty_core::policy::{decide, Activity, ControlMode, Inputs, Outputs, Override};
use eighty_core::proto::{DoctorReport, Health, Request, Response, Status};
use eighty_core::{HARD_FLOOR, SETTLE_SECS, TICK_SECS};
use eighty_hw::battery::{BatterySnapshot, BatterySource};
use eighty_hw::smc::SmcIo;

use crate::control::{detect, detect_read_only, Detection, SmcControl, ACLC};
use crate::log::log;
use crate::store::Store;
use crate::system::SystemProbe;

pub enum Event {
    Tick,
    PowerChanged,
    WillSleep(mpsc::Sender<()>),
    DidWake,
    Request { uid: u32, req: Request, reply: mpsc::Sender<Response> },
    Shutdown,
}

pub struct Deps<S: SmcIo, B: BatterySource, Y: SystemProbe> {
    pub smc: S,
    pub battery: B,
    pub system: Y,
    pub store: Store,
    /// Returns Unix seconds.
    pub clock: Box<dyn FnMut() -> u64>,
}

pub struct Engine<S: SmcIo, B: BatterySource, Y: SystemProbe> {
    smc: S,
    battery: B,
    system: Y,
    store: Store,
    clock: Box<dyn FnMut() -> u64>,
    config: Config,
    config_warning: Option<String>,
    light_warning: Option<String>,
    /// One warning per key the last restore failed to write; empty once a restore
    /// (or a successful apply) puts the keys back in a known state.
    restore_failures: Vec<String>,
    detection: Detection,
    control: SmcControl,
    health: Health,
    activity: Activity,
    override_: Override,
    snapshot: Option<BatterySnapshot>,
    /// Outputs last applied successfully; the restore state after a restore.
    prev: Outputs,
    /// When charging outputs last changed, and to what; judged by the watchdog.
    pending: Option<(u64, Outputs)>,
}

impl<S: SmcIo, B: BatterySource, Y: SystemProbe> Engine<S, B, Y> {
    pub fn start(d: Deps<S, B, Y>) -> Engine<S, B, Y> {
        let Deps { smc, battery, system, store, clock } = d;
        let (config, config_warning) = store.load();
        if let Some(w) = &config_warning {
            log(w);
        }
        // Placeholder until `bring_up` detects the keys; nothing reads it before then.
        let detection = Detection { mode: ControlMode::Unsupported, light: false, keys: Vec::new() };
        let control = SmcControl::new(&detection);
        let mut e = Engine {
            smc,
            battery,
            system,
            store,
            clock,
            config,
            config_warning,
            light_warning: None,
            restore_failures: Vec::new(),
            detection,
            control,
            health: Health::Ok,
            activity: Activity::Paused,
            override_: Override::None,
            snapshot: None,
            prev: Outputs::default(),
            pending: None,
        };
        e.bring_up();
        e.tick();
        e
    }

    /// Check for other limiters, detect the keys, set health, and restore if the keys
    /// are ours. Returns true if the restore ran and every key was written.
    fn bring_up(&mut self) -> bool {
        let limiters =
            if self.config.accept_other_limiters { Vec::new() } else { self.system.other_limiters() };
        // With another limiter present, not even detect's write-back may happen.
        self.detection =
            if limiters.is_empty() { detect(&mut self.smc) } else { detect_read_only(&mut self.smc) };
        let d = &self.detection;
        log(&format!("mode {:?}, light {}, keys {:?}", d.mode, d.light, d.keys));
        self.control = SmcControl::new(&self.detection);
        if !limiters.is_empty() {
            self.set_health(Health::OtherLimiters(limiters));
            false
        } else if self.detection.mode == ControlMode::Unsupported {
            self.set_health(Health::Unsupported);
            false
        } else {
            self.set_health(Health::Ok);
            // Crash recovery (or fault recovery): charging may have been left inhibited.
            self.restore();
            self.restore_failures.is_empty()
        }
    }

    /// Re-run startup minus the config load; a clean restore also drops any override.
    fn clear_fault(&mut self) {
        if self.bring_up() {
            self.override_ = Override::None;
        }
        self.tick();
    }

    /// Handle one event. Returns false once the engine has shut down.
    pub fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Tick | Event::PowerChanged | Event::DidWake => self.tick(),
            Event::WillSleep(ack) => {
                self.tick();
                let _ = ack.send(());
            }
            Event::Request { uid, req, reply } => {
                let resp = self.request(uid, req);
                let _ = reply.send(resp);
            }
            Event::Shutdown => {
                self.shutdown();
                return false;
            }
        }
        true
    }

    /// Process events until `Shutdown` or until every sender is gone; tick every `TICK_SECS`.
    pub fn run(mut self, rx: Receiver<Event>) {
        let period = Duration::from_secs(TICK_SECS);
        let mut next_tick = Instant::now() + period;
        loop {
            let ev = match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => {
                    next_tick = Instant::now() + period;
                    Event::Tick
                }
                Err(RecvTimeoutError::Disconnected) => Event::Shutdown,
            };
            if !self.handle(ev) {
                break;
            }
        }
    }

    pub fn status(&self) -> Status {
        let s = self.snapshot.as_ref();
        Status {
            percent: s.map_or(0, |s| s.percent),
            external: s.is_some_and(|s| s.external_connected),
            charging: s.is_some_and(|s| s.is_charging),
            amperage_ma: s.map_or(0, |s| s.amperage_ma),
            mode: self.detection.mode,
            activity: if self.health.is_ok() { self.activity } else { Activity::Paused },
            health: self.health.clone(),
            settings: self.config.settings,
            override_: self.override_,
            light_supported: self.detection.light,
            warnings: self
                .config_warning
                .iter()
                .chain(&self.light_warning)
                .chain(&self.restore_failures)
                .cloned()
                .collect(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn request(&mut self, uid: u32, req: Request) -> Response {
        if req.is_mutating() && uid != 0 && Some(uid) != self.system.console_uid() {
            return Response::Error(
                "permission denied: only an admin (sudo) or the logged-in user can change eighty".into(),
            );
        }
        match req {
            Request::Status => return Response::Status(self.status()),
            Request::Doctor => {
                return Response::Doctor(Box::new(DoctorReport {
                    machine: self.system.machine(),
                    keys: self.detection.keys.clone(),
                    status: self.status(),
                    other_limiters: self.system.other_limiters(),
                }))
            }
            Request::SetLimits { upper, lower } => {
                let settings = match self.config.settings.with_limits(upper, lower) {
                    Ok(s) => s,
                    Err(m) => return Response::Error(m),
                };
                if let Err(m) = self.save(Config { settings, ..self.config.clone() }) {
                    return Response::Error(m);
                }
                self.tick();
            }
            Request::SetAutoDischarge { on } => {
                let s = Settings { auto_discharge: on, ..self.config.settings };
                let settings = match s.with_limits(s.upper, Some(s.lower)) {
                    Ok(s) => s,
                    Err(m) => return Response::Error(m),
                };
                if let Err(m) = self.save(Config { settings, ..self.config.clone() }) {
                    return Response::Error(m);
                }
                self.tick();
            }
            Request::ChargeFull => {
                self.override_ = Override::FullCharge;
                self.tick();
            }
            Request::ChargeAuto => {
                self.override_ = Override::None;
                self.tick();
            }
            Request::DischargeTo { percent } => {
                if !(HARD_FLOOR..=100).contains(&percent) {
                    return Response::Error("discharge target must be between 10 and 100".into());
                }
                if self.detection.mode == ControlMode::Unsupported {
                    return Response::Error(Health::Unsupported.message());
                }
                if !self.health.is_ok() {
                    return Response::Error(self.health.message());
                }
                self.override_ = Override::DischargeTo(percent);
                self.tick();
            }
            Request::ClearFault => self.clear_fault(),
            Request::AcceptOtherLimiters => {
                if let Err(m) = self.save(Config { accept_other_limiters: true, ..self.config.clone() }) {
                    return Response::Error(m);
                }
                self.clear_fault();
            }
        }
        Response::Status(self.status())
    }

    /// Persist `c` and adopt it; on failure nothing changes, so memory matches disk.
    fn save(&mut self, c: Config) -> Result<(), String> {
        self.store.save(&c).map_err(|e| format!("couldn't save config: {e}"))?;
        // The file on disk is now valid, so a warning about ignoring it no longer applies.
        self.config_warning = None;
        self.config = c;
        Ok(())
    }

    /// Judge whether the last charging change took effect, once a reading is fresh enough.
    /// Returns the fault if it did not.
    fn watchdog(&mut self, snap: &BatterySnapshot) -> Option<Health> {
        let (t, o) = self.pending?;
        // No timestamp, or a reading from before the change settled: wait for a newer one.
        let fresh = snap.update_time.is_some_and(|u| u >= t + SETTLE_SECS);
        if !fresh {
            return None;
        }
        self.pending = None;
        if !snap.external_connected {
            // Unplugged: the battery discharges whatever we wrote, so there is nothing to judge.
            return None;
        }
        let what = if o.discharge && snap.amperage_ma >= 0 {
            "CHIE (run from battery)"
        } else if o.inhibit && !o.discharge && snap.is_charging && snap.amperage_ma > 50 {
            "CHTE (stop charging)"
        } else {
            return None;
        };
        Some(Health::NotEffective { what: what.into() })
    }

    /// Health states in which the engine keeps controlling the keys.
    fn may_control(&self) -> bool {
        self.detection.mode != ControlMode::Unsupported
            && matches!(self.health, Health::Ok | Health::BatteryUnreadable(_))
    }

    /// Whether the engine may write the keys at all, even just to restore them.
    /// Conflict, other limiters and unsupported mean the keys are not ours to touch.
    fn may_touch_keys(&self) -> bool {
        self.detection.mode != ControlMode::Unsupported
            && !matches!(
                self.health,
                Health::OtherLimiters(_) | Health::Conflict { .. } | Health::Unsupported
            )
    }

    fn tick(&mut self) {
        // Always read the battery, even when paused, so status shows current numbers.
        let snap = self.battery.snapshot();
        if let Ok(s) = &snap {
            self.snapshot = Some(s.clone());
        }
        if !self.may_control() {
            // A failed restore may have left charging inhibited: keep retrying it.
            if !self.restore_failures.is_empty() && self.may_touch_keys() {
                self.restore();
            }
            self.set_activity(Activity::Paused);
            return;
        }
        let snap = match snap {
            Ok(s) => s,
            Err(err) => {
                let was_unreadable = matches!(self.health, Health::BatteryUnreadable(_));
                self.set_health(Health::BatteryUnreadable(err.to_string()));
                if !was_unreadable || !self.restore_failures.is_empty() {
                    self.restore();
                }
                self.set_activity(Activity::Paused);
                return;
            }
        };
        if matches!(self.health, Health::BatteryUnreadable(_)) {
            self.set_health(Health::Ok);
        }

        if let Err(h) = self.control.check_unchanged(&mut self.smc) {
            // A conflict means something else owns the keys: stop without fighting it.
            // A failed read is treated like a failed write: restore.
            let restore = matches!(h, Health::WriteFailed { .. });
            self.set_health(h);
            if restore {
                self.restore();
            }
            self.set_activity(Activity::Paused);
            return;
        }

        // Health is Ok here: may_control passed and BatteryUnreadable was just cleared.
        if let Some(fault) = self.watchdog(&snap) {
            self.set_health(fault);
            self.restore();
            self.set_activity(Activity::Paused);
            return;
        }

        let inputs = Inputs {
            percent: snap.percent,
            external: snap.external_connected,
            lid_closed: self.system.lid_closed(),
            mode: self.detection.mode,
        };
        let d = decide(&self.config.settings, self.override_, &inputs, &self.prev);
        if d.clear_override {
            log(&format!("override {:?} finished", self.override_));
            self.override_ = Override::None;
        }

        let wrote = match self.control.apply(&mut self.smc, &d.outputs) {
            Ok(wrote) => {
                self.set_light_warning(None);
                wrote
            }
            // The light is cosmetic: CHTE/CHIE were already written, so keep limiting.
            Err((key, err)) if key == ACLC => {
                self.set_light_warning(Some(format!("couldn't set the charge light (ACLC): {err}")));
                false
            }
            Err((key, err)) => {
                self.set_health(Health::WriteFailed { key, error: err.to_string() });
                self.restore();
                self.set_activity(Activity::Paused);
                return;
            }
        };
        // CHTE/CHIE now hold values we chose, so an earlier failed restore no longer matters.
        self.set_restore_failures(Vec::new());
        let charging_changed =
            d.outputs.inhibit != self.prev.inhibit || d.outputs.discharge != self.prev.discharge;
        if wrote || charging_changed {
            self.pending = Some(((self.clock)(), d.outputs));
        }
        if d.outputs != self.prev {
            log(&format!(
                "outputs inhibit={} discharge={} light={:?}",
                d.outputs.inhibit, d.outputs.discharge, d.outputs.light
            ));
        }
        self.prev = d.outputs;
        self.set_activity(d.activity);
    }

    fn shutdown(&mut self) {
        if !self.may_touch_keys() {
            log("shutting down; keys left untouched");
        } else {
            log("shutting down; restoring charging");
            self.restore();
        }
    }

    /// Write the restore state and forget the previous outputs and any pending check.
    fn restore(&mut self) {
        let failures = self
            .control
            .restore(&mut self.smc)
            .into_iter()
            .map(|(key, err)| {
                let effect = if key == ACLC {
                    "the charge light may be wrong"
                } else {
                    "charging may still be inhibited"
                };
                format!("couldn't restore {key}: {err}; {effect}")
            })
            .collect();
        self.set_restore_failures(failures);
        self.prev = Outputs::default();
        self.pending = None;
    }

    fn set_health(&mut self, h: Health) {
        if h != self.health {
            log(&format!("health: {}", h.message()));
            self.health = h;
        }
    }

    fn set_activity(&mut self, a: Activity) {
        if a != self.activity {
            log(&format!("activity {a:?}"));
            self.activity = a;
        }
    }

    fn set_restore_failures(&mut self, failures: Vec<String>) {
        if failures != self.restore_failures {
            if failures.is_empty() {
                log("restore succeeded");
            }
            for f in &failures {
                log(f);
            }
            self.restore_failures = failures;
        }
    }

    fn set_light_warning(&mut self, w: Option<String>) {
        if w != self.light_warning {
            if let Some(w) = &w {
                log(w);
            }
            self.light_warning = w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::FakeSystem;
    use eighty_core::policy::{Activity, ControlMode};
    use eighty_core::proto::{Health, KeyState};
    use eighty_hw::battery::fake::FakeBattery;
    use eighty_hw::battery::{BatteryError, BatterySnapshot};
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Rig {
        smc: FakeSmc,
        sys: FakeSystem,
        store: Store,
        clock: Rc<Cell<u64>>,
        fail: Rc<Cell<bool>>,
        snap: Rc<RefCell<BatterySnapshot>>,
        fallible: bool,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        fn with(smc: FakeSmc) -> Rig {
            let dir = tempfile::tempdir().unwrap();
            Rig {
                smc,
                sys: FakeSystem::default(),
                store: Store::new(dir.path().into()),
                clock: Rc::new(Cell::new(1_000)),
                fail: Rc::new(Cell::new(false)),
                snap: Rc::new(RefCell::new(BatterySnapshot {
                    percent: 50,
                    is_charging: false,
                    external_connected: false,
                    amperage_ma: 0,
                    temperature_c: None,
                    cycle_count: None,
                    update_time: None,
                })),
                fallible: false,
                _dir: dir,
            }
        }
        fn hold() -> Rig {
            let s = FakeSmc::new();
            s.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
            s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
            s.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
            Rig::with(s)
        }
        fn cycle() -> Rig {
            let s = FakeSmc::new();
            s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
            s.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
            Rig::with(s)
        }
        /// Like `hold`, but the battery returns an error while `fail` is set.
        fn hold_fallible() -> Rig {
            Rig { fallible: true, ..Rig::hold() }
        }
        fn set(&self, pct: u8, charging: bool, external: bool, amp: i32, update_time: Option<u64>) {
            let mut s = self.snap.borrow_mut();
            s.percent = pct;
            s.is_charging = charging;
            s.external_connected = external;
            s.amperage_ma = amp;
            s.update_time = update_time;
        }
        fn engine(&self) -> Engine<FakeSmc, FakeBattery, FakeSystem> {
            let snap = self.snap.clone();
            let battery = if self.fallible {
                let fail = self.fail.clone();
                FakeBattery::fallible(move || {
                    if fail.get() {
                        Err(BatteryError::NotFound)
                    } else {
                        Ok(snap.borrow().clone())
                    }
                })
            } else {
                FakeBattery::new(move || snap.borrow().clone())
            };
            let clock = self.clock.clone();
            Engine::start(Deps {
                smc: self.smc.clone(),
                battery,
                system: self.sys.clone(),
                store: Store::new(self._dir.path().into()),
                clock: Box::new(move || clock.get()),
            })
        }
    }

    #[test] fn startup_hold_at_limit_inhibits_and_lights_green() {
        let r = Rig::hold(); r.set(80, true, true, 1200, Some(900));
        let e = r.engine();
        assert_eq!(e.status().mode, ControlMode::Hold);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
        assert_eq!(r.smc.value("ACLC"), Some(vec![3]));
        assert_eq!(e.status().activity, Activity::Holding);
    }
    #[test] fn startup_recovers_stale_discharge() {
        let r = Rig::hold(); r.smc.insert("CHIE", "ui8 ", &[8], WriteMode::Accept); r.set(60, false, true, -400, Some(900));
        let _e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
    }
    #[test] fn other_limiter_blocks_all_writes() {
        let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(90, true, true, 1000, Some(900));
        let mut e = r.engine(); e.handle(Event::Tick); e.handle(Event::Shutdown);
        assert!(r.smc.writes().is_empty());
        assert!(matches!(e.status().health, Health::OtherLimiters(_)));
        assert_eq!(e.status().activity, Activity::Paused);
    }
    #[test] fn unsupported_reports_and_writes_nothing() {
        let r = Rig::with(FakeSmc::new()); r.set(90, true, true, 1000, Some(900));
        let e = r.engine();
        assert_eq!(e.status().health, Health::Unsupported);
        assert!(r.smc.writes().is_empty());
    }
    #[test] fn conflict_stops_writing_and_does_not_restore() {
        let r = Rig::hold(); r.set(80, true, true, 1200, Some(900));
        let mut e = r.engine();
        r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
        let n = r.smc.writes().len();
        e.handle(Event::Tick); e.handle(Event::Shutdown);
        assert!(matches!(e.status().health, Health::Conflict { .. }));
        assert_eq!(r.smc.writes().len(), n);
    }
    #[test] fn write_failure_restores_and_reports() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900));
        let mut e = r.engine();
        r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::NotPrivileged);
        r.set(80, true, true, 1200, Some(960)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::WriteFailed { ref key, .. } if key == "CHTE"));
        assert_eq!((r.smc.value("CHIE"), r.smc.value("ACLC")), (Some(vec![0]), Some(vec![0])));
    }
    #[test] fn failed_restore_is_reported_and_retried() {
        let r = Rig::hold(); r.set(80, true, true, 1200, Some(900));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
        // CHTE is stuck at "stop charging": allowing charge fails, and so does restoring.
        r.smc.insert("CHTE", "ui32", &[1, 0, 0, 0], WriteMode::NotPrivileged);
        r.set(74, true, true, 0, Some(960)); e.handle(Event::Tick);
        let st = e.status();
        assert!(matches!(st.health, Health::WriteFailed { ref key, .. } if key == "CHTE"));
        assert!(st.warnings.iter().any(|w| w.contains("couldn't restore CHTE")), "{:?}", st.warnings);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
        // Once the key is writable again, the next tick's retry restores it.
        r.smc.insert("CHTE", "ui32", &[1, 0, 0, 0], WriteMode::Accept);
        e.handle(Event::Tick);
        assert_eq!(r.smc.value("CHTE"), Some(vec![0; 4]));
        let st = e.status();
        assert!(st.warnings.iter().all(|w| !w.contains("restore")), "{:?}", st.warnings);
        assert!(matches!(st.health, Health::WriteFailed { .. }));
    }
    #[test] fn conflict_never_retries_restore() {
        let r = Rig::hold(); r.set(80, true, true, 1200, Some(900));
        let mut e = r.engine();
        e.restore_failures = vec!["couldn't restore CHTE: x".into()];
        r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
        let n = r.smc.writes().len();
        e.handle(Event::Tick); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::Conflict { .. }));
        assert_eq!(r.smc.writes().len(), n);
    }
    #[test] fn battery_unreadable_restores_then_recovers() {
        // Rig::hold_fallible: the battery closure returns Err while r.fail is set
        let r = Rig::hold_fallible(); r.set(85, true, true, 1200, Some(900));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.fail.set(true); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::BatteryUnreadable(_)));
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
        r.fail.set(false); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
    }
    #[test] fn shutdown_restores() {
        let r = Rig::hold(); r.set(85, true, true, 1200, Some(900));
        let mut e = r.engine(); assert!(!e.handle(Event::Shutdown));
        assert_eq!((r.smc.value("CHTE"), r.smc.value("CHIE"), r.smc.value("ACLC")), (Some(vec![0; 4]), Some(vec![0]), Some(vec![0])));
    }
    #[test] fn will_sleep_ticks_then_acks() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900));
        let mut e = r.engine(); r.set(80, true, true, 1200, Some(960));
        let (tx, rx) = std::sync::mpsc::channel(); e.handle(Event::WillSleep(tx));
        rx.try_recv().unwrap();
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
    }
    #[test] fn cycle_mode_on_m2_shape() {
        let r = Rig::cycle(); r.set(80, true, true, 2700, Some(900));
        let e = r.engine();
        assert_eq!(e.status().mode, ControlMode::Cycle);
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
    }

    // Added beyond the brief.

    #[test] fn light_write_failure_is_only_a_warning() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900));
        let mut e = r.engine();
        assert_eq!(r.smc.value("ACLC"), Some(vec![4]));
        r.smc.insert("ACLC", "ui8 ", &[4], WriteMode::NotPrivileged);
        r.clock.set(2_000);
        r.set(80, true, true, 1200, Some(960)); e.handle(Event::Tick);
        let st = e.status();
        assert_eq!(st.health, Health::Ok);
        assert_eq!(st.activity, Activity::Holding);
        assert!(st.warnings.iter().any(|w| w.contains("ACLC")), "{:?}", st.warnings);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
        assert_eq!(e.pending.map(|(t, o)| (t, o.inhibit)), Some((2_000, true)));
    }
    #[test] fn paused_status_still_shows_battery_numbers() {
        let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(90, true, true, 1000, Some(900));
        let st = r.engine().status();
        assert_eq!((st.percent, st.external, st.charging, st.amperage_ma), (90, true, true, 1000));
    }
    #[test] fn status_without_any_snapshot_is_zeroed() {
        let r = Rig::hold_fallible(); r.fail.set(true);
        let st = r.engine().status();
        assert!(matches!(st.health, Health::BatteryUnreadable(_)));
        assert_eq!((st.percent, st.external, st.charging, st.amperage_ma), (0, false, false, 0));
        assert_eq!(st.activity, Activity::Paused);
    }
    #[test] fn config_warning_and_version_in_status() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900));
        std::fs::write(r._dir.path().join("config.toml"), "not toml [[[").unwrap();
        let st = r.engine().status();
        assert!(st.warnings.iter().any(|w| w.contains("config.toml")));
        assert_eq!(st.version, env!("CARGO_PKG_VERSION"));
        assert!(st.light_supported);
    }
    #[test] fn accepted_other_limiters_do_not_block() {
        let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(80, true, true, 1200, Some(900));
        r.store.save(&Config { accept_other_limiters: true, ..Config::default() }).unwrap();
        let e = r.engine();
        assert_eq!(e.status().health, Health::Ok);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
    }

    #[test] fn watchdog_flags_ineffective_inhibit() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
        let mut e = r.engine();                                  // writes CHTE at t=1000
        r.set(80, true, true, 1200, Some(1010)); e.handle(Event::Tick); // too fresh: no verdict
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1030)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHTE")));
        assert_eq!(r.smc.value("CHTE"), Some(vec![0; 4]));
    }
    #[test] fn watchdog_accepts_effective_discharge() {
        let r = Rig::cycle(); r.clock.set(1000); r.set(81, true, true, 2700, Some(990));
        let mut e = r.engine();
        r.set(81, false, true, -410, Some(1030)); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
    }
    #[test] fn watchdog_waits_on_missing_update_time() {
        let r = Rig::cycle(); r.clock.set(1000); r.set(81, true, true, 2700, None);
        let mut e = r.engine(); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
    }
    fn req(e: &mut Engine<FakeSmc, FakeBattery, FakeSystem>, uid: u32, q: Request) -> Response {
        let (tx, rx) = mpsc::channel(); e.handle(Event::Request { uid, req: q, reply: tx }); rx.recv().unwrap()
    }
    #[test] fn non_console_user_cannot_change_but_can_read() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
        assert!(matches!(req(&mut e, 502, Request::ChargeFull), Response::Error(m) if m.starts_with("permission denied")));
        assert!(matches!(req(&mut e, 502, Request::Status), Response::Status(_)));
        assert!(matches!(req(&mut e, 0, Request::ChargeFull), Response::Status(_)));
    }
    #[test] fn set_limits_persists_and_applies() {
        let r = Rig::hold(); r.set(72, true, true, 1200, Some(900)); let mut e = r.engine();
        let Response::Status(s) = req(&mut e, 501, Request::SetLimits { upper: 70, lower: None }) else { panic!() };
        assert_eq!(s.settings.lower, 65);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
        assert_eq!(r.store.load().0.settings.upper, 70);
        assert!(matches!(req(&mut e, 501, Request::SetLimits { upper: 10, lower: None }), Response::Error(_)));
    }
    #[test] fn discharge_below_floor_rejected() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
        assert!(matches!(req(&mut e, 501, Request::DischargeTo { percent: 9 }), Response::Error(_)));
    }
    #[test] fn clear_fault_retries() {
        let r = Rig::hold(); r.set(80, true, true, 1200, Some(900)); let mut e = r.engine();
        r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::Conflict { .. }));
        req(&mut e, 501, Request::ClearFault);
        assert_eq!(e.status().health, Health::Ok);
    }
    #[test] fn doctor_lists_keys_and_limiters() {
        let r = Rig::cycle(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
        let Response::Doctor(d) = req(&mut e, 502, Request::Doctor) else { panic!() };
        assert!(d.keys.contains(&("CHTE".into(), KeyState::Missing)));
    }

    // Added beyond the brief (Task 7).

    #[test] fn watchdog_flags_ineffective_discharge() {
        let r = Rig::cycle(); r.clock.set(1000); r.set(81, true, true, 2700, Some(990));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.set(81, true, true, 500, Some(1020)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHIE")));
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
        assert_eq!(e.status().activity, Activity::Paused);
    }
    #[test] fn watchdog_does_not_judge_unplugged_reading() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
        let mut e = r.engine();
        r.set(80, true, false, 1200, Some(1030)); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
    }
    #[test] fn clear_fault_drops_override_after_clean_restore() {
        let r = Rig::hold(); r.set(80, true, true, 1200, Some(900)); let mut e = r.engine();
        req(&mut e, 501, Request::ChargeFull);
        assert_eq!(e.status().override_, Override::FullCharge);
        let Response::Status(s) = req(&mut e, 501, Request::ClearFault) else { panic!() };
        assert_eq!(s.override_, Override::None);
    }
    #[test] fn accept_other_limiters_persists_and_unblocks() {
        let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(80, true, true, 1200, Some(900));
        let mut e = r.engine();
        assert!(r.smc.writes().is_empty());
        let Response::Status(s) = req(&mut e, 0, Request::AcceptOtherLimiters) else { panic!() };
        assert_eq!(s.health, Health::Ok);
        assert!(r.store.load().0.accept_other_limiters);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
        let Response::Doctor(d) = req(&mut e, 0, Request::Doctor) else { panic!() };
        assert_eq!(d.other_limiters, vec!["battery".to_string()]);
    }
    #[test] fn discharge_refused_while_unhealthy() {
        let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(80, true, true, 1200, Some(900));
        let mut e = r.engine();
        assert!(matches!(req(&mut e, 501, Request::DischargeTo { percent: 50 }), Response::Error(m) if m.contains("another charge limiter")));
        assert_eq!(e.status().override_, Override::None);
    }
    #[test] fn failed_save_changes_nothing() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
        std::fs::create_dir(r._dir.path().join("config.toml.tmp")).unwrap(); // makes save fail
        assert!(matches!(req(&mut e, 501, Request::SetAutoDischarge { on: false }), Response::Error(m) if m.starts_with("couldn't save config")));
        assert!(e.status().settings.auto_discharge);
        assert!(matches!(req(&mut e, 501, Request::SetLimits { upper: 60, lower: None }), Response::Error(_)));
        assert_eq!(e.status().settings.upper, 80);
    }
}
