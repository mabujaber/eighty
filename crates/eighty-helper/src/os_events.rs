//! OS power events (power-source changes, sleep and wake) delivered to the engine.
#![allow(non_snake_case, non_upper_case_globals)]
use std::ffi::c_void;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource, CFRunLoopSourceRef};

use crate::engine::Event;
use crate::log::log;

type IoConnect = u32;
type IoObject = u32;
type IoReturn = i32;
type IONotificationPortRef = *mut c_void;
type IOServiceInterestCallback =
    extern "C" fn(refcon: *mut c_void, service: IoObject, message_type: u32, message_argument: *mut c_void);
type IOPowerSourceCallbackType = extern "C" fn(context: *mut c_void);

const kIOMessageCanSystemSleep: u32 = 0xe000_0270;
const kIOMessageSystemWillSleep: u32 = 0xe000_0280;
const kIOMessageSystemHasPoweredOn: u32 = 0xe000_0300;

/// How long a sleep waits for the engine to finish its pre-sleep tick.
const SLEEP_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> IoReturn;
    fn IONotificationPortGetRunLoopSource(notify: IONotificationPortRef) -> CFRunLoopSourceRef;
    fn IOPSNotificationCreateRunLoopSource(
        callback: IOPowerSourceCallbackType,
        context: *mut c_void,
    ) -> CFRunLoopSourceRef;
}

/// Refcon for the system-power callback. Leaked: it lives for the process.
struct PowerCtx {
    tx: Sender<Event>,
    /// Root power-domain connection; written once registration returns, before
    /// the run loop runs, so every callback sees it set.
    root_port: IoConnect,
}

extern "C" fn power_source_changed(context: *mut c_void) {
    // SAFETY: `context` is the leaked `Box<Sender<Event>>` from `spawn_os_events`;
    // it is never freed and only this run-loop thread touches it.
    let tx = unsafe { &*(context as *const Sender<Event>) };
    let _ = tx.send(Event::PowerChanged);
}

extern "C" fn system_power(refcon: *mut c_void, _service: IoObject, message_type: u32, message_argument: *mut c_void) {
    // SAFETY: `refcon` is the leaked `Box<PowerCtx>` from `spawn_os_events`; it is
    // never freed and only this run-loop thread touches it.
    let ctx = unsafe { &*(refcon as *const PowerCtx) };
    // The notification ID IOAllowPowerChange expects is passed as the argument.
    let id = message_argument as isize;
    match message_type {
        kIOMessageCanSystemSleep => allow(ctx.root_port, id),
        kIOMessageSystemWillSleep => {
            let (ack_tx, ack_rx) = mpsc::channel();
            if ctx.tx.send(Event::WillSleep(ack_tx)).is_ok() && ack_rx.recv_timeout(SLEEP_ACK_TIMEOUT).is_err() {
                log("sleep: engine did not finish its pre-sleep tick within 5 s; allowing sleep");
            }
            allow(ctx.root_port, id);
        }
        kIOMessageSystemHasPoweredOn => {
            let _ = ctx.tx.send(Event::DidWake);
        }
        _ => {}
    }
}

fn allow(root_port: IoConnect, id: isize) {
    // SAFETY: plain IOKit call on the root port returned by IORegisterForSystemPower.
    let kr = unsafe { IOAllowPowerChange(root_port, id) };
    if kr != 0 {
        log(&format!("IOAllowPowerChange failed: IOKit error 0x{kr:08x}"));
    }
}

/// Start a thread running a CFRunLoop that sends `PowerChanged`, `WillSleep` and
/// `DidWake` to the engine. A failed registration is logged; ticks still run.
pub fn spawn_os_events(tx: Sender<Event>) {
    std::thread::Builder::new()
        .name("os-events".into())
        .spawn(move || run(tx))
        .expect("spawning os-events thread");
}

fn run(tx: Sender<Event>) {
    let run_loop = CFRunLoop::get_current();
    // SAFETY: reading an immutable CoreFoundation constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    let mut sources = 0;

    let ps_ctx = Box::into_raw(Box::new(tx.clone())) as *mut c_void;
    // SAFETY: the callback matches IOPowerSourceCallbackType and `ps_ctx` stays valid
    // for the process. The returned source follows the create rule, so we own it.
    let ps_source = unsafe { IOPSNotificationCreateRunLoopSource(power_source_changed, ps_ctx) };
    if ps_source.is_null() {
        log("IOPSNotificationCreateRunLoopSource failed: power-source changes will not be seen");
    } else {
        // SAFETY: non-null source we own (create rule).
        let source = unsafe { CFRunLoopSource::wrap_under_create_rule(ps_source) };
        run_loop.add_source(&source, mode);
        sources += 1;
    }

    let ctx = Box::into_raw(Box::new(PowerCtx { tx, root_port: 0 }));
    let mut port: IONotificationPortRef = std::ptr::null_mut();
    let mut notifier: IoObject = 0;
    // SAFETY: valid out-pointers; the callback matches IOServiceInterestCallback and
    // `ctx` stays valid for the process. No callback runs before the run loop does.
    let root_port = unsafe { IORegisterForSystemPower(ctx as *mut c_void, &mut port, system_power, &mut notifier) };
    if root_port == 0 || port.is_null() {
        log("IORegisterForSystemPower failed: sleep and wake will not be seen");
    } else {
        // SAFETY: `ctx` is valid and no callback can read it until the run loop runs below.
        unsafe { (*ctx).root_port = root_port };
        // SAFETY: `port` is the valid notification port from registration. The source
        // follows the get rule: the port owns it, and the port lives for the process.
        let source = unsafe { CFRunLoopSource::wrap_under_get_rule(IONotificationPortGetRunLoopSource(port)) };
        run_loop.add_source(&source, mode);
        sources += 1;
    }

    if sources > 0 {
        // Returns only if every source is removed, which never happens.
        CFRunLoop::run_current();
    }
}
