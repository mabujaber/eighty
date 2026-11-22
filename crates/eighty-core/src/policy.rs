//! Pure charge policy: given settings, override, readings and the previous
//! outputs, decide what the helper should write this tick. No I/O.
use crate::calibration::{CalStep, CAL_LOW};
use crate::{Settings, HARD_FLOOR};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlMode {
    Hold,
    Native,
    Unsupported,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Green,
    Orange,
    System,
}

/// Desired control state. `light: None` means don't touch the light.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Outputs {
    pub inhibit: bool,
    pub discharge: bool,
    pub light: Option<Light>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Override {
    #[default]
    None,
    FullCharge,
    DischargeTo(u8),
    /// A calibration run is in `step`; set by the helper engine, never cleared by `decide`.
    Calibrate(CalStep),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Charging { to: u8 },
    Holding,
    Discharging { to: u8 },
    Calibrating { step: CalStep },
    OnBattery,
    LidClosed,
    Paused,
}

pub struct Inputs {
    pub percent: u8,
    pub external: bool,
    pub lid_closed: bool,
    pub mode: ControlMode,
    /// Lowest native charge-limit step below 100 (callers pass 80 when unknown).
    pub native_lowest: u8,
}

pub struct Decision {
    pub outputs: Outputs,
    pub activity: Activity,
    pub clear_override: bool,
    /// The macOS Charge Limit to ask for. `Some` only in Native mode.
    pub native_limit: Option<u8>,
}

fn light_for(external: bool, inhibit: bool, discharge: bool) -> Option<Light> {
    if !external {
        None
    } else if inhibit || discharge {
        Some(Light::Green)
    } else {
        Some(Light::Orange)
    }
}

/// Outputs for "discharge towards `n`" (shared by `DischargeTo` and calibration's discharge step).
/// Returns (inhibit, discharge, light, activity); the light is already `None` in Native mode.
fn discharge_step(s: &Settings, i: &Inputs, n: u8) -> (bool, bool, Option<Light>, Activity) {
    let hold = i.mode == ControlMode::Hold;
    let native = i.mode == ControlMode::Native;
    let (inhibit, discharge, light, activity) = if i.lid_closed {
        // "Lid closed: holding" only makes sense on the charger.
        let activity = if i.external { Activity::LidClosed } else { Activity::OnBattery };
        (hold, false, light_for(i.external, hold, false), activity)
    } else if i.percent <= HARD_FLOOR {
        // Final guard: never discharge at or below the floor; charge normally.
        (false, false, light_for(i.external, false, false), Activity::Charging { to: s.upper })
    } else {
        (hold, true, if native { None } else { Some(Light::Green) }, Activity::Discharging { to: n })
    };
    (inhibit, discharge, if native { None } else { light }, activity)
}

pub fn decide(s: &Settings, ov: Override, i: &Inputs, prev: &Outputs) -> Decision {
    if i.mode == ControlMode::Unsupported {
        return Decision { outputs: Outputs::default(), activity: Activity::Paused, clear_override: false, native_limit: None };
    }
    let native = i.mode == ControlMode::Native;
    let nl = |n: u8| if native { Some(n) } else { None };
    let above_floor = i.percent > HARD_FLOOR;
    let mut clear_override = false;
    let mut ov = ov;

    match ov {
        Override::FullCharge if !i.external => {
            clear_override = true;
            ov = Override::None;
        }
        Override::DischargeTo(n) if i.percent <= n => {
            clear_override = true;
            ov = Override::None;
        }
        _ => {}
    }

    match ov {
        Override::FullCharge => {
            return Decision {
                outputs: Outputs { inhibit: false, discharge: false, light: if native { None } else { Some(Light::System) } },
                activity: Activity::Charging { to: 100 },
                clear_override,
                native_limit: nl(100),
            };
        }
        Override::DischargeTo(n) => {
            let (inhibit, discharge, light, activity) = discharge_step(s, i, n);
            return Decision { outputs: Outputs { inhibit, discharge, light }, activity, clear_override, native_limit: nl(s.upper) };
        }
        Override::Calibrate(step @ CalStep::Discharge) => {
            let (inhibit, discharge, light, activity) = discharge_step(s, i, CAL_LOW);
            let activity = match activity {
                Activity::LidClosed | Activity::OnBattery => activity,
                _ if !i.external => Activity::OnBattery,
                _ => Activity::Calibrating { step },
            };
            return Decision { outputs: Outputs { inhibit, discharge, light }, activity, clear_override, native_limit: nl(i.native_lowest) };
        }
        Override::Calibrate(step) => {
            return Decision {
                outputs: Outputs { inhibit: false, discharge: false, light: if native { None } else { Some(Light::System) } },
                activity: Activity::Calibrating { step },
                clear_override,
                native_limit: nl(100),
            };
        }
        Override::None => {}
    }

    if native {
        let activity = if !i.external {
            Activity::OnBattery
        } else if i.percent > s.upper {
            Activity::Discharging { to: s.upper }
        } else if i.percent == s.upper {
            Activity::Holding
        } else {
            Activity::Charging { to: s.upper }
        };
        return Decision { outputs: Outputs::default(), activity, clear_override, native_limit: Some(s.upper) };
    }

    let inhibit = if i.percent >= s.upper {
        true
    } else if i.percent < s.lower {
        false
    } else {
        prev.inhibit
    };
    let discharge = s.auto_discharge && i.external && !i.lid_closed && i.percent > s.upper && above_floor;
    let activity = if !i.external {
        Activity::OnBattery
    } else if i.lid_closed && s.auto_discharge && i.percent > s.upper {
        // Would discharge, but never with the lid closed: say so instead of "Holding at <upper>%".
        Activity::LidClosed
    } else if discharge {
        Activity::Discharging { to: s.upper }
    } else if inhibit {
        Activity::Holding
    } else {
        Activity::Charging { to: s.upper }
    };

    Decision {
        outputs: Outputs { inhibit, discharge, light: light_for(i.external, inhibit, discharge) },
        activity,
        clear_override,
        native_limit: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::CalStep;
    use crate::Settings;
    use Activity::*;
    use ControlMode::*;
    use Light::*;

    fn s() -> Settings { Settings::default() }
    fn i(percent: u8, external: bool, lid_closed: bool, mode: ControlMode) -> Inputs {
        Inputs { percent, external, lid_closed, mode, native_lowest: 80 }
    }
    fn prev(inhibit: bool, discharge: bool) -> Outputs { Outputs { inhibit, discharge, light: None } }

    #[test]
    fn hold_mode_table() {
        let rows = [
            (70, true, false, false, false, Some(Orange), Charging { to: 80 }),
            (80, true, false, true, false, Some(Green), Holding),
            (77, true, true, true, false, Some(Green), Holding),
            (77, true, false, false, false, Some(Orange), Charging { to: 80 }),
            (74, true, true, false, false, Some(Orange), Charging { to: 80 }),
            (85, true, false, true, true, Some(Green), Discharging { to: 80 }),
            (85, false, true, true, false, None, OnBattery),
        ];
        for (p, ext, pi, inh, dis, light, act) in rows {
            let d = decide(&s(), Override::None, &i(p, ext, false, Hold), &prev(pi, false));
            assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.outputs.light, d.activity), (inh, dis, light, act), "pct {p}");
        }
    }
    #[test]
    fn hold_no_auto_discharge_just_holds() {
        let st = Settings { auto_discharge: false, ..s() };
        let d = decide(&st, Override::None, &i(85, true, false, Hold), &prev(false, false));
        assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (true, false, Holding));
    }
    #[test]
    fn hold_lid_closed_never_discharges() {
        let off = Settings { auto_discharge: false, ..s() };
        // (settings, percent, external, inhibit, activity)
        let rows = [
            (s(), 85, true, true, LidClosed), // would discharge with the lid open
            (s(), 80, true, true, Holding),
            (off, 85, true, true, Holding),
            (s(), 85, false, true, OnBattery),
        ];
        for (st, p, ext, inh, act) in rows {
            let d = decide(&st, Override::None, &i(p, ext, true, Hold), &prev(true, false));
            assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (inh, false, act), "pct {p} ext {ext} auto {}", st.auto_discharge);
        }
    }
    #[test] fn native_mode_table() {
        let d = |p, ext| decide(&s(), Override::None, &i(p, ext, false, Native), &prev(false, false));
        for (p, ext, act) in [(85, true, Discharging { to: 80 }), (80, true, Holding), (70, true, Charging { to: 80 }), (85, false, OnBattery)] {
            let r = d(p, ext);
            assert_eq!((r.activity, r.native_limit), (act, Some(80)), "{p} {ext}");
            assert_eq!(r.outputs, Outputs { inhibit: false, discharge: false, light: None });
        }
    }
    #[test] fn native_full_charge_asks_for_100() {
        let r = decide(&s(), Override::FullCharge, &i(80, true, false, Native), &prev(false, false));
        assert_eq!((r.native_limit, r.activity, r.outputs.light), (Some(100), Charging { to: 100 }, None));
    }
    #[test] fn native_discharge_to_uses_chie_and_keeps_the_limit() {
        let r = decide(&s(), Override::DischargeTo(70), &i(78, true, false, Native), &prev(false, false));
        assert_eq!((r.outputs.discharge, r.outputs.inhibit, r.native_limit), (true, false, Some(80)));
        let r = decide(&s(), Override::DischargeTo(70), &i(78, true, true, Native), &prev(false, true));
        assert!(!r.outputs.discharge); // lid closed
    }
    #[test] fn native_discharge_to_never_touches_the_light() {
        // lid closed on AC, and at or below the floor on AC
        for (pct, lid) in [(78, true), (10, false), (8, false)] {
            let r = decide(&s(), Override::DischargeTo(5), &i(pct, true, lid, Native), &prev(false, false));
            assert_eq!(r.outputs.light, None, "pct {pct} lid {lid}");
        }
    }
    #[test] fn hold_mode_has_no_native_limit() {
        assert_eq!(decide(&s(), Override::None, &i(80, true, false, Hold), &prev(false, false)).native_limit, None);
    }
    #[test]
    fn full_charge_override_until_unplug() {
        let d = decide(&s(), Override::FullCharge, &i(90, true, false, Hold), &prev(true, false));
        assert_eq!(
            (d.outputs, d.activity, d.clear_override),
            (Outputs { inhibit: false, discharge: false, light: Some(System) }, Charging { to: 100 }, false)
        );
        let d = decide(&s(), Override::FullCharge, &i(90, false, false, Hold), &prev(false, false));
        assert!(d.clear_override);
        assert_eq!(d.activity, OnBattery);
    }
    #[test]
    fn discharge_to_target_then_clears() {
        let d = decide(&s(), Override::DischargeTo(50), &i(60, true, false, Hold), &prev(false, false));
        assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (true, true, Discharging { to: 50 }));
        let d = decide(&s(), Override::DischargeTo(50), &i(50, true, false, Hold), &prev(true, true));
        assert!(d.clear_override);
        assert!(!d.outputs.discharge);
    }
    #[test]
    fn discharge_to_lid_closed_says_on_battery_when_unplugged() {
        for mode in [Hold, Native] {
            let d = decide(&s(), Override::DischargeTo(50), &i(60, true, true, mode), &prev(false, true));
            assert_eq!((d.outputs.discharge, d.activity), (false, LidClosed), "{mode:?}");
            let d = decide(&s(), Override::DischargeTo(50), &i(60, false, true, mode), &prev(false, true));
            assert_eq!((d.outputs.discharge, d.outputs.light, d.activity), (false, None, OnBattery), "{mode:?}");
        }
    }
    #[test]
    fn never_discharges_at_or_below_floor() {
        for (ov, mode) in [(Override::DischargeTo(5), Hold), (Override::DischargeTo(5), Native)] {
            let d = decide(&s(), ov, &i(10, true, false, mode), &prev(false, true));
            assert!(!d.outputs.discharge, "{mode:?}");
        }
    }
    #[test]
    fn calibrate_discharge_step_native_limit_is_lowest() {
        let d = decide(&s(), Override::Calibrate(CalStep::Discharge), &i(60, true, false, Native), &prev(false, false));
        assert!(d.outputs.discharge);
        assert_eq!(d.native_limit, Some(80));
        assert_eq!(d.activity, Calibrating { step: CalStep::Discharge });
        assert!(!d.clear_override);
    }
    #[test]
    fn calibrate_discharge_lid_closed_holds() {
        let ov = Override::Calibrate(CalStep::Discharge);
        let d = decide(&s(), ov, &i(60, true, true, Hold), &prev(false, true));
        assert_eq!((d.outputs.discharge, d.outputs.inhibit, d.activity), (false, true, LidClosed));
        let d = decide(&s(), ov, &i(60, false, true, Hold), &prev(false, true));
        assert_eq!(d.activity, OnBattery);
        assert!(!d.clear_override);
    }
    #[test]
    fn calibrate_never_discharges_at_floor() {
        for mode in [Hold, Native] {
            let d = decide(&s(), Override::Calibrate(CalStep::Discharge), &i(10, true, false, mode), &prev(false, true));
            assert!(!d.outputs.discharge, "{mode:?}");
            assert!(!d.clear_override);
        }
    }
    #[test]
    fn calibrate_charge_and_hold_lift_to_100() {
        for step in [CalStep::Charge, CalStep::Hold] {
            let d = decide(&s(), Override::Calibrate(step), &i(90, false, false, Native), &prev(false, false));
            assert_eq!((d.native_limit, d.clear_override, d.activity), (Some(100), false, Calibrating { step }));
            let d = decide(&s(), Override::Calibrate(step), &i(90, false, false, Hold), &prev(true, false));
            assert_eq!((d.outputs.inhibit, d.outputs.light, d.native_limit, d.clear_override), (false, Some(Light::System), None, false));
        }
    }
    #[test]
    fn unsupported_does_nothing() {
        let d = decide(&s(), Override::None, &i(95, true, false, Unsupported), &prev(false, false));
        assert_eq!((d.outputs, d.activity), (Outputs::default(), Paused));
    }
}
