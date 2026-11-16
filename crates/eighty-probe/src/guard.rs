//! Restores every SMC key the probe changed, even on error or panic.
use eighty_hw::smc::{FourCC, SmcError, SmcIo};

pub struct RestoreGuard<'a> {
    smc: &'a mut dyn SmcIo,
    pending: Vec<(FourCC, Vec<u8>)>,
}

impl<'a> RestoreGuard<'a> {
    pub fn new(smc: &'a mut dyn SmcIo) -> Self {
        RestoreGuard { smc, pending: Vec::new() }
    }

    /// Writes `value`, remembering the key's original value the first time
    /// it is touched. A failed write is not tracked, so nothing is restored
    /// for a key that was never changed.
    pub fn write_tracked(&mut self, key: FourCC, value: &[u8]) -> Result<(), SmcError> {
        let tracked = self.pending.iter().any(|(k, _)| *k == key);
        let original = if tracked { None } else { Some(self.smc.read(key)?) };
        self.smc.write(key, value)?;
        if let Some(original) = original {
            self.pending.push((key, original));
        }
        Ok(())
    }

    /// Writes originals back in reverse order and verifies each by read-back.
    pub fn restore_all(&mut self) -> Vec<(String, Result<(), String>)> {
        let mut out = Vec::new();
        while let Some((key, original)) = self.pending.pop() {
            let result = self
                .smc
                .write(key, &original)
                .map_err(|e| e.to_string())
                .and_then(|()| match self.smc.read(key) {
                    Ok(v) if v == original => Ok(()),
                    Ok(v) => Err(format!("read back {:?}, expected {:?}", v, original)),
                    Err(e) => Err(e.to_string()),
                });
            out.push((key.as_str(), result));
        }
        out
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        for (key, result) in self.restore_all() {
            if let Err(e) = result {
                eprintln!("eighty-probe: FAILED to restore {key}: {e}");
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
