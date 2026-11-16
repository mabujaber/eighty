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
    pub temperature_c: f32,
    pub cycle_count: u32,
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
