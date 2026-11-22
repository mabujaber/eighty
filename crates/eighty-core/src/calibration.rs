//! Pure calibration state machine: one full battery cycle (discharge, charge,
//! hold at full) driven by `advance()` once per tick. No I/O.
use serde::{Deserialize, Serialize};

/// Step 1 ends at or below this charge.
pub const CAL_LOW: u8 = 15;
/// Step 3 falls back to step 2 below this charge.
pub const CAL_HOLD_MIN: u8 = 95;
/// Seconds on AC at full that step 3 needs.
pub const CAL_HOLD_SECS: u64 = 3600;
/// Seconds without progress before the calibration is abandoned.
pub const CAL_GIVE_UP_SECS: u64 = 86_400;
/// Seconds between attempts to restart charging in step 2.
pub const CAL_KICK_SECS: u64 = 300;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalStep {
    Discharge,
    Charge,
    Hold,
}

impl CalStep {
    /// Step number out of 4 (step 4 is returning to the limit).
    pub fn number(self) -> u8 {
        match self {
            CalStep::Discharge => 1,
            CalStep::Charge => 2,
            CalStep::Hold => 3,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalWait {
    PlugIn,
    LidClosed,
    Fault,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Calibration {
    pub step: CalStep,
    pub started_at: u64,
    pub scheduled: bool,
    pub step_started_at: u64,
    pub held_secs: u64,
    pub last_progress_at: u64,
    /// Lowest charge seen in Discharge, highest in Charge: progress is beating it.
    pub best_percent: u8,
    /// Previous tick: (time, on AC). `None` after a load, so downtime never counts.
    pub last_tick: Option<(u64, bool)>,
    pub last_kick_at: Option<u64>,
}

impl Calibration {
    pub fn new(now: u64, percent: u8, scheduled: bool) -> Calibration {
        Calibration {
            step: CalStep::Discharge,
            started_at: now,
            scheduled,
            step_started_at: now,
            held_secs: 0,
            last_progress_at: now,
            best_percent: percent,
            last_tick: None,
            last_kick_at: None,
        }
    }

    fn change_step(&mut self, step: CalStep, now: u64, percent: u8) -> CalOutcome {
        self.step = step;
        self.step_started_at = now;
        self.best_percent = percent;
        self.last_progress_at = now;
        CalOutcome::StepChanged(step)
    }
}

pub struct CalInputs {
    pub now: u64,
    pub percent: u8,
    pub external: bool,
    pub charging: bool,
    pub fully_charged: Option<bool>,
    pub lid_closed: bool,
    pub settled: bool,
    pub faulted: bool,
}

#[derive(Debug, PartialEq)]
pub enum CalOutcome {
    Continue,
    StepChanged(CalStep),
    KickCharging,
    Finished,
    Abandoned(String),
}

/// Full = 100% and macOS says fully charged; without that field, 100% on AC and not charging.
fn is_full(i: &CalInputs) -> bool {
    i.percent >= 100
        && match i.fully_charged {
            Some(f) => f,
            None => i.external && !i.charging,
        }
}

pub fn advance(c: &mut Calibration, i: &CalInputs) -> CalOutcome {
    // Hold clock: only AC-to-AC time between consecutive ticks counts.
    if c.step == CalStep::Hold && i.external && !i.faulted {
        if let Some((t, true)) = c.last_tick {
            let dt = i.now.saturating_sub(t);
            if dt > 0 {
                c.held_secs += dt;
                c.last_progress_at = i.now;
            }
        }
    }
    c.last_tick = Some((i.now, i.external));

    let mut out = CalOutcome::Continue;
    if !i.faulted {
        match c.step {
            CalStep::Discharge => {
                if i.percent <= CAL_LOW {
                    out = c.change_step(CalStep::Charge, i.now, i.percent);
                } else if i.percent < c.best_percent {
                    c.best_percent = i.percent;
                    c.last_progress_at = i.now;
                }
            }
            CalStep::Charge => {
                if is_full(i) {
                    out = c.change_step(CalStep::Hold, i.now, i.percent);
                } else {
                    if i.percent > c.best_percent {
                        c.best_percent = i.percent;
                        c.last_progress_at = i.now;
                    }
                    let since = c
                        .last_kick_at
                        .map_or(c.step_started_at, |k| k.max(c.step_started_at));
                    if i.external
                        && !i.charging
                        && i.settled
                        && i.now.saturating_sub(since) >= CAL_KICK_SECS
                    {
                        c.last_kick_at = Some(i.now);
                        out = CalOutcome::KickCharging;
                    }
                }
            }
            CalStep::Hold => {
                if i.percent < CAL_HOLD_MIN {
                    c.held_secs = 0;
                    out = c.change_step(CalStep::Charge, i.now, i.percent);
                } else if c.held_secs >= CAL_HOLD_SECS {
                    out = CalOutcome::Finished;
                }
            }
        }
    }

    if out == CalOutcome::Continue && i.now.saturating_sub(c.last_progress_at) >= CAL_GIVE_UP_SECS {
        let wait = match waiting(c, i) {
            Some(CalWait::PlugIn) => ": waiting for the charger",
            Some(CalWait::LidClosed) => ": lid closed",
            Some(CalWait::Fault) => ": eighty is in a fault",
            None => "",
        };
        out = CalOutcome::Abandoned(format!(
            "no progress in step {}/4 for 24 h{}",
            c.step.number(),
            wait
        ));
    }
    out
}

/// What the calibration is stuck on, if anything.
pub fn waiting(c: &Calibration, i: &CalInputs) -> Option<CalWait> {
    if i.faulted {
        Some(CalWait::Fault)
    } else if !i.external && matches!(c.step, CalStep::Charge | CalStep::Hold) {
        Some(CalWait::PlugIn)
    } else if i.lid_closed && i.external && c.step == CalStep::Discharge {
        Some(CalWait::LidClosed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(now: u64, percent: u8, external: bool) -> CalInputs {
        CalInputs {
            now,
            percent,
            external,
            charging: false,
            fully_charged: None,
            lid_closed: false,
            settled: true,
            faulted: false,
        }
    }

    fn in_step(step: CalStep, now: u64, percent: u8) -> Calibration {
        let mut c = Calibration::new(now, percent, false);
        c.step = step;
        c
    }

    #[test]
    fn discharge_progresses_and_finishes_at_low() {
        let mut c = Calibration::new(1000, 60, false);
        assert_eq!(advance(&mut c, &at(1060, 50, false)), CalOutcome::Continue);
        assert_eq!(c.last_progress_at, 1060);
        assert_eq!(
            advance(&mut c, &at(1120, 15, false)),
            CalOutcome::StepChanged(CalStep::Charge)
        );
        assert_eq!(c.step, CalStep::Charge);
    }

    #[test]
    fn discharge_step_done_on_battery_below_low() {
        let mut c = Calibration::new(1000, 40, false);
        assert_eq!(
            advance(&mut c, &at(1060, 12, false)),
            CalOutcome::StepChanged(CalStep::Charge)
        );
        let i = at(1120, 12, false);
        assert_eq!(advance(&mut c, &i), CalOutcome::Continue);
        assert_eq!(waiting(&c, &i), Some(CalWait::PlugIn));
    }

    #[test]
    fn charge_needs_fully_charged() {
        let run = |fc: Option<bool>, charging: bool| {
            let mut c = in_step(CalStep::Charge, 1000, 99);
            let mut i = at(1060, 100, true);
            i.fully_charged = fc;
            i.charging = charging;
            advance(&mut c, &i)
        };
        assert_eq!(run(Some(false), false), CalOutcome::Continue);
        assert_eq!(
            run(Some(true), false),
            CalOutcome::StepChanged(CalStep::Hold)
        );
        assert_eq!(run(None, false), CalOutcome::StepChanged(CalStep::Hold));
        assert_eq!(run(None, true), CalOutcome::Continue);
    }

    #[test]
    fn charge_kicks_when_not_charging() {
        let mut c = in_step(CalStep::Charge, 1000, 40);
        assert_eq!(
            advance(&mut c, &at(1301, 40, true)),
            CalOutcome::KickCharging
        );
        assert_eq!(c.last_kick_at, Some(1301));
        assert_eq!(advance(&mut c, &at(1401, 40, true)), CalOutcome::Continue);
        let mut c = in_step(CalStep::Charge, 1000, 40);
        let mut i = at(1301, 40, true);
        i.settled = false;
        assert_eq!(advance(&mut c, &i), CalOutcome::Continue);
    }

    #[test]
    fn hold_clock_counts_only_ac_to_ac() {
        let t0 = 10_000;
        let mut c = in_step(CalStep::Hold, t0, 100);
        advance(&mut c, &at(t0, 100, true));
        advance(&mut c, &at(t0 + 1800, 100, false));
        advance(&mut c, &at(t0 + 3600, 100, true));
        assert_eq!(c.held_secs, 0);
        advance(&mut c, &at(t0 + 5400, 100, true));
        assert_eq!(c.held_secs, 1800);
    }

    #[test]
    fn hold_drop_goes_back_to_charge() {
        let mut c = in_step(CalStep::Hold, 1000, 100);
        c.held_secs = 1200;
        assert_eq!(
            advance(&mut c, &at(1060, 94, true)),
            CalOutcome::StepChanged(CalStep::Charge)
        );
        assert_eq!(c.held_secs, 0);
    }

    #[test]
    fn hold_finishes_after_an_hour_on_ac() {
        let t0 = 5000;
        let mut c = in_step(CalStep::Hold, t0, 100);
        let mut last = CalOutcome::Continue;
        for k in 0..=60 {
            last = advance(&mut c, &at(t0 + k * 60, 100, true));
        }
        assert_eq!(last, CalOutcome::Finished);
    }

    #[test]
    fn gives_up_after_24h_without_progress() {
        let mut c = in_step(CalStep::Charge, 1000, 30);
        let out = advance(&mut c, &at(1000 + 86_400, 30, false));
        assert_eq!(
            out,
            CalOutcome::Abandoned(
                "no progress in step 2/4 for 24 h: waiting for the charger".into()
            )
        );
    }

    #[test]
    fn fault_only_checks_give_up() {
        let mut c = Calibration::new(1000, 30, false);
        let mut i = at(1060, 10, false);
        i.faulted = true;
        assert_eq!(advance(&mut c, &i), CalOutcome::Continue);
        assert_eq!(c.step, CalStep::Discharge);
        let mut i = at(1000 + 86_400, 10, false);
        i.faulted = true;
        assert_eq!(
            advance(&mut c, &i),
            CalOutcome::Abandoned("no progress in step 1/4 for 24 h: eighty is in a fault".into())
        );
    }

    #[test]
    fn calibration_roundtrips_toml() {
        let mut c = Calibration::new(1000, 60, true);
        c.last_tick = Some((1060, true));
        c.last_kick_at = Some(1200);
        let s = toml::to_string(&c).unwrap();
        assert_eq!(toml::from_str::<Calibration>(&s).unwrap(), c);
    }
}
