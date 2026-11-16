//! The probe report: text for people, JSON for issues and later plans.
use std::fmt::Write;

use eighty_hw::battery::BatterySnapshot;
use serde::Serialize;

use crate::catalog::Role;
use crate::machine::MachineInfo;
use crate::reads::{KeyReading, ReadStatus};
use crate::writes::{Verdict, WriteResult};

#[derive(Debug, Serialize)]
pub struct ProbeReport {
    pub tool_version: String,
    pub machine: MachineInfo,
    pub battery: Option<BatterySnapshot>,
    pub reads: Vec<KeyReading>,
    pub writes: Option<Vec<WriteResult>>,
}

/// Best outcome for a role: the working key, why it couldn't be tested,
/// a key macOS blocked, or "none".
fn role_summary(writes: &[WriteResult], role: Role, ok: &Verdict) -> String {
    let of_role: Vec<_> = writes.iter().filter(|w| w.role == role).collect();
    if let Some(w) = of_role.iter().find(|w| &w.verdict == ok) {
        return w.key.clone();
    }
    if let Some((w, why)) = of_role.iter().find_map(|w| match &w.verdict {
        Verdict::CannotVerify(why) => Some((w, why)),
        _ => None,
    }) {
        return format!("untested ({}: {})", w.key, why);
    }
    if let Some(w) = of_role.iter().find(|w| w.verdict == Verdict::Blocked) {
        return format!("blocked ({})", w.key);
    }
    "none".into()
}

pub fn render_text(r: &ProbeReport) -> String {
    let mut t = String::new();
    let m = &r.machine;
    let _ = writeln!(t, "eighty-probe {} — {}, macOS {} ({}), {}", r.tool_version, m.chip, m.macos, m.build, m.model);
    match &r.battery {
        Some(b) => {
            let _ = writeln!(
                t,
                "Battery: {}% charging={} plugged={} {} mA {:.1} °C",
                b.percent, b.is_charging, b.external_connected, b.amperage_ma, b.temperature_c
            );
        }
        None => t.push_str("Battery: unavailable\n"),
    }
    for k in &r.reads {
        let _ = match &k.status {
            ReadStatus::Present { size, data_type, value_hex } => {
                writeln!(t, "READ  {}  present {}B {} = {}", k.key, size, data_type, value_hex)
            }
            ReadStatus::Missing => writeln!(t, "READ  {}  missing", k.key),
            ReadStatus::Error(e) => writeln!(t, "READ  {}  error: {}", k.key, e),
        };
    }
    let writes = r.writes.as_deref().unwrap_or(&[]);
    for w in writes {
        let _ = writeln!(t, "WRITE {}  {}/{:?}  {:?}", w.key, w.backend, w.role, w.verdict);
    }
    let inhibit = role_summary(writes, Role::InhibitCharging, &Verdict::Verified);
    let discharge = role_summary(writes, Role::ForceDischarge, &Verdict::Verified);
    let light = role_summary(writes, Role::Light, &Verdict::WriteAccepted);
    if [&inhibit, &discharge, &light].iter().any(|s| s.starts_with("untested")) {
        t.push_str(
            "Hint: some tests could not run. Set System Settings → Battery → Charge Limit to 100%, \
             keep the Mac on MagSafe and charging between 20% and 95%, then re-run.\n",
        );
    }
    let _ = writeln!(t, "Summary: inhibit={inhibit}, discharge={discharge}, light={light}");
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Role;
    use crate::machine::MachineInfo;
    use crate::reads::{KeyReading, ReadStatus};
    use crate::writes::{Verdict, WriteResult};

    fn machine() -> MachineInfo {
        MachineInfo { model: "Mac16,1".into(), chip: "Apple M5 Pro".into(), macos: "26.6.2".into(), build: "25G83".into() }
    }

    fn w(key: &str, role: Role, verdict: Verdict) -> WriteResult {
        WriteResult { backend: "tahoe".into(), key: key.into(), role, verdict, before: None, after: None }
    }

    #[test]
    fn renders_summary_line() {
        let r = ProbeReport {
            tool_version: "0.1.0".into(),
            machine: machine(),
            battery: None,
            reads: vec![
                KeyReading { key: "CHTE".into(), status: ReadStatus::Present { size: 4, data_type: "ui32".into(), value_hex: "00000000".into() } },
                KeyReading { key: "CH0B".into(), status: ReadStatus::Missing },
            ],
            writes: Some(vec![
                w("CHTE", Role::InhibitCharging, Verdict::Verified),
                w("CHIE", Role::ForceDischarge, Verdict::NoEffect),
                w("CH0J", Role::ForceDischarge, Verdict::Verified),
                w("ACLC", Role::Light, Verdict::WriteAccepted),
            ]),
        };
        let t = render_text(&r);
        assert!(t.starts_with("eighty-probe 0.1.0 — Apple M5 Pro, macOS 26.6.2 (25G83), Mac16,1\n"));
        assert!(t.contains("READ  CHTE  present 4B ui32 = 00000000"));
        assert!(t.contains("READ  CH0B  missing"));
        assert!(t.contains("WRITE CHTE  tahoe/InhibitCharging  Verified"));
        assert!(t.ends_with("Summary: inhibit=CHTE, discharge=CH0J, light=ACLC\n"));
    }

    #[test]
    fn summary_none_when_nothing_works() {
        let r = ProbeReport {
            tool_version: "0.1.0".into(),
            machine: machine(),
            battery: None,
            reads: vec![],
            writes: Some(vec![w("CHTE", Role::InhibitCharging, Verdict::Missing), w("ACLC", Role::Light, Verdict::Missing)]),
        };
        assert!(render_text(&r).ends_with("Summary: inhibit=none, discharge=none, light=none\n"));
    }

    #[test]
    fn summary_marks_untested_roles() {
        let r = ProbeReport {
            tool_version: "0.1.0".into(),
            machine: machine(),
            battery: None,
            reads: vec![],
            writes: Some(vec![
                w("CHTE", Role::InhibitCharging, Verdict::CannotVerify("battery is not charging".into())),
                w("CH0J", Role::ForceDischarge, Verdict::Blocked),
                w("ACLC", Role::Light, Verdict::WriteAccepted),
            ]),
        };
        let t = render_text(&r);
        assert!(t.contains("Hint: "));
        assert!(t.ends_with(
            "Summary: inhibit=untested (CHTE: battery is not charging), discharge=blocked (CH0J), light=ACLC\n"
        ));
    }
}
