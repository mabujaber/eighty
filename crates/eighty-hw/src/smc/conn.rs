//! Real SMC access through the AppleSMC IOKit user client.
use std::ffi::c_void;

use super::ffi::*;
use super::{FourCC, KeyInfo, SmcError, SmcIo};

pub(crate) fn map_kern_return(kr: i32) -> SmcError {
    if kr == K_IO_RETURN_NOT_PRIVILEGED {
        SmcError::NotPrivileged
    } else {
        SmcError::IoKit(kr)
    }
}

/// An open connection to AppleSMC. Reads work as any user; writes need root.
#[derive(Debug)]
pub struct SmcConnection {
    conn: IoObject,
}

impl SmcConnection {
    pub fn open() -> Result<SmcConnection, SmcError> {
        // SAFETY: plain IOKit calls; the matching dict is consumed by
        // IOServiceGetMatchingService and the service is released after open.
        unsafe {
            let matching = IOServiceMatching(c"AppleSMC".as_ptr());
            if matching.is_null() {
                return Err(SmcError::ServiceNotFound);
            }
            let service = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
            if service == 0 {
                return Err(SmcError::ServiceNotFound);
            }
            let mut conn: IoObject = 0;
            let kr = IOServiceOpen(service, mach2::traps::mach_task_self(), 0, &mut conn);
            IOObjectRelease(service);
            if kr != 0 {
                return Err(map_kern_return(kr));
            }
            Ok(SmcConnection { conn })
        }
    }

    fn call(&self, input: &SmcParam) -> Result<SmcParam, SmcError> {
        let mut output = SmcParam::default();
        let mut out_size = std::mem::size_of::<SmcParam>();
        // SAFETY: input/output are valid repr(C) SmcParam of the declared size.
        let kr = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                input as *const SmcParam as *const c_void,
                std::mem::size_of::<SmcParam>(),
                &mut output as *mut SmcParam as *mut c_void,
                &mut out_size,
            )
        };
        if kr != 0 {
            return Err(map_kern_return(kr));
        }
        Ok(output)
    }
}

impl SmcIo for SmcConnection {
    fn key_info(&mut self, key: FourCC) -> Result<KeyInfo, SmcError> {
        let input = SmcParam { key: key.0, data8: CMD_READ_KEYINFO, ..Default::default() };
        let out = self.call(&input)?;
        if out.result == RESULT_KEY_NOT_FOUND || out.key_info.data_size == 0 {
            return Err(SmcError::KeyNotFound(key.as_str()));
        }
        if out.result != 0 {
            return Err(SmcError::IoKit(out.result as i32));
        }
        Ok(KeyInfo {
            size: out.key_info.data_size,
            data_type: FourCC(out.key_info.data_type),
            attributes: out.key_info.data_attributes,
        })
    }

    fn read(&mut self, key: FourCC) -> Result<Vec<u8>, SmcError> {
        let info = self.key_info(key)?;
        let mut input = SmcParam { key: key.0, data8: CMD_READ_BYTES, ..Default::default() };
        input.key_info.data_size = info.size;
        let out = self.call(&input)?;
        if out.result == RESULT_KEY_NOT_FOUND {
            return Err(SmcError::KeyNotFound(key.as_str()));
        }
        if out.result != 0 {
            return Err(SmcError::IoKit(out.result as i32));
        }
        let n = (info.size as usize).min(out.bytes.len());
        Ok(out.bytes[..n].to_vec())
    }

    fn write(&mut self, key: FourCC, bytes: &[u8]) -> Result<(), SmcError> {
        let info = self.key_info(key)?;
        if bytes.len() != info.size as usize || bytes.len() > 32 {
            return Err(SmcError::SizeMismatch { key: key.as_str(), expected: info.size, got: bytes.len() });
        }
        let mut input = SmcParam { key: key.0, data8: CMD_WRITE_BYTES, ..Default::default() };
        input.key_info.data_size = info.size;
        input.bytes[..bytes.len()].copy_from_slice(bytes);
        let out = self.call(&input)?;
        if out.result != 0 {
            return Err(SmcError::IoKit(out.result as i32));
        }
        Ok(())
    }
}

impl Drop for SmcConnection {
    fn drop(&mut self) {
        // SAFETY: conn came from a successful IOServiceOpen.
        unsafe {
            IOServiceClose(self.conn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smc::{FourCC, SmcError, SmcIo};

    #[test]
    fn maps_not_privileged() {
        assert!(matches!(map_kern_return(0xE00002C1u32 as i32), SmcError::NotPrivileged));
        assert!(matches!(map_kern_return(-536870206), SmcError::IoKit(-536870206)));
    }

    #[test]
    #[ignore = "hardware"]
    fn reads_key_count_on_real_mac() {
        let mut c = SmcConnection::open().unwrap();
        let info = c.key_info(FourCC::new("#KEY").unwrap()).unwrap();
        assert_eq!(info.size, 4);
        let n = c.read(FourCC::new("#KEY").unwrap()).unwrap();
        assert!(u32::from_be_bytes(n.try_into().unwrap()) > 100);
    }

    #[test]
    #[ignore = "hardware"]
    fn missing_key_is_not_found() {
        let mut c = SmcConnection::open().unwrap();
        assert!(matches!(c.read(FourCC::new("ZZZZ").unwrap()), Err(SmcError::KeyNotFound(_))));
    }
}
