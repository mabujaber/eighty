//! System probes the daemon needs: lid, console user, other limiters, machine identity.
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use eighty_core::limiters::find_other_limiters;
use eighty_hw::machine::MachineInfo;

pub trait SystemProbe {
    fn lid_closed(&mut self) -> bool;
    fn console_uid(&mut self) -> Option<u32>;
    fn other_limiters(&mut self) -> Vec<String>;
    fn machine(&mut self) -> MachineInfo;
}

pub struct MacSystem;

/// Home directories under /Users (skipping `Shared` and dot-dirs) with their owner uids.
fn user_homes() -> Vec<(PathBuf, u32)> {
    let Ok(rd) = std::fs::read_dir("/Users") else { return vec![] };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n != "Shared" && !n.starts_with('.')
        })
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_dir().then(|| (e.path(), m.uid()))
        })
        .collect()
}

fn launchctl_loaded(target: &str) -> bool {
    Command::new("/bin/launchctl")
        .args(["print", target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

impl SystemProbe for MacSystem {
    fn lid_closed(&mut self) -> bool {
        eighty_hw::system::lid_closed().unwrap_or(false)
    }
    fn console_uid(&mut self) -> Option<u32> {
        eighty_hw::system::console_uid()
    }
    fn other_limiters(&mut self) -> Vec<String> {
        let users = user_homes();
        let homes: Vec<PathBuf> = users.iter().map(|(p, _)| p.clone()).collect();
        let mut uids: Vec<u32> = users.iter().map(|(_, u)| *u).collect();
        uids.sort_unstable();
        uids.dedup();
        find_other_limiters(Path::new("/"), &homes, &mut |label| {
            launchctl_loaded(&format!("system/{label}"))
                || uids.iter().any(|u| launchctl_loaded(&format!("gui/{u}/{label}")))
        })
    }
    fn machine(&mut self) -> MachineInfo {
        MachineInfo::current()
    }
}

#[cfg(test)]
pub use fake::{FakeSystem, FakeSystemState};

#[cfg(test)]
mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug)]
    pub struct FakeSystemState {
        pub lid_closed: bool,
        pub console_uid: Option<u32>,
        pub other_limiters: Vec<String>,
    }
    impl Default for FakeSystemState {
        fn default() -> Self {
            FakeSystemState { lid_closed: false, console_uid: Some(501), other_limiters: vec![] }
        }
    }

    #[derive(Clone, Default)]
    pub struct FakeSystem(pub Rc<RefCell<FakeSystemState>>);

    impl SystemProbe for FakeSystem {
        fn lid_closed(&mut self) -> bool {
            self.0.borrow().lid_closed
        }
        fn console_uid(&mut self) -> Option<u32> {
            self.0.borrow().console_uid
        }
        fn other_limiters(&mut self) -> Vec<String> {
            self.0.borrow().other_limiters.clone()
        }
        fn machine(&mut self) -> MachineInfo {
            MachineInfo {
                model: "Mac14,6".into(),
                chip: "Apple M2 Max".into(),
                macos: "27.2".into(),
                build: "26B123".into(),
            }
        }
    }
}
