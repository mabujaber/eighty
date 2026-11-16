//! In-memory SMC for tests.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::{FourCC, KeyInfo, SmcError, SmcIo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Accept,
    /// Writes are rejected with kIOReturnNotPrivileged.
    NotPrivileged,
    /// The first write succeeds; later writes fail (simulates a failed restore).
    AcceptOnce,
    /// Even key_info and read are rejected with kIOReturnNotPrivileged.
    InfoNotPrivileged,
}

#[derive(Debug, Clone)]
struct FakeKey {
    data_type: String,
    value: Vec<u8>,
    mode: WriteMode,
    written: bool,
}

#[derive(Debug, Default)]
struct State {
    keys: BTreeMap<String, FakeKey>,
    writes: Vec<(String, Vec<u8>)>,
}

/// Fake SMC. Clones share state so tests can observe what code under test did.
#[derive(Debug, Clone, Default)]
pub struct FakeSmc {
    state: Rc<RefCell<State>>,
}

impl FakeSmc {
    pub fn new() -> FakeSmc {
        FakeSmc::default()
    }

    pub fn insert(&self, key: &str, data_type: &str, value: &[u8], mode: WriteMode) {
        self.state.borrow_mut().keys.insert(
            key.to_string(),
            FakeKey { data_type: data_type.to_string(), value: value.to_vec(), mode, written: false },
        );
    }

    pub fn value(&self, key: &str) -> Option<Vec<u8>> {
        self.state.borrow().keys.get(key).map(|k| k.value.clone())
    }

    /// Successful writes, in order.
    pub fn writes(&self) -> Vec<(String, Vec<u8>)> {
        self.state.borrow().writes.clone()
    }

    fn get(&self, key: FourCC) -> Result<FakeKey, SmcError> {
        let k = self
            .state
            .borrow()
            .keys
            .get(&key.as_str())
            .cloned()
            .ok_or_else(|| SmcError::KeyNotFound(key.as_str()))?;
        if k.mode == WriteMode::InfoNotPrivileged {
            return Err(SmcError::NotPrivileged);
        }
        Ok(k)
    }
}

impl SmcIo for FakeSmc {
    fn key_info(&mut self, key: FourCC) -> Result<KeyInfo, SmcError> {
        let k = self.get(key)?;
        Ok(KeyInfo {
            size: k.value.len() as u32,
            data_type: FourCC::new(&k.data_type)?,
            attributes: 0,
        })
    }

    fn read(&mut self, key: FourCC) -> Result<Vec<u8>, SmcError> {
        Ok(self.get(key)?.value)
    }

    fn write(&mut self, key: FourCC, bytes: &[u8]) -> Result<(), SmcError> {
        let k = self.get(key)?;
        if bytes.len() != k.value.len() {
            return Err(SmcError::SizeMismatch {
                key: key.as_str(),
                expected: k.value.len() as u32,
                got: bytes.len(),
            });
        }
        if k.mode == WriteMode::NotPrivileged {
            return Err(SmcError::NotPrivileged);
        }
        if k.mode == WriteMode::AcceptOnce && k.written {
            return Err(SmcError::IoKit(-1));
        }
        let mut st = self.state.borrow_mut();
        let entry = st.keys.get_mut(&key.as_str()).unwrap();
        entry.value = bytes.to_vec();
        entry.written = true;
        st.writes.push((key.as_str(), bytes.to_vec()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smc::{FourCC, SmcError, SmcIo};

    #[test]
    fn fake_missing_key() {
        let mut s = FakeSmc::new();
        assert!(matches!(s.read(FourCC::new("CH0B").unwrap()), Err(SmcError::KeyNotFound(_))));
    }

    #[test]
    fn fake_size_mismatch_does_not_write() {
        let mut s = FakeSmc::new();
        s.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        let r = s.write(FourCC::new("CHTE").unwrap(), &[1]);
        assert!(matches!(r, Err(SmcError::SizeMismatch { expected: 4, got: 1, .. })));
        assert!(s.writes().is_empty());
    }

    #[test]
    fn fake_not_privileged() {
        let mut s = FakeSmc::new();
        s.insert("CHIB", "ui8 ", &[0], WriteMode::NotPrivileged);
        assert!(matches!(s.write(FourCC::new("CHIB").unwrap(), &[1]), Err(SmcError::NotPrivileged)));
        assert_eq!(s.value("CHIB"), Some(vec![0]));
    }

    #[test]
    fn fake_clone_shares_state() {
        let s = FakeSmc::new();
        s.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
        let mut c = s.clone();
        c.write(FourCC::new("ACLC").unwrap(), &[3]).unwrap();
        assert_eq!(s.value("ACLC"), Some(vec![3]));
        assert_eq!(s.writes(), vec![("ACLC".to_string(), vec![3])]);
    }

    #[test]
    fn fake_accept_once_then_fails() {
        let mut s = FakeSmc::new();
        s.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::AcceptOnce);
        let k = FourCC::new("CHTE").unwrap();
        s.write(k, &[1, 0, 0, 0]).unwrap();
        assert!(matches!(s.write(k, &[0, 0, 0, 0]), Err(SmcError::IoKit(_))));
    }

    #[test]
    fn fake_info_not_privileged() {
        let mut s = FakeSmc::new();
        s.insert("CH0J", "ui8 ", &[0], WriteMode::InfoNotPrivileged);
        assert!(matches!(s.key_info(FourCC::new("CH0J").unwrap()), Err(SmcError::NotPrivileged)));
    }
}
