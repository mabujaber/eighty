//! SMC control: key detection, applying desired outputs, conflict check, restore.
use std::collections::HashMap;

use eighty_core::policy::{ControlMode, Light, Outputs};
use eighty_core::proto::{Health, KeyState};
use eighty_hw::smc::{hex, FourCC, SmcError, SmcIo};

const CHTE: &str = "CHTE";
const CHIE: &str = "CHIE";
/// The charge-light key; failures writing it are only warnings.
pub const ACLC: &str = "ACLC";

const CHTE_STOP: [u8; 4] = [1, 0, 0, 0];
const CHTE_ALLOW: [u8; 4] = [0; 4];
const CHIE_BATTERY: [u8; 1] = [CHIE_DISCHARGE_BIT];
/// The `CHIE` bit eighty owns. Firmware may set other bits (seen: `0x10` after a
/// power-source change), so ownership is judged by this bit alone.
const CHIE_DISCHARGE_BIT: u8 = 0x08;
const CHIE_OFF: [u8; 1] = [0];

/// Keys eighty uses, with the size each must have.
const KEYS: [(&str, u32); 3] = [(CHTE, 4), (CHIE, 1), (ACLC, 1)];

/// Result of probing the SMC for usable keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub mode: ControlMode,
    pub light: bool,
    pub keys: Vec<(String, KeyState)>,
}

fn fourcc(key: &str) -> FourCC {
    FourCC::new(key).expect("static key names are valid")
}

fn state_of(err: &SmcError) -> KeyState {
    match err {
        SmcError::KeyNotFound(_) => KeyState::Missing,
        SmcError::NotPrivileged => KeyState::Blocked,
        e => KeyState::Error(e.to_string()),
    }
}

fn probe_key(smc: &mut dyn SmcIo, key: &str, size: u32, write_back: bool) -> KeyState {
    let k = fourcc(key);
    match smc.key_info(k) {
        Err(e) => return state_of(&e),
        Ok(info) if info.size != size => return KeyState::WrongSize { size: info.size },
        Ok(_) => {}
    }
    let value = match smc.read(k) {
        Ok(v) => v,
        Err(e) => return state_of(&e),
    };
    if write_back {
        if let Err(e) = smc.write(k, &value) {
            return state_of(&e);
        }
    }
    KeyState::Usable
}

fn detect_with(smc: &mut dyn SmcIo, write_back: bool) -> Detection {
    let keys: Vec<(String, KeyState)> = KEYS
        .iter()
        .map(|&(key, size)| (key.to_string(), probe_key(smc, key, size, write_back)))
        .collect();
    let usable = |name: &str| keys.iter().any(|(k, s)| k == name && *s == KeyState::Usable);
    let mode = match (usable(CHTE), usable(CHIE)) {
        (true, true) => ControlMode::Hold,
        (false, true) => ControlMode::Cycle,
        _ => ControlMode::Unsupported,
    };
    let light = usable(ACLC);
    Detection { mode, light, keys }
}

/// Probe each key, including a write-back of its current value.
pub fn detect(smc: &mut dyn SmcIo) -> Detection {
    detect_with(smc, true)
}

/// Like `detect` but never writes; used while other limiters are present.
pub fn detect_read_only(smc: &mut dyn SmcIo) -> Detection {
    detect_with(smc, false)
}

pub struct SmcControl {
    mode: ControlMode,
    light: bool,
    last: HashMap<&'static str, Vec<u8>>,
}

fn light_byte(l: Light) -> u8 {
    match l {
        Light::Green => 3,
        Light::Orange => 4,
        Light::System => 0,
    }
}

impl SmcControl {
    pub fn new(d: &Detection) -> SmcControl {
        SmcControl { mode: d.mode, light: d.light, last: HashMap::new() }
    }

    /// Write the desired state. Returns true if a charging key (`CHTE`/`CHIE`) was written.
    pub fn apply(&mut self, smc: &mut dyn SmcIo, o: &Outputs) -> Result<bool, (String, SmcError)> {
        let mut desired: Vec<(&'static str, Vec<u8>)> = Vec::new();
        if self.mode == ControlMode::Hold {
            let v = if o.inhibit { CHTE_STOP } else { CHTE_ALLOW };
            desired.push((CHTE, v.to_vec()));
        }
        if self.mode != ControlMode::Unsupported {
            let v = if o.discharge { CHIE_BATTERY } else { CHIE_OFF };
            desired.push((CHIE, v.to_vec()));
        }

        let mut wrote = false;
        for (key, bytes) in desired {
            if self.last.get(key) == Some(&bytes) {
                continue;
            }
            smc.write(fourcc(key), &bytes).map_err(|e| (key.to_string(), e))?;
            self.last.insert(key, bytes);
            wrote = true;
        }

        if let (true, Some(l)) = (self.light, o.light) {
            let want = [light_byte(l)];
            let k = fourcc(ACLC);
            let current = smc.read(k).map_err(|e| (ACLC.to_string(), e))?;
            if current != want {
                smc.write(k, &want).map_err(|e| (ACLC.to_string(), e))?;
            }
        }
        Ok(wrote)
    }

    /// Verify `CHTE`/`CHIE` still hold the values we last wrote: `CHTE` exactly,
    /// `CHIE` by our discharge bit only.
    pub fn check_unchanged(&self, smc: &mut dyn SmcIo) -> Result<(), Health> {
        for key in [CHTE, CHIE] {
            let Some(expected) = self.last.get(key) else { continue };
            let found = smc
                .read(fourcc(key))
                .map_err(|e| Health::WriteFailed { key: key.to_string(), error: e.to_string() })?;
            let unchanged = if key == CHIE {
                found.len() == expected.len() && chie_bit(&found) == chie_bit(expected)
            } else {
                &found == expected
            };
            if !unchanged {
                return Err(Health::Conflict {
                    key: key.to_string(),
                    expected: hex(expected),
                    found: hex(&found),
                });
            }
        }
        Ok(())
    }

    /// After a conflict: if we last wrote `CHIE=08` and it still has our bit, write
    /// `CHIE=00` so a plugged-in Mac does not drain with nothing enforcing the floor.
    /// Writes nothing else. Returns true if it wrote.
    pub fn stop_discharge_if_ours(&mut self, smc: &mut dyn SmcIo) -> Result<bool, SmcError> {
        if self.last.get(CHIE).map(Vec::as_slice) != Some(&CHIE_BATTERY[..]) {
            return Ok(false);
        }
        let k = fourcc(CHIE);
        if !chie_bit(&smc.read(k)?) {
            return Ok(false); // someone else owns CHIE now
        }
        smc.write(k, &CHIE_OFF)?;
        self.last.insert(CHIE, CHIE_OFF.to_vec());
        Ok(true)
    }

    /// Write the restore state to the present keys and forget what we wrote.
    pub fn restore(&mut self, smc: &mut dyn SmcIo) -> Vec<(String, SmcError)> {
        self.last.clear();
        restore_all(smc)
    }
}

/// Whether a `CHIE` value has our discharge bit set.
fn chie_bit(v: &[u8]) -> bool {
    v.first().is_some_and(|b| b & CHIE_DISCHARGE_BIT != 0)
}

/// Write the restore state (`CHTE=0`, `CHIE=0`, `ACLC=0`) to every key that exists.
pub fn restore_all(smc: &mut dyn SmcIo) -> Vec<(String, SmcError)> {
    let mut errors = Vec::new();
    for (key, size) in KEYS {
        let k = fourcc(key);
        match smc.key_info(k) {
            Err(SmcError::KeyNotFound(_)) => continue,
            Err(e) => {
                errors.push((key.to_string(), e));
                continue;
            }
            Ok(_) => {}
        }
        if let Err(e) = smc.write(k, &vec![0u8; size as usize]) {
            errors.push((key.to_string(), e));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};

    fn hold_smc() -> FakeSmc { let s = FakeSmc::new(); s.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept); s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); s }
    fn cycle_smc() -> FakeSmc { let s = FakeSmc::new(); s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); s }
    fn on(inhibit: bool, discharge: bool, light: Option<Light>) -> Outputs { Outputs { inhibit, discharge, light } }

    #[test] fn detects_hold_cycle_unsupported() {
        assert_eq!(detect(&mut hold_smc()).mode, ControlMode::Hold);
        assert_eq!(detect(&mut cycle_smc()).mode, ControlMode::Cycle);
        let m2 = FakeSmc::new(); m2.insert("CHIE", "ui8 ", &[0], WriteMode::NotPrivileged);
        let d = detect(&mut m2.clone());
        assert_eq!(d.mode, ControlMode::Unsupported);
        assert!(d.keys.contains(&("CHIE".into(), KeyState::Blocked)));
        assert!(d.keys.contains(&("CHTE".into(), KeyState::Missing)));
    }
    #[test] fn detect_writes_back_same_value_only() {
        let s = hold_smc(); s.insert("CHIE", "ui8 ", &[8], WriteMode::Accept);
        detect(&mut s.clone());
        assert_eq!(s.value("CHIE"), Some(vec![8]));
    }
    #[test] fn apply_hold_writes_exact_bytes_once() {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        let before = s.writes().len();
        assert!(c.apply(&mut smc, &on(true, false, Some(Light::Green))).unwrap());
        assert_eq!(s.value("CHTE"), Some(vec![1, 0, 0, 0]));
        assert_eq!(s.value("ACLC"), Some(vec![3]));
        let n = s.writes().len();
        assert!(!c.apply(&mut smc, &on(true, false, Some(Light::Green))).unwrap());
        assert_eq!(s.writes().len(), n);
        assert!(n > before);
    }
    #[test] fn apply_cycle_never_touches_chte_and_reasserts_light() {
        let s = cycle_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap();
        assert_eq!(s.value("CHIE"), Some(vec![8]));
        s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); // macOS reset the light
        c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap();
        assert_eq!(s.value("ACLC"), Some(vec![3]));
    }
    #[test] fn detect_read_only_never_writes() {
        let s = hold_smc(); assert_eq!(detect_read_only(&mut s.clone()).mode, ControlMode::Hold);
        assert!(s.writes().is_empty());
    }
    #[test] fn external_change_is_conflict() {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        c.apply(&mut smc, &on(true, false, None)).unwrap();
        s.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
        assert!(matches!(c.check_unchanged(&mut smc), Err(Health::Conflict { ref key, .. }) if key == "CHTE"));
    }
    fn check_chie(expected_discharge: bool, found: u8) -> Result<(), Health> {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        c.apply(&mut smc, &on(true, expected_discharge, None)).unwrap();
        s.insert("CHIE", "ui8 ", &[found], WriteMode::Accept);
        c.check_unchanged(&mut smc)
    }
    #[test] fn chie_ownership_is_judged_by_the_discharge_bit() {
        assert_eq!(check_chie(true, 0x18), Ok(())); // firmware added a bit: still ours
        assert_eq!(check_chie(false, 0x10), Ok(()));
        for (dis, found) in [(true, 0x10), (true, 0x00), (false, 0x08)] {
            assert!(matches!(check_chie(dis, found), Err(Health::Conflict { ref key, .. }) if key == "CHIE"), "{dis} {found:#x}");
        }
    }
    #[test] fn chte_comparison_stays_exact() {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        c.apply(&mut smc, &on(true, false, None)).unwrap();
        s.insert("CHTE", "ui32", &[1, 0, 0, 8], WriteMode::Accept);
        assert!(matches!(c.check_unchanged(&mut smc), Err(Health::Conflict { ref key, .. }) if key == "CHTE"));
    }
    #[test] fn stop_discharge_if_ours_with_extra_firmware_bits() {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        c.apply(&mut smc, &on(true, true, None)).unwrap();
        s.insert("CHIE", "ui8 ", &[0x18], WriteMode::Accept);
        assert!(c.stop_discharge_if_ours(&mut smc).unwrap());
        assert_eq!(s.value("CHIE"), Some(vec![0]));
        assert_eq!(s.writes().last(), Some(&("CHIE".to_string(), vec![0])));
    }
    #[test] fn restore_writes_restore_state() {
        let s = hold_smc(); s.insert("CHTE", "ui32", &[1, 0, 0, 0], WriteMode::Accept); s.insert("CHIE", "ui8 ", &[8], WriteMode::Accept);
        assert!(restore_all(&mut s.clone()).is_empty());
        assert_eq!((s.value("CHTE"), s.value("CHIE"), s.value("ACLC")), (Some(vec![0; 4]), Some(vec![0]), Some(vec![0])));
    }
    #[test] fn write_error_names_key() {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        s.insert("CHTE", "ui32", &[0; 4], WriteMode::NotPrivileged);
        assert_eq!(c.apply(&mut smc, &on(true, false, None)).unwrap_err().0, "CHTE");
    }
}
