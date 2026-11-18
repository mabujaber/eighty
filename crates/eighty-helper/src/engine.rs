//! The control engine: startup recovery, periodic ticks, health and shutdown restore.
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use eighty_core::config::{Config, Settings};
use eighty_core::policy::{decide, Activity, ControlMode, Inputs, Outputs, Override};
use eighty_core::proto::{DoctorReport, Health, NativeState, Request, Response, Status};
use eighty_core::{HARD_FLOOR, SETTLE_SECS, TICK_SECS, WATCHDOG_STRIKES};
use eighty_hw::battery::{BatterySnapshot, BatterySource};
use eighty_hw::native::NativeLimit;
use eighty_hw::smc::SmcIo;

use crate::control::{chie_discharging, detect, detect_read_only, Detection, SmcControl, ACLC};
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

pub struct Deps<S: SmcIo, B: BatterySource, Y: SystemProbe, N: NativeLimit> {
    pub smc: S,
    pub battery: B,
    pub system: Y,
    pub store: Store,
    /// Returns Unix seconds.
    pub clock: Box<dyn FnMut() -> u64>,
    /// macOS's own Charge Limit, used in Native mode.
    pub native: N,
}

pub struct Engine<S: SmcIo, B: BatterySource, Y: SystemProbe, N: NativeLimit> {
    smc: S,
    battery: B,
    system: Y,
    store: Store,
    clock: Box<dyn FnMut() -> u64>,
    native: N,
    config: Config,
    config_warning: Option<String>,
    light_warning: Option<String>,
    /// The native limit eighty last set successfully; `None` until the first write
    /// (and again after each bring-up, so the limit is reasserted).
    native_written: Option<u8>,
    /// Clock time of the last successful `set_limit`, or of adopting a limit changed
    /// outside eighty: the native watchdog only judges readings `SETTLE_SECS` after it.
    native_written_at: Option<u64>,
    /// Set while macOS charges past the native limit (calibration); never a fault.
    native_warning: Option<String>,
    /// Between `WillSleep` and `DidWake`: never discharge, since nothing enforces the
    /// floor while the Mac is asleep.
    sleeping: bool,
    /// Set when a conflict found our discharge running and stopping it failed;
    /// cleared by the next restore, which writes `CHIE=00` itself.
    discharge_warning: Option<String>,
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
    /// When the watchdog's settle window last restarted: on a change of charging
    /// outputs, on replug, and on wake. Readings from `since + SETTLE_SECS` on are judged.
    since: Option<u64>,
    /// Consecutive failing watchdog readings since the settle window last restarted.
    strikes: u32,
    /// `update_time` of the last failing reading, so a repeated reading is not a new strike.
    last_strike: Option<u64>,
    /// `external_connected` from the last reading taken while we were not discharging.
    trusted_external: bool,
    /// When our discharge last stopped, until a reading shows macOS caught up with it.
    discharge_stopped: Option<u64>,
    /// When `inhibit` or `discharge` last changed. Status marks a battery reading taken
    /// before `SETTLE_SECS` after it as stale: its current shows the old state.
    charging_changed_at: Option<u64>,
}

impl<S: SmcIo, B: BatterySource, Y: SystemProbe, N: NativeLimit> Engine<S, B, Y, N> {
    pub fn start(d: Deps<S, B, Y, N>) -> Engine<S, B, Y, N> {
        let Deps { smc, battery, system, store, clock, native } = d;
        let (config, config_warning) = store.load();
        if let Some(w) = &config_warning {
            log(w);
        }
        // Placeholder until `bring_up` detects the keys; nothing reads it before then.
        let detection =
            Detection { mode: ControlMode::Unsupported, light: false, keys: Vec::new(), native_steps: Vec::new() };
        let control = SmcControl::new(&detection);
        let mut e = Engine {
            smc,
            battery,
            system,
            store,
            clock,
            native,
            config,
            config_warning,
            light_warning: None,
            native_written: None,
            native_written_at: None,
            native_warning: None,
            sleeping: false,
            discharge_warning: None,
            restore_failures: Vec::new(),
            detection,
            control,
            health: Health::Ok,
            activity: Activity::Paused,
            override_: Override::None,
            snapshot: None,
            prev: Outputs::default(),
            since: None,
            strikes: 0,
            last_strike: None,
            trusted_external: false,
            discharge_stopped: None,
            charging_changed_at: None,
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
        self.detection = if limiters.is_empty() {
            detect(&mut self.smc, &mut self.native)
        } else {
            detect_read_only(&mut self.smc, &mut self.native)
        };
        let d = &self.detection;
        log(&format!("mode {:?}, light {}, keys {:?}, native steps {:?}", d.mode, d.light, d.keys, d.native_steps));
        self.control = SmcControl::new(&self.detection);
        self.native_written = None;
        self.set_native_warning(None);
        if !limiters.is_empty() {
            self.set_health(Health::OtherLimiters(limiters));
            false
        } else if self.detection.mode == ControlMode::Unsupported {
            self.set_health(Health::Unsupported);
            false
        } else {
            self.set_health(Health::Ok);
            // Crash recovery (or fault recovery): charging may have been left inhibited.
            let was_discharging = chie_discharging(&mut self.smc);
            self.restore();
            if was_discharging && !chie_discharging(&mut self.smc) {
                // This clears a CHIE=08 left behind, so macOS may still report the
                // adapter missing for a moment: open the same window as after our own
                // discharge stops. CHIE=08 is only set while plugged in (or by a
                // `DischargeTo`, where on AC is the likely case), so assume on AC meanwhile.
                self.trusted_external = true;
                self.discharge_stopped = Some((self.clock)());
            }
            if self.detection.mode == ControlMode::Native {
                if let Err(error) = self.native_bring_up() {
                    self.set_health(Health::WriteFailed { key: "MCL".into(), error });
                    return false;
                }
            }
            self.restore_failures.is_empty()
        }
    }

    /// Native mode, before the first `set_limit`: remember the Charge Limit macOS had
    /// before eighty first changed it, and say if the configured limit isn't a step.
    fn native_bring_up(&mut self) -> Result<(), String> {
        if self.config.native_original.is_none() {
            let original = self.native.limit().map_err(|e| e.to_string())?;
            log(&format!("native: macOS's Charge Limit was {original}% before eighty"));
            if self.config_warning.is_some() {
                // The file on disk couldn't be read and may hold the real original:
                // don't overwrite it now, but keep it so the next successful save has it.
                log("native: not saving the original Charge Limit over an unreadable config file");
                self.config.native_original = Some(original);
            } else {
                let c = Config { native_original: Some(original), ..self.config.clone() };
                self.save(c).map_err(|m| format!("{m}; not changing the Charge Limit without it"))?;
            }
        }
        if let Some(w) = self.native_step_warning() {
            log(&w);
        }
        Ok(())
    }

    /// In Native mode with a configured limit macOS doesn't offer (an old config):
    /// the nearest step at or above it, used for this run.
    fn native_step(&self) -> Option<u8> {
        let steps = &self.detection.native_steps;
        let upper = self.config.settings.upper;
        if self.detection.mode != ControlMode::Native || steps.is_empty() || steps.contains(&upper) {
            return None;
        }
        steps.iter().copied().find(|&s| s >= upper).or(steps.last().copied())
    }

    fn native_step_warning(&self) -> Option<String> {
        self.native_step().map(|step| {
            format!("limit {}% isn't available on this Mac; using {step}%", self.config.settings.upper)
        })
    }

    /// The settings in effect: the config, with the upper limit moved to a native step if needed.
    fn settings(&self) -> Settings {
        match self.native_step() {
            Some(upper) => Settings { upper, ..self.config.settings },
            None => self.config.settings,
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
            Event::Tick | Event::PowerChanged => self.tick(),
            Event::DidWake => {
                log("wake: macOS woke");
                self.sleeping = false;
                let now = (self.clock)();
                self.restart_settle(Some(now));
                self.tick();
            }
            Event::WillSleep(ack) => {
                log("sleep: macOS is going to sleep");
                self.sleeping = true;
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
            amperage_stale: matches!(
                (self.charging_changed_at, s.and_then(|s| s.update_time)),
                (Some(changed), Some(updated)) if updated < changed + SETTLE_SECS
            ),
            mode: self.detection.mode,
            activity: match (self.health.is_ok(), &self.native_warning) {
                (false, _) => Activity::Paused,
                // macOS is charging past the limit: don't contradict the battery.
                (true, Some(_)) => Activity::Charging { to: 100 },
                (true, None) => self.activity,
            },
            health: self.health.clone(),
            settings: self.settings(),
            override_: self.override_,
            light_supported: self.detection.light,
            warnings: self
                .config_warning
                .iter()
                .cloned()
                .chain(self.native_step_warning())
                .chain(self.native_warning.iter().cloned())
                .chain(self.light_warning.iter().cloned())
                .chain(self.discharge_warning.iter().cloned())
                .chain(self.restore_failures.iter().cloned())
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
                    native: self.native_state(),
                }))
            }
            Request::SetLimits { upper, lower } => {
                let settings = if self.detection.mode == ControlMode::Native {
                    self.config.settings.with_native_limit(upper, lower, &self.detection.native_steps)
                } else {
                    self.config.settings.with_limits(upper, lower)
                };
                let settings = match settings {
                    Ok(s) => s,
                    Err(m) => return Response::Error(m),
                };
                if let Err(m) = self.save(Config { settings, ..self.config.clone() }) {
                    return Response::Error(m);
                }
                self.tick();
            }
            Request::SetAutoDischarge { on } => {
                if self.detection.mode == ControlMode::Native {
                    return Response::Error(
                        "macOS brings the charge down to the limit on this Mac; it can't be turned off".into(),
                    );
                }
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
                if !self.snapshot.as_ref().is_some_and(|s| s.external_connected) {
                    return Response::Error("plug in first: a full charge needs the charger".into());
                }
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
                if self.detection.mode == ControlMode::Native && !self.control.can_discharge() {
                    return Response::Error(
                        "this Mac can't run from the battery while plugged in, so `discharge` isn't available"
                            .into(),
                    );
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

    /// Restart (or clear) the watchdog's settle window; strikes start over with it.
    fn restart_settle(&mut self, since: Option<u64>) {
        self.since = since;
        self.strikes = 0;
        self.last_strike = None;
    }

    /// Judge whether the applied charging outputs are taking effect, on every reading
    /// fresh enough to show it. Returns the fault once `WATCHDOG_STRIKES` consecutive
    /// judged readings, each with a distinct `update_time`, show they are not.
    fn watchdog(&mut self, snap: &BatterySnapshot) -> Option<Health> {
        let o = self.prev;
        // Asleep, unplugged (the battery discharges whatever we wrote), or nothing
        // inhibited: there is nothing to judge.
        if self.sleeping || !snap.external_connected || !(o.inhibit || o.discharge) {
            return None;
        }
        // No timestamp, or a reading from before the change settled: wait for a newer one.
        let since = self.since?;
        let updated = snap.update_time.filter(|&u| u >= since + SETTLE_SECS)?;
        let what = if o.discharge && snap.amperage_ma >= 0 {
            "CHIE (run from battery)"
        } else if o.inhibit && !o.discharge && snap.is_charging && snap.amperage_ma > 50 {
            "CHTE (stop charging)"
        } else {
            // A passing reading: the outputs work, so earlier strikes were blips.
            self.strikes = 0;
            self.last_strike = None;
            return None;
        };
        // The same reading seen again is not new evidence.
        if self.last_strike == Some(updated) {
            return None;
        }
        self.strikes += 1;
        self.last_strike = Some(updated);
        if self.strikes < WATCHDOG_STRIKES {
            log(&format!("watchdog: {what} looks ineffective ({}/{WATCHDOG_STRIKES})", self.strikes));
            return None;
        }
        Some(Health::NotEffective { what: what.into() })
    }

    /// Native mode, before deciding: if the Charge Limit differs from the last value
    /// eighty set (the user changed it in System Settings), make it the configured limit
    /// instead of setting it back. Not while `charge full` has lifted the limit, and not
    /// before eighty's own first write since bring-up (`native_written` is `None`).
    fn adopt_native_limit(&mut self) {
        if self.override_ == Override::FullCharge {
            return;
        }
        let Some(written) = self.native_written else { return };
        // A failed read is left to the next `set_limit`, which reports it.
        let Ok(now) = self.native.limit() else { return };
        if now == written {
            return;
        }
        log(&format!("native limit changed outside eighty: now {now}%"));
        let settings = match self.config.settings.with_native_upper(now) {
            Ok(s) => s,
            Err(m) => {
                log(&format!("native: can't adopt {now}%: {m}"));
                return;
            }
        };
        if let Err(m) = self.save(Config { settings, ..self.config.clone() }) {
            // Keep it for this run anyway, rather than fight the user's choice.
            log(&format!("{m}; using {now}% until eighty restarts"));
            self.config.settings = settings;
        }
        self.native_written = Some(now);
        self.native_written_at = Some((self.clock)());
    }

    /// Native mode: macOS sometimes charges past its own limit to calibrate the
    /// battery. Warn while a fresh reading on AC shows that; never fault or restore.
    fn native_watchdog(&mut self, snap: &BatterySnapshot) {
        let (Some(limit), Some(at)) = (self.native_written, self.native_written_at) else { return };
        // A reading from before the last write or adoption settled can't be judged.
        if !snap.update_time.is_some_and(|u| u >= at + SETTLE_SECS) {
            return;
        }
        if snap.external_connected && snap.percent > limit && snap.is_charging && snap.amperage_ma > 50 {
            self.set_native_warning(Some(format!(
                "macOS is charging past {limit}% (it does this now and then to calibrate the battery); eighty can't stop it"
            )));
        } else if !snap.is_charging || snap.percent <= limit {
            self.set_native_warning(None);
        }
    }

    fn set_native_warning(&mut self, w: Option<String>) {
        if w != self.native_warning {
            match &w {
                Some(w) => log(w),
                None => log("native: macOS stopped charging past the limit"),
            }
            self.native_warning = w;
        }
    }

    /// The doctor's view of macOS's Charge Limit; `None` where it isn't supported.
    fn native_state(&mut self) -> Option<NativeState> {
        if !self.native.supported() {
            return None;
        }
        Some(NativeState {
            available: self.native.available().ok()?,
            limit: self.native.limit().ok(),
            enabled: self.native.enabled().ok(),
        })
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
        let mut snap = self.battery.snapshot();
        if let Ok(s) = &mut snap {
            // While CHIE=08 is ours, macOS reports the adapter as disconnected even
            // though it is not, so an unplugged reading then can't be trusted: keep the
            // last reading taken while we weren't discharging. A real unplug mid-discharge
            // is harmless: the battery was discharging anyway, and the discharge still
            // stops at the target or the floor (decided by percent). After CHIE=00 macOS
            // may still report unplugged for a moment, so an unplugged reading is trusted
            // again only once it is newer than the stop (or, without a timestamp, a full
            // tick later), and in any case from three ticks after the stop on; a
            // plugged-in reading is always trusted.
            // Known limitation: a discharge started while unplugged (a `DischargeTo`
            // override) latches "unplugged", and plugging in mid-discharge can't be seen
            // while CHIE=08 hides the adapter. Status says on battery and the watchdog
            // skips until the discharge stops (target, floor, lid or sleep). CHTE/CHIE
            // are decided the same either way; only the status and `charge full`
            // ("plug in first") are wrong meanwhile.
            if self.prev.discharge {
                s.external_connected |= self.trusted_external;
            } else if let Some(stopped) = self.discharge_stopped {
                let now = (self.clock)();
                // Bounded: a frozen update_time must not keep the window open for good.
                let fresh = now >= stopped + 3 * TICK_SECS
                    || match s.update_time {
                        Some(u) => u > stopped,
                        None => now >= stopped + TICK_SECS,
                    };
                if s.external_connected || fresh {
                    self.discharge_stopped = None;
                    self.trusted_external = s.external_connected;
                } else {
                    s.external_connected = self.trusted_external;
                }
            } else {
                self.trusted_external = s.external_connected;
            }
            let was_external = self.snapshot.as_ref().is_some_and(|p| p.external_connected);
            if s.external_connected && !was_external {
                // Just plugged in: give the charger time to settle before judging.
                let now = (self.clock)();
                self.restart_settle(Some(now));
            }
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
            // A conflict means something else owns the keys: stop without fighting it,
            // except that our own discharge must not outlive our control of the floor.
            // A failed read is treated like a failed write: restore.
            let restore = matches!(h, Health::WriteFailed { .. });
            let conflict = matches!(h, Health::Conflict { .. });
            self.set_health(h);
            if restore {
                self.restore();
            } else if conflict {
                self.stop_our_discharge();
            }
            self.set_activity(Activity::Paused);
            return;
        }

        // Health is Ok here: may_control passed and BatteryUnreadable was just cleared.
        // In Native mode macOS holds the limit, so the SMC watchdog has nothing to judge;
        // `native_watchdog` below only warns.
        if self.detection.mode != ControlMode::Native {
            if let Some(fault) = self.watchdog(&snap) {
                self.set_health(fault);
                self.restore();
                self.set_activity(Activity::Paused);
                return;
            }
        }

        if self.detection.mode == ControlMode::Native {
            self.adopt_native_limit();
        }

        let inputs = Inputs {
            percent: snap.percent,
            external: snap.external_connected,
            lid_closed: self.system.lid_closed(),
            mode: self.detection.mode,
        };
        let mut d = decide(&self.settings(), self.override_, &inputs, &self.prev);
        if d.clear_override {
            log(&format!("override {:?} finished", self.override_));
            self.override_ = Override::None;
        }
        if self.sleeping && d.outputs.discharge {
            // Nothing watches the floor while asleep: stop discharging. Hold mode keeps
            // inhibit (a true hold); Native mode's Charge Limit holds on its own.
            d.outputs.discharge = false;
        }

        if let Some(limit) = d.native_limit {
            if self.native_written != Some(limit) {
                if let Err(err) = self.native.set_limit(limit) {
                    self.set_health(Health::WriteFailed { key: "MCL".into(), error: err.to_string() });
                    self.restore();
                    self.set_activity(Activity::Paused);
                    return;
                }
                log(&format!("native: set macOS's Charge Limit to {limit}%"));
                self.native_written = Some(limit);
                self.native_written_at = Some((self.clock)());
            }
        }
        if self.detection.mode == ControlMode::Native {
            self.native_watchdog(&snap);
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
            let now = (self.clock)();
            self.restart_settle(Some(now));
        }
        if d.outputs != self.prev {
            log(&format!(
                "outputs inhibit={} discharge={} light={:?}",
                d.outputs.inhibit, d.outputs.discharge, d.outputs.light
            ));
        }
        let old = self.prev;
        self.prev = d.outputs;
        self.note_prev_change(old);
        self.set_activity(d.activity);
    }

    /// Call after changing `prev` from `old`: remember when charging changed, and when
    /// our discharge stopped.
    fn note_prev_change(&mut self, old: Outputs) {
        let p = self.prev;
        if old.inhibit != p.inhibit || old.discharge != p.discharge {
            self.charging_changed_at = Some((self.clock)());
        }
        if old.discharge && !p.discharge {
            self.discharge_stopped = Some((self.clock)());
        }
    }

    /// On entering a conflict: stop our discharge if `CHIE` still holds our `08`;
    /// every other key stays as the other writer left it.
    fn stop_our_discharge(&mut self) {
        let old = self.prev;
        match self.control.stop_discharge_if_ours(&mut self.smc) {
            Ok(true) => {
                log("conflict: stopped our discharge (CHIE=00); other keys left as found");
                self.prev.discharge = false;
            }
            // Not ours any more (or never was): our discharge isn't running either.
            Ok(false) => self.prev.discharge = false,
            Err(err) => {
                let w = format!(
                    "couldn't stop discharging (CHIE): {err}; the battery may drain while plugged in"
                );
                log(&w);
                self.discharge_warning = Some(w);
            }
        }
        self.note_prev_change(old);
    }

    fn shutdown(&mut self) {
        if !self.may_touch_keys() {
            log("shutting down; keys left untouched");
        } else {
            log("shutting down; restoring charging");
            self.restore();
        }
    }

    /// Write the restore state and forget the previous outputs and the settle window.
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
        self.discharge_warning = None;
        // Paused from here on: nothing judges the native limit until control resumes.
        self.set_native_warning(None);
        let old = self.prev;
        self.prev = Outputs::default();
        self.note_prev_change(old);
        self.restart_settle(None);
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
    use eighty_core::proto::{Health, KeyState, NativeState};
    use eighty_hw::battery::fake::FakeBattery;
    use eighty_hw::battery::{BatteryError, BatterySnapshot};
    use eighty_hw::native::FakeNative;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    type TestEngine = Engine<FakeSmc, FakeBattery, FakeSystem, FakeNative>;

    struct Rig {
        smc: FakeSmc,
        native: FakeNative,
        sys: FakeSystem,
        store: Store,
        clock: Rc<Cell<u64>>,
        fail: Rc<Cell<bool>>,
        snap: Rc<RefCell<BatterySnapshot>>,
        fallible: bool,
        /// Mirrors the hardware: while CHIE has the discharge bit set, macOS reports
        /// the adapter as disconnected.
        chie_hides_ac: bool,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        fn with(smc: FakeSmc) -> Rig {
            let dir = tempfile::tempdir().unwrap();
            Rig {
                smc,
                native: FakeNative::unsupported(),
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
                chie_hides_ac: false,
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
        /// No `CHTE`, but macOS's own Charge Limit: native mode.
        fn native() -> Rig {
            Rig::native_with(FakeNative::new(&[80, 85, 90, 95, 100], 100))
        }
        fn native_with(native: FakeNative) -> Rig {
            let s = FakeSmc::new();
            s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept);
            s.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
            Rig { native, ..Rig::with(s) }
        }
        fn write_config(&self, text: &str) {
            std::fs::write(self._dir.path().join("config.toml"), text).unwrap();
        }
        /// Like `hold`, but the battery returns an error while `fail` is set.
        fn hold_fallible() -> Rig {
            Rig { fallible: true, ..Rig::hold() }
        }
        /// Battery reports `external_connected: false` whenever CHIE has bit 0x08 set.
        fn hiding_ac(self) -> Rig {
            Rig { chie_hides_ac: true, ..self }
        }
        fn set(&self, pct: u8, charging: bool, external: bool, amp: i32, update_time: Option<u64>) {
            let mut s = self.snap.borrow_mut();
            s.percent = pct;
            s.is_charging = charging;
            s.external_connected = external;
            s.amperage_ma = amp;
            s.update_time = update_time;
        }
        fn engine(&self) -> TestEngine {
            let snap = self.snap.clone();
            let (smc, hides) = (self.smc.clone(), self.chie_hides_ac);
            let snap = Rc::new(move || {
                let mut s = snap.borrow().clone();
                if hides && smc.value("CHIE").is_some_and(|v| v[0] & 0x08 != 0) {
                    s.external_connected = false;
                }
                s
            });
            let battery = if self.fallible {
                let fail = self.fail.clone();
                FakeBattery::fallible(move || {
                    if fail.get() {
                        Err(BatteryError::NotFound)
                    } else {
                        Ok(snap())
                    }
                })
            } else {
                FakeBattery::new(move || snap())
            };
            let clock = self.clock.clone();
            Engine::start(Deps {
                smc: self.smc.clone(),
                battery,
                system: self.sys.clone(),
                store: Store::new(self._dir.path().into()),
                clock: Box::new(move || clock.get()),
                native: self.native.clone(),
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
    #[test] fn startup_restores_before_deciding() {
        // Unplugged below the limit: the decision leaves the light alone (light: None),
        // so only the startup restore can clear a stale green light.
        let r = Rig::hold(); r.smc.insert("ACLC", "ui8 ", &[3], WriteMode::Accept); r.set(60, false, false, -400, Some(900));
        let _e = r.engine();
        assert_eq!(r.smc.value("ACLC"), Some(vec![0]));
        let w = |k: &str, v: &[u8]| (k.to_string(), v.to_vec());
        assert_eq!(r.smc.writes(), vec![
            w("CHTE", &[0; 4]), w("CHIE", &[0]), w("ACLC", &[3]), // detect's write-back
            w("CHTE", &[0; 4]), w("CHIE", &[0]), w("ACLC", &[0]), // startup restore
            w("CHTE", &[0; 4]), w("CHIE", &[0]),                  // first decision
        ]);
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
    /// Hold rig at 85% on the charger: inhibit and discharge on (CHTE=01000000, CHIE=08).
    fn discharging_rig() -> (Rig, TestEngine) {
        let r = Rig::hold(); r.set(85, false, true, -400, Some(900));
        let e = r.engine();
        assert_eq!((r.smc.value("CHTE"), r.smc.value("CHIE")), (Some(vec![1, 0, 0, 0]), Some(vec![8])));
        (r, e)
    }
    #[test] fn conflict_while_discharging_stops_our_discharge() {
        let (r, mut e) = discharging_rig();
        let aclc = r.smc.value("ACLC");
        r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept); // another writer allows charging
        let n = r.smc.writes().len();
        e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::Conflict { ref key, .. } if key == "CHTE"));
        assert_eq!(r.smc.writes()[n..], [("CHIE".to_string(), vec![0])]);
        assert_eq!((r.smc.value("CHTE"), r.smc.value("CHIE"), r.smc.value("ACLC")), (Some(vec![0; 4]), Some(vec![0]), aclc));
        assert!(!e.prev.discharge);
        e.handle(Event::Tick); e.handle(Event::Shutdown);
        assert_eq!(r.smc.writes().len(), n + 1);
    }
    #[test] fn conflict_on_chie_leaves_it_alone() {
        let (r, mut e) = discharging_rig();
        r.smc.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); // another writer owns CHIE now
        let n = r.smc.writes().len();
        e.handle(Event::Tick); e.handle(Event::Shutdown);
        assert!(matches!(e.status().health, Health::Conflict { ref key, .. } if key == "CHIE"));
        assert_eq!(r.smc.writes().len(), n);
    }
    #[test] fn conflict_failed_stop_discharge_warns() {
        let (r, mut e) = discharging_rig();
        r.smc.insert("CHIE", "ui8 ", &[8], WriteMode::NotPrivileged);
        r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
        let n = r.smc.writes().len();
        e.handle(Event::Tick);
        let st = e.status();
        assert!(matches!(st.health, Health::Conflict { ref key, .. } if key == "CHTE"));
        assert!(st.warnings.iter().any(|w| w.starts_with("couldn't stop discharging (CHIE): ") && w.ends_with("; the battery may drain while plugged in")), "{:?}", st.warnings);
        assert_eq!((r.smc.value("CHIE"), r.smc.writes().len()), (Some(vec![8]), n));
    }
    #[test] fn extra_chie_bits_while_discharging_are_not_a_conflict() {
        let (r, mut e) = discharging_rig();
        r.smc.insert("CHIE", "ui8 ", &[0x18], WriteMode::Accept); // firmware set another bit
        let n = r.smc.writes().len();
        e.handle(Event::Tick);
        let st = e.status();
        assert_eq!((st.health, st.activity), (Health::Ok, Activity::Discharging { to: 80 }));
        assert_eq!((r.smc.value("CHIE"), r.smc.writes().len()), (Some(vec![0x18]), n));
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
    fn sleep(e: &mut TestEngine) {
        let (tx, rx) = std::sync::mpsc::channel(); e.handle(Event::WillSleep(tx));
        rx.try_recv().expect("sleep acked");
    }
    #[test] fn hold_sleep_holds_instead_of_discharging() {
        let r = Rig::hold(); r.set(85, false, true, -400, Some(900));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        sleep(&mut e);
        assert_eq!((r.smc.value("CHIE"), r.smc.value("CHTE")), (Some(vec![0]), Some(vec![1, 0, 0, 0])));
    }
    #[test] fn discharge_override_pauses_while_asleep() {
        let r = Rig::hold(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
        req(&mut e, 501, Request::DischargeTo { percent: 50 });
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        sleep(&mut e);
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
        assert_eq!(e.status().override_, Override::DischargeTo(50));
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
        assert_eq!((e.since, e.prev.inhibit), (Some(2_000), true));
    }
    #[test] fn paused_status_still_shows_battery_numbers() {
        let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(90, true, true, 1000, Some(900));
        let st = r.engine().status();
        assert_eq!((st.percent, st.external, st.charging, st.amperage_ma), (90, true, true, 1000));
    }
    #[test] fn battery_reading_from_before_a_switch_is_marked_stale() {
        // Hold mode at 81% with auto-discharge: discharge starts at t=1000 while the last
        // reading (t=990) still shows the charger's current.
        let r = Rig::hold(); r.set(81, true, true, 2157, Some(990));
        let mut e = r.engine();
        assert!(e.prev.discharge);
        assert!(e.status().amperage_stale);
        // Too soon after the switch to show its effect: still stale.
        r.clock.set(1_060); r.set(81, false, true, 2157, Some(1_001)); e.handle(Event::Tick);
        assert!(e.status().amperage_stale);
        // A reading SETTLE_SECS after the switch is fresh.
        r.clock.set(1_120); r.set(81, false, true, -820, Some(1_000 + SETTLE_SECS)); e.handle(Event::Tick);
        assert!(!e.status().amperage_stale);
        // A reading without a timestamp can't be judged, so it's shown as is.
        r.clock.set(1_180); r.set(75, false, true, -820, None); e.handle(Event::Tick);
        assert!(!e.prev.discharge);
        assert!(!e.status().amperage_stale);
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
        r.set(80, true, true, 1200, Some(1030)); e.handle(Event::Tick); // first strike
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1090)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHTE")));
        assert_eq!(r.smc.value("CHTE"), Some(vec![0; 4]));
    }
    #[test] fn watchdog_ignores_a_single_blip() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
        let mut e = r.engine();                                          // writes CHTE at t=1000
        r.set(80, true, true, 1200, Some(1030)); e.handle(Event::Tick);  // failing
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, false, true, 0, Some(1090)); e.handle(Event::Tick);    // passing: strikes reset
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1150)); e.handle(Event::Tick);  // failing again: one strike
        assert_eq!(e.status().health, Health::Ok);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
    }
    #[test] fn watchdog_needs_two_distinct_readings() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
        let mut e = r.engine();
        r.set(80, true, true, 1200, Some(1030)); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
        e.handle(Event::Tick);                                           // same reading again: no new strike
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1090)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHTE")));
        assert_eq!(r.smc.value("CHTE"), Some(vec![0; 4]));
    }
    #[test] fn watchdog_strikes_reset_on_resettle() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
        let mut e = r.engine();
        r.set(80, true, true, 1200, Some(1030)); e.handle(Event::Tick);  // first strike
        sleep(&mut e);
        r.clock.set(2000);
        r.set(80, true, true, 1200, Some(2010)); e.handle(Event::DidWake); // wake restarts the window
        r.set(80, true, true, 1200, Some(2030)); e.handle(Event::Tick);  // one strike after the new settle
        assert_eq!(e.status().health, Health::Ok);
        assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
    }
    #[test] fn watchdog_accepts_effective_discharge() {
        let r = Rig::hold(); r.clock.set(1000); r.set(81, true, true, 2700, Some(990));
        let mut e = r.engine();
        r.set(81, false, true, -410, Some(1030)); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
    }
    #[test] fn watchdog_waits_on_missing_update_time() {
        let r = Rig::hold(); r.clock.set(1000); r.set(81, true, true, 2700, None);
        let mut e = r.engine(); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
    }
    #[test] fn watchdog_judges_after_replug() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
        let mut e = r.engine();                                       // writes CHTE at t=1000
        r.set(80, false, false, -300, Some(1030)); e.handle(Event::Tick); // unplugged: not judged
        r.clock.set(1040);
        r.set(80, true, true, 1200, Some(1050)); e.handle(Event::Tick);  // replugged at 1040: too fresh
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1070)); e.handle(Event::Tick);  // first strike
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1130)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHTE")));
    }
    #[test] fn watchdog_keeps_checking_after_first_verdict() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, false, true, 0, Some(990));
        let mut e = r.engine();
        r.set(80, false, true, 0, Some(1030)); e.handle(Event::Tick);
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1090)); e.handle(Event::Tick); // first strike
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(1150)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHTE")));
    }
    #[test] fn watchdog_skips_sleep_and_resettles_after_wake() {
        let r = Rig::hold(); r.clock.set(1000); r.set(80, false, true, 0, Some(990));
        let mut e = r.engine();
        sleep(&mut e);
        r.set(80, true, true, 1200, Some(1100)); e.handle(Event::Tick); // asleep: not judged
        assert_eq!(e.status().health, Health::Ok);
        r.clock.set(2000);
        r.set(80, true, true, 1200, Some(2010)); e.handle(Event::DidWake); // settling after wake
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(2030)); e.handle(Event::Tick); // first strike
        assert_eq!(e.status().health, Health::Ok);
        r.set(80, true, true, 1200, Some(2090)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { .. }));
    }
    #[test] fn discharge_does_not_flap_when_macos_reports_unplugged() {
        let r = Rig::hold().hiding_ac(); r.set(100, false, true, -400, Some(900));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        let n = r.smc.writes().len();
        for _ in 0..3 { e.handle(Event::Tick); e.handle(Event::PowerChanged); }
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        assert!(!r.smc.writes()[n..].iter().any(|(k, _)| k == "CHIE"), "{:?}", &r.smc.writes()[n..]);
        let st = e.status();
        assert_eq!((st.activity, st.external, st.health), (Activity::Discharging { to: 80 }, true, Health::Ok));
    }
    #[test] fn discharge_still_stops_at_target() {
        // Hold: stops at the upper limit.
        let r = Rig::hold().hiding_ac(); r.set(100, false, true, -400, Some(900));
        let mut e = r.engine();
        e.handle(Event::Tick);
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.set(80, false, true, -400, Some(950)); e.handle(Event::Tick);
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
        assert_eq!(e.status().activity, Activity::Holding);
        r.set(80, false, false, -400, Some(1010)); e.handle(Event::Tick); // a real unplug, read after the stop at t=1000
        assert_eq!((e.status().activity, e.status().external), (Activity::OnBattery, false));
    }
    #[test] fn chie_induced_external_flip_is_not_a_replug() {
        let r = Rig::hold().hiding_ac(); r.clock.set(1000); r.set(100, false, true, -400, Some(990));
        let mut e = r.engine();                                          // CHIE=08 at t=1000
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.clock.set(2000);
        r.set(80, false, true, -400, Some(1990)); e.handle(Event::Tick); // at the limit: CHIE=00 at t=2000
        assert_eq!((r.smc.value("CHIE"), e.since), (Some(vec![0]), Some(2000)));
        // macOS reports the adapter again now that CHIE is off: not a replug.
        r.clock.set(2100);
        r.set(80, true, true, 1200, Some(2050)); e.handle(Event::Tick); // failing CHTE reading: first strike
        assert_eq!((e.since, e.strikes), (Some(2000), 1));
        assert_eq!(e.status().health, Health::Ok);
    }
    /// Hiding hold rig: CHIE=08 at t=1000, then CHIE=00 at t=2000 on reaching the limit.
    fn stopped_discharge_rig() -> (Rig, TestEngine) {
        let r = Rig::hold().hiding_ac(); r.clock.set(1000); r.set(100, false, true, -400, Some(990));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.clock.set(2000);
        r.set(80, false, true, -400, Some(1990)); e.handle(Event::Tick);
        assert_eq!((r.smc.value("CHIE"), e.since), (Some(vec![0]), Some(2000)));
        (r, e)
    }
    #[test] fn lagging_unplugged_reading_after_discharge_is_not_trusted() {
        let (r, mut e) = stopped_discharge_rig();
        // macOS still reports the adapter missing, on a reading from before the stop.
        r.clock.set(2010);
        r.set(80, false, false, 0, Some(1990)); e.handle(Event::Tick);
        let st = e.status();
        assert_eq!((st.external, st.activity), (true, Activity::Holding));
        // The adapter shows up again: not a replug, the settle window keeps running.
        r.clock.set(2100);
        r.set(80, false, true, 0, Some(2050)); e.handle(Event::Tick);
        assert_eq!((e.since, e.status().external, e.status().activity), (Some(2000), true, Activity::Holding));
    }
    #[test] fn fresh_unplugged_reading_after_discharge_is_honoured() {
        let (r, mut e) = stopped_discharge_rig();
        r.clock.set(2010);
        r.set(80, false, false, 0, Some(1990)); e.handle(Event::Tick); // lagging: distrusted
        assert!(e.status().external);
        r.clock.set(2070);
        r.set(80, false, false, -300, Some(2050)); e.handle(Event::Tick); // newer than the stop
        assert_eq!((e.status().external, e.status().activity), (false, Activity::OnBattery));
    }
    #[test] fn unplugged_reading_without_update_time_is_trusted_after_a_tick() {
        let (r, mut e) = stopped_discharge_rig();
        r.clock.set(2010);
        r.set(80, false, false, 0, None); e.handle(Event::Tick);
        assert!(e.status().external);
        r.clock.set(2000 + TICK_SECS);
        e.handle(Event::Tick);
        assert_eq!((e.status().external, e.status().activity), (false, Activity::OnBattery));
    }
    #[test] fn frozen_update_time_does_not_distrust_forever() {
        let (r, mut e) = stopped_discharge_rig();
        // update_time never moves past the stop: still distrusted just before the cap...
        r.clock.set(2000 + 3 * TICK_SECS - 1);
        r.set(80, false, false, 0, Some(1990)); e.handle(Event::Tick);
        assert!(e.status().external);
        // ...and trusted from three ticks after the stop on.
        r.clock.set(2000 + 3 * TICK_SECS);
        e.handle(Event::Tick);
        assert_eq!((e.status().external, e.status().activity), (false, Activity::OnBattery));
        assert_eq!(e.discharge_stopped, None);
    }
    #[test] fn lagging_unplugged_reading_after_startup_restore_is_not_trusted() {
        // A crash left CHIE=08; the startup restore clears it at t=1000, but macOS still
        // reports the adapter missing on a reading from before the restore.
        let r = Rig::hold().hiding_ac(); r.smc.insert("CHIE", "ui8 ", &[8], WriteMode::Accept);
        r.clock.set(1000); r.set(78, false, false, 0, Some(990));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
        assert!(e.status().external);
        assert_ne!(e.status().activity, Activity::OnBattery);
        r.clock.set(1070);
        r.set(78, false, false, -300, Some(1050)); e.handle(Event::Tick); // newer than the restore
        assert_eq!((e.status().external, e.status().activity), (false, Activity::OnBattery));
    }
    #[test] fn unplugged_start_without_stale_discharge_is_trusted() {
        let r = Rig::hold().hiding_ac(); r.clock.set(1000); r.set(78, false, false, 0, Some(990));
        let e = r.engine();
        assert_eq!((e.status().external, e.status().activity), (false, Activity::OnBattery));
    }
    /// Hiding hold rig at 100% on the charger: CHIE=08 written at t=1000.
    fn hidden_discharge_rig(amp: i32) -> (Rig, TestEngine) {
        let r = Rig::hold().hiding_ac(); r.clock.set(1000); r.set(100, false, true, amp, Some(990));
        let e = r.engine();
        assert_eq!((r.smc.value("CHIE"), e.prev.discharge), (Some(vec![8]), true));
        (r, e)
    }
    #[test] fn watchdog_flags_ineffective_discharge_under_hiding() {
        let (r, mut e) = hidden_discharge_rig(0);
        r.set(100, false, true, 0, Some(1030)); e.handle(Event::Tick); // first strike
        assert_eq!((e.status().health, e.strikes), (Health::Ok, 1));
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.set(100, false, true, 0, Some(1090)); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHIE")));
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
    }
    #[test] fn conflict_that_cleared_our_chie_bit_returns_trust() {
        let (r, mut e) = hidden_discharge_rig(-400);
        r.smc.insert("CHIE", "ui8 ", &[0x10], WriteMode::Accept); // another writer: our bit is gone
        let n = r.smc.writes().len();
        r.clock.set(1100); e.handle(Event::Tick);
        assert!(matches!(e.status().health, Health::Conflict { ref key, .. } if key == "CHIE"));
        assert_eq!((r.smc.writes().len(), e.prev.discharge), (n, false));
        r.clock.set(1200); r.set(100, false, false, -400, Some(1150)); e.handle(Event::Tick); // a real unplug
        assert!(!e.status().external);
    }
    #[test] fn sleep_while_discharging_under_hiding_returns_trust() {
        let (r, mut e) = hidden_discharge_rig(-400);
        r.clock.set(1100); r.set(100, false, true, -400, Some(1090)); sleep(&mut e);
        assert_eq!((r.smc.value("CHIE"), r.smc.value("CHTE")), (Some(vec![0]), Some(vec![1, 0, 0, 0])));
        assert_eq!((e.prev.discharge, e.status().external), (false, true));
        r.clock.set(1200); r.set(100, false, false, -400, Some(1150)); e.handle(Event::Tick); // a real unplug
        assert_eq!((e.status().external, e.status().activity), (false, Activity::OnBattery));
    }
    #[test] fn real_unplug_mid_discharge_under_hiding_continues_to_target() {
        let (r, mut e) = hidden_discharge_rig(-400);
        // The user unplugs; with CHIE=08 that looks the same as before, so keep going.
        r.clock.set(2000); r.set(90, false, false, -600, Some(1990)); e.handle(Event::Tick);
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        assert_eq!((e.status().activity, e.status().external), (Activity::Discharging { to: 80 }, true));
        r.clock.set(3000); r.set(80, false, false, -600, Some(2990)); e.handle(Event::Tick);
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
        r.clock.set(3100); r.set(80, false, false, -600, Some(3050)); e.handle(Event::Tick);
        assert_eq!((e.status().activity, e.status().external), (Activity::OnBattery, false));
    }
    fn req(e: &mut TestEngine, uid: u32, q: Request) -> Response {
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
    #[test] fn charge_full_needs_the_charger() {
        let r = Rig::hold(); r.set(70, false, false, -400, Some(900)); let mut e = r.engine();
        assert_eq!(req(&mut e, 501, Request::ChargeFull), Response::Error("plug in first: a full charge needs the charger".into()));
        assert_eq!(e.status().override_, Override::None);
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
        let r = Rig::native(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
        let Response::Doctor(d) = req(&mut e, 502, Request::Doctor) else { panic!() };
        assert!(d.keys.contains(&("CHTE".into(), KeyState::Missing)));
    }

    // Added beyond the brief (Task 7).

    #[test] fn watchdog_flags_ineffective_discharge() {
        let r = Rig::hold(); r.clock.set(1000); r.set(81, true, true, 2700, Some(990));
        let mut e = r.engine();
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.set(81, true, true, 500, Some(1020)); e.handle(Event::Tick); // first strike
        assert_eq!(e.status().health, Health::Ok);
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        r.set(81, true, true, 500, Some(1080)); e.handle(Event::Tick);
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

    // Native mode (macOS's own Charge Limit).

    #[test] fn native_falls_back_to_unsupported_without_mcl() {
        let r = Rig::native_with(FakeNative::unsupported()); r.set(80, false, true, 0, Some(900));
        assert_eq!(r.engine().status().health, Health::Unsupported);
    }
    #[test] fn native_startup_records_the_original_and_sets_the_limit() {
        let r = Rig::native(); r.set(85, false, true, -700, Some(900));
        let e = r.engine();
        assert_eq!(e.status().mode, ControlMode::Native);
        assert_eq!((r.native.limit_now(), r.native.writes()), (80, vec![80]));
        assert_eq!(r.store.load().0.native_original, Some(100));
        assert_eq!(e.status().activity, Activity::Discharging { to: 80 });
        assert_eq!(r.smc.value("ACLC"), Some(vec![0])); // never written
        assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
    }
    #[test] fn native_startup_reapplies_the_configured_limit() {
        let r = Rig::native(); r.set(90, true, true, 1000, Some(900));
        r.native.set_externally(100); // e.g. left at 100 by a crash during `charge full`
        r.write_config("[settings]\nupper = 85\n");
        r.engine();
        assert_eq!(r.native.limit_now(), 85);
    }
    #[test] fn native_charge_full_lifts_the_limit_until_unplug() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        e.request(0, Request::ChargeFull);
        assert_eq!(r.native.limit_now(), 100);
        r.set(80, false, false, -500, Some(1_000)); e.handle(Event::Tick);
        assert_eq!((r.native.limit_now(), e.status().override_), (80, Override::None));
    }
    #[test] fn native_set_limit_failure_is_loud() {
        let r = Rig::native(); r.native.fail_calls(true); r.set(80, false, true, 0, Some(900));
        let st = r.engine().status();
        assert!(matches!(st.health, Health::WriteFailed { ref key, .. } if key == "MCL"), "{:?}", st.health);
    }
    #[test] fn native_old_limit_outside_the_steps_uses_the_next_step_up() {
        let r = Rig::native(); r.write_config("[settings]\nupper = 70\nlower = 65\n"); r.set(75, true, true, 900, Some(900));
        let e = r.engine();
        assert_eq!(r.native.limit_now(), 80);
        assert!(e.status().warnings.iter().any(|w| w == "limit 70% isn't available on this Mac; using 80%"));
    }
    #[test] fn native_set_limit_failure_mid_run_restores_and_pauses() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        r.native.fail_calls(true);
        e.request(0, Request::ChargeFull);
        let st = e.status();
        assert!(matches!(st.health, Health::WriteFailed { ref key, .. } if key == "MCL"), "{:?}", st.health);
        assert_eq!((st.activity, r.native.limit_now(), r.smc.value("CHIE")), (Activity::Paused, 80, Some(vec![0])));
        assert!(!r.smc.writes().iter().any(|(k, _)| k == "ACLC" || k == "CHTE"), "{:?}", r.smc.writes());
    }
    #[test] fn native_adopts_a_limit_changed_in_system_settings() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        r.native.set_externally(90); r.clock.set(1_060); e.handle(Event::Tick);
        assert_eq!((e.status().settings.upper, r.store.load().0.settings.upper), (90, 90));
        assert_eq!(e.status().health, Health::Ok);
        assert_eq!(r.native.writes(), vec![80]); // adopted, not overwritten
    }
    #[test] fn native_adoption_keeps_a_hold_era_config_loadable() {
        let r = Rig::native(); r.set(85, false, true, 0, Some(900));
        r.write_config("[settings]\nupper = 90\nlower = 85\n"); // left from Hold mode
        let mut e = r.engine();
        assert_eq!(r.native.limit_now(), 90);
        r.native.set_externally(80); r.clock.set(1_060); e.handle(Event::Tick);
        let (c, warning) = r.store.load();
        assert_eq!(warning, None);
        assert_eq!((c.settings.upper, c.settings.lower, c.native_original), (80, 80, Some(100)));
        assert_eq!(r.native.writes(), vec![90]);
    }
    #[test] fn native_set_limits_below_a_hold_era_lower_keeps_the_config_loadable() {
        let r = Rig::native(); r.set(85, false, true, 0, Some(900));
        r.write_config("[settings]\nupper = 90\nlower = 85\n");
        let mut e = r.engine();
        assert!(matches!(e.request(0, Request::SetLimits { upper: 80, lower: None }), Response::Status(_)));
        let (c, warning) = r.store.load();
        assert_eq!((warning, c.settings.upper, c.settings.lower, c.native_original), (None, 80, 80, Some(100)));
    }
    #[test] fn native_charging_past_the_limit_warns_without_faulting() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        r.clock.set(1_100); r.set(83, true, true, 2000, Some(1_090)); e.handle(Event::Tick);
        let st = e.status();
        assert_eq!(st.health, Health::Ok);
        assert!(st.warnings.iter().any(|w| w.starts_with("macOS is charging past 80%")), "{:?}", st.warnings);
        assert_eq!(st.activity, Activity::Charging { to: 100 });
        r.clock.set(1_200); r.set(83, false, true, -600, Some(1_190)); e.handle(Event::Tick);
        assert!(e.status().warnings.is_empty());
    }
    #[test] fn native_limit_requests_follow_macos_steps() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        assert!(matches!(e.request(0, Request::SetLimits { upper: 70, lower: None }), Response::Error(m) if m.starts_with("this Mac only supports")));
        assert!(matches!(e.request(0, Request::SetLimits { upper: 85, lower: None }), Response::Status(_)));
        assert_eq!(r.native.limit_now(), 85);
        assert!(matches!(e.request(0, Request::SetAutoDischarge { on: false }), Response::Error(_)));
    }
    #[test] fn native_doctor_reports_the_native_limit() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let Response::Doctor(d) = r.engine().request(0, Request::Doctor) else { panic!() };
        assert_eq!(d.native, Some(NativeState { available: vec![80, 85, 90, 95, 100], limit: Some(80), enabled: Some(true) }));
    }

    // Added beyond the brief (Task 4).

    /// Native rig whose `CHIE` rejects writes, so detection marks it unusable.
    fn native_blocked_chie() -> Rig {
        let r = Rig::native(); r.smc.insert("CHIE", "ui8 ", &[0], WriteMode::NotPrivileged); r
    }
    #[test] fn native_discharge_needs_a_usable_chie() {
        let r = native_blocked_chie(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        assert_eq!(e.status().mode, ControlMode::Native);
        assert_eq!(e.request(0, Request::DischargeTo { percent: 50 }),
            Response::Error("this Mac can't run from the battery while plugged in, so `discharge` isn't available".into()));
        assert_eq!(e.status().override_, Override::None);
    }
    #[test] fn native_restore_skips_an_unusable_chie() {
        let r = native_blocked_chie(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        let st = e.status();
        assert_eq!(st.health, Health::Ok);
        assert!(st.warnings.is_empty(), "{:?}", st.warnings);
        e.handle(Event::Shutdown);
        assert!(r.smc.writes().is_empty(), "{:?}", r.smc.writes());
        assert!(e.status().warnings.is_empty(), "{:?}", e.status().warnings);
    }
    #[test] fn native_original_is_kept_in_memory_over_an_unreadable_config() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        r.write_config("not toml [[[");
        let mut e = r.engine();
        assert_eq!(std::fs::read_to_string(r._dir.path().join("config.toml")).unwrap(), "not toml [[[");
        assert_eq!(e.config.native_original, Some(100));
        assert!(matches!(e.request(0, Request::SetLimits { upper: 85, lower: None }), Response::Status(_)));
        assert_eq!(r.store.load().0.native_original, Some(100));
    }
    #[test] fn native_adoption_waits_for_charge_full_to_end() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine();
        e.request(0, Request::ChargeFull);
        assert_eq!(r.native.limit_now(), 100);
        r.native.set_externally(90); e.handle(Event::Tick);
        assert_eq!((e.status().settings.upper, r.native.writes()), (80, vec![80, 100]));
    }
    #[test] fn native_watchdog_waits_for_a_fresh_reading_after_a_write() {
        let r = Rig::native(); r.set(80, false, true, 0, Some(900));
        let mut e = r.engine(); // writes 80 at t=1000
        r.clock.set(1_010); r.set(83, true, true, 2000, Some(1_010)); e.handle(Event::Tick);
        assert!(e.status().warnings.is_empty(), "{:?}", e.status().warnings);
        assert_eq!(e.status().activity, Activity::Discharging { to: 80 });
    }
    #[test] fn native_mode_never_runs_the_smc_watchdog() {
        let r = Rig::native(); r.set(78, false, true, 0, Some(900));
        let mut e = r.engine();
        e.request(0, Request::DischargeTo { percent: 50 });
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
        // Readings that would be CHIE strikes in Hold mode.
        for t in [1_030, 1_090, 1_150] { r.clock.set(t); r.set(78, false, true, 0, Some(t)); e.handle(Event::Tick); }
        assert_eq!(e.status().health, Health::Ok);
        assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
    }
}
