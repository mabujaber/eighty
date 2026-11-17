//! Pure charge policy: given settings, override, readings and the previous
//! outputs, decide what the helper should write this tick. No I/O.
use crate::{Settings, HARD_FLOOR};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlMode {
    Hold,
    Cycle,
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
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Charging { to: u8 },
    Holding,
    Discharging { to: u8 },
    OnBattery,
    LidClosed,
    Paused,
}

pub struct Inputs {
    pub percent: u8,
    pub external: bool,
    pub lid_closed: bool,
    pub mode: ControlMode,
}

pub struct Decision {
    pub outputs: Outputs,
    pub activity: Activity,
    pub clear_override: bool,
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

pub fn decide(s: &Settings, ov: Override, i: &Inputs, prev: &Outputs) -> Decision {
    if i.mode == ControlMode::Unsupported {
        return Decision { outputs: Outputs::default(), activity: Activity::Paused, clear_override: false };
    }
    let hold = i.mode == ControlMode::Hold;
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
                outputs: Outputs { inhibit: false, discharge: false, light: Some(Light::System) },
                activity: Activity::Charging { to: 100 },
                clear_override,
            };
        }
        Override::DischargeTo(n) => {
            let (inhibit, discharge, light, activity) = if i.lid_closed {
                (hold, false, light_for(i.external, hold, false), Activity::LidClosed)
            } else if !above_floor {
                // Final guard: never discharge at or below the floor; charge normally.
                (false, false, light_for(i.external, false, false), Activity::Charging { to: s.upper })
            } else {
                (hold, true, Some(Light::Green), Activity::Discharging { to: n })
            };
            return Decision { outputs: Outputs { inhibit, discharge, light }, activity, clear_override };
        }
        Override::None => {}
    }

    let (inhibit, discharge, activity) = if hold {
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
        } else if discharge {
            Activity::Discharging { to: s.upper }
        } else if inhibit {
            Activity::Holding
        } else {
            Activity::Charging { to: s.upper }
        };
        (inhibit, discharge, activity)
    } else {
        let discharge = if !i.external || i.lid_closed {
            false
        } else if i.percent >= s.upper {
            true
        } else if i.percent <= s.lower {
            false
        } else {
            prev.discharge
        } && above_floor;
        let activity = if !i.external {
            Activity::OnBattery
        } else if i.lid_closed && i.percent >= s.upper {
            Activity::LidClosed
        } else if discharge {
            Activity::Discharging { to: s.lower }
        } else {
            Activity::Charging { to: s.upper }
        };
        (false, discharge, activity)
    };

    Decision {
        outputs: Outputs { inhibit, discharge, light: light_for(i.external, inhibit, discharge) },
        activity,
        clear_override,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Settings;
    use Activity::*;
    use ControlMode::*;
    use Light::*;

    fn s() -> Settings { Settings::default() }
    fn i(percent: u8, external: bool, lid_closed: bool, mode: ControlMode) -> Inputs {
        Inputs { percent, external, lid_closed, mode }
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
        let d = decide(&s(), Override::None, &i(85, true, true, Hold), &prev(true, false));
        assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (true, false, Holding));
    }
    #[test]
    fn cycle_mode_table() {
        let rows = [
            (70, false, false, Some(Orange), Charging { to: 80 }),
            (80, false, true, Some(Green), Discharging { to: 75 }),
            (77, true, true, Some(Green), Discharging { to: 75 }),
            (75, true, false, Some(Orange), Charging { to: 80 }),
            (77, false, false, Some(Orange), Charging { to: 80 }),
        ];
        for (p, pd, dis, light, act) in rows {
            let d = decide(&s(), Override::None, &i(p, true, false, Cycle), &prev(false, pd));
            assert!(!d.outputs.inhibit);
            assert_eq!((d.outputs.discharge, d.outputs.light, d.activity), (dis, light, act), "pct {p}");
        }
    }
    #[test]
    fn cycle_lid_closed_reports_not_enforced() {
        let d = decide(&s(), Override::None, &i(82, true, true, Cycle), &prev(false, true));
        assert_eq!((d.outputs.discharge, d.activity), (false, LidClosed));
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
    fn never_discharges_at_or_below_floor() {
        for (ov, mode) in [(Override::DischargeTo(5), Hold), (Override::DischargeTo(5), Cycle)] {
            let d = decide(&s(), ov, &i(10, true, false, mode), &prev(false, true));
            assert!(!d.outputs.discharge, "{mode:?}");
        }
        let low = Settings { upper: 20, lower: 10, auto_discharge: true };
        let d = decide(&low, Override::None, &i(10, true, false, Cycle), &prev(false, true));
        assert!(!d.outputs.discharge);
    }
    #[test]
    fn unsupported_does_nothing() {
        let d = decide(&s(), Override::None, &i(95, true, false, Unsupported), &prev(false, false));
        assert_eq!((d.outputs, d.activity), (Outputs::default(), Paused));
    }
}
