use std::fmt;

use super::SmcError;

/// A four-character SMC key, packed big-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCC(pub u32);

impl FourCC {
    pub fn new(s: &str) -> Result<FourCC, SmcError> {
        let b = s.as_bytes();
        if b.len() != 4 || !s.is_ascii() {
            return Err(SmcError::InvalidKey(s.to_string()));
        }
        Ok(FourCC(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
    }

    pub fn as_str(&self) -> String {
        self.0.to_be_bytes().iter().map(|&c| c as char).collect()
    }
}

impl fmt::Display for FourCC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// Lowercase hex with no separators.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smc::SmcError;

    #[test]
    fn fourcc_roundtrip() {
        let k = FourCC::new("CHTE").unwrap();
        assert_eq!(k.0, 0x4348_5445);
        assert_eq!(k.as_str(), "CHTE");
    }

    #[test]
    fn fourcc_rejects_bad_length() {
        assert!(matches!(FourCC::new("CH0"), Err(SmcError::InvalidKey(_))));
        assert!(matches!(FourCC::new("CH0BX"), Err(SmcError::InvalidKey(_))));
    }

    #[test]
    fn hex_formats() {
        assert_eq!(hex(&[1, 0, 0, 0]), "01000000");
        assert_eq!(hex(&[]), "");
    }
}
