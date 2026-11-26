//! Overheat protection: a pure latch plus a decision layer applied after `policy::decide`. No I/O.
use crate::policy::{ControlMode, Decision, Light};
use serde::{Deserialize, Serialize};

/// Native mode: a pause ends at or below this charge percentage.
pub const HEAT_FLOOR: u8 = 20;
/// After the floor ends a pause, no new pause starts until charge reaches this percentage.
pub const HEAT_REARM: u8 = 30;
/// Whether a heat pause may force discharge (CHIE) in native mode with the lid closed.
/// Set by the owner's lid-closed CHIE soak check (docs/testing/plan4-acceptance.md,
/// "Lid-closed CHIE check"). `false`: on the M5 Pro / macOS 27.2.1 (2026-11-26) a real heat
/// pause while charging made macOS report the adapter gone (`ExternalConnected = No`) and put
/// the lid-closed Mac into 'Clamshell Sleep' within ~20 s, on every wake.
pub const HEAT_PAUSE_LID_CLOSED: bool = false;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct OverheatConfig {
    pub enabled: bool,
    pub pause_c: u8,
    pub resume_c: u8,
    /// Hidden: widens the allowed threshold ranges so the feature can be tried without a hot battery.
    pub testing: bool,
}

impl Default for OverheatConfig {
    fn default() -> OverheatConfig {
        OverheatConfig { enabled: true, pause_c: 35, resume_c: 32, testing: false }
    }
}

impl OverheatConfig {
    pub fn validate(&self) -> Result<(), String> {
        let (pause_min, resume_min) = if self.testing { (20, 15) } else { (30, 25) };
        if !(pause_min..=45).contains(&self.pause_c) {
            return Err(format!("pause temperature must be between {pause_min} and 45 °C (got {})", self.pause_c));
        }
        let resume_max = self.pause_c - 2;
        if self.resume_c < resume_min || self.resume_c > resume_max {
            return Err(format!(
                "resume temperature must be between {resume_min} and {resume_max} °C, 2 below the pause temperature (got {})",
                self.resume_c
            ));
        }
        Ok(())
    }

    /// Returns a copy with new thresholds; `resume_c` defaults to `pause_c - 3`.
    pub fn with_thresholds(self, pause_c: u8, resume_c: Option<u8>, testing: bool) -> Result<OverheatConfig, String> {
        let c = OverheatConfig { pause_c, resume_c: resume_c.unwrap_or_else(|| pause_c.saturating_sub(3)), testing, ..self };
        c.validate()?;
        Ok(c)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HeatState {
    pub paused: bool,
    /// The native floor ended a pause; no new pause until charge recovers or the battery cools.
    pub floor_hit: bool,
}

pub struct HeatInputs {
    pub temperature_c: Option<f32>,
    pub percent: u8,
    pub external: bool,
    pub charging: bool,
    pub mode: ControlMode,
}

/// Advances the heat latch by one reading.
pub fn heat_step(cfg: &OverheatConfig, prev: HeatState, i: &HeatInputs) -> HeatState {
    let t = match i.temperature_c {
        Some(t) if cfg.enabled && t > f32::from(cfg.resume_c) => t,
        _ => return HeatState::default(),
    };
    let mut prev = prev;
    if prev.floor_hit {
        if i.percent < HEAT_REARM {
            return prev;
        }
        prev.floor_hit = false;
    }
    if prev.paused && i.mode == ControlMode::Native && i.percent <= HEAT_FLOOR {
        return HeatState { paused: false, floor_hit: true };
    }
    if prev.paused {
        return prev;
    }
    if t >= f32::from(cfg.pause_c) && i.external && i.charging {
        // Native mode: a pause at or below the floor would only drain an already low battery.
        if i.mode == ControlMode::Native && i.percent <= HEAT_FLOOR {
            return HeatState { paused: false, floor_hit: true };
        }
        return HeatState { paused: true, floor_hit: false };
    }
    HeatState::default()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeatNote {
    Paused,
    LidClosed,
    Unable,
    ChargingAtFloor,
}

/// What a heat pause can act on this tick.
#[derive(Clone, Copy, Debug)]
pub struct HeatEnv {
    pub mode: ControlMode,
    pub external: bool,
    pub lid_closed: bool,
    /// Whether `CHIE` is usable (native mode's only way to stop charging).
    pub can_discharge: bool,
    /// Between sleep and wake: native mode never forces `CHIE` then.
    pub sleeping: bool,
    pub percent: u8,
}

/// Whether a heat pause actually stops charging now. When it doesn't (lid closed,
/// no `CHIE`, asleep, at the floor), the battery charges as usual, so anything waiting
/// on charging (the calibration clocks) must keep counting.
pub fn pause_in_effect(h: HeatState, env: &HeatEnv) -> bool {
    if !h.paused || !env.external {
        return false;
    }
    match env.mode {
        ControlMode::Hold => true,
        ControlMode::Native => {
            !env.sleeping
                && env.percent > HEAT_FLOOR
                && (HEAT_PAUSE_LID_CLOSED || !env.lid_closed)
                && env.can_discharge
        }
        ControlMode::Unsupported => false,
    }
}

/// Layers the heat pause onto a decision. Returns a note when heat changed or blocked the outcome.
pub fn apply_heat(mut d: Decision, h: HeatState, env: &HeatEnv) -> (Decision, Option<HeatNote>) {
    if !env.external || env.mode == ControlMode::Unsupported {
        return (d, None);
    }
    if !h.paused {
        let note = h.floor_hit.then_some(HeatNote::ChargingAtFloor);
        return (d, note);
    }
    if d.outputs.discharge {
        return (d, None);
    }
    if env.mode == ControlMode::Hold {
        d.outputs.inhibit = true;
        d.outputs.light = Some(Light::Green);
        return (d, Some(HeatNote::Paused));
    }
    if env.sleeping {
        // The sleep rule keeps `CHIE` off, so the pause does nothing until wake.
        return (d, None);
    }
    if env.percent <= HEAT_FLOOR {
        // `heat_step` already ends a native pause here; never drain below the floor.
        return (d, Some(HeatNote::ChargingAtFloor));
    }
    if env.lid_closed && !HEAT_PAUSE_LID_CLOSED {
        return (d, Some(HeatNote::LidClosed));
    }
    if !env.can_discharge {
        return (d, Some(HeatNote::Unable));
    }
    d.outputs.discharge = true;
    d.outputs.inhibit = false;
    d.outputs.light = None;
    (d, Some(HeatNote::Paused))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Activity, ControlMode::*, Light::*, Outputs};
    use crate::Config;

    fn cfg() -> OverheatConfig { OverheatConfig::default() }
    fn hi(t: Option<f32>, percent: u8, external: bool, charging: bool, mode: ControlMode) -> HeatInputs {
        HeatInputs { temperature_c: t, percent, external, charging, mode }
    }
    const P: HeatState = HeatState { paused: true, floor_hit: false };
    fn env(mode: ControlMode, external: bool, lid_closed: bool, can_discharge: bool) -> HeatEnv {
        HeatEnv { mode, external, lid_closed, can_discharge, sleeping: false, percent: 60 }
    }
    fn dec(inhibit: bool, discharge: bool, light: Option<Light>, native_limit: Option<u8>) -> Decision {
        Decision { outputs: Outputs { inhibit, discharge, light }, activity: Activity::Charging { to: 80 }, clear_override: false, native_limit }
    }

    #[test] fn defaults() { assert_eq!(OverheatConfig::default(), OverheatConfig { enabled: true, pause_c: 35, resume_c: 32, testing: false }); }
    #[test] fn thresholds_validate() {
        let d = OverheatConfig::default();
        assert_eq!(d.with_thresholds(38, None, false).unwrap().resume_c, 35);
        assert!(d.with_thresholds(29, None, false).is_err());
        assert!(d.with_thresholds(46, None, false).is_err());
        assert!(d.with_thresholds(35, Some(34), false).is_err());
        assert!(d.with_thresholds(30, Some(24), false).is_err());
        assert_eq!(d.with_thresholds(22, Some(20), true).unwrap(), OverheatConfig { enabled: true, pause_c: 22, resume_c: 20, testing: true });
        assert!(d.with_thresholds(19, None, true).is_err());
        assert!(!d.with_thresholds(36, None, false).unwrap().testing);
    }
    #[test] fn old_config_without_overheat_loads_defaults() { assert_eq!(toml::from_str::<Config>("[settings]\nupper = 80\n").unwrap().overheat, OverheatConfig::default()); }
    #[test] fn enters_at_pause_when_charging_on_ac() { assert!(heat_step(&cfg(), HeatState::default(), &hi(Some(35.0), 60, true, true, Native)).paused); }
    #[test] fn no_pause_when_not_charging() { assert!(!heat_step(&cfg(), HeatState::default(), &hi(Some(40.0), 80, true, false, Native)).paused); }
    #[test] fn no_pause_on_battery() { assert!(!heat_step(&cfg(), HeatState::default(), &hi(Some(40.0), 60, false, true, Native)).paused); }
    #[test] fn stays_paused_between_thresholds_even_not_charging() {
        assert!(heat_step(&cfg(), P, &hi(Some(33.0), 60, true, false, Native)).paused);
        assert!(!heat_step(&cfg(), P, &hi(Some(32.0), 60, true, false, Native)).paused);
    }
    #[test] fn missing_reading_clears() { assert_eq!(heat_step(&cfg(), HeatState { paused: true, floor_hit: true }, &hi(None, 60, true, true, Native)), HeatState::default()); }
    #[test] fn disabled_clears() { let c = OverheatConfig { enabled: false, ..cfg() }; assert_eq!(heat_step(&c, P, &hi(Some(40.0), 60, true, true, Native)), HeatState::default()); }
    #[test] fn floor_ends_pause_and_rearms_at_30() {
        let s = heat_step(&cfg(), P, &hi(Some(37.0), 20, true, false, Native));
        assert_eq!(s, HeatState { paused: false, floor_hit: true });
        assert_eq!(heat_step(&cfg(), s, &hi(Some(37.0), 29, true, true, Native)), s);
        assert_eq!(heat_step(&cfg(), s, &hi(Some(37.0), 30, true, true, Native)), HeatState { paused: true, floor_hit: false });
        assert_eq!(heat_step(&cfg(), s, &hi(Some(32.0), 25, true, true, Native)), HeatState::default());
    }
    #[test] fn native_never_enters_pause_at_or_below_floor() {
        assert_eq!(heat_step(&cfg(), HeatState::default(), &hi(Some(37.0), 15, true, true, Native)), HeatState { paused: false, floor_hit: true });
        assert_eq!(heat_step(&cfg(), HeatState::default(), &hi(Some(37.0), 20, true, true, Native)), HeatState { paused: false, floor_hit: true });
        assert!(heat_step(&cfg(), HeatState::default(), &hi(Some(37.0), 21, true, true, Native)).paused);
        assert!(heat_step(&cfg(), HeatState::default(), &hi(Some(37.0), 15, true, true, Hold)).paused);
    }
    #[test] fn hold_mode_has_no_floor() { assert!(heat_step(&cfg(), P, &hi(Some(37.0), 15, true, false, Hold)).paused); }

    #[test] fn hold_pause_inhibits_green() { let (d, n) = apply_heat(dec(false, false, Some(Orange), None), P, &env(Hold, true, false, true)); assert_eq!(d.outputs, Outputs { inhibit: true, discharge: false, light: Some(Green) }); assert_eq!(n, Some(HeatNote::Paused)); }
    #[test] fn native_pause_discharges_keeps_limit() { let (d, n) = apply_heat(dec(false, false, None, Some(100)), P, &env(Native, true, false, true)); assert_eq!(d.outputs, Outputs { inhibit: false, discharge: true, light: None }); assert_eq!(d.native_limit, Some(100)); assert_eq!(n, Some(HeatNote::Paused)); }
    #[test]
    fn native_lid_closed_follows_constant() {
        let (d, n) = apply_heat(dec(false, false, None, Some(80)), P, &env(Native, true, true, true));
        if HEAT_PAUSE_LID_CLOSED { assert!(d.outputs.discharge); assert_eq!(n, Some(HeatNote::Paused)); } else { assert!(!d.outputs.discharge); assert_eq!(n, Some(HeatNote::LidClosed)); }
    }
    #[test] fn native_without_chie_is_unable() { let (d, n) = apply_heat(dec(false, false, None, Some(80)), P, &env(Native, true, false, false)); assert!(!d.outputs.discharge); assert_eq!(n, Some(HeatNote::Unable)); }
    #[test] fn already_discharging_unchanged() { let d0 = dec(true, true, Some(Green), None); let (d, n) = apply_heat(d0.clone(), P, &env(Hold, true, false, true)); assert_eq!((d.outputs, n), (d0.outputs, None)); }
    #[test] fn on_battery_or_unsupported_unchanged() { assert_eq!(apply_heat(dec(false, false, None, Some(80)), P, &env(Native, false, false, true)).1, None); assert_eq!(apply_heat(dec(false, false, None, None), P, &env(Unsupported, true, false, true)).1, None); }
    #[test] fn floor_hit_reports_charging_at_floor() { let (d, n) = apply_heat(dec(false, false, None, Some(80)), HeatState { paused: false, floor_hit: true }, &env(Native, true, false, true)); assert!(!d.outputs.discharge); assert_eq!(n, Some(HeatNote::ChargingAtFloor)); }
    #[test] fn native_never_discharges_at_or_below_floor() {
        let e = HeatEnv { percent: HEAT_FLOOR, ..env(Native, true, false, true) };
        let (d, n) = apply_heat(dec(false, false, None, Some(80)), P, &e);
        assert!(!d.outputs.discharge);
        assert_eq!(n, Some(HeatNote::ChargingAtFloor));
        let e = HeatEnv { percent: HEAT_FLOOR + 1, ..e };
        assert!(apply_heat(dec(false, false, None, Some(80)), P, &e).0.outputs.discharge);
    }
    #[test] fn native_asleep_no_discharge_no_note() {
        let e = HeatEnv { sleeping: true, ..env(Native, true, false, true) };
        let (d, n) = apply_heat(dec(false, false, None, Some(80)), P, &e);
        assert!(!d.outputs.discharge);
        assert_eq!(n, None);
        let e = HeatEnv { sleeping: true, ..env(Hold, true, false, true) };
        assert_eq!(apply_heat(dec(false, false, None, None), P, &e).1, Some(HeatNote::Paused));
    }
    #[test] fn pause_in_effect_cases() {
        let n = env(Native, true, false, true);
        assert!(pause_in_effect(P, &n));
        assert!(!pause_in_effect(HeatState::default(), &n));
        assert!(!pause_in_effect(HeatState { paused: false, floor_hit: true }, &n));
        assert!(!pause_in_effect(P, &HeatEnv { external: false, ..n }));
        assert_eq!(pause_in_effect(P, &HeatEnv { lid_closed: true, ..n }), HEAT_PAUSE_LID_CLOSED);
        assert!(!pause_in_effect(P, &HeatEnv { can_discharge: false, ..n }));
        assert!(!pause_in_effect(P, &HeatEnv { sleeping: true, ..n }));
        assert!(!pause_in_effect(P, &HeatEnv { percent: HEAT_FLOOR, ..n }));
        assert!(pause_in_effect(P, &HeatEnv { percent: HEAT_FLOOR + 1, ..n }));
        let h = env(Hold, true, true, false);
        assert!(pause_in_effect(P, &HeatEnv { sleeping: true, percent: 5, ..h }));
        assert!(!pause_in_effect(P, &HeatEnv { external: false, ..h }));
        assert!(!pause_in_effect(P, &env(Unsupported, true, false, true)));
    }
    /// The predicate matches what `apply_heat` does: a `Paused` note exactly when it holds.
    #[test] fn pause_in_effect_matches_apply_heat() {
        for mode in [Hold, Native, Unsupported] {
            for bits in 0..32u8 {
                for percent in [HEAT_FLOOR, HEAT_FLOOR + 1] {
                    let b = |k: u8| bits & (1 << k) != 0;
                    let e = HeatEnv { mode, external: b(0), lid_closed: b(1), can_discharge: b(2), sleeping: b(3), percent };
                    let h = HeatState { paused: b(4), floor_hit: false };
                    let (_, note) = apply_heat(dec(false, false, None, Some(80)), h, &e);
                    assert_eq!(pause_in_effect(h, &e), note == Some(HeatNote::Paused), "{e:?} {h:?}");
                }
            }
        }
    }
}
