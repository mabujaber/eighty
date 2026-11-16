use std::collections::HashMap;
use std::ffi::c_void;

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use super::{parse_snapshot, BatteryError, BatterySnapshot, BatterySource, Prop};
use crate::smc::ffi::{
    IOObjectRelease, IORegistryEntryCreateCFProperties, IOServiceGetMatchingService, IOServiceMatching,
    K_IO_MAIN_PORT_DEFAULT,
};

/// Reads the battery from IORegistry. No privileges needed.
#[derive(Debug, Default, Clone, Copy)]
pub struct IoRegBattery;

fn read_props() -> Result<HashMap<String, Prop>, BatteryError> {
    // SAFETY: standard IOKit registry calls; the returned dictionary follows
    // the create rule and is released when `dict` drops.
    unsafe {
        let matching = IOServiceMatching(c"AppleSmartBattery".as_ptr());
        if matching.is_null() {
            return Err(BatteryError::NotFound);
        }
        let service = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
        if service == 0 {
            return Err(BatteryError::NotFound);
        }
        let mut raw: *mut c_void = std::ptr::null_mut();
        let kr = IORegistryEntryCreateCFProperties(service, &mut raw, std::ptr::null(), 0);
        IOObjectRelease(service);
        if kr != 0 || raw.is_null() {
            return Err(BatteryError::IoKit(kr));
        }
        let dict: CFDictionary = CFDictionary::wrap_under_create_rule(raw as CFDictionaryRef);
        let (keys, values) = dict.get_keys_and_values();
        let mut out = HashMap::new();
        for (k, v) in keys.into_iter().zip(values) {
            let key = CFType::wrap_under_get_rule(k as _);
            let Some(key) = key.downcast::<CFString>() else { continue };
            let value = CFType::wrap_under_get_rule(v as _);
            let prop = if let Some(n) = value.downcast::<CFNumber>() {
                n.to_i64().map(Prop::Int)
            } else {
                value.downcast::<CFBoolean>().map(|b| Prop::Bool(b.into()))
            };
            if let Some(p) = prop {
                out.insert(key.to_string(), p);
            }
        }
        Ok(out)
    }
}

impl BatterySource for IoRegBattery {
    fn snapshot(&mut self) -> Result<BatterySnapshot, BatteryError> {
        parse_snapshot(&read_props()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::BatterySource;

    #[test]
    #[ignore = "hardware"]
    fn reads_real_battery() {
        let s = IoRegBattery.snapshot().unwrap();
        assert!(s.percent <= 100 && s.temperature_c > 0.0 && s.temperature_c < 80.0);
    }
}
