//! Shared types for the eighty helper and CLI. No hardware or OS calls.
pub mod config;
pub mod paths;

pub use config::{Config, Settings};

/// Never discharge from battery at or below this charge percentage.
pub const HARD_FLOOR: u8 = 10;
/// Seconds to wait after a write before judging its effect.
pub const SETTLE_SECS: u64 = 20;
/// Seconds between control-loop ticks.
pub const TICK_SECS: u64 = 60;
