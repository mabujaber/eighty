//! The real backend: private `PowerUISmartChargeClient` from PowerUI.framework.
use std::ffi::{c_char, c_void, CStr};
use std::ptr;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSError, NSNumber, NSString};

use super::{guard_constructor, guard_encodings, NativeError, NativeLimit};

const FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/PowerUI.framework/PowerUI";
const CLASS: &CStr = c"PowerUISmartChargeClient";
const CLIENT_NAME: &str = "eighty";

extern "C" {
    // libobjc, already linked by objc2.
    fn method_getTypeEncoding(method: *const c_void) -> *const c_char;
}

/// Type encoding of `class`'s instance method `name`, or None if it has none.
fn type_encoding(class: &AnyClass, name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    let method = class.instance_method(Sel::register(&cname))?;
    // SAFETY: `method` is a live Method; the returned string is owned by the runtime.
    let enc = unsafe { method_getTypeEncoding(method as *const _ as *const c_void) };
    if enc.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(enc) }.to_string_lossy().into_owned())
}

/// Native Charge Limit through PowerUI. `new` never panics; if anything is
/// missing or changed, every call returns `NativeError::Unavailable`.
pub struct PowerUi {
    client: Option<Retained<AnyObject>>,
    supported: bool,
}

impl PowerUi {
    pub fn new() -> Self {
        let mut me = PowerUi { client: None, supported: false };
        me.client = Self::create_client();
        me.supported = me.probe();
        me
    }

    fn create_client() -> Option<Retained<AnyObject>> {
        // SAFETY: plain dlopen of a system framework; the handle is intentionally leaked.
        let handle = unsafe { libc::dlopen(FRAMEWORK.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            return None;
        }
        let class = AnyClass::get(CLASS)?;
        if !guard_constructor(&|name| type_encoding(class, name)) {
            return None;
        }
        autoreleasepool(|_| {
            let name = NSString::from_str(CLIENT_NAME);
            // SAFETY: `initWithClientName:` is `@24@0:8@16`, taking an NSString.
            unsafe {
                let obj: *mut AnyObject = msg_send![class, alloc];
                let obj: *mut AnyObject = msg_send![obj, initWithClientName: &*name];
                Retained::from_raw(obj)
            }
        })
    }

    fn probe(&self) -> bool {
        let Some(client) = &self.client else { return false };
        let class = client.class();
        if !guard_encodings(&|name| type_encoding(class, name)) {
            return false;
        }
        autoreleasepool(|_| {
            // SAFETY: encoding verified as `B16@0:8`.
            let ok: bool = unsafe { msg_send![client, isMCLSupported] };
            ok
        })
    }

    fn client(&self) -> Result<&AnyObject, NativeError> {
        match (&self.client, self.supported) {
            (Some(c), true) => Ok(c),
            _ => Err(NativeError::Unavailable),
        }
    }
}

impl Default for PowerUi {
    fn default() -> Self {
        Self::new()
    }
}

/// Text of a non-null NSError, or a generic message.
///
/// # Safety
/// `err` is null or a valid NSError, and the caller is inside an autorelease pool.
unsafe fn describe(err: *mut NSError) -> String {
    match unsafe { err.as_ref() } {
        Some(e) => e.localizedDescription().to_string(),
        None => "PowerUI call failed".to_string(),
    }
}

/// The limits in `availableChargeLimitsWithError:`'s array. The array's element type
/// isn't checked by the guard, so each element is checked here: anything but an
/// NSNumber from 0 to 100 means the private API changed, and is an error rather than
/// an Objective-C exception (or a wrapped value) in a root daemon.
fn steps_of(list: &NSArray<AnyObject>) -> Result<Vec<u8>, NativeError> {
    list.iter()
        .map(|item| {
            let n = item.downcast_ref::<NSNumber>().ok_or_else(|| {
                NativeError::Call(format!("available limits: {} is not a number", item.class().name().to_string_lossy()))
            })?;
            let v = n.unsignedIntegerValue();
            u8::try_from(v)
                .ok()
                .filter(|&p| p <= 100)
                .ok_or_else(|| NativeError::Call(format!("available limits: {v} is not a percentage")))
        })
        .collect()
}

impl NativeLimit for PowerUi {
    fn supported(&mut self) -> bool {
        self.supported
    }

    fn available(&mut self) -> Result<Vec<u8>, NativeError> {
        let client = self.client()?;
        autoreleasepool(|_| {
            let mut err: *mut NSError = ptr::null_mut();
            // SAFETY: encoding verified as `@24@0:8^@16`.
            // The elements are taken as AnyObject and checked one by one in `steps_of`.
            let list: Option<Retained<NSArray<AnyObject>>> =
                unsafe { msg_send![client, availableChargeLimitsWithError: &mut err as *mut *mut NSError] };
            match list {
                Some(list) => steps_of(&list),
                None => Err(NativeError::Call(unsafe { describe(err) })),
            }
        })
    }

    fn limit(&mut self) -> Result<u8, NativeError> {
        let client = self.client()?;
        autoreleasepool(|_| {
            let mut err: *mut NSError = ptr::null_mut();
            // SAFETY: encoding verified as `C24@0:8^@16`.
            let v: u8 = unsafe { msg_send![client, getMCLLimitWithError: &mut err as *mut *mut NSError] };
            if err.is_null() { Ok(v) } else { Err(NativeError::Call(unsafe { describe(err) })) }
        })
    }

    fn enabled(&mut self) -> Result<bool, NativeError> {
        let client = self.client()?;
        autoreleasepool(|_| {
            let mut err: *mut NSError = ptr::null_mut();
            // SAFETY: encoding verified as `Q24@0:8^@16`.
            let v: u64 = unsafe { msg_send![client, isMCLCurrentlyEnabled: &mut err as *mut *mut NSError] };
            if err.is_null() { Ok(v != 0) } else { Err(NativeError::Call(unsafe { describe(err) })) }
        })
    }

    fn set_limit(&mut self, percent: u8) -> Result<(), NativeError> {
        let client = self.client()?;
        autoreleasepool(|_| {
            let mut err: *mut NSError = ptr::null_mut();
            // SAFETY: encoding verified as `B28@0:8C16^@20`.
            let ok: bool = unsafe {
                msg_send![client, setMCLLimit: percent, error: &mut err as *mut *mut NSError]
            };
            if ok { Ok(()) } else { Err(NativeError::Call(unsafe { describe(err) })) }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_accept_numbers_and_reject_anything_else() {
        let (n80, n100) = (NSNumber::new_u8(80), NSNumber::new_u8(100));
        let ok = NSArray::<AnyObject>::from_slice(&[&*n80 as &AnyObject, &*n100]);
        assert_eq!(steps_of(&ok), Ok(vec![80, 100]));
        let text = NSString::from_str("80");
        let mixed = NSArray::<AnyObject>::from_slice(&[&*n80 as &AnyObject, &*text]);
        assert!(matches!(steps_of(&mixed), Err(NativeError::Call(m)) if m.contains("not a number")));
        let big = NSNumber::new_u64(300); // would wrap to 44 as a u8
        let wide = NSArray::<AnyObject>::from_slice(&[&*big as &AnyObject]);
        assert!(matches!(steps_of(&wide), Err(NativeError::Call(m)) if m.contains("300")));
    }

    #[test]
    #[ignore = "needs macOS 26.4+ with MCL"]
    fn powerui_reads_on_this_mac() {
        let mut p = PowerUi::new();
        assert!(p.supported());
        assert!(p.available().unwrap().contains(&80));
        let l = p.limit().unwrap();
        assert!((80..=100).contains(&l));
    }
}
