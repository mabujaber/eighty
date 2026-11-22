//! Lid and console-user probes. No privileges needed.
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use std::os::unix::fs::MetadataExt;

use crate::smc::ffi::{
    IOObjectRelease, IORegistryEntryCreateCFProperty, IOServiceGetMatchingService, IOServiceMatching,
    K_IO_MAIN_PORT_DEFAULT,
};

/// `AppleClamshellState` of `IOPMrootDomain`; `None` when it cannot be read.
pub fn lid_closed() -> Option<bool> {
    // SAFETY: standard IOKit registry calls; the property follows the create rule.
    unsafe {
        let matching = IOServiceMatching(c"IOPMrootDomain".as_ptr());
        if matching.is_null() {
            return None;
        }
        let service = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
        if service == 0 {
            return None;
        }
        let key = CFString::new("AppleClamshellState");
        let raw = IORegistryEntryCreateCFProperty(service, key.as_concrete_TypeRef().cast(), std::ptr::null(), 0);
        IOObjectRelease(service);
        if raw.is_null() {
            return None;
        }
        CFType::wrap_under_create_rule(raw as _).downcast::<CFBoolean>().map(bool::from)
    }
}

/// Owner uid of `/dev/console` (the logged-in GUI user, or root at the login window).
pub fn console_uid() -> Option<u32> {
    std::fs::metadata("/dev/console").ok().map(|m| m.uid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "hardware"]
    fn reads_lid_and_console() {
        assert!(lid_closed().is_some());
        assert!(console_uid().is_some());
    }
}
