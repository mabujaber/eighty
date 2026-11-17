//! Machine identity via sysctl.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInfo {
    pub model: String,
    pub chip: String,
    pub macos: String,
    pub build: String,
}

fn sysctl_string(name: &str) -> String {
    let Ok(cname) = std::ffi::CString::new(name) else { return "unknown".into() };
    let mut len: libc::size_t = 0;
    // SAFETY: first call queries the length, second fills a buffer of that length.
    unsafe {
        if libc::sysctlbyname(cname.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
            return "unknown".into();
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return "unknown".into();
        }
        buf.truncate(len);
        String::from_utf8_lossy(&buf).trim_end_matches('\0').trim().to_string()
    }
}

impl MachineInfo {
    pub fn current() -> MachineInfo {
        MachineInfo {
            model: sysctl_string("hw.model"),
            chip: sysctl_string("machdep.cpu.brand_string"),
            macos: sysctl_string("kern.osproductversion"),
            build: sysctl_string("kern.osversion"),
        }
    }
}
