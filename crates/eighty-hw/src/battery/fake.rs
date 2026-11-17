//! Scripted battery for tests.
use super::{BatteryError, BatterySnapshot, BatterySource};

/// Returns whatever the closure produces on each call.
pub struct FakeBattery {
    f: Box<dyn FnMut() -> Result<BatterySnapshot, BatteryError>>,
}

impl FakeBattery {
    pub fn new(mut f: impl FnMut() -> BatterySnapshot + 'static) -> FakeBattery {
        FakeBattery { f: Box::new(move || Ok(f())) }
    }

    /// Like `new`, but the closure may fail to simulate read errors.
    pub fn fallible(f: impl FnMut() -> Result<BatterySnapshot, BatteryError> + 'static) -> FakeBattery {
        FakeBattery { f: Box::new(f) }
    }
}

impl BatterySource for FakeBattery {
    fn snapshot(&mut self) -> Result<BatterySnapshot, BatteryError> {
        (self.f)()
    }
}
