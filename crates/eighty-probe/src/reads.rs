//! Read-only probe: which keys exist and what they hold.
use eighty_hw::smc::{hex, FourCC, SmcError, SmcIo};
use serde::Serialize;

use crate::catalog::{CANDIDATES, READ_ONLY_KEYS};

#[derive(Debug, Serialize)]
pub struct KeyReading {
    pub key: String,
    pub status: ReadStatus,
}

#[derive(Debug, Serialize)]
pub enum ReadStatus {
    Present { size: u32, data_type: String, value_hex: String },
    Missing,
    Error(String),
}

fn read_one(smc: &mut dyn SmcIo, key: &str) -> ReadStatus {
    let result = FourCC::new(key).and_then(|k| {
        let info = smc.key_info(k)?;
        let value = smc.read(k)?;
        Ok((info, value))
    });
    match result {
        Ok((info, value)) => ReadStatus::Present {
            size: info.size,
            data_type: info.data_type.as_str(),
            value_hex: hex(&value),
        },
        Err(SmcError::KeyNotFound(_)) => ReadStatus::Missing,
        Err(e) => ReadStatus::Error(e.to_string()),
    }
}

/// Reads every catalog key, then every read-only key, deduplicated, in order.
pub fn probe_reads(smc: &mut dyn SmcIo) -> Vec<KeyReading> {
    let mut seen = Vec::new();
    for key in CANDIDATES.iter().map(|c| c.key).chain(READ_ONLY_KEYS.iter().copied()) {
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    seen.into_iter()
        .map(|key| KeyReading { key: key.to_string(), status: read_one(smc, key) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};

    #[test]
    fn reads_present_and_missing() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        smc.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
        let r = probe_reads(&mut smc.clone());
        assert_eq!(r.len(), 11);
        let chte = r.iter().find(|k| k.key == "CHTE").unwrap();
        assert!(matches!(&chte.status, ReadStatus::Present { size: 4, value_hex, .. } if value_hex == "00000000"));
        assert!(matches!(r.iter().find(|k| k.key == "CH0B").unwrap().status, ReadStatus::Missing));
        assert!(smc.writes().is_empty());
    }
}
