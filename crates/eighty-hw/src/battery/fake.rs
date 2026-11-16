//! Scripted battery for tests.
use super::{BatteryError, BatterySnapshot, BatterySource};

/// Returns whatever the closure produces on each call.
pub struct FakeBattery {
    f: Box<dyn FnMut() -> BatterySnapshot>,
}

impl FakeBattery {
    pub fn new(f: impl FnMut() -> BatterySnapshot + 'static) -> FakeBattery {
        FakeBattery { f: Box::new(f) }
    }
}

impl BatterySource for FakeBattery {
    fn snapshot(&mut self) -> Result<BatterySnapshot, BatteryError> {
        Ok((self.f)())
    }
}
