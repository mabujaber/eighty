//! System Management Controller access.
mod conn;
pub(crate) mod ffi;
mod key;
#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use conn::SmcConnection;
pub use key::{hex, FourCC};

/// Size, type and attributes of an SMC key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInfo {
    pub size: u32,
    pub data_type: FourCC,
    pub attributes: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum SmcError {
    #[error("invalid SMC key {0:?}: must be exactly 4 ASCII characters")]
    InvalidKey(String),
    #[error("SMC key {0} not found")]
    KeyNotFound(String),
    #[error("SMC write not privileged (kIOReturnNotPrivileged)")]
    NotPrivileged,
    #[error("SMC key {key} is {expected} bytes, got {got}")]
    SizeMismatch { key: String, expected: u32, got: usize },
    #[error("AppleSMC service not found")]
    ServiceNotFound,
    #[error("IOKit error 0x{0:08x}")]
    IoKit(i32),
}

/// Raw access to SMC keys.
///
/// `write` must return `SizeMismatch` without writing when `bytes.len()`
/// differs from the key's size.
pub trait SmcIo {
    fn key_info(&mut self, key: FourCC) -> Result<KeyInfo, SmcError>;
    fn read(&mut self, key: FourCC) -> Result<Vec<u8>, SmcError>;
    fn write(&mut self, key: FourCC, bytes: &[u8]) -> Result<(), SmcError>;
}
