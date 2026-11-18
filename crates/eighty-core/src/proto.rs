//! IPC protocol between the CLI and the helper: one JSON message per line.
use crate::config::Settings;
use crate::policy::{Activity, ControlMode, Override};
use eighty_hw::machine::MachineInfo;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};

pub const MAX_LINE: usize = 64 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    Doctor,
    SetLimits { upper: u8, lower: Option<u8> },
    SetAutoDischarge { on: bool },
    ChargeFull,
    ChargeAuto,
    DischargeTo { percent: u8 },
    ClearFault,
    AcceptOtherLimiters,
}

impl Request {
    pub fn is_mutating(&self) -> bool {
        !matches!(self, Request::Status | Request::Doctor)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Health {
    Ok,
    Unsupported,
    OtherLimiters(Vec<String>),
    Conflict { key: String, expected: String, found: String },
    WriteFailed { key: String, error: String },
    NotEffective { what: String },
    BatteryUnreadable(String),
}

impl Health {
    pub fn is_ok(&self) -> bool {
        matches!(self, Health::Ok)
    }

    pub fn message(&self) -> String {
        match self {
            Health::Ok => "ok".to_string(),
            Health::Unsupported => "this Mac has no SMC key eighty can use to control charging. Run `eighty doctor` and report it.".to_string(),
            Health::OtherLimiters(names) => format!(
                "another charge limiter is installed ({}). Remove it, or run `eighty doctor --accept-other-limiters`.",
                names.join(", ")
            ),
            Health::Conflict { key, expected, found } => format!(
                "{key} was changed by something else (expected {expected}, found {found}). Another charge limiter or macOS's own Charge Limit may be active. eighty has stopped writing; fix it, then run `eighty doctor --retry`."
            ),
            Health::WriteFailed { key, error } => format!(
                "writing {key} failed: {error}. Charging restored. Run `eighty doctor --retry`."
            ),
            Health::NotEffective { what } => format!(
                "{what} had no effect on this Mac. Charging restored. Run `eighty doctor --retry`."
            ),
            Health::BatteryUnreadable(e) => {
                format!("can't read the battery: {e}. Charging restored; retrying.")
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Status {
    pub percent: u8,
    pub external: bool,
    pub charging: bool,
    pub amperage_ma: i32,
    /// The reading predates the last charging switch, so `charging` and `amperage_ma`
    /// still show the old state.
    #[serde(default)]
    pub amperage_stale: bool,
    pub mode: ControlMode,
    pub activity: Activity,
    pub health: Health,
    pub settings: Settings,
    #[serde(rename = "override")]
    pub override_: Override,
    pub light_supported: bool,
    pub warnings: Vec<String>,
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum KeyState {
    Usable,
    Missing,
    Blocked,
    WrongSize { size: u32 },
    Error(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DoctorReport {
    pub machine: MachineInfo,
    pub keys: Vec<(String, KeyState)>,
    pub status: Status,
    pub other_limiters: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Response {
    Status(Status),
    Doctor(Box<DoctorReport>),
    Error(String),
}

/// Write one JSON message followed by a newline.
pub fn write_msg<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Read one JSON line of at most `MAX_LINE` bytes.
pub fn read_msg<T: DeserializeOwned>(r: &mut impl BufRead) -> io::Result<T> {
    let mut buf = Vec::new();
    r.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if buf.is_empty() {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if buf.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too long"));
    }
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape() {
        assert_eq!(
            serde_json::to_string(&Request::SetLimits { upper: 80, lower: None }).unwrap(),
            r#"{"op":"set_limits","upper":80,"lower":null}"#
        );
    }
    #[test]
    fn framing_roundtrip() {
        let mut buf = Vec::new();
        write_msg(&mut buf, &Request::DischargeTo { percent: 50 }).unwrap();
        assert!(buf.ends_with(b"\n"));
        let r: Request = read_msg(&mut &buf[..]).unwrap();
        assert_eq!(r, Request::DischargeTo { percent: 50 });
    }
    #[test]
    fn rejects_overlong_line() {
        let line = vec![b'a'; MAX_LINE + 10];
        let e = read_msg::<Request>(&mut &line[..]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
    #[test]
    fn only_status_and_doctor_are_read_only() {
        assert!(!Request::Status.is_mutating() && !Request::Doctor.is_mutating());
        assert!(Request::ChargeAuto.is_mutating());
    }
    #[test]
    fn status_json_names_override_field() {
        let s = Status {
            percent: 80,
            external: true,
            charging: false,
            amperage_ma: 0,
            amperage_stale: false,
            mode: ControlMode::Hold,
            activity: Activity::Holding,
            health: Health::Ok,
            settings: Settings::default(),
            override_: Override::DischargeTo(50),
            light_supported: true,
            warnings: vec![],
            version: "0.1.0".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["override"], serde_json::json!({ "DischargeTo": 50 }));
        assert!(v.get("override_").is_none());
        assert_eq!(serde_json::from_value::<Status>(v).unwrap(), s);
    }
    #[test]
    fn conflict_message_mentions_native_limit() {
        let h = Health::Conflict {
            key: "CHTE".into(),
            expected: "01000000".into(),
            found: "00000000".into(),
        };
        assert!(h.message().contains("macOS's own Charge Limit"));
    }
}
