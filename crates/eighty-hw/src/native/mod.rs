//! macOS's own Charge Limit, through the private `PowerUISmartChargeClient`.
mod powerui;
#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use powerui::PowerUi;
#[cfg(any(test, feature = "fake"))]
pub use fake::FakeNative;

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum NativeError {
    #[error("PowerUI isn't available")]
    Unavailable,
    #[error("{0}")]
    Call(String),
}

pub trait NativeLimit {
    /// Class present, every guarded selector matches, and isMCLSupported is YES.
    fn supported(&mut self) -> bool;
    fn available(&mut self) -> Result<Vec<u8>, NativeError>;
    /// `getMCLLimitWithError:`
    fn limit(&mut self) -> Result<u8, NativeError>;
    /// `isMCLCurrentlyEnabled:` != 0
    fn enabled(&mut self) -> Result<bool, NativeError>;
    /// `setMCLLimit:error:`
    fn set_limit(&mut self, percent: u8) -> Result<(), NativeError>;
}

/// Selectors the native backend calls, with their exact type encodings from
/// the macOS 27.2 runtime. If any differs, the private API has changed and we
/// must not call it.
pub(crate) const GUARDED: [(&str, &str); 5] = [
    ("isMCLSupported", "B16@0:8"),
    ("availableChargeLimitsWithError:", "@24@0:8^@16"),
    ("getMCLLimitWithError:", "C24@0:8^@16"),
    ("isMCLCurrentlyEnabled:", "Q24@0:8^@16"),
    ("setMCLLimit:error:", "B28@0:8C16^@20"),
];

/// True when `found` reports exactly the expected encoding for every guarded selector.
pub fn guard_encodings(found: &dyn Fn(&str) -> Option<String>) -> bool {
    GUARDED
        .iter()
        .all(|(sel, expected)| found(sel).as_deref() == Some(*expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_accepts_the_macos_27_encodings() {
        assert!(guard_encodings(&|sel| GUARDED
            .iter()
            .find(|(s, _)| *s == sel)
            .map(|(_, e)| e.to_string())));
    }

    #[test]
    fn guard_rejects_changed_encoding() {
        assert!(!guard_encodings(&|sel| match sel {
            "setMCLLimit:error:" => Some("B28@0:8Q16^@20".into()), // limit widened to u64
            s => GUARDED.iter().find(|(g, _)| *g == s).map(|(_, e)| e.to_string()),
        }));
        assert!(!guard_encodings(&|_| None));
    }
}
