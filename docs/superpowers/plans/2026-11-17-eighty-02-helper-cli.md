# eighty Plan 2 — Helper Daemon and CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a working charge limiter: a root LaunchDaemon (`eighty-helper`) that holds the battery between an upper and a lower limit on both of the owner's Macs, plus the `eighty` CLI that installs, controls, diagnoses and uninstalls it.

**Architecture:** Three new crates on top of `eighty-hw`. `eighty-core` has no hardware or OS calls: config, the pure policy function, the IPC protocol and the other-limiter finder. `eighty-helper` (lib + bin) owns hardware. A single engine thread owns `SmcIo` and `BatterySource` and handles `Event`s from an mpsc channel. Three producers feed that channel: a CFRunLoop thread (power-source changes and sleep/wake), a Unix-socket IPC thread, and a 60 s fallback timer (`recv_timeout`). `eighty-cli` (bin `eighty`) is a thin client. The engine runs in one of two modes, chosen at startup from the probe results. **Hold** (`CHTE` + `CHIE`, e.g. M5 Pro on macOS 26) stops charging at the limit. **Cycle** (`CHIE` only, e.g. M2 Max on macOS 27) runs from the battery at the upper limit and lets the charger back in at the lower limit. Every decision is verified against a fresh battery reading, and any failure is reported loudly.

**Tech Stack:** Rust stable (edition 2021), existing `eighty-hw` (hand-written IOKit FFI, `core-foundation` 0.10), `serde`/`serde_json` 1, `toml` 0.8, `clap` 4.5 (derive), `ctrlc` 3.4 (`termination`), `libc` 0.2, `thiserror` 2, `anyhow` 1, `tempfile` 3 (dev).

**Spec:** `docs/superpowers/specs/2026-11-16-eighty-design.md` — this plan implements §2 goal 3 (charge limiter, discharge to limit, MagSafe light), goal 5 (fail loudly), §4, §5.1 (helper + CLI), §5.2 (backends, from `docs/probe/README.md`), §5.3, §5.4, §5.5 (other limiters, conflicts), §7 (uninstall restores charging) and §8.

**Scope split (builder's call).** The spec covers four subsystems, so the work is split into separate plans that each produce working software:
- **Plan 2 (this):** helper + CLI, installed with `sudo eighty install` as a plain LaunchDaemon. That's enough to run on the owner's two Macs.
- **Plan 3:** Eighty.app menu bar, `SMAppService` registration replacing `eighty install`, notifications, first-run flow, and detecting macOS's native Charge Limit / Optimized Charging.
- **Plan 4:** calibration (+ smart resume), overheat protection (incl. a temperature source on macOS 27), zones, sleep dark-wakes, full clamshell support, and the PowerUI/MCL backend.
- **Plan 5:** signing, notarization, GitHub Actions, the Homebrew cask, and the 2–5 Mac release bar.

## Global Constraints

- Apple Silicon only. Minimum macOS 14. Test machines: M5 Pro (Mac17,8) on macOS 26.6.2, and M2 Max (Mac14,6) on macOS 27.2.
- Key values, verbatim from `docs/probe/README.md`. `CHTE` (ui32, 4 bytes): `01000000` = stop charging, `00000000` = allow. `CHIE` (ui8): `08` = run from battery, `00` = off. `ACLC` (ui8): `03` = green, `04` = orange, `00` = system control.
- Restore state (on exit, uninstall, fault, and startup recovery): `CHTE=00000000`, `CHIE=00`, `ACLC=00`. Only write to keys that exist.
- Hard floor: never set `CHIE=08` when the charge is ≤ 10% (`HARD_FLOOR = 10`). Calibration, which can go below it, is Plan 4.
- Defaults: upper 80, lower 75 (lower defaults to `upper − 5` when not given), auto-discharge on. Valid ranges: 20 ≤ upper ≤ 100, 10 ≤ lower ≤ upper.
- Nothing outside the helper writes SMC keys, and the IPC protocol has no raw key/value request (spec §4). The only exception is `eighty-helper --restore`, which writes the fixed restore state.
- No shelling out to `pmset`, `ioreg` or `smc` (spec §5.3). `launchctl` is allowed for install, uninstall and limiter detection.
- No sudoers entries. Status is readable by any local user. Changing anything requires the peer to be root or the console user (spec §8).
- Paths and names (`eighty-core::paths`):
  - label: `io.github.mabujaber.eighty.helper`
  - socket: `/var/run/eighty.sock`
  - config: `/Library/Application Support/eighty/config.toml`
  - log: `/Library/Logs/eighty/helper.log`
  - installed helper: `/Library/PrivilegedHelperTools/io.github.mabujaber.eighty.helper`
  - plist: `/Library/LaunchDaemons/io.github.mabujaber.eighty.helper.plist`
- Battery readings refresh about every 60 s. Effects are only judged on a reading whose `UpdateTime` ≥ write time + `SETTLE_SECS` (20) (spec §5.3).
- Fail loudly: if the helper isn't limiting, `eighty status` says `NOT LIMITING:` with the reason on its first line and exits 3.

## Review Focus

1. **The helper is killed (SIGKILL / crash) while `CHIE=08` or `CHTE=01000000`.** launchd restarts it, and startup recovery restores the keys before the first decision, so a crash never leaves the Mac stuck discharging. Pinned by `startup_recovers_stale_discharge` (Task 6).
2. **`discharge <n>` or auto-discharge near the floor.** No path sets `CHIE=08` at ≤ 10%, even if validation is bypassed. Pinned by `never_discharges_at_or_below_floor` (Task 2).
3. **A missing, unreadable or hand-edited invalid `config.toml`.** The helper starts with defaults and shows a warning in status. It never crash-loops. Pinned by `invalid_config_falls_back_with_warning` (Task 5).
4. **A misbehaving client** (sends garbage, an over-long line, or connects and goes silent). It gets an error or times out, and the server keeps serving others. Pinned by `garbage_and_silent_clients_do_not_block_server` (Task 8).
5. **Lid closed in cycle mode at or above the upper limit.** eighty can't discharge, so the Mac keeps charging. Status shows a warning instead of claiming the limit is held. Pinned by `cycle_lid_closed_reports_not_enforced` (Task 2).

---

## File Structure

```
crates/eighty-hw/src/machine.rs          (moved from eighty-probe) MachineInfo
crates/eighty-hw/src/system.rs           lid_closed(), console_uid()
crates/eighty-hw/src/battery/fake.rs     + FakeBattery::fallible
crates/eighty-core/src/lib.rs
crates/eighty-core/src/paths.rs          constants above
crates/eighty-core/src/config.rs         Settings, Config, validation
crates/eighty-core/src/policy.rs         decide(): pure policy
crates/eighty-core/src/proto.rs          Request/Response/Status/Health/DoctorReport, line framing
crates/eighty-core/src/limiters.rs       find other charge limiters
crates/eighty-helper/src/lib.rs
crates/eighty-helper/src/control.rs      SmcControl: detect mode, apply, conflict check, restore
crates/eighty-helper/src/store.rs        config load/save
crates/eighty-helper/src/system.rs       SystemProbe trait + MacSystem
crates/eighty-helper/src/engine.rs       Engine: events, health, watchdog, requests
crates/eighty-helper/src/ipc.rs          Unix socket server
crates/eighty-helper/src/os_events.rs    CFRunLoop: IOPS + system power FFI
crates/eighty-helper/src/main.rs         wiring, signals, --restore
crates/eighty-cli/src/main.rs            clap commands
crates/eighty-cli/src/client.rs          socket client
crates/eighty-cli/src/render.rs          status/doctor text
crates/eighty-cli/src/install.rs         plist, install/uninstall
docs/testing/plan2-acceptance.md         results on both Macs
```

---

### Task 1: `eighty-core` scaffold, config, and shared hardware helpers

**Files:**
- Create: `crates/eighty-core/{Cargo.toml,src/lib.rs,src/paths.rs,src/config.rs}`
- Move: `crates/eighty-probe/src/machine.rs` → `crates/eighty-hw/src/machine.rs` (update the probe's imports)
- Modify: `crates/eighty-hw/src/lib.rs`, `crates/eighty-hw/src/battery/fake.rs`

**Interfaces:**
- Produces:
  - `eighty_core::paths::{LABEL, SOCKET_PATH, CONFIG_DIR, CONFIG_FILE, LOG_PATH, HELPER_INSTALL_PATH, PLIST_PATH}: &str`
  - `eighty_core::{HARD_FLOOR: u8 = 10, SETTLE_SECS: u64 = 20, TICK_SECS: u64 = 60}`
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)] pub struct Settings { pub upper: u8, pub lower: u8, pub auto_discharge: bool }`
  - `impl Default for Settings` (80, 75, true)
  - `Settings::validate(&self) -> Result<(), String>`
  - `Settings::with_limits(self, upper: u8, lower: Option<u8>) -> Result<Settings, String>`
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)] #[serde(default)] pub struct Config { pub settings: Settings, pub accept_other_limiters: bool }`
  - `eighty_hw::machine::MachineInfo` (unchanged, now in `eighty-hw`, plus `Serialize, Deserialize, Clone`)
  - `FakeBattery::fallible(f: impl FnMut() -> Result<BatterySnapshot, BatteryError> + 'static) -> FakeBattery`

- [ ] **Step 1: Write failing tests in `config.rs`**

```rust
#[test] fn defaults_are_80_75_auto() { assert_eq!(Settings::default(), Settings { upper: 80, lower: 75, auto_discharge: true }); }
#[test] fn lower_defaults_to_upper_minus_5() { assert_eq!(Settings::default().with_limits(90, None).unwrap().lower, 85); }
#[test] fn rejects_out_of_range() {
    let d = Settings::default();
    assert!(d.with_limits(19, None).is_err());
    assert!(d.with_limits(101, None).is_err());
    assert!(d.with_limits(80, Some(81)).is_err());
    assert!(d.with_limits(80, Some(9)).is_err());
    assert!(d.with_limits(80, Some(80)).is_ok());
}
#[test] fn config_toml_roundtrip_and_partial() {
    let c: Config = toml::from_str("[settings]\nupper = 70\nlower = 60\nauto_discharge = false\n").unwrap();
    assert_eq!(c.settings.upper, 70);
    assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
}
```

- [ ] **Step 2: Run `cargo test -p eighty-core`.** Expected: compile failure (types not defined).
- [ ] **Step 3: Implement.** Add `eighty-core` to the workspace (it's picked up by `crates/*`). Dependencies: `serde`, `serde_json`, `toml = "0.8"`. Move `machine.rs` into `eighty-hw` as `pub mod machine`, and make the probe use `eighty_hw::machine::MachineInfo`. Add `FakeBattery::fallible` by changing the internal closure type to return `Result`. `new` wraps its closure in `Ok`.
- [ ] **Step 4: Run `cargo test --workspace`.** Expected: all pass, including the existing 42+ probe tests.
- [ ] **Step 5: Commit** — `feat(core): add eighty-core with settings and paths`

---

### Task 2: Policy — `decide()`

**Files:**
- Create: `crates/eighty-core/src/policy.rs`

**Interfaces:**
- Consumes: `Settings`, `HARD_FLOOR`
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)] pub enum ControlMode { Hold, Cycle, Unsupported }`
  - `#[derive(..same..)] pub enum Light { Green, Orange, System }`
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)] pub struct Outputs { pub inhibit: bool, pub discharge: bool, pub light: Option<Light> }`. `light: None` means don't touch the light.
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum Override { #[default] None, FullCharge, DischargeTo(u8) }`
  - `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)] pub enum Activity { Charging { to: u8 }, Holding, Discharging { to: u8 }, OnBattery, LidClosed, Paused }`
  - `pub struct Inputs { pub percent: u8, pub external: bool, pub lid_closed: bool, pub mode: ControlMode }`
  - `pub struct Decision { pub outputs: Outputs, pub activity: Activity, pub clear_override: bool }`
  - `pub fn decide(s: &Settings, ov: Override, i: &Inputs, prev: &Outputs) -> Decision`

Rules (these are the spec; the tests below pin them):
- **Unsupported:** `Outputs::default()`, `Paused`.
- **FullCharge:** if `!external`, set `clear_override` and fall through to the normal rule. Otherwise inhibit/discharge off, light `System`, `Charging{to:100}`.
- **DischargeTo(n):** if `percent <= n`, set `clear_override` and fall through. If the lid is closed, discharge off, inhibit on (Hold mode only), `LidClosed`. Otherwise discharge on, inhibit on (Hold only), `Green`, `Discharging{to:n}`.
- **Normal, Hold:**
  - inhibit = `percent >= upper` → true; `percent < lower` → false; otherwise `prev.inhibit`.
  - discharge = `auto_discharge && external && !lid_closed && percent > upper`.
  - Activity: `OnBattery` if `!external`, else `Discharging{to:upper}` if discharging, else `Holding` if inhibiting, else `Charging{to:upper}`.
- **Normal, Cycle:** inhibit is always false.
  - discharge = `false` if `!external || lid_closed`; else `percent >= upper` → true; `percent <= lower` → false; otherwise `prev.discharge`.
  - `auto_discharge` is ignored here, because it's the only mechanism.
  - Activity: `OnBattery`, `LidClosed` (lid closed and `percent >= upper`), `Discharging{to:lower}`, or `Charging{to:upper}`.
- **Light** (every branch that doesn't set it above): `None` if `!external`; `Green` if inhibit or discharge; `Orange` otherwise.
- **Final guard, always last:** if `percent <= HARD_FLOOR`, discharge = false.

- [ ] **Step 1: Write failing tests**

```rust
fn s() -> Settings { Settings::default() } // 80/75/auto
fn i(percent: u8, external: bool, lid_closed: bool, mode: ControlMode) -> Inputs { Inputs { percent, external, lid_closed, mode } }
fn prev(inhibit: bool, discharge: bool) -> Outputs { Outputs { inhibit, discharge, light: None } }
use Activity::*; use Light::*; use ControlMode::*;

#[test] fn hold_mode_table() {
    // (pct, external, prev_inhibit) -> (inhibit, discharge, light, activity)
    let rows = [
        (70, true, false, false, false, Some(Orange), Charging { to: 80 }),
        (80, true, false, true,  false, Some(Green),  Holding),
        (77, true, true,  true,  false, Some(Green),  Holding),
        (77, true, false, false, false, Some(Orange), Charging { to: 80 }),
        (74, true, true,  false, false, Some(Orange), Charging { to: 80 }),
        (85, true, false, true,  true,  Some(Green),  Discharging { to: 80 }),
        (85, false, true, true,  false, None,         OnBattery),
    ];
    for (p, ext, pi, inh, dis, light, act) in rows {
        let d = decide(&s(), Override::None, &i(p, ext, false, Hold), &prev(pi, false));
        assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.outputs.light, d.activity), (inh, dis, light, act), "pct {p}");
    }
}
#[test] fn hold_no_auto_discharge_just_holds() {
    let st = Settings { auto_discharge: false, ..s() };
    let d = decide(&st, Override::None, &i(85, true, false, Hold), &prev(false, false));
    assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (true, false, Holding));
}
#[test] fn hold_lid_closed_never_discharges() {
    let d = decide(&s(), Override::None, &i(85, true, true, Hold), &prev(true, false));
    assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (true, false, Holding));
}
#[test] fn cycle_mode_table() {
    // (pct, prev_discharge) -> (discharge, light, activity)
    let rows = [
        (70, false, false, Some(Orange), Charging { to: 80 }),
        (80, false, true,  Some(Green),  Discharging { to: 75 }),
        (77, true,  true,  Some(Green),  Discharging { to: 75 }),
        (75, true,  false, Some(Orange), Charging { to: 80 }),
        (77, false, false, Some(Orange), Charging { to: 80 }),
    ];
    for (p, pd, dis, light, act) in rows {
        let d = decide(&s(), Override::None, &i(p, true, false, Cycle), &prev(false, pd));
        assert!(!d.outputs.inhibit);
        assert_eq!((d.outputs.discharge, d.outputs.light, d.activity), (dis, light, act), "pct {p}");
    }
}
#[test] fn cycle_lid_closed_reports_not_enforced() {
    let d = decide(&s(), Override::None, &i(82, true, true, Cycle), &prev(false, true));
    assert_eq!((d.outputs.discharge, d.activity), (false, LidClosed));
}
#[test] fn full_charge_override_until_unplug() {
    let d = decide(&s(), Override::FullCharge, &i(90, true, false, Hold), &prev(true, false));
    assert_eq!((d.outputs, d.activity, d.clear_override), (Outputs { inhibit: false, discharge: false, light: Some(System) }, Charging { to: 100 }, false));
    let d = decide(&s(), Override::FullCharge, &i(90, false, false, Hold), &prev(false, false));
    assert!(d.clear_override);
    assert_eq!(d.activity, OnBattery);
}
#[test] fn discharge_to_target_then_clears() {
    let d = decide(&s(), Override::DischargeTo(50), &i(60, true, false, Hold), &prev(false, false));
    assert_eq!((d.outputs.inhibit, d.outputs.discharge, d.activity), (true, true, Discharging { to: 50 }));
    let d = decide(&s(), Override::DischargeTo(50), &i(50, true, false, Hold), &prev(true, true));
    assert!(d.clear_override);
    assert!(!d.outputs.discharge);
}
#[test] fn never_discharges_at_or_below_floor() {
    for (ov, mode) in [(Override::DischargeTo(5), Hold), (Override::DischargeTo(5), Cycle)] {
        let d = decide(&s(), ov, &i(10, true, false, mode), &prev(false, true));
        assert!(!d.outputs.discharge, "{mode:?}");
    }
    let low = Settings { upper: 20, lower: 10, auto_discharge: true };
    let d = decide(&low, Override::None, &i(10, true, false, Cycle), &prev(false, true));
    assert!(!d.outputs.discharge);
}
#[test] fn unsupported_does_nothing() {
    let d = decide(&s(), Override::None, &i(95, true, false, Unsupported), &prev(false, false));
    assert_eq!((d.outputs, d.activity), (Outputs::default(), Paused));
}
```

- [ ] **Step 2: Run `cargo test -p eighty-core policy`.** Expected: compile failure.
- [ ] **Step 3: Implement `decide`** per the rules above.
- [ ] **Step 4: Run `cargo test -p eighty-core`.** Expected: PASS.
- [ ] **Step 5: Commit** — `feat(core): add charge policy for hold and cycle modes`

---

### Task 3: Protocol and other-limiter finder

**Files:**
- Create: `crates/eighty-core/src/proto.rs`, `crates/eighty-core/src/limiters.rs`

**Interfaces:**
- Consumes: `Settings`, `ControlMode`, `Activity`, `Override`, `MachineInfo` (add `eighty-hw` as a dependency of core for this type only; it doesn't call hardware)
- Produces:
  - `#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)] #[serde(tag = "op", rename_all = "snake_case")] pub enum Request { Status, Doctor, SetLimits { upper: u8, lower: Option<u8> }, SetAutoDischarge { on: bool }, ChargeFull, ChargeAuto, DischargeTo { percent: u8 }, ClearFault, AcceptOtherLimiters }`
  - `Request::is_mutating(&self) -> bool`. False only for `Status` and `Doctor`.
  - `pub enum Health { Ok, Unsupported, OtherLimiters(Vec<String>), Conflict { key: String, expected: String, found: String }, WriteFailed { key: String, error: String }, NotEffective { what: String }, BatteryUnreadable(String) }`
  - `Health::is_ok(&self) -> bool`
  - `Health::message(&self) -> String`, with this exact copy for each variant:
    - Unsupported: `this Mac has no SMC key eighty can use to control charging. Run \`eighty doctor\` and report it.`
    - OtherLimiters: `another charge limiter is installed ({names joined ", "}). Remove it, or run \`eighty doctor --accept-other-limiters\`.`
    - Conflict: `{key} was changed by something else (expected {expected}, found {found}). Another charge limiter or macOS's own Charge Limit may be active. eighty has stopped writing; fix it, then run \`eighty doctor --retry\`.`
    - WriteFailed: `writing {key} failed: {error}. Charging restored. Run \`eighty doctor --retry\`.`
    - NotEffective: `{what} had no effect on this Mac. Charging restored. Run \`eighty doctor --retry\`.`
    - BatteryUnreadable: `can't read the battery: {0}. Charging restored; retrying.`
  - `pub struct Status { pub percent: u8, pub external: bool, pub charging: bool, pub amperage_ma: i32, pub mode: ControlMode, pub activity: Activity, pub health: Health, pub settings: Settings, pub override_: Override, pub light_supported: bool, pub warnings: Vec<String>, pub version: String }`
  - `pub enum KeyState { Usable, Missing, Blocked, WrongSize { size: u32 }, Error(String) }`
  - `pub struct DoctorReport { pub machine: MachineInfo, pub keys: Vec<(String, KeyState)>, pub status: Status, pub other_limiters: Vec<String> }`
  - `pub enum Response { Status(Status), Doctor(Box<DoctorReport>), Error(String) }`
  - All of these derive `Serialize, Deserialize, Debug, Clone, PartialEq`.
  - `pub const MAX_LINE: usize = 64 * 1024`
  - `pub fn write_msg<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()>` writes one JSON line.
  - `pub fn read_msg<T: DeserializeOwned>(r: &mut impl BufRead) -> io::Result<T>`. Reads at most `MAX_LINE` bytes; `InvalidData` on a bad line or an over-long one.
  - `pub struct KnownLimiter { pub name: &'static str, pub paths: &'static [&'static str], pub labels: &'static [&'static str] }`. A path starting with `~/` is checked in every home directory.
  - `pub const KNOWN_LIMITERS: &[KnownLimiter]`
  - `pub fn find_other_limiters(root: &Path, homes: &[PathBuf], label_loaded: &mut dyn FnMut(&str) -> bool) -> Vec<String>`. `root` prefixes absolute paths (`/` in production, a tempdir in tests). Returns limiter names, deduplicated, in catalog order.

`KNOWN_LIMITERS` entries:
- `battery`:
  - paths `~/Library/LaunchAgents/battery.plist`, `/usr/local/co.palokaj.battery`, `/etc/sudoers.d/battery`
  - labels `com.battery.app`
- `batt`:
  - paths `/Library/LaunchDaemons/cc.chlc.batt.plist`
  - labels `cc.chlc.batt`
- `BattOpt`: read BattOpt's install script (github.com/js4jiang5/BattOpt) for its LaunchAgent/Daemon plist paths and labels, and add them. Learning file names isn't copying code (spec §4).

- [ ] **Step 1: Write failing tests**

```rust
// proto.rs
#[test] fn request_json_shape() {
    assert_eq!(serde_json::to_string(&Request::SetLimits { upper: 80, lower: None }).unwrap(), r#"{"op":"set_limits","upper":80,"lower":null}"#);
}
#[test] fn framing_roundtrip() {
    let mut buf = Vec::new();
    write_msg(&mut buf, &Request::DischargeTo { percent: 50 }).unwrap();
    assert!(buf.ends_with(b"\n"));
    let r: Request = read_msg(&mut &buf[..]).unwrap();
    assert_eq!(r, Request::DischargeTo { percent: 50 });
}
#[test] fn rejects_overlong_line() {
    let line = vec![b'a'; MAX_LINE + 10];
    let e = read_msg::<Request>(&mut &line[..]).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
}
#[test] fn only_status_and_doctor_are_read_only() {
    assert!(!Request::Status.is_mutating() && !Request::Doctor.is_mutating());
    assert!(Request::ChargeAuto.is_mutating());
}
#[test] fn conflict_message_mentions_native_limit() {
    let h = Health::Conflict { key: "CHTE".into(), expected: "01000000".into(), found: "00000000".into() };
    assert!(h.message().contains("macOS's own Charge Limit"));
}
// limiters.rs
#[test] fn finds_battery_leftovers_by_file_and_label() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("Users/a");
    std::fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
    std::fs::write(home.join("Library/LaunchAgents/battery.plist"), "").unwrap();
    assert_eq!(find_other_limiters(root.path(), &[home.clone()], &mut |_| false), vec!["battery"]);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(find_other_limiters(empty.path(), &[], &mut |l| l == "cc.chlc.batt"), vec!["batt"]);
    assert!(find_other_limiters(empty.path(), &[], &mut |_| false).is_empty());
}
```

- [ ] **Step 2: Run `cargo test -p eighty-core`.** Expected: compile failure.
- [ ] **Step 3: Implement.** For `read_msg`, call `read_until(b'\n')` on `r.take(MAX_LINE as u64 + 1)`. If the result is longer than `MAX_LINE`, return `InvalidData`. EOF before any byte → `UnexpectedEof`.
- [ ] **Step 4: Run `cargo test -p eighty-core`.** Expected: PASS.
- [ ] **Step 5: Commit** — `feat(core): add IPC protocol and other-limiter detection`

---

### Task 4: `SmcControl` — mode detection, apply, conflict check, restore

**Files:**
- Create: `crates/eighty-helper/{Cargo.toml,src/lib.rs,src/control.rs}`

**Interfaces:**
- Consumes: `SmcIo`, `FourCC`, `hex`, `FakeSmc`/`WriteMode` (dev, feature `fake`), `ControlMode`, `Outputs`, `Light`, `KeyState`
- Produces:
  - `pub struct Detection { pub mode: ControlMode, pub light: bool, pub keys: Vec<(String, KeyState)> }`
  - `pub fn detect(smc: &mut dyn SmcIo) -> Detection`
  - `pub fn detect_read_only(smc: &mut dyn SmcIo) -> Detection`. Same as `detect` but skips the write-back: a readable key of the right size counts as Usable. Used while other limiters are present.
  - `pub struct SmcControl { mode: ControlMode, light: bool, last: HashMap<&'static str, Vec<u8>> }`
  - `SmcControl::new(d: &Detection) -> SmcControl`
  - `SmcControl::apply(&mut self, smc: &mut dyn SmcIo, o: &Outputs) -> Result<bool, (String, SmcError)>`. Returns true if any charging key (`CHTE`/`CHIE`) was written. The error carries the key name.
  - `SmcControl::check_unchanged(&self, smc: &mut dyn SmcIo) -> Result<(), Health>`. Returns `Health::Conflict` for `CHTE`/`CHIE` only.
  - `SmcControl::restore(&mut self, smc: &mut dyn SmcIo) -> Vec<(String, SmcError)>`. Writes the restore state to the present keys and clears `last`.
  - `pub fn restore_all(smc: &mut dyn SmcIo) -> Vec<(String, SmcError)>`. Same, without any detection; used by `--restore`.

Details:
- `detect` checks each of `CHTE` (size 4), `CHIE` (size 1) and `ACLC` (size 1):
  - `key_info` → `KeyNotFound` = Missing; `NotPrivileged` = Blocked; a size mismatch = WrongSize.
  - Then it reads the key and writes the same bytes back; `NotPrivileged` = Blocked.
  - Otherwise the key is Usable.
- Mode: `CHTE` and `CHIE` both usable → `Hold`; only `CHIE` → `Cycle`; anything else → `Unsupported`. `light = ACLC` usable.
- `apply`:
  - Writes each key only when the desired bytes differ from `last`, or `last` has no entry for it. Keys the mode doesn't have are skipped.
  - The light: `Some(l)` is written whenever the current read value differs from it (re-assert, since macOS may change it). `None` means no light write.
- `check_unchanged` only compares keys present in `last`.

- [ ] **Step 1: Write failing tests**

```rust
fn hold_smc() -> FakeSmc { let s = FakeSmc::new(); s.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept); s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); s }
fn cycle_smc() -> FakeSmc { let s = FakeSmc::new(); s.insert("CHIE", "ui8 ", &[0], WriteMode::Accept); s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); s }
fn on(inhibit: bool, discharge: bool, light: Option<Light>) -> Outputs { Outputs { inhibit, discharge, light } }

#[test] fn detects_hold_cycle_unsupported() {
    assert_eq!(detect(&mut hold_smc()).mode, ControlMode::Hold);
    assert_eq!(detect(&mut cycle_smc()).mode, ControlMode::Cycle);
    let m2 = FakeSmc::new(); m2.insert("CHIE", "ui8 ", &[0], WriteMode::NotPrivileged);
    let d = detect(&mut m2.clone());
    assert_eq!(d.mode, ControlMode::Unsupported);
    assert!(d.keys.contains(&("CHIE".into(), KeyState::Blocked)));
    assert!(d.keys.contains(&("CHTE".into(), KeyState::Missing)));
}
#[test] fn detect_writes_back_same_value_only() {
    let s = hold_smc(); s.insert("CHIE", "ui8 ", &[8], WriteMode::Accept);
    detect(&mut s.clone());
    assert_eq!(s.value("CHIE"), Some(vec![8]));
}
#[test] fn apply_hold_writes_exact_bytes_once() {
    let s = hold_smc(); let mut smc = s.clone();
    let mut c = SmcControl::new(&detect(&mut smc));
    let before = s.writes().len();
    assert!(c.apply(&mut smc, &on(true, false, Some(Light::Green))).unwrap());
    assert_eq!(s.value("CHTE"), Some(vec![1, 0, 0, 0]));
    assert_eq!(s.value("ACLC"), Some(vec![3]));
    let n = s.writes().len();
    assert!(!c.apply(&mut smc, &on(true, false, Some(Light::Green))).unwrap());
    assert_eq!(s.writes().len(), n);
    assert!(n > before);
}
#[test] fn apply_cycle_never_touches_chte_and_reasserts_light() {
    let s = cycle_smc(); let mut smc = s.clone();
    let mut c = SmcControl::new(&detect(&mut smc));
    c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap();
    assert_eq!(s.value("CHIE"), Some(vec![8]));
    s.insert("ACLC", "ui8 ", &[4], WriteMode::Accept); // macOS reset the light
    c.apply(&mut smc, &on(true, true, Some(Light::Green))).unwrap();
    assert_eq!(s.value("ACLC"), Some(vec![3]));
}
#[test] fn detect_read_only_never_writes() {
    let s = hold_smc(); assert_eq!(detect_read_only(&mut s.clone()).mode, ControlMode::Hold);
    assert!(s.writes().is_empty());
}
#[test] fn external_change_is_conflict() {
    let s = hold_smc(); let mut smc = s.clone();
    let mut c = SmcControl::new(&detect(&mut smc));
    c.apply(&mut smc, &on(true, false, None)).unwrap();
    s.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
    assert!(matches!(c.check_unchanged(&mut smc), Err(Health::Conflict { ref key, .. }) if key == "CHTE"));
}
#[test] fn restore_writes_restore_state() {
    let s = hold_smc(); s.insert("CHTE", "ui32", &[1, 0, 0, 0], WriteMode::Accept); s.insert("CHIE", "ui8 ", &[8], WriteMode::Accept);
    assert!(restore_all(&mut s.clone()).is_empty());
    assert_eq!((s.value("CHTE"), s.value("CHIE"), s.value("ACLC")), (Some(vec![0; 4]), Some(vec![0]), Some(vec![0])));
}
#[test] fn write_error_names_key() {
    let s = hold_smc(); let mut smc = s.clone();
    let mut c = SmcControl::new(&detect(&mut smc));
    s.insert("CHTE", "ui32", &[0; 4], WriteMode::NotPrivileged);
    assert_eq!(c.apply(&mut smc, &on(true, false, None)).unwrap_err().0, "CHTE");
}
```

- [ ] **Step 2: Run `cargo test -p eighty-helper control`.** Expected: compile failure.
- [ ] **Step 3: Implement** `control.rs`. Crate dependencies: `eighty-hw`, `eighty-core`, `serde_json`, `libc`, `ctrlc`, `anyhow`, `core-foundation`. Dev-dependencies: `eighty-hw` with `fake`, `tempfile`.
- [ ] **Step 4: Run `cargo test -p eighty-helper`.** Expected: PASS.
- [ ] **Step 5: Commit** — `feat(helper): add SMC control with mode detection and restore`

---

### Task 5: Config store and system probe

**Files:**
- Create: `crates/eighty-helper/src/store.rs`, `crates/eighty-helper/src/system.rs`, `crates/eighty-hw/src/system.rs`

**Interfaces:**
- Produces:
  - `pub struct Store { dir: PathBuf }`
  - `Store::new(dir: PathBuf) -> Store`
  - `Store::load(&self) -> (Config, Option<String>)`. The warning is `Some` when the file existed but was unreadable or invalid. A missing file means defaults with no warning.
  - `Store::save(&self, c: &Config) -> io::Result<()>`. Writes `config.toml.tmp`, then renames; creates the dir with mode 0755.
  - `eighty_hw::system::lid_closed() -> Option<bool>`. Reads the `AppleClamshellState` boolean of the `IOPMrootDomain` service, using the same pattern as `battery/ioreg.rs`.
  - `eighty_hw::system::console_uid() -> Option<u32>`. The owner uid of `/dev/console`.
  - `pub trait SystemProbe { fn lid_closed(&mut self) -> bool; fn console_uid(&mut self) -> Option<u32>; fn other_limiters(&mut self) -> Vec<String>; fn machine(&mut self) -> MachineInfo; }`
  - `pub struct MacSystem`. `lid_closed` treats `None` as false. `other_limiters` calls `find_other_limiters(Path::new("/"), <dirs under /Users except Shared>, label check)`. The label check runs `launchctl print gui/<uid>/<label>` for each uid that owns a `/Users/*` dir, plus `launchctl print system/<label>`; exit status 0 means loaded.
  - `#[cfg(test)] pub struct FakeSystem(pub Rc<RefCell<FakeSystemState>>)`, with `pub struct FakeSystemState { pub lid_closed: bool, pub console_uid: Option<u32>, pub other_limiters: Vec<String> }`. `Clone` shares the state. `Default` has the console uid set to 501.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn missing_config_is_defaults_without_warning() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(Store::new(d.path().into()).load(), (Config::default(), None));
}
#[test] fn save_then_load() {
    let d = tempfile::tempdir().unwrap(); let st = Store::new(d.path().join("eighty"));
    let c = Config { settings: Settings { upper: 70, lower: 60, auto_discharge: false }, accept_other_limiters: true };
    st.save(&c).unwrap();
    assert_eq!(st.load(), (c, None));
}
#[test] fn invalid_config_falls_back_with_warning() {
    let d = tempfile::tempdir().unwrap(); let st = Store::new(d.path().into());
    for bad in ["not toml [[[", "[settings]\nupper = 300\n", "[settings]\nupper = 80\nlower = 90\n"] {
        std::fs::write(d.path().join("config.toml"), bad).unwrap();
        let (c, w) = st.load();
        assert_eq!(c, Config::default());
        assert!(w.unwrap().contains("config.toml"));
    }
}
// eighty-hw/src/system.rs
#[test] #[ignore = "hardware"] fn reads_lid_and_console() { assert!(lid_closed().is_some()); assert!(console_uid().is_some()); }
```

- [ ] **Step 2: Run `cargo test -p eighty-helper store`.** Expected: compile failure.
- [ ] **Step 3: Implement.** A `u8` that overflows (300) is a parse error, which counts as invalid.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo test -p eighty-hw -- --ignored reads_lid_and_console`. Expected: PASS on this Mac.
- [ ] **Step 5: Commit** — `feat(helper): add config store and system probe`

---

### Task 6: Engine — startup, ticks, health, shutdown

**Files:**
- Create: `crates/eighty-helper/src/engine.rs`

**Interfaces:**
- Consumes: Tasks 2–5
- Produces:
  - `pub enum Event { Tick, PowerChanged, WillSleep(mpsc::Sender<()>), DidWake, Request { uid: u32, req: Request, reply: mpsc::Sender<Response> }, Shutdown }`
  - `pub struct Deps<S: SmcIo, B: BatterySource, Y: SystemProbe> { pub smc: S, pub battery: B, pub system: Y, pub store: Store, pub clock: Box<dyn FnMut() -> u64> }`. The clock returns Unix seconds.
  - `Engine::start(d: Deps<S,B,Y>) -> Engine<S,B,Y>`
  - `Engine::handle(&mut self, ev: Event) -> bool`. Returns false after `Shutdown`.
  - `Engine::run(mut self, rx: Receiver<Event>)`. Loops on `recv_timeout(TICK_SECS)`; a timeout is a `Tick`.
  - `Engine::status(&self) -> Status`

Behavior:
- **`start`:**
  1. Load the config. A warning goes into `warnings`.
  2. Run `detect`.
  3. If `!accept_other_limiters` and `other_limiters()` is non-empty: `Health::OtherLimiters`, and **no writes at all**.
  4. Else if the mode is `Unsupported`: `Health::Unsupported`, no writes.
  5. Else `restore` (crash recovery), then `tick`.
- **`tick`:** does nothing unless health is `Ok` or `BatteryUnreadable`.
  1. Take a snapshot. On error → `BatteryUnreadable` + restore, then stop. On success, clear `BatteryUnreadable`.
  2. `check_unchanged`. On error → that `Conflict`, and **no restore** (don't fight).
  3. `decide`, and apply `clear_override`.
  4. `apply`. On error → `WriteFailed` + restore. If `CHTE`/`CHIE` were written, record `pending = Some((now, outputs))` for the watchdog (Task 7).
  5. Store the outputs as `prev`.
- **Health while not `Ok`:** activity `Paused`. Status still shows battery numbers from the latest snapshot. The engine takes a snapshot even when paused, without writing.
- **`Shutdown`:** restore unless health is `OtherLimiters`, `Conflict` or `Unsupported`.
- **`PowerChanged`, `DidWake`:** tick.
- **`WillSleep(ack)`:** tick, then `ack.send(())`.
- Every state change is logged with `eprintln!` as `<RFC3339-ish UTC timestamp> <message>`, e.g. health changes, mode, outputs written, and overrides.

- [ ] **Step 1: Write failing tests**

A test `Rig` holds a `FakeSmc`, a shared `Rc<Cell<BatterySnapshot>>` behind a `FakeBattery`, a `FakeSystem`, a tempdir `Store` and a `Rc<Cell<u64>>` clock. It has:
- `rig.engine()` → `Engine::start(..)`
- `rig.set(pct, charging, external, amp, update_time)`

```rust
#[test] fn startup_hold_at_limit_inhibits_and_lights_green() {
    let r = Rig::hold(); r.set(80, true, true, 1200, Some(900));
    let e = r.engine();
    assert_eq!(e.status().mode, ControlMode::Hold);
    assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
    assert_eq!(r.smc.value("ACLC"), Some(vec![3]));
    assert_eq!(e.status().activity, Activity::Holding);
}
#[test] fn startup_recovers_stale_discharge() {
    let r = Rig::hold(); r.smc.insert("CHIE", "ui8 ", &[8], WriteMode::Accept); r.set(60, false, true, -400, Some(900));
    let _e = r.engine();
    assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
}
#[test] fn other_limiter_blocks_all_writes() {
    let r = Rig::hold(); r.sys.0.borrow_mut().other_limiters = vec!["battery".into()]; r.set(90, true, true, 1000, Some(900));
    let mut e = r.engine(); e.handle(Event::Tick); e.handle(Event::Shutdown);
    assert!(r.smc.writes().is_empty());
    assert!(matches!(e.status().health, Health::OtherLimiters(_)));
    assert_eq!(e.status().activity, Activity::Paused);
}
#[test] fn unsupported_reports_and_writes_nothing() {
    let r = Rig::with(FakeSmc::new()); r.set(90, true, true, 1000, Some(900));
    let e = r.engine();
    assert_eq!(e.status().health, Health::Unsupported);
    assert!(r.smc.writes().is_empty());
}
#[test] fn conflict_stops_writing_and_does_not_restore() {
    let r = Rig::hold(); r.set(80, true, true, 1200, Some(900));
    let mut e = r.engine();
    r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept);
    let n = r.smc.writes().len();
    e.handle(Event::Tick); e.handle(Event::Shutdown);
    assert!(matches!(e.status().health, Health::Conflict { .. }));
    assert_eq!(r.smc.writes().len(), n);
}
#[test] fn write_failure_restores_and_reports() {
    let r = Rig::hold(); r.set(70, true, true, 1200, Some(900));
    let mut e = r.engine();
    r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::NotPrivileged);
    r.set(80, true, true, 1200, Some(960)); e.handle(Event::Tick);
    assert!(matches!(e.status().health, Health::WriteFailed { ref key, .. } if key == "CHTE"));
}
#[test] fn battery_unreadable_restores_then_recovers() {
    // Rig::hold_fallible: the battery closure returns Err while r.fail is set
    let r = Rig::hold_fallible(); r.set(85, true, true, 1200, Some(900));
    let mut e = r.engine();
    assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
    r.fail.set(true); e.handle(Event::Tick);
    assert!(matches!(e.status().health, Health::BatteryUnreadable(_)));
    assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
    r.fail.set(false); e.handle(Event::Tick);
    assert_eq!(e.status().health, Health::Ok);
}
#[test] fn shutdown_restores() {
    let r = Rig::hold(); r.set(85, true, true, 1200, Some(900));
    let mut e = r.engine(); assert!(!e.handle(Event::Shutdown));
    assert_eq!((r.smc.value("CHTE"), r.smc.value("CHIE"), r.smc.value("ACLC")), (Some(vec![0; 4]), Some(vec![0]), Some(vec![0])));
}
#[test] fn will_sleep_ticks_then_acks() {
    let r = Rig::hold(); r.set(70, true, true, 1200, Some(900));
    let mut e = r.engine(); r.set(80, true, true, 1200, Some(960));
    let (tx, rx) = std::sync::mpsc::channel(); e.handle(Event::WillSleep(tx));
    rx.try_recv().unwrap();
    assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
}
#[test] fn cycle_mode_on_m2_shape() {
    let r = Rig::cycle(); r.set(80, true, true, 2700, Some(900));
    let e = r.engine();
    assert_eq!(e.status().mode, ControlMode::Cycle);
    assert_eq!(r.smc.value("CHIE"), Some(vec![8]));
}
```

- [ ] **Step 2: Run `cargo test -p eighty-helper engine`.** Expected: compile failure.
- [ ] **Step 3: Implement `engine.rs`.** In `start`, check other limiters **before** detection. If they're blocking, use `detect_read_only`, so not even the write-back happens.
- [ ] **Step 4: Run `cargo test -p eighty-helper`.** Expected: PASS.
- [ ] **Step 5: Commit** — `feat(helper): add engine with startup recovery, health and shutdown restore`

---

### Task 7: Engine — watchdog and requests

**Files:**
- Modify: `crates/eighty-helper/src/engine.rs`

**Interfaces:**
- Consumes: Task 6 engine, `Request`, `Response`, `DoctorReport`
- Produces:
  - Watchdog behavior inside `tick`.
  - `Event::Request` handling.

Watchdog (spec §5.2 "verifiably works"):
- With `pending = Some((t, o))`, evaluate the first snapshot where `update_time >= t + SETTLE_SECS` and `external` is true:
  - `o.discharge && amperage_ma >= 0` → `NotEffective { what: "CHIE (run from battery)" }`
  - else `o.inhibit && !o.discharge && is_charging && amperage_ma > 50` → `NotEffective { what: "CHTE (stop charging)" }`
- On a fault: restore.
- After evaluating, clear `pending`.
- A snapshot with `update_time == None`, or one older than the threshold, leaves `pending` in place.
- If `external` is false, clear `pending` without judging.

Requests:
- Mutating requests need `uid == 0 || Some(uid) == system.console_uid()`. Otherwise reply `Error("permission denied: only an admin (sudo) or the logged-in user can change eighty")`.
- `SetLimits` and `SetAutoDischarge`: validate with `Settings::with_limits`; if invalid, reply `Error(msg)`. Otherwise save the config, tick, and reply `Status`.
- `ChargeFull` / `ChargeAuto`: set `Override::FullCharge` / `None`, tick, reply `Status`.
- `DischargeTo { percent }`:
  - `percent < HARD_FLOOR || percent > 100` → `Error("discharge target must be between 10 and 100")`.
  - In `Unsupported`, or a non-`Ok` health → `Error(health.message())`.
  - Otherwise set the override, tick, and reply.
- `ClearFault`: re-run the `start` sequence minus the config load (limiters, detect, restore, tick), then reply `Status`.
- `AcceptOtherLimiters`: set `accept_other_limiters = true`, save, behave like `ClearFault`.
- `Status`: reply `Status`.
- `Doctor`: reply `Doctor` with `machine()`, the detection keys, the status and `other_limiters()` (always the live result, even if accepted).

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn watchdog_flags_ineffective_inhibit() {
    let r = Rig::hold(); r.clock.set(1000); r.set(80, true, true, 1200, Some(990));
    let mut e = r.engine();                                  // writes CHTE at t=1000
    r.set(80, true, true, 1200, Some(1010)); e.handle(Event::Tick); // too fresh: no verdict
    assert_eq!(e.status().health, Health::Ok);
    r.set(80, true, true, 1200, Some(1030)); e.handle(Event::Tick);
    assert!(matches!(e.status().health, Health::NotEffective { ref what } if what.starts_with("CHTE")));
    assert_eq!(r.smc.value("CHTE"), Some(vec![0; 4]));
}
#[test] fn watchdog_accepts_effective_discharge() {
    let r = Rig::cycle(); r.clock.set(1000); r.set(81, true, true, 2700, Some(990));
    let mut e = r.engine();
    r.set(81, false, true, -410, Some(1030)); e.handle(Event::Tick);
    assert_eq!(e.status().health, Health::Ok);
}
#[test] fn watchdog_waits_on_missing_update_time() {
    let r = Rig::cycle(); r.clock.set(1000); r.set(81, true, true, 2700, None);
    let mut e = r.engine(); e.handle(Event::Tick);
    assert_eq!(e.status().health, Health::Ok);
}
fn req(e: &mut Engine<..>, uid: u32, q: Request) -> Response { let (tx, rx) = mpsc::channel(); e.handle(Event::Request { uid, req: q, reply: tx }); rx.recv().unwrap() }
#[test] fn non_console_user_cannot_change_but_can_read() {
    let r = Rig::hold(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
    assert!(matches!(req(&mut e, 502, Request::ChargeFull), Response::Error(m) if m.starts_with("permission denied")));
    assert!(matches!(req(&mut e, 502, Request::Status), Response::Status(_)));
    assert!(matches!(req(&mut e, 0, Request::ChargeFull), Response::Status(_)));
}
#[test] fn set_limits_persists_and_applies() {
    let r = Rig::hold(); r.set(72, true, true, 1200, Some(900)); let mut e = r.engine();
    let Response::Status(s) = req(&mut e, 501, Request::SetLimits { upper: 70, lower: None }) else { panic!() };
    assert_eq!(s.settings.lower, 65);
    assert_eq!(r.smc.value("CHTE"), Some(vec![1, 0, 0, 0]));
    assert_eq!(r.store.load().0.settings.upper, 70);
    assert!(matches!(req(&mut e, 501, Request::SetLimits { upper: 10, lower: None }), Response::Error(_)));
}
#[test] fn discharge_below_floor_rejected() {
    let r = Rig::hold(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
    assert!(matches!(req(&mut e, 501, Request::DischargeTo { percent: 9 }), Response::Error(_)));
}
#[test] fn clear_fault_retries() {
    let r = Rig::hold(); r.set(80, true, true, 1200, Some(900)); let mut e = r.engine();
    r.smc.insert("CHTE", "ui32", &[0; 4], WriteMode::Accept); e.handle(Event::Tick);
    assert!(matches!(e.status().health, Health::Conflict { .. }));
    req(&mut e, 501, Request::ClearFault);
    assert_eq!(e.status().health, Health::Ok);
}
#[test] fn doctor_lists_keys_and_limiters() {
    let r = Rig::cycle(); r.set(70, true, true, 1200, Some(900)); let mut e = r.engine();
    let Response::Doctor(d) = req(&mut e, 502, Request::Doctor) else { panic!() };
    assert!(d.keys.contains(&("CHTE".into(), KeyState::Missing)));
}
```

- [ ] **Step 2: Run `cargo test -p eighty-helper engine`.** Expected: the new tests fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p eighty-helper`.** Expected: PASS.
- [ ] **Step 5: Commit** — `feat(helper): verify effects with a watchdog and handle client requests`

---

### Task 8: IPC server and client

**Files:**
- Create: `crates/eighty-helper/src/ipc.rs`, `crates/eighty-cli/{Cargo.toml,src/client.rs}`

**Interfaces:**
- Consumes: `Event`, `read_msg`/`write_msg`, `Request`/`Response`
- Produces:
  - `pub fn bind(path: &Path) -> io::Result<UnixListener>`. Removes a stale socket, binds, and sets mode 0o666.
  - `pub fn serve(l: UnixListener, tx: Sender<Event>)`. Handles one connection at a time:
    - Read timeout 2 s; reply wait ≤ 10 s. Peer uid comes from `libc::getpeereid`.
    - A bad request gets `Response::Error("bad request: …")`.
    - It never panics on client I/O errors.
    - It returns when `tx` sends fail (engine gone).
  - `eighty_cli::client::call(path: &Path, req: &Request) -> Result<Response, ClientError>`. 5 s read timeout.
  - `#[derive(thiserror::Error)] pub enum ClientError { NotRunning, Io(io::Error), Protocol(String) }`. `NotRunning` covers connect failing with `NotFound` or `ConnectionRefused`. Its message: `the eighty helper isn't running. Install it with \`sudo eighty install\`.`

- [ ] **Step 1: Write failing tests** (in `ipc.rs`; use a fake engine thread that echoes `Status` requests as `Error(format!("uid {uid}"))`)

```rust
#[test] fn request_reaches_engine_with_peer_uid() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("s.sock");
    let (tx, rx) = mpsc::channel(); let l = bind(&path).unwrap();
    std::thread::spawn(move || serve(l, tx)); spawn_echo_engine(rx);
    let mut c = UnixStream::connect(&path).unwrap();
    write_msg(&mut c, &Request::Status).unwrap();
    let r: Response = read_msg(&mut BufReader::new(c)).unwrap();
    assert_eq!(r, Response::Error(format!("uid {}", unsafe { libc::getuid() })));
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o666);
}
#[test] fn garbage_and_silent_clients_do_not_block_server() {
    // set up as above, then:
    let silent = UnixStream::connect(&path).unwrap();            // never writes
    let mut bad = UnixStream::connect(&path).unwrap(); bad.write_all(b"{nope\n").unwrap();
    let mut long = UnixStream::connect(&path).unwrap(); let _ = long.write_all(&vec![b'a'; MAX_LINE + 1]);
    let t = std::time::Instant::now();
    let mut ok = UnixStream::connect(&path).unwrap(); write_msg(&mut ok, &Request::Status).unwrap();
    let _: Response = read_msg(&mut BufReader::new(ok)).unwrap();
    assert!(t.elapsed() < Duration::from_secs(8));
    let r: Response = read_msg(&mut BufReader::new(bad)).unwrap();
    assert!(matches!(r, Response::Error(m) if m.starts_with("bad request")));
    drop(silent);
}
#[test] fn bind_replaces_stale_socket() { /* create a file at path; bind succeeds */ }
// eighty-cli client.rs
#[test] fn not_running_when_no_socket() {
    let d = tempfile::tempdir().unwrap();
    assert!(matches!(call(&d.path().join("none.sock"), &Request::Status), Err(ClientError::NotRunning)));
}
```

- [ ] **Step 2: Run `cargo test -p eighty-helper ipc && cargo test -p eighty-cli`.** Expected: compile failure.
- [ ] **Step 3: Implement.** `eighty-cli` is a `[[bin]] name = "eighty"` crate with a `lib.rs` exposing `client`, `render` and `install`. Dependencies: `eighty-core`, `clap`, `anyhow`, `thiserror`, `serde_json`, `libc`.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** — `feat: add Unix-socket IPC with peer-uid auth`

---

### Task 9: OS events and helper binary

**Files:**
- Create: `crates/eighty-helper/src/os_events.rs`, `crates/eighty-helper/src/main.rs`

**Interfaces:**
- Consumes: `Engine`, `Deps`, `ipc::{bind, serve}`, `restore_all`, `SmcConnection`, `IoRegBattery`, `MacSystem`, `Store`
- Produces:
  - `pub fn spawn_os_events(tx: Sender<Event>)`. Starts a thread that runs a CFRunLoop with two sources:
    - **Power-source changes:** `IOPSNotificationCreateRunLoopSource(cb, ctx) -> CFRunLoopSourceRef`. The callback sends `PowerChanged`.
    - **System power:** `IORegisterForSystemPower(refcon, &mut port, cb, &mut notifier) -> io_connect_t` with `IONotificationPortGetRunLoopSource`. The callback handles three messages:
      - `kIOMessageCanSystemSleep` (`0xe0000270`): `IOAllowPowerChange(root, arg as isize)`.
      - `kIOMessageSystemWillSleep` (`0xe0000280`): send `WillSleep(ack)`, wait for the ack for up to 5 s, then `IOAllowPowerChange`.
      - `kIOMessageSystemHasPoweredOn` (`0xe0000300`): send `DidWake`.
  - Binary `eighty-helper`, with three modes:
    - **No args:** refuse unless `geteuid() == 0`. Engine thread with `Deps { SmcConnection::open()?, IoRegBattery, MacSystem, Store::new(CONFIG_DIR), clock = SystemTime unix secs }`; IPC thread on `SOCKET_PATH`; `spawn_os_events`; `ctrlc` handler (termination) sends `Shutdown`.
      - On SIGTERM/SIGINT, main joins the engine thread, removes the socket, and exits 0.
      - If the engine thread panics, the join returns `Err`. Main then calls `restore_all(&mut SmcConnection::open()?)`, logs it, and exits 1, so launchd restarts it.
    - **`--restore`:** root required; `restore_all`, print each key's result, exit 0 if no errors.
    - **`--version`.**

These are thin FFI layers with no unit tests (as in Plan 1). They're verified by the manual checks below and by Task 11.

- [ ] **Step 1: Implement `os_events.rs`.** Declare the FFI in the module: `IOKit` framework for `IORegisterForSystemPower`, `IOAllowPowerChange`, `IONotificationPortGetRunLoopSource`, and `IOPSNotificationCreateRunLoopSource`. Use `core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode}` to add the sources and run. Pass `Box::into_raw(Box::new(ctx))` as refcon. The context holds `tx` and the root `io_connect_t` (written after registration), and lives for the process.
- [ ] **Step 2: Implement `main.rs`.**
- [ ] **Step 3: Build and check by hand.** Run `cargo build --release -p eighty-helper && sudo ./target/release/eighty-helper`. In another terminal, run `echo '{"op":"status"}' | nc -U /var/run/eighty.sock`. Expected: a JSON `Status` with `"mode":"Hold"` on the M5.
  - Unplug: the helper log shows a power change within about 2 s.
  - Ctrl-C the helper: `sudo ./target/release/eighty-probe` shows `CHTE=00000000`, `CHIE=00`, `ACLC=00`.
- [ ] **Step 4: Run `cargo test --workspace && cargo clippy --workspace -- -D warnings`.** Expected: clean.
- [ ] **Step 5: Commit** — `feat(helper): wire OS events, IPC and signals into the helper binary`

---

### Task 10: CLI commands and rendering

**Files:**
- Create: `crates/eighty-cli/src/main.rs`, `crates/eighty-cli/src/render.rs`

**Interfaces:**
- Consumes: `client::call`, `Status`, `DoctorReport`, `Health::message`
- Produces:
  - `pub fn render_status(s: &Status) -> String`
  - `pub fn render_doctor(d: &DoctorReport) -> String`
  - Commands:
    - `eighty status [--json]`
    - `eighty limit <upper> [lower]`
    - `eighty auto-discharge <on|off>`
    - `eighty charge <full|auto>`
    - `eighty discharge <percent>`
    - `eighty doctor [--json] [--retry] [--accept-other-limiters]`
    - `eighty logs [-n <lines>=100]`
    - `eighty install`, `eighty uninstall [--purge]` (Task 11)
  - Exit codes: 0 ok; 1 error (`Response::Error`, I/O); 2 helper not running; 3 `status` when `!health.is_ok()`.
  - Commands that change settings print the resulting status.
  - `doctor --retry` sends `ClearFault`, and `--accept-other-limiters` sends `AcceptOtherLimiters`, before fetching `Doctor`.
  - `logs` prints the last N lines of `LOG_PATH`, read directly, so it works without the helper running.

Status copy (pinned):
```
NOT LIMITING: <health.message()>                          (only when !health.is_ok())
Battery   78% · charging 1197 mA · on AC                 ("on battery" when !external; "not charging" when !charging)
Limit     80% (resumes below 75%) · auto-discharge on
State     Charging to 80%                                 (Holding at 80% | Discharging to 75% | On battery | Lid closed: can't hold the limit in cycle mode | Paused)
Mode      hold (CHTE) · MagSafe light: yes                (cycle (CHIE only) | unsupported)
Override  charge to 100% until unplugged                  (or "discharge to 50%"; line omitted when none)
Warning   <each warning>
```
In cycle mode with `auto-discharge off`, the Limit line says `auto-discharge n/a in cycle mode`.

- [ ] **Step 1: Write failing tests** (`render.rs`)

```rust
fn st() -> Status { /* percent 78, external, charging, 1197 mA, Hold, Charging{to:80}, Ok, default settings, no override, light true, no warnings */ }
#[test] fn renders_ok_status() {
    let out = render_status(&st());
    assert_eq!(out.lines().next().unwrap(), "Battery   78% · charging 1197 mA · on AC");
    assert!(out.contains("Limit     80% (resumes below 75%) · auto-discharge on"));
    assert!(out.contains("State     Charging to 80%"));
    assert!(!out.contains("NOT LIMITING"));
}
#[test] fn fault_is_first_line() {
    let s = Status { health: Health::Unsupported, activity: Activity::Paused, ..st() };
    assert!(render_status(&s).starts_with("NOT LIMITING: this Mac has no SMC key"));
}
#[test] fn lid_closed_cycle_warns() {
    let s = Status { mode: ControlMode::Cycle, activity: Activity::LidClosed, ..st() };
    assert!(render_status(&s).contains("Lid closed: can't hold the limit in cycle mode"));
}
#[test] fn doctor_lists_every_key() {
    let out = render_doctor(&DoctorReport { /* keys CHTE Missing, CHIE Usable, ACLC Usable, machine M2 */ });
    assert!(out.contains("CHTE  missing") && out.contains("CHIE  usable") && out.contains("Mode      cycle"));
}
```

- [ ] **Step 2: Run `cargo test -p eighty-cli`.** Expected: compile failure.
- [ ] **Step 3: Implement** `render.rs` and `main.rs` (clap derive). For the doctor layout: machine (model, chip, macOS + build), helper version, mode, one line per key (`<KEY>  usable|missing|blocked|wrong size (<n> bytes)|error: <e>`), other limiters found (or `none`), health message, then the rendered status.
- [ ] **Step 4: Run `cargo test -p eighty-cli`.** Expected: PASS.
- [ ] **Step 5: Commit** — `feat(cli): add status, limit, charge, discharge, doctor and logs commands`

---

### Task 11: Install and uninstall

**Files:**
- Create: `crates/eighty-cli/src/install.rs`

**Interfaces:**
- Consumes: `paths::*`, `client::call`
- Produces:
  - `pub fn plist(helper_path: &str, log_path: &str) -> String`
  - `pub fn install() -> anyhow::Result<()>`
  - `pub fn uninstall(purge: bool) -> anyhow::Result<()>`

`plist` output: an XML plist with:
- `Label` = `LABEL`
- `ProgramArguments` = `[helper_path]`
- `RunAtLoad` = true, `KeepAlive` = true
- `StandardErrorPath` and `StandardOutPath` = `log_path`
- `ProcessType` = `Interactive`, so it isn't throttled on battery

`install` (root required, else the error `run with sudo`):
1. Find `eighty-helper` next to `std::env::current_exe()`.
2. If a plist already exists, `launchctl bootout system/<LABEL>`; ignore its failure.
3. Copy the helper to `HELPER_INSTALL_PATH` (mode 0755, owner root:wheel).
4. Create the dirs of `LOG_PATH` and `CONFIG_DIR`.
5. Write `PLIST_PATH` (0644, root:wheel).
6. Run `launchctl bootstrap system PLIST_PATH`.
7. Poll `client::call(Status)` for up to 5 s.
8. Print the status (with `NOT LIMITING` if applicable) and `installed`.

`uninstall` (root required). Each step logs and continues on error, so a half-installed state still gets cleaned:
1. `launchctl bootout system/<LABEL>` (SIGTERM → the helper restores).
2. Run `HELPER_INSTALL_PATH --restore` if it exists. If the binary is missing, run the sibling `eighty-helper --restore`.
3. Remove the plist, the helper binary and `SOCKET_PATH`.
4. If `purge`, also remove `CONFIG_DIR` and the log dir.
5. Print `charging restored; eighty uninstalled`.

- [ ] **Step 1: Write failing test**

```rust
#[test] fn plist_has_label_program_and_keepalive() {
    let p = plist("/Library/PrivilegedHelperTools/x", "/Library/Logs/eighty/helper.log");
    for needle in ["<string>io.github.mabujaber.eighty.helper</string>", "<string>/Library/PrivilegedHelperTools/x</string>",
                   "<key>KeepAlive</key>\n\t<true/>", "<key>RunAtLoad</key>\n\t<true/>", "<string>/Library/Logs/eighty/helper.log</string>"] {
        assert!(p.contains(needle), "{needle}");
    }
}
```

- [ ] **Step 2: Run `cargo test -p eighty-cli install`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Use one tab of indentation inside `<dict>`.
- [ ] **Step 4: Run `cargo test --workspace && cargo clippy --workspace -- -D warnings`.** Expected: PASS.
- [ ] **Step 5: Check by hand on the M5.**
  1. `cargo build --release && sudo ./target/release/eighty install`. Expected: status printed, `Mode hold (CHTE)`.
  2. `sudo pkill -9 -f io.github.mabujaber.eighty.helper`, then `eighty status` within 10 s. Expected: launchd restarted it, and the state is correct.
  3. `sudo ./target/release/eighty uninstall`. Expected: `eighty-probe` shows the restore state and the Mac charges.
- [ ] **Step 6: Commit** — `feat(cli): add install and uninstall with charging restore`

---

### Task 12: Acceptance on both Macs

**Files:**
- Create: `docs/testing/plan2-acceptance.md`
- Modify: `README.md` (create it if missing: what eighty is, build, `sudo eighty install`, commands, uninstall)

Run each check on the **M5 Pro (hold mode)** and the **M2 Max (cycle mode)**, with macOS Charge Limit at 100% and Optimized Charging off. Record each result (pass/fail + `eighty status` output) in the doc, as a table per Mac.

- [ ] **Step 1: Limit.**
  1. Install, then `eighty limit <current% + 2>`. Wait until it's reached.
  2. Expected on the M5: `Holding`, amperage ≈ 0, MagSafe light **green**.
  3. Expected on the M2: `Discharging to <lower>`, amperage < 0, light green. Then, at the lower limit, `Charging`, light **orange**.
- [ ] **Step 2: Auto-discharge.** `eighty limit <current% − 3>`. Expected: `Discharging to <upper>`, negative amperage, then hold/cycle.
- [ ] **Step 3: Overrides.** `eighty charge full` → light back to macOS, charging. Unplug, then replug → the override is cleared. `eighty discharge <current% − 2>` → discharges, then resumes normal limits.
- [ ] **Step 4: Sleep.**
  1. At the limit, close the lid with no external display (sleep) for 30+ min, then wake. Expected: still at or below the limit (record the %), and the helper log shows WillSleep/DidWake.
  2. Lid **open**: while discharging (cycle mode on the M2, auto-discharge on the M5), put the Mac to sleep via idle sleep or Apple menu → Sleep for 30+ min, then wake. Record the % before and after. Expected: no discharge while asleep (`CHIE=00`). On the M2, `eighty status` shows the warning `the limit isn't enforced while asleep in cycle mode; the battery may charge past <upper>%` until the charge is back at or below the upper limit.
  3. Record whether macOS changes `CHTE`/`CHIE` across sleep or wake. A `NOT LIMITING: … was changed by something else` (Conflict) right after wake means it does.
- [ ] **Step 4b: Watchdog across replug.** Run `eighty limit <current%>` so the M5 holds, unplug within 20 s, then replug. Expected: after a minute or two, still `Holding`, no `NotEffective` fault, amperage ≈ 0.
- [ ] **Step 5: Crash and reboot.** `sudo pkill -9 -f io.github.mabujaber.eighty.helper` while discharging → it restarts and recovers. Reboot → the helper starts before login (`eighty status` from SSH or right after login) and applies the limit.
- [ ] **Step 6: Conflict.** With the helper holding (M5), set macOS's own Charge Limit to 80%, then 100% again after a few minutes. Record whether eighty reported `NOT LIMITING: CHTE was changed by something else…`, and whether `eighty doctor --retry` recovered. Either way, this tells us whether the native limit writes `CHTE` (spec §5.5, input to Plan 3). The conflict path itself is covered by unit tests.
- [ ] **Step 7: Permissions.** As a second, non-admin user (or `sudo -u nobody eighty limit 70`): `permission denied`, while `eighty status` works.
- [ ] **Step 8: Uninstall.** `sudo eighty uninstall` → `eighty-probe` read shows the restore state, `ACLC=00` hands the light back to macOS (record the colour seen), and the Mac charges to 100%.
- [ ] **Step 9: Write it up.** Record the results and any bugs found (fix them in follow-up commits with tests). Update the README, then commit — `docs: add plan 2 acceptance results and README`.

---

## Out of scope (later plans)

Menu-bar app, `SMAppService`, notifications, native Charge Limit / Optimized Charging detection (Plan 3). Calibration, overheat, zones, dark-wake while charging asleep, PowerUI/MCL backend (Plan 4). Signing, notarization, Homebrew cask, CI (Plan 5).
