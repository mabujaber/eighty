//! Restores every SMC key the probe changed, even on error or panic.
use eighty_hw::smc::{hex, FourCC, SmcError, SmcIo};

pub struct RestoreGuard<'a> {
    smc: &'a mut dyn SmcIo,
    /// Key, original value, and whether the restore is checked by reading it back.
    pending: Vec<(FourCC, Vec<u8>, bool)>,
}

impl<'a> RestoreGuard<'a> {
    pub fn new(smc: &'a mut dyn SmcIo) -> Self {
        RestoreGuard { smc, pending: Vec::new() }
    }

    /// Writes `value`, remembering the key's original value the first time
    /// it is touched. A failed write is not tracked, so nothing is restored
    /// for a key that was never changed.
    pub fn write_tracked(&mut self, key: FourCC, value: &[u8]) -> Result<(), SmcError> {
        self.track(key, value, true)
    }

    /// Like `write_tracked`, for a key the firmware also drives (the MagSafe light):
    /// the firmware may set its own value right after the restore, so the restore
    /// counts once the original is written back, whatever a read then shows.
    pub fn write_tracked_firmware_owned(&mut self, key: FourCC, value: &[u8]) -> Result<(), SmcError> {
        self.track(key, value, false)
    }

    fn track(&mut self, key: FourCC, value: &[u8], verify: bool) -> Result<(), SmcError> {
        let tracked = self.pending.iter().any(|(k, _, _)| *k == key);
        let original = if tracked { None } else { Some(self.smc.read(key)?) };
        self.smc.write(key, value)?;
        if let Some(original) = original {
            self.pending.push((key, original, verify));
        }
        Ok(())
    }

    /// Writes originals back in reverse order, retrying up to 3 times, and
    /// verifies each by read-back (except firmware-owned keys). Error messages
    /// include the original value.
    pub fn restore_all(&mut self) -> Vec<(String, Result<(), String>)> {
        let mut out = Vec::new();
        while let Some((key, original, verify)) = self.pending.pop() {
            let mut result = Err(String::new());
            for _ in 0..3 {
                result = self
                    .smc
                    .write(key, &original)
                    .map_err(|e| e.to_string())
                    .and_then(|()| match self.smc.read(key) {
                        _ if !verify => Ok(()),
                        Ok(v) if v == original => Ok(()),
                        Ok(v) => Err(format!("read back {}", hex(&v))),
                        Err(e) => Err(e.to_string()),
                    });
                if result.is_ok() {
                    break;
                }
            }
            let result = result.map_err(|e| format!("{e} (original value {})", hex(&original)));
            out.push((key.as_str(), result));
        }
        out
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        for (key, result) in self.restore_all() {
            if let Err(e) = result {
                eprintln!("eighty-probe: FAILED to restore {key}: {e}. Re-run eighty-probe, or reboot to reset the SMC.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_hw::smc::fake::{FakeSmc, WriteMode};
    use eighty_hw::smc::FourCC;

    #[test]
    fn guard_restores_on_drop() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
        {
            let mut c = smc.clone();
            let mut g = RestoreGuard::new(&mut c);
            g.write_tracked(FourCC::new("CHTE").unwrap(), &[1, 0, 0, 0]).unwrap();
            g.write_tracked(FourCC::new("CHTE").unwrap(), &[2, 0, 0, 0]).unwrap();
        }
        assert_eq!(smc.value("CHTE"), Some(vec![0, 0, 0, 0]));
    }

    /// The MagSafe light as seen on the M5 Pro (macOS 27.2.1): writes are accepted,
    /// but the firmware shows its own value (orange, `04`) right after.
    struct FirmwareLight(FakeSmc);
    impl SmcIo for FirmwareLight {
        fn key_info(&mut self, key: FourCC) -> Result<eighty_hw::smc::KeyInfo, SmcError> {
            self.0.key_info(key)
        }
        fn read(&mut self, key: FourCC) -> Result<Vec<u8>, SmcError> {
            self.0.read(key)
        }
        fn write(&mut self, key: FourCC, bytes: &[u8]) -> Result<(), SmcError> {
            self.0.write(key, bytes)?;
            self.0.insert("ACLC", "ui8 ", &[4], WriteMode::Accept);
            Ok(())
        }
    }

    #[test]
    fn firmware_owned_restore_skips_read_back() {
        let smc = FakeSmc::new();
        smc.insert("ACLC", "ui8 ", &[3], WriteMode::Accept);
        let key = FourCC::new("ACLC").unwrap();
        let mut light = FirmwareLight(smc.clone());
        let mut g = RestoreGuard::new(&mut light);
        g.write_tracked_firmware_owned(key, &[3]).unwrap();
        g.write_tracked_firmware_owned(key, &[4]).unwrap();
        assert_eq!(g.restore_all(), vec![("ACLC".to_string(), Ok(()))]);
        assert_eq!(smc.writes().last(), Some(&("ACLC".to_string(), vec![3])));
    }

    #[test]
    fn verified_restore_fails_on_firmware_value() {
        let smc = FakeSmc::new();
        smc.insert("ACLC", "ui8 ", &[3], WriteMode::Accept);
        let mut light = FirmwareLight(smc.clone());
        let mut g = RestoreGuard::new(&mut light);
        g.write_tracked(FourCC::new("ACLC").unwrap(), &[4]).unwrap();
        assert!(matches!(&g.restore_all()[0].1, Err(m) if m.contains("read back 04")));
    }

    #[test]
    fn failed_write_is_not_tracked() {
        let smc = FakeSmc::new();
        smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::NotPrivileged);
        let mut c = smc.clone();
        let mut g = RestoreGuard::new(&mut c);
        assert!(g.write_tracked(FourCC::new("CHTE").unwrap(), &[1, 0, 0, 0]).is_err());
        assert!(g.restore_all().is_empty());
    }
}
