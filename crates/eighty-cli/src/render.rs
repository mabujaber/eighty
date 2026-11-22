//! Plain-text rendering of status and doctor output.
use eighty_core::policy::{Activity, ControlMode, Override};
use eighty_core::proto::{DoctorReport, KeyState, NativeState, Status};

fn line(out: &mut String, label: &str, text: &str) {
    out.push_str(&format!("{:<10}{}\n", label, text));
}

fn mode_text(m: ControlMode) -> &'static str {
    match m {
        ControlMode::Hold => "hold (CHTE)",
        ControlMode::Native => "native (macOS Charge Limit)",
        ControlMode::Unsupported => "unsupported",
    }
}

fn activity_text(s: &Status) -> String {
    match s.activity {
        Activity::Charging { to } => format!("Charging to {to}%"),
        Activity::Holding => format!("Holding at {}%", s.percent),
        Activity::Discharging { to } => format!("Discharging to {to}%"),
        Activity::Calibrating { step } => format!("Calibrating: step {}/4", step.number()),
        Activity::OnBattery => "On battery".to_string(),
        Activity::LidClosed if s.mode == ControlMode::Hold => {
            let to = match s.override_ {
                Override::DischargeTo(n) => n,
                _ => s.settings.upper,
            };
            format!("Lid closed: holding at {}%, discharges to {to}% when the lid opens", s.percent)
        }
        Activity::LidClosed => "Lid closed".to_string(),
        Activity::Paused => "Paused".to_string(),
    }
}

/// The health message, without claiming charging was restored when a restore failed.
fn health_text(s: &Status) -> String {
    let msg = s.health.message();
    if s.warnings.iter().any(|w| w.starts_with("couldn't restore")) {
        msg.replace("Charging restored", "Charging may NOT be restored — see warnings")
    } else {
        msg
    }
}

pub fn render_status(s: &Status) -> String {
    let mut out = String::new();
    if !s.health.is_ok() {
        out.push_str(&format!("NOT LIMITING: {}\n", health_text(s)));
    }
    let charging_part = if s.amperage_stale {
        "updating…".to_string()
    } else if s.charging {
        format!("charging {} mA", s.amperage_ma)
    } else if s.amperage_ma < 0 {
        format!("discharging {} mA", s.amperage_ma.abs())
    } else {
        "not charging".to_string()
    };
    let power = if s.external { "on AC" } else { "on battery" };
    line(&mut out, "Battery", &format!("{}% · {} · {}", s.percent, charging_part, power));
    let limit_text = match s.mode {
        // `charge full` sets macOS's limit to 100% until unplugged; the setting stays.
        ControlMode::Native if s.override_ == Override::FullCharge => {
            format!("{}% (lifted to 100% until unplugged)", s.settings.upper)
        }
        ControlMode::Native => format!("{}% (held by macOS)", s.settings.upper),
        _ => {
            let auto = if s.settings.auto_discharge { "auto-discharge on" } else { "auto-discharge off" };
            format!("{}% (resumes below {}%) · {}", s.settings.upper, s.settings.lower, auto)
        }
    };
    line(&mut out, "Limit", &limit_text);
    line(&mut out, "State", &activity_text(s));
    let light = match (s.mode, s.light_supported) {
        (ControlMode::Native, _) => "macOS",
        (_, true) => "yes",
        (_, false) => "no",
    };
    line(&mut out, "Mode", &format!("{} · MagSafe light: {}", mode_text(s.mode), light));
    match s.override_ {
        Override::None => {}
        Override::FullCharge => line(&mut out, "Override", "charge to 100% until unplugged"),
        Override::DischargeTo(p) => line(&mut out, "Override", &format!("discharge to {p}%")),
        Override::Calibrate(_) => {}
    }
    for w in &s.warnings {
        line(&mut out, "Warning", w);
    }
    out
}

fn key_text(k: &KeyState) -> String {
    match k {
        KeyState::Usable => "usable".to_string(),
        KeyState::Missing => "missing".to_string(),
        KeyState::Blocked => "blocked".to_string(),
        KeyState::WrongSize { size } => format!("wrong size ({size} bytes)"),
        KeyState::Error(e) => format!("error: {e}"),
    }
}

fn native_text(n: &NativeState) -> String {
    let limit = n.limit.map_or("unreadable".to_string(), |l| format!("{l}%"));
    let steps: Vec<String> = n.available.iter().map(|a| a.to_string()).collect();
    format!("{limit} · steps {}", steps.join(", "))
}

pub fn render_doctor(d: &DoctorReport) -> String {
    let mut out = String::new();
    line(&mut out, "Machine", &format!("{} · {}", d.machine.model, d.machine.chip));
    line(&mut out, "macOS", &format!("{} ({})", d.machine.macos, d.machine.build));
    line(&mut out, "Helper", &d.status.version);
    line(&mut out, "Mode", mode_text(d.status.mode));
    out.push_str("SMC keys\n");
    for (key, state) in &d.keys {
        out.push_str(&format!("  {key}  {}\n", key_text(state)));
    }
    let others = if d.other_limiters.is_empty() { "none".to_string() } else { d.other_limiters.join(", ") };
    line(&mut out, "Limiters", &others);
    if let Some(n) = &d.native {
        line(&mut out, "Native", &native_text(n));
    }
    line(&mut out, "Health", &health_text(&d.status));
    out.push('\n');
    out.push_str(&render_status(&d.status));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_core::policy::{Activity, ControlMode, Override};
    use eighty_core::proto::{DoctorReport, Health, KeyState, NativeState, Status};
    use eighty_core::Settings;
    use eighty_hw::machine::MachineInfo;

    fn st() -> Status {
        Status {
            percent: 78,
            external: true,
            charging: true,
            amperage_ma: 1197,
            amperage_stale: false,
            mode: ControlMode::Hold,
            activity: Activity::Charging { to: 80 },
            health: Health::Ok,
            settings: Settings::default(),
            override_: Override::None,
            light_supported: true,
            warnings: vec![],
            version: "0.1.0".into(),
        }
    }

    #[test]
    fn renders_ok_status() {
        let out = render_status(&st());
        assert_eq!(out.lines().next().unwrap(), "Battery   78% · charging 1197 mA · on AC");
        assert!(out.contains("Limit     80% (resumes below 75%) · auto-discharge on"));
        assert!(out.contains("State     Charging to 80%"));
        assert!(out.contains("Mode      hold (CHTE) · MagSafe light: yes"));
        assert!(!out.contains("NOT LIMITING"));
    }

    #[test]
    fn stale_reading_hides_the_current() {
        let s = Status { amperage_stale: true, ..st() };
        assert_eq!(render_status(&s).lines().next().unwrap(), "Battery   78% · updating… · on AC");
    }

    #[test]
    fn fault_is_first_line() {
        let s = Status { health: Health::Unsupported, activity: Activity::Paused, ..st() };
        assert!(render_status(&s).starts_with("NOT LIMITING: this Mac has no SMC key"));
    }

    #[test]
    fn failed_restore_does_not_claim_charging_restored() {
        let s = Status {
            health: Health::WriteFailed { key: "CHTE".into(), error: "x".into() },
            activity: Activity::Paused,
            warnings: vec!["couldn't restore CHTE: x; charging may still be inhibited".into()],
            ..st()
        };
        let out = render_status(&s);
        assert_eq!(
            out.lines().next().unwrap(),
            "NOT LIMITING: writing CHTE failed: x. Charging may NOT be restored — see warnings. Run `eighty doctor --retry`."
        );
        let s = Status { health: Health::BatteryUnreadable("gone".into()), ..s };
        let out = render_status(&s);
        assert!(!out.contains("Charging restored"), "{out}");
        let doc = render_doctor(&DoctorReport {
            machine: MachineInfo { model: "m".into(), chip: "c".into(), macos: "v".into(), build: "b".into() },
            keys: vec![],
            status: s,
            other_limiters: vec![],
            native: None,
        });
        assert!(!doc.contains("Charging restored"), "{doc}");
    }

    #[test]
    fn native_unavailable_is_not_limiting_and_claims_no_restore() {
        let s = Status {
            mode: ControlMode::Native,
            health: Health::NativeUnavailable("x".into()),
            activity: Activity::Paused,
            warnings: vec!["couldn't restore CHIE: y; charging may still be inhibited".into()],
            ..st()
        };
        assert_eq!(
            render_status(&s).lines().next().unwrap(),
            "NOT LIMITING: couldn't reach macOS's Charge Limit (PowerUI): x. macOS keeps its current limit; eighty is waiting for macOS and retrying."
        );
    }

    #[test]
    fn lid_closed_hold_explains_the_hold() {
        let s = Status { percent: 100, activity: Activity::LidClosed, ..st() };
        assert!(render_status(&s).contains("State     Lid closed: holding at 100%, discharges to 80% when the lid opens\n"));
        let s = Status { override_: Override::DischargeTo(50), ..s };
        assert!(render_status(&s).contains("State     Lid closed: holding at 100%, discharges to 50% when the lid opens\n"));
    }

    #[test]
    fn holding_shows_the_current_charge() {
        let s = Status { charging: false, amperage_ma: 0, activity: Activity::Holding, ..st() };
        let out = render_status(&s);
        assert!(out.contains("State     Holding at 78%\n"), "{out}");
    }

    #[test]
    fn native_status_text() {
        let s = Status { mode: ControlMode::Native, activity: Activity::Holding, charging: false, amperage_ma: 0, ..st() };
        let out = render_status(&s);
        assert!(out.contains("Limit     80% (held by macOS)\n"), "{out}");
        assert!(out.contains("Mode      native (macOS Charge Limit) · MagSafe light: macOS\n"), "{out}");
        assert!(!out.contains("resumes below") && !out.contains("auto-discharge"), "{out}");
    }

    #[test]
    fn native_charge_full_says_the_limit_is_lifted() {
        let s = Status {
            mode: ControlMode::Native,
            activity: Activity::Charging { to: 100 },
            override_: Override::FullCharge,
            ..st()
        };
        let out = render_status(&s);
        assert!(out.contains("Limit     80% (lifted to 100% until unplugged)\n"), "{out}");
        assert!(!out.contains("held by macOS"), "{out}");
    }

    fn native_doctor(limit: Option<u8>) -> String {
        render_doctor(&DoctorReport {
            machine: MachineInfo { model: "m".into(), chip: "c".into(), macos: "v".into(), build: "b".into() },
            keys: vec![],
            status: Status { mode: ControlMode::Native, ..st() },
            other_limiters: vec![],
            native: Some(NativeState { available: vec![80, 85, 90, 95, 100], limit, enabled: Some(true) }),
        })
    }

    #[test]
    fn doctor_shows_the_native_limit() {
        assert!(native_doctor(Some(80)).contains("Native    80% · steps 80, 85, 90, 95, 100\n"));
        assert!(native_doctor(None).contains("Native    unreadable · steps 80, 85, 90, 95, 100\n"));
    }

    #[test]
    fn lid_closed_native_is_plain() {
        let s = Status { mode: ControlMode::Native, activity: Activity::LidClosed, ..st() };
        assert!(render_status(&s).contains("State     Lid closed\n"));
    }

    #[test]
    fn battery_variants_override_and_warnings() {
        let s = Status {
            external: false,
            charging: false,
            amperage_ma: -850,
            override_: Override::DischargeTo(50),
            warnings: vec!["w1".into()],
            ..st()
        };
        let out = render_status(&s);
        assert!(out.starts_with("Battery   78% · discharging 850 mA · on battery"));
        assert!(out.contains("Override  discharge to 50%"));
        assert!(out.contains("Warning   w1"));
        let s = Status { charging: false, amperage_ma: 0, override_: Override::FullCharge, ..st() };
        let out = render_status(&s);
        assert!(out.contains("not charging · on AC"));
        assert!(out.contains("Override  charge to 100% until unplugged"));
    }

    #[test]
    fn doctor_lists_every_key() {
        let out = render_doctor(&DoctorReport {
            machine: MachineInfo {
                model: "Mac14,2".into(),
                chip: "Apple M2".into(),
                macos: "26.1".into(),
                build: "25B78".into(),
            },
            keys: vec![
                ("CHTE".into(), KeyState::Missing),
                ("CHIE".into(), KeyState::Usable),
                ("ACLC".into(), KeyState::Usable),
            ],
            status: Status { mode: ControlMode::Unsupported, ..st() },
            other_limiters: vec![],
            native: None,
        });
        assert!(out.contains("CHTE  missing") && out.contains("CHIE  usable") && out.contains("Mode      unsupported"));
        assert!(out.contains("Limiters  none\n"));
    }

    #[test]
    fn doctor_details() {
        let out = render_doctor(&DoctorReport {
            machine: MachineInfo {
                model: "Mac14,2".into(),
                chip: "Apple M2".into(),
                macos: "26.1".into(),
                build: "25B78".into(),
            },
            keys: vec![
                ("CHTE".into(), KeyState::WrongSize { size: 2 }),
                ("CHIE".into(), KeyState::Error("boom".into())),
                ("ACLC".into(), KeyState::Blocked),
            ],
            status: Status { health: Health::Unsupported, ..st() },
            other_limiters: vec!["AlDente".into(), "BCLM".into()],
            native: None,
        });
        assert!(out.contains("Machine   Mac14,2 · Apple M2\n"));
        assert!(out.contains("macOS     26.1 (25B78)\n"));
        assert!(out.contains("Helper    0.1.0\n"));
        assert!(out.contains("  CHTE  wrong size (2 bytes)\n"));
        assert!(out.contains("  CHIE  error: boom\n"));
        assert!(out.contains("  ACLC  blocked\n"));
        assert!(out.contains("Limiters  AlDente, BCLM\n"));
        assert!(out.contains("Health    this Mac has no SMC key"));
    }
}
