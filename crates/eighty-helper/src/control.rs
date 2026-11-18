//! SMC control: key detection, applying desired outputs, conflict check, restore.
use std::collections::HashMap;

use eighty_core::policy::{ControlMode, Light, Outputs};
use eighty_core::proto::{Health, KeyState};
use eighty_hw::native::NativeLimit;
use eighty_hw::smc::{hex, FourCC, SmcError, SmcIo};

use crate::log::log;
use crate::store::Store;

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
    /// The macOS Charge Limit steps, ascending; empty unless the mode is Native.
    pub native_steps: Vec<u8>,
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

fn detect_with(smc: &mut dyn SmcIo, native: &mut dyn NativeLimit, write_back: bool) -> Detection {
    let chte = probe_key(smc, CHTE, 4, write_back);
    let chie = probe_key(smc, CHIE, 1, write_back);
    let hold = chte == KeyState::Usable && chie == KeyState::Usable;
    let mut native_steps = Vec::new();
    let mode = if hold {
        ControlMode::Hold
    } else if native.supported() {
        // A failing `available()` on a Mac that supports the Charge Limit is a fault,
        // not "unsupported": stay Native so the first call's error is reported.
        match native.available() {
            Ok(mut steps) => {
                steps.sort_unstable();
                native_steps = steps;
            }
            Err(e) => log(&format!("native: couldn't read the available limits: {e}")),
        }
        ControlMode::Native
    } else {
        ControlMode::Unsupported
    };
    // The light is ours only in Hold mode; otherwise don't even write it back.
    let aclc = probe_key(smc, ACLC, 1, write_back && mode == ControlMode::Hold);
    let light = mode == ControlMode::Hold && aclc == KeyState::Usable;
    let keys = vec![(CHTE.to_string(), chte), (CHIE.to_string(), chie), (ACLC.to_string(), aclc)];
    Detection { mode, light, keys, native_steps }
}

/// Probe each key, including a write-back of its current value, and the native limit.
pub fn detect(smc: &mut dyn SmcIo, native: &mut dyn NativeLimit) -> Detection {
    detect_with(smc, native, true)
}

/// Like `detect` but never writes; used while other limiters are present.
pub fn detect_read_only(smc: &mut dyn SmcIo, native: &mut dyn NativeLimit) -> Detection {
    detect_with(smc, native, false)
}

pub struct SmcControl {
    mode: ControlMode,
    light: bool,
    /// Whether `CHIE` is usable; in Native mode it is optional (only `DischargeTo` uses it).
    chie: bool,
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
    /// Whether `CHIE` is usable, so `DischargeTo` can run from the battery.
    pub fn can_discharge(&self) -> bool {
        self.chie
    }

    pub fn new(d: &Detection) -> SmcControl {
        let chie = d.keys.iter().any(|(k, s)| k == CHIE && *s == KeyState::Usable);
        SmcControl { mode: d.mode, light: d.light && d.mode == ControlMode::Hold, chie, last: HashMap::new() }
    }

    /// Write the desired state. Returns true if a charging key (`CHTE`/`CHIE`) was written.
    /// Native mode writes only `CHIE`: never `CHTE`, never the light.
    pub fn apply(&mut self, smc: &mut dyn SmcIo, o: &Outputs) -> Result<bool, (String, SmcError)> {
        let mut desired: Vec<(&'static str, Vec<u8>)> = Vec::new();
        if self.mode == ControlMode::Hold {
            let v = if o.inhibit { CHTE_STOP } else { CHTE_ALLOW };
            desired.push((CHTE, v.to_vec()));
        }
        if self.mode == ControlMode::Hold || (self.mode == ControlMode::Native && self.chie) {
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

        if let (true, ControlMode::Hold, Some(l)) = (self.light, self.mode, o.light) {
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
    /// Native mode restores only `CHIE`, and only if it is usable: it never touches
    /// `CHTE` or the light, and a blocked `CHIE` was never ours to restore.
    pub fn restore(&mut self, smc: &mut dyn SmcIo) -> Vec<(String, SmcError)> {
        self.last.clear();
        if self.mode == ControlMode::Native {
            if self.chie {
                restore_keys(smc, &[(CHIE, 1)])
            } else {
                Vec::new()
            }
        } else {
            restore_all(smc)
        }
    }
}

/// Whether a `CHIE` value has our discharge bit set.
fn chie_bit(v: &[u8]) -> bool {
    v.first().is_some_and(|b| b & CHIE_DISCHARGE_BIT != 0)
}

/// Read-only: whether `CHIE` exists and currently has our discharge bit set.
pub fn chie_discharging(smc: &mut dyn SmcIo) -> bool {
    smc.read(fourcc(CHIE)).is_ok_and(|v| chie_bit(&v))
}

/// Write the restore state (`CHTE=0`, `CHIE=0`, `ACLC=0`) to every key that exists.
pub fn restore_all(smc: &mut dyn SmcIo) -> Vec<(String, SmcError)> {
    restore_keys(smc, &KEYS)
}

fn restore_keys(smc: &mut dyn SmcIo, keys: &[(&str, u32)]) -> Vec<(String, SmcError)> {
    let mut errors = Vec::new();
    for &(key, size) in keys {
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

/// What `restore_native` did.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeRestore {
    /// Set the native limit back to the recorded original.
    Restored(u8),
    /// No original recorded, or no Charge Limit on this Mac: nothing to put back.
    LeftAsIs,
}

/// Why `restore_native` couldn't put the user's limit back. Either way `--restore`
/// fails, so `eighty uninstall` doesn't report success.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeRestoreError {
    /// The config (and so the recorded original) couldn't be read: the store's warning.
    /// The native limit is left as is.
    ConfigUnreadable(String),
    /// Setting the original back failed.
    SetFailed(String),
}

impl NativeRestoreError {
    /// The line `--restore` prints.
    pub fn message(&self) -> String {
        match self {
            NativeRestoreError::ConfigUnreadable(w) => {
                format!("restore: config unreadable ({w}); native limit left as is")
            }
            NativeRestoreError::SetFailed(e) => format!("restore native: FAILED: {e}"),
        }
    }
}

/// Put the user's own native Charge Limit back (the one recorded when eighty
/// first took control). Does nothing without a record or without Charge Limit.
pub fn restore_native(native: &mut dyn NativeLimit, store: &Store) -> Result<NativeRestore, NativeRestoreError> {
    let (config, warning) = store.load();
    if let Some(w) = warning {
        return Err(NativeRestoreError::ConfigUnreadable(w));
    }
    let Some(original) = config.native_original else { return Ok(NativeRestore::LeftAsIs) };
    if !native.supported() {
        return Ok(NativeRestore::LeftAsIs);
    }
    native
        .set_limit(original)
        .map_err(|e| NativeRestoreError::SetFailed(format!("setting the native limit back to {original}%: {e}")))?;
    Ok(NativeRestore::Restored(original))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_hw::native::FakeNative;
    use eighty_core::config::Config;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};

    fn no_native() -> FakeNative { FakeNative::unsupported() }
    fn detect(smc: &mut dyn SmcIo) -> Detection { super::detect(smc, &mut no_native()) }

    fn hold_smc() -> FakeSmc { let s = FakeSmc::new(); s.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept); s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); s }
    fn native_smc() -> FakeSmc { let s = FakeSmc::new(); s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); s }
    fn on(inhibit: bool, discharge: bool, light: Option<Light>) -> Outputs { Outputs { inhibit, discharge, light } }

    fn mcl() -> FakeNative { FakeNative::new(&[80, 85, 90, 95, 100], 100) }
    #[test] fn detects_hold_native_unsupported() {
        assert_eq!(detect(&mut hold_smc()).mode, ControlMode::Hold);
        assert_eq!(super::detect(&mut hold_smc(), &mut mcl()).mode, ControlMode::Hold); // CHTE wins
        let n = super::detect(&mut native_smc(), &mut mcl());
        assert_eq!((n.mode, n.native_steps, n.light), (ControlMode::Native, vec![80, 85, 90, 95, 100], false));
        let u = detect(&mut native_smc());
        assert_eq!((u.mode, u.native_steps), (ControlMode::Unsupported, vec![]));
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
    #[test] fn apply_reasserts_light_in_hold() {
        let s = hold_smc(); let mut smc = s.clone();
        let mut c = SmcControl::new(&detect(&mut smc));
        c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap();
        s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); // macOS reset the light
        c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap();
        assert_eq!(s.value("ACLC"), Some(vec![3]));
    }
    #[test] fn native_detect_apply_and_restore_write_only_chie() {
        let s = native_smc(); s.insert("CHTE", "ui32", &[0; 4], WriteMode::NotPrivileged); let mut smc = s.clone();
        let mut c = SmcControl::new(&super::detect(&mut smc, &mut mcl()));
        c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap(); // even if asked to
        assert_eq!(s.value("CHIE"), Some(vec![8]));
        assert!(c.restore(&mut smc).is_empty());
        let w = |k: &str, v: &[u8]| (k.to_string(), v.to_vec());
        assert_eq!(s.writes(), vec![w("CHIE", &[0]), w("CHIE", &[8]), w("CHIE", &[0])]);
        assert_eq!(s.value("ACLC"), Some(vec![4]));
    }
    #[test] fn detect_read_only_never_writes() {
        let s = hold_smc(); assert_eq!(detect_read_only(&mut s.clone(), &mut no_native()).mode, ControlMode::Hold);
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

    #[test] fn restore_native_puts_back_the_original() {
        let dir = tempfile::tempdir().unwrap(); let store = Store::new(dir.path().into());
        store.save(&Config { native_original: Some(95), ..Config::default() }).unwrap();
        let mut n = FakeNative::new(&[80, 85, 90, 95, 100], 80);
        assert_eq!(restore_native(&mut n, &store), Ok(NativeRestore::Restored(95)));
        assert_eq!(n.limit_now(), 95);
    }
    #[test] fn restore_native_without_a_record_leaves_the_limit() {
        let dir = tempfile::tempdir().unwrap(); let store = Store::new(dir.path().into());
        let mut n = FakeNative::new(&[80, 85, 90, 95, 100], 80);
        assert_eq!(restore_native(&mut n, &store), Ok(NativeRestore::LeftAsIs));
        assert_eq!(n.writes(), Vec::<u8>::new());
    }
    #[test] fn restore_native_reports_an_unreadable_config() {
        let dir = tempfile::tempdir().unwrap(); let store = Store::new(dir.path().into());
        std::fs::write(dir.path().join("config.toml"), "not toml [[[").unwrap();
        let mut n = FakeNative::new(&[80, 85, 90, 95, 100], 80);
        // An error, so `--restore` (and with it `eighty uninstall`) reports a failure.
        let Err(e @ NativeRestoreError::ConfigUnreadable(_)) = restore_native(&mut n, &store) else { panic!() };
        let NativeRestoreError::ConfigUnreadable(w) = &e else { unreachable!() };
        assert!(w.contains("config.toml"), "{w}");
        assert_eq!(e.message(), format!("restore: config unreadable ({w}); native limit left as is"));
        assert_eq!(n.writes(), Vec::<u8>::new());
    }
    #[test] fn restore_native_reports_a_failed_set() {
        let dir = tempfile::tempdir().unwrap(); let store = Store::new(dir.path().into());
        store.save(&Config { native_original: Some(95), ..Config::default() }).unwrap();
        let mut n = FakeNative::new(&[80, 85, 90, 95, 100], 80); n.fail_calls(true);
        let Err(e) = restore_native(&mut n, &store) else { panic!() };
        assert_eq!(e.message(), "restore native: FAILED: setting the native limit back to 95%: fake native call failed");
    }
}
