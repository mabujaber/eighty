//! Shared types for the eighty helper and CLI. No hardware or OS calls.
pub mod config;
pub mod limiters;
pub mod paths;
pub mod policy;
pub mod proto;

pub use config::{Config, Settings};

/// Never discharge from battery at or below this charge percentage.
pub const HARD_FLOOR: u8 = 10;
/// Seconds to wait after a write before judging its effect.
pub const SETTLE_SECS: u64 = 20;
/// Consecutive failing watchdog readings, each with a distinct update time, before a fault.
pub const WATCHDOG_STRIKES: u32 = 2;
/// Seconds between control-loop ticks.
pub const TICK_SECS: u64 = 60;
