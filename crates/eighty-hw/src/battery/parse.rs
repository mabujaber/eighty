use std::collections::HashMap;

use super::{BatteryError, BatterySnapshot};

/// A battery property value as read from IORegistry.
#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    Int(i64),
    Bool(bool),
}

fn int(props: &HashMap<String, Prop>, name: &'static str) -> Result<i64, BatteryError> {
    match props.get(name) {
        Some(Prop::Int(v)) => Ok(*v),
        _ => Err(BatteryError::MissingProp(name)),
    }
}

fn boolean(props: &HashMap<String, Prop>, name: &'static str) -> Result<bool, BatteryError> {
    match props.get(name) {
        Some(Prop::Bool(v)) => Ok(*v),
        _ => Err(BatteryError::MissingProp(name)),
    }
}

pub fn parse_snapshot(props: &HashMap<String, Prop>) -> Result<BatterySnapshot, BatteryError> {
    let cur = int(props, "CurrentCapacity")?;
    let max = int(props, "MaxCapacity")?;
    let percent = if max == 100 || max <= 0 {
        cur
    } else {
        ((cur as f64) * 100.0 / (max as f64)).round() as i64
    };
    Ok(BatterySnapshot {
        percent: percent.clamp(0, 100) as u8,
        is_charging: boolean(props, "IsCharging")?,
        external_connected: boolean(props, "ExternalConnected")?,
        amperage_ma: int(props, "Amperage")? as i32,
        temperature_c: ["Temperature", "VirtualTemperature"]
            .into_iter()
            .find_map(|k| int(props, k).ok())
            .map(|t| t as f32 / 100.0),
        cycle_count: int(props, "CycleCount").ok().map(|c| c as u32),
        update_time: int(props, "UpdateTime").ok().and_then(|t| u64::try_from(t).ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::{BatteryError, BatterySnapshot};
    use std::collections::HashMap;

    fn props(pairs: &[(&str, Prop)]) -> HashMap<String, Prop> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn base(cur: i64, max: i64, amp: i64) -> HashMap<String, Prop> {
        props(&[
            ("CurrentCapacity", Prop::Int(cur)),
            ("MaxCapacity", Prop::Int(max)),
            ("IsCharging", Prop::Bool(false)),
            ("ExternalConnected", Prop::Bool(true)),
            ("Amperage", Prop::Int(amp)),
            ("Temperature", Prop::Int(3050)),
            ("CycleCount", Prop::Int(42)),
        ])
    }

    #[test]
    fn parses_apple_silicon_percent() {
        let s = parse_snapshot(&base(81, 100, -100)).unwrap();
        assert_eq!(
            s,
            BatterySnapshot {
                percent: 81,
                is_charging: false,
                external_connected: true,
                amperage_ma: -100,
                temperature_c: Some(30.5),
                cycle_count: Some(42),
                update_time: None
            }
        );
    }

    #[test]
    fn amperage_wraps_u32() {
        assert_eq!(parse_snapshot(&base(81, 100, 4294967196)).unwrap().amperage_ma, -100);
    }

    #[test]
    fn computes_percent_from_mah() {
        assert_eq!(parse_snapshot(&base(4000, 5000, 0)).unwrap().percent, 80);
    }

    #[test]
    fn reads_update_time() {
        let mut p = base(81, 100, 0);
        p.insert("UpdateTime".into(), Prop::Int(1_794_840_390));
        assert_eq!(parse_snapshot(&p).unwrap().update_time, Some(1_794_840_390));
    }

    #[test]
    fn temperature_is_optional() {
        let mut p = base(81, 100, 0);
        p.remove("Temperature");
        assert_eq!(parse_snapshot(&p).unwrap().temperature_c, None);
        p.insert("VirtualTemperature".into(), Prop::Int(2990));
        assert_eq!(parse_snapshot(&p).unwrap().temperature_c, Some(29.9));
    }

    #[test]
    fn missing_prop_is_error() {
        let r = parse_snapshot(&props(&[("CurrentCapacity", Prop::Int(50))]));
        assert!(matches!(r, Err(BatteryError::MissingProp(_))));
    }
}
