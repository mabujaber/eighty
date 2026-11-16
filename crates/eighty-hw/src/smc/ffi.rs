//! IOKit FFI for AppleSMC.
#![allow(non_snake_case)]
use std::ffi::{c_char, c_void};

pub type KernReturn = i32;
pub type IoObject = u32;

pub const KERNEL_INDEX_SMC: u32 = 2; // kSMCHandleYPCEvent
pub const CMD_READ_BYTES: u8 = 5;
pub const CMD_WRITE_BYTES: u8 = 6;
pub const CMD_READ_KEYINFO: u8 = 9;
pub const RESULT_KEY_NOT_FOUND: u8 = 132; // 0x84
pub const K_IO_RETURN_NOT_PRIVILEGED: KernReturn = 0xE00002C1u32 as i32;
pub const K_IO_MAIN_PORT_DEFAULT: u32 = 0;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SmcVersion {
    pub major: u8,
    pub minor: u8,
    pub build: u8,
    pub reserved: u8,
    pub release: u16,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SmcPLimitData {
    pub version: u16,
    pub length: u16,
    pub cpu_p_limit: u32,
    pub gpu_p_limit: u32,
    pub mem_p_limit: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SmcKeyInfoData {
    pub data_size: u32,
    pub data_type: u32,
    pub data_attributes: u8,
}

/// `SMCKeyData_t` from the AppleSMC user client. Must be 80 bytes.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SmcParam {
    pub key: u32,
    pub vers: SmcVersion,
    pub p_limit: SmcPLimitData,
    pub key_info: SmcKeyInfoData,
    pub result: u8,
    pub status: u8,
    pub data8: u8,
    pub data32: u32,
    pub bytes: [u8; 32],
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    pub fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    pub fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> IoObject;
    pub fn IOServiceOpen(service: IoObject, owning_task: u32, typ: u32, connect: *mut IoObject) -> KernReturn;
    pub fn IOServiceClose(connect: IoObject) -> KernReturn;
    pub fn IOObjectRelease(object: IoObject) -> KernReturn;
    pub fn IOConnectCallStructMethod(
        connection: IoObject,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
    pub fn IORegistryEntryCreateCFProperties(
        entry: IoObject,
        properties: *mut *mut c_void,
        allocator: *const c_void,
        options: u32,
    ) -> KernReturn;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smc_param_is_80_bytes() {
        assert_eq!(std::mem::size_of::<SmcParam>(), 80);
    }
}
