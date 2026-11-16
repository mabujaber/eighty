//! Battery state from the AppleSmartBattery IORegistry entry.
mod ioreg;
mod parse;
#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use ioreg::IoRegBattery;
pub use parse::{parse_snapshot, Prop};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BatterySnapshot {
    pub percent: u8,
    pub is_charging: bool,
    pub external_connected: bool,
    pub amperage_ma: i32,
    /// macOS 27 no longer reports `Temperature` on some Macs (seen on an M2 Max).
    pub temperature_c: Option<f32>,
    pub cycle_count: Option<u32>,
    /// When macOS last refreshed these readings (Unix seconds, `UpdateTime`).
    /// macOS refreshes about once a minute, so a reading can be up to ~60 s old.
    pub update_time: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum BatteryError {
    #[error("AppleSmartBattery not found")]
    NotFound,
    #[error("battery property {0} missing")]
    MissingProp(&'static str),
    #[error("IOKit error 0x{0:08x}")]
    IoKit(i32),
}

pub trait BatterySource {
    fn snapshot(&mut self) -> Result<BatterySnapshot, BatteryError>;
}
