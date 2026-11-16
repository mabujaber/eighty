//! The SMC keys eighty knows about (spec §5.2, battery#482).
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Role {
    InhibitCharging,
    ForceDischarge,
    Light,
}

/// A key the probe may write. For `Light`, `on` is green and `off` is orange.
/// Restores always use the original value read, never `off`.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub backend: &'static str,
    pub role: Role,
    pub key: &'static str,
    pub on: &'static [u8],
    pub off: &'static [u8],
}

const fn c(backend: &'static str, role: Role, key: &'static str, on: &'static [u8], off: &'static [u8]) -> Candidate {
    Candidate { backend, role, key, on, off }
}

pub const CANDIDATES: &[Candidate] = &[
    c("legacy", Role::InhibitCharging, "CH0B", &[0x02], &[0x00]),
    c("legacy", Role::InhibitCharging, "CH0C", &[0x02], &[0x00]),
    c("legacy", Role::ForceDischarge, "CH0I", &[0x01], &[0x00]),
    c("tahoe", Role::InhibitCharging, "CHTE", &[0x01, 0, 0, 0], &[0, 0, 0, 0]),
    c("tahoe", Role::ForceDischarge, "CHIE", &[0x08], &[0x00]),
    c("tahoe", Role::ForceDischarge, "CH0J", &[0x01], &[0x00]),
    c("magsafe", Role::Light, "ACLC", &[0x03], &[0x04]),
];

/// Keys read for presence and value only; their write semantics are unknown.
pub const READ_ONLY_KEYS: &[&str] = &["bfD0", "bfE0", "bfF0", "CHIB"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_spec_keys() {
        let keys: Vec<_> = CANDIDATES.iter().map(|c| c.key).collect();
        assert_eq!(keys, ["CH0B", "CH0C", "CH0I", "CHTE", "CHIE", "CH0J", "ACLC"]);
        assert!(CANDIDATES.iter().all(|c| c.on.len() == c.off.len()));
    }
}
