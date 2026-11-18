//! In-memory native limit for tests.
use std::cell::RefCell;
use std::rc::Rc;

use super::{NativeError, NativeLimit};

#[derive(Debug)]
struct State {
    supported: bool,
    available: Vec<u8>,
    limit: u8,
    fail: bool,
    writes: Vec<u8>,
    /// Reads after each write that still return the value from before it.
    lag: usize,
    lag_left: usize,
    before_write: u8,
    /// While set, every read returns this value, whatever was written.
    frozen: Option<u8>,
}

/// Clones share state, so a test can keep a handle while the code under test owns another.
#[derive(Debug, Clone)]
pub struct FakeNative(Rc<RefCell<State>>);

impl FakeNative {
    pub fn new(available: &[u8], limit: u8) -> Self {
        FakeNative(Rc::new(RefCell::new(State {
            supported: true,
            available: available.to_vec(),
            limit,
            fail: false,
            writes: Vec::new(),
            lag: 0,
            lag_left: 0,
            before_write: limit,
            frozen: None,
        })))
    }

    pub fn unsupported() -> Self {
        let f = Self::new(&[], 100);
        f.0.borrow_mut().supported = false;
        f
    }

    pub fn limit_now(&self) -> u8 {
        self.0.borrow().limit
    }

    /// Changes the limit as if the user did it in System Settings (not recorded as a write).
    pub fn set_externally(&self, percent: u8) {
        let mut s = self.0.borrow_mut();
        s.limit = percent;
        s.lag_left = 0;
    }

    /// From the next write on, the first `n` reads after each write still return the
    /// value from before it, as PowerUI may do for a moment.
    pub fn lag_reads(&self, n: usize) {
        self.0.borrow_mut().lag = n;
    }

    /// While `Some(v)`, every read returns `v` however often eighty writes: PowerUI
    /// accepting `setMCLLimit` but never reporting the new value. `None` ends it.
    pub fn freeze_reads(&self, value: Option<u8>) {
        self.0.borrow_mut().frozen = value;
    }

    pub fn fail_calls(&self, fail: bool) {
        self.0.borrow_mut().fail = fail;
    }

    pub fn writes(&self) -> Vec<u8> {
        self.0.borrow().writes.clone()
    }

    fn check(&self) -> Result<(), NativeError> {
        let s = self.0.borrow();
        if !s.supported {
            Err(NativeError::Unavailable)
        } else if s.fail {
            Err(NativeError::Call("fake native call failed".into()))
        } else {
            Ok(())
        }
    }
}

impl NativeLimit for FakeNative {
    fn supported(&mut self) -> bool {
        self.0.borrow().supported
    }

    fn available(&mut self) -> Result<Vec<u8>, NativeError> {
        self.check()?;
        Ok(self.0.borrow().available.clone())
    }

    fn limit(&mut self) -> Result<u8, NativeError> {
        self.check()?;
        let mut s = self.0.borrow_mut();
        if let Some(v) = s.frozen {
            return Ok(v);
        }
        if s.lag_left > 0 {
            s.lag_left -= 1;
            return Ok(s.before_write);
        }
        Ok(s.limit)
    }

    fn enabled(&mut self) -> Result<bool, NativeError> {
        self.check()?;
        Ok(self.0.borrow().limit < 100)
    }

    fn set_limit(&mut self, percent: u8) -> Result<(), NativeError> {
        self.check()?;
        let mut s = self.0.borrow_mut();
        s.before_write = s.limit;
        s.limit = percent;
        s.lag_left = s.lag;
        s.writes.push(percent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_records_writes_and_external_changes() {
        let mut f = FakeNative::new(&[80, 85, 90, 95, 100], 100);
        f.set_limit(80).unwrap();
        assert_eq!((f.limit().unwrap(), f.writes()), (80, vec![80]));
        f.set_externally(90);
        assert_eq!(f.limit().unwrap(), 90);
        assert!(!FakeNative::unsupported().supported());
    }

    #[test]
    fn fake_lagging_reads_return_the_value_before_the_write() {
        let mut f = FakeNative::new(&[80, 85, 90, 95, 100], 80);
        f.lag_reads(2);
        f.set_limit(85).unwrap();
        assert_eq!([f.limit().unwrap(), f.limit().unwrap(), f.limit().unwrap()], [80, 80, 85]);
        assert_eq!(f.limit_now(), 85);
    }

    #[test]
    fn fake_frozen_reads_ignore_writes_until_cleared() {
        let mut f = FakeNative::new(&[80, 85, 90, 95, 100], 100);
        f.freeze_reads(Some(100));
        f.set_limit(80).unwrap();
        f.set_limit(80).unwrap();
        assert_eq!((f.limit().unwrap(), f.limit_now()), (100, 80));
        f.freeze_reads(None);
        assert_eq!(f.limit().unwrap(), 80);
    }
}
