use std::collections::HashMap;
use std::ffi::{c_void, CStr};

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use super::{pack_temperature, parse_snapshot, BatteryError, BatterySnapshot, BatterySource, Prop, PACK_TEMPERATURE};
use crate::smc::ffi::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperties, IOServiceGetMatchingService,
    IOServiceGetMatchingServices, IOServiceMatching, IoObject, K_IO_MAIN_PORT_DEFAULT,
};

/// Reads the battery from IORegistry. No privileges needed.
#[derive(Debug, Default, Clone, Copy)]
pub struct IoRegBattery;

/// All properties of one registry entry. The dictionary follows the create rule.
///
/// # Safety
/// `entry` must be a valid IORegistry object; the caller keeps ownership of it.
unsafe fn entry_properties(entry: IoObject) -> Result<CFDictionary, BatteryError> {
    let mut raw: *mut c_void = std::ptr::null_mut();
    let kr = IORegistryEntryCreateCFProperties(entry, &mut raw, std::ptr::null(), 0);
    if kr != 0 || raw.is_null() {
        return Err(BatteryError::IoKit(kr));
    }
    Ok(CFDictionary::wrap_under_create_rule(raw as CFDictionaryRef))
}

/// The number and boolean values of a dictionary; anything else is skipped.
fn flatten(dict: &CFDictionary) -> HashMap<String, Prop> {
    let (keys, values) = dict.get_keys_and_values();
    let mut out = HashMap::new();
    for (k, v) in keys.into_iter().zip(values) {
        // SAFETY: keys and values come from a live dictionary; get-rule wrapping retains them.
        let (key, value) = unsafe { (CFType::wrap_under_get_rule(k as _), CFType::wrap_under_get_rule(v as _)) };
        let Some(key) = key.downcast::<CFString>() else { continue };
        let prop = if let Some(n) = value.downcast::<CFNumber>() {
            n.to_i64().map(Prop::Int)
        } else {
            value.downcast::<CFBoolean>().map(|b| Prop::Bool(b.into()))
        };
        if let Some(p) = prop {
            out.insert(key.to_string(), p);
        }
    }
    out
}

/// A nested dictionary value, flattened (e.g. a pack's `BatteryData`).
fn nested(dict: &CFDictionary, name: &str) -> Option<HashMap<String, Prop>> {
    let key = CFString::new(name);
    let value = dict.find(key.as_concrete_TypeRef() as *const c_void)?;
    // SAFETY: the value lives as long as `dict`; get-rule wrapping retains it.
    let value = unsafe { CFType::wrap_under_get_rule(*value as _) };
    value.downcast::<CFDictionary>().map(|d| flatten(&d))
}

/// Every registry entry of a class, each as its properties.
fn entries_of(class: &CStr) -> Vec<CFDictionary> {
    // SAFETY: standard IOKit matching and iteration; every object is released.
    unsafe {
        let matching = IOServiceMatching(class.as_ptr());
        if matching.is_null() {
            return Vec::new();
        }
        let mut iter: IoObject = 0;
        if IOServiceGetMatchingServices(K_IO_MAIN_PORT_DEFAULT, matching, &mut iter) != 0 || iter == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            if let Ok(d) = entry_properties(entry) {
                out.push(d);
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
        out
    }
}

fn read_props() -> Result<HashMap<String, Prop>, BatteryError> {
    // SAFETY: standard IOKit registry calls; the service is released after reading.
    let dict = unsafe {
        let matching = IOServiceMatching(c"AppleSmartBattery".as_ptr());
        if matching.is_null() {
            return Err(BatteryError::NotFound);
        }
        let service = IOServiceGetMatchingService(K_IO_MAIN_PORT_DEFAULT, matching);
        if service == 0 {
            return Err(BatteryError::NotFound);
        }
        let dict = entry_properties(service);
        IOObjectRelease(service);
        dict?
    };
    let mut out = flatten(&dict);
    // macOS 27 reports the temperature only on the pack entries, in their `BatteryData`.
    if !out.contains_key("Temperature") && !out.contains_key("VirtualTemperature") {
        let packs: Vec<_> =
            entries_of(c"AppleSmartBatteryPack").iter().filter_map(|d| nested(d, "BatteryData")).collect();
        if let Some(t) = pack_temperature(&packs) {
            out.insert(PACK_TEMPERATURE.into(), Prop::Int(t));
        }
    }
    Ok(out)
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
        assert!(s.percent <= 100 && s.temperature_c.is_none_or(|t| t > 0.0 && t < 80.0));
    }

    /// Every Mac eighty has been tested on reports a temperature, at the top level
    /// (macOS 26) or on the pack (macOS 27).
    #[test]
    #[ignore = "hardware"]
    fn reads_real_temperature() {
        let t = IoRegBattery.snapshot().unwrap().temperature_c;
        assert!(t.is_some_and(|t| t > 0.0 && t < 80.0), "{t:?}");
    }
}
