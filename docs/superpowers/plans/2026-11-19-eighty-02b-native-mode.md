# eighty Plan 2b — Native Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On Macs without `CHTE` (e.g. M2 Max on macOS 27), hold the battery with macOS's own Charge Limit instead of cycling, and remove cycle mode.

**Architecture:** A new `NativeLimit` trait in `eighty-hw` wraps the private `PowerUISmartChargeClient` (PowerUI.framework) through `objc2`, with a fake for tests. Mode detection becomes: `CHTE` usable → **Hold**; else MCL supported → **Native**; else **Unsupported**. In Native mode the policy asks for a native limit (the user's upper limit, or 100 during `charge full`) instead of `CHTE`, and still uses `CHIE` only for an explicit `discharge <n>`. The engine writes the native limit, reads it back each tick, adopts changes made in System Settings, and restores the user's original native limit only on uninstall.

**Tech Stack:** Rust stable (edition 2021), existing crates, plus `objc2` 0.6 and `objc2-foundation` 0.3 in `eighty-hw` (spec §7 names `objc2`). Look up the current `objc2` docs for `AnyClass::get`, `msg_send!`, `instance_method` and type encodings rather than relying on memory.

**Spec:** `docs/superpowers/specs/2026-11-16-eighty-design.md` — this plan implements §5.6 (native mode), the §5.2 PowerUI backend and mode order, §5.5's native-mode coexistence rule, and the §10 guard on private APIs. Evidence: `docs/testing/plan2-acceptance.md` (M2 Max, findings G–I).

## Global Constraints

- Mode order at startup: `CHTE` usable → Hold; else `isMCLSupported` and the guarded selectors present → Native; else Unsupported. `CHIE`-only Macs without MCL are Unsupported. Cycle mode is removed entirely.
- PowerUI: load `/System/Library/PrivateFrameworks/PowerUI.framework/PowerUI`, class `PowerUISmartChargeClient`, created with `initWithClientName:` (`@24@0:8@16`) using the name `"eighty"`. Only the synchronous selectors, never the `…WithHandler:` ones.
- Guarded selectors and their exact type encodings (from the M2 Max / macOS 27.2 runtime). If any is missing or differs, MCL counts as unsupported:
  - `isMCLSupported` `B16@0:8`
  - `availableChargeLimitsWithError:` `@24@0:8^@16`
  - `getMCLLimitWithError:` `C24@0:8^@16`
  - `isMCLCurrentlyEnabled:` `Q24@0:8^@16`
  - `setMCLLimit:error:` `B28@0:8C16^@20`
- Native limits are only the values `availableChargeLimitsWithError:` returns (80, 85, 90, 95, 100 today). 100 means "no limit". There is no lower limit in Native mode.
- Exact refusal text for a limit macOS doesn't offer: `this Mac only supports a limit of 80, 85, 90, 95 or 100%` (the list built from the available steps).
- Exact refusal text for a lower limit in Native mode: `macOS decides when charging resumes on this Mac, so there is no lower limit`.
- eighty never writes `ACLC` in Native mode. macOS sets the light.
- Hard floor, lid and sleep rules for `CHIE` are unchanged (`HARD_FLOOR = 10`).
- Shutdown, crash, fault and emergency restores write the SMC restore state only (`CHIE=00`, plus `CHTE`/`ACLC` where present). They never change the native limit. Only `eighty-helper --restore` (run by `eighty uninstall`) sets the native limit back to the recorded original.
- Hold mode behaviour on the M5 Pro must not change.

## Review Focus

1. **The user changes the limit in System Settings while eighty runs.** eighty adopts the new value as its setting, saves it and logs it. It never fights it or reports a Conflict. Pinned by `native_adopts_a_limit_changed_in_system_settings` (Task 4).
2. **macOS charges past the limit (its calibration, or anything else).** eighty can't stop it, so `status` shows a warning instead of claiming the limit holds, and nothing is restored or faulted. Pinned by `native_charging_past_the_limit_warns_without_faulting` (Task 4).
3. **A future macOS changes or removes the private API.** A missing class or a changed type encoding means Unsupported, loudly, never a crash from a bad `msg_send!`. Pinned by `guard_rejects_changed_encoding` (Task 1) and `native_falls_back_to_unsupported_without_mcl` (Task 3).
4. **`charge full` ends.** On unplug or `charge auto`, the native limit goes back to the user's limit. A crash mid-override doesn't leave it at 100 for good, because startup re-applies the configured limit. Pinned by `native_charge_full_lifts_the_limit_until_unplug` and `native_startup_reapplies_the_configured_limit` (Task 3).
5. **Uninstall on a Mac whose user had their own native limit before eighty.** Their value comes back. If eighty never recorded one, the native limit is left as it is. Pinned by `restore_native_puts_back_the_original` (Task 5).

---

## File Structure

```
crates/eighty-hw/Cargo.toml                + objc2, objc2-foundation
crates/eighty-hw/src/native/mod.rs         NativeLimit trait, NativeError, guard_encodings()
crates/eighty-hw/src/native/powerui.rs     PowerUi: the objc2 implementation
crates/eighty-hw/src/native/fake.rs        FakeNative (feature "fake")
crates/eighty-core/src/policy.rs           ControlMode::Native replaces Cycle; Decision.native_limit
crates/eighty-core/src/config.rs           Config.native_original; Settings::with_native_limit
crates/eighty-core/src/proto.rs            DoctorReport.native
crates/eighty-helper/src/control.rs        detect() takes the native probe; no Cycle
crates/eighty-helper/src/engine.rs         N: NativeLimit; apply, adopt, warn; no sleep_warning
crates/eighty-helper/src/main.rs           wire PowerUi; --restore restores the native original
crates/eighty-cli/src/render.rs            Native mode text; doctor's native line
README.md, docs/testing/plan2b-acceptance.md
```

---

### Task 1: `NativeLimit` in `eighty-hw`

**Files:**
- Create: `crates/eighty-hw/src/native/mod.rs`, `crates/eighty-hw/src/native/powerui.rs`, `crates/eighty-hw/src/native/fake.rs`
- Modify: `crates/eighty-hw/src/lib.rs`, `crates/eighty-hw/Cargo.toml`

**Interfaces:**
- Produces:
  ```rust
  pub trait NativeLimit {
      /// Class present, every guarded selector matches, and isMCLSupported is YES.
      fn supported(&mut self) -> bool;
      fn available(&mut self) -> Result<Vec<u8>, NativeError>;
      fn limit(&mut self) -> Result<u8, NativeError>;      // getMCLLimitWithError:
      fn enabled(&mut self) -> Result<bool, NativeError>;  // isMCLCurrentlyEnabled: != 0
      fn set_limit(&mut self, percent: u8) -> Result<(), NativeError>; // setMCLLimit:error:
  }
  #[derive(Debug, thiserror::Error, Clone, PartialEq)]
  pub enum NativeError { #[error("PowerUI isn't available")] Unavailable, #[error("{0}")] Call(String) }
  pub fn guard_encodings(found: &dyn Fn(&str) -> Option<String>) -> bool;
  pub struct PowerUi;            // PowerUi::new() -> PowerUi; never panics
  pub struct FakeNative;         // cfg(any(test, feature = "fake")); Clone, shared state
  ```
  `FakeNative` API: `FakeNative::new(available: &[u8], limit: u8)`, `FakeNative::unsupported()`, `.limit_now() -> u8`, `.set_externally(percent: u8)`, `.fail_calls(bool)`, `.writes() -> Vec<u8>`.

- [ ] **Step 1: Write the failing tests** in `native/mod.rs` and `native/fake.rs`:

```rust
#[test] fn guard_accepts_the_macos_27_encodings() {
    assert!(guard_encodings(&|sel| GUARDED.iter().find(|(s, _)| *s == sel).map(|(_, e)| e.to_string())));
}
#[test] fn guard_rejects_changed_encoding() {
    assert!(!guard_encodings(&|sel| match sel {
        "setMCLLimit:error:" => Some("B28@0:8Q16^@20".into()), // limit widened to u64
        s => GUARDED.iter().find(|(g, _)| *g == s).map(|(_, e)| e.to_string()),
    }));
    assert!(!guard_encodings(&|_| None));
}
#[test] fn fake_records_writes_and_external_changes() {
    let mut f = FakeNative::new(&[80, 85, 90, 95, 100], 100);
    f.set_limit(80).unwrap();
    assert_eq!((f.limit().unwrap(), f.writes()), (80, vec![80]));
    f.set_externally(90);
    assert_eq!(f.limit().unwrap(), 90);
    assert!(!FakeNative::unsupported().supported());
}
```

`GUARDED: [(&str, &str); 5]` is the selector list from Global Constraints.

- [ ] **Step 2: Run them, expect a compile failure:** `cargo test -p eighty-hw native`

- [ ] **Step 3: Implement `native/mod.rs`, `native/fake.rs`, and `PowerUi` in `native/powerui.rs`**

`PowerUi::new()` `dlopen`s the framework once (`libc::dlopen`, `RTLD_NOW`). It looks up the class with `AnyClass::get`, and creates the client with `initWithClientName:` inside `objc2::rc::autoreleasepool`. `supported()` runs `guard_encodings` over `class.instance_method(sel).types()` (or the objc2 equivalent), then calls `isMCLSupported`. Every other method returns `NativeError::Unavailable` unless `supported()` was true. Pass `&mut *mut NSError` for the error out-param, and turn a non-null error into `NativeError::Call(localizedDescription)`. `available()` maps the `NSArray<NSNumber>` to `u8`.

- [ ] **Step 4: Add one hardware test, `#[ignore]`, that only reads:**

```rust
#[test] #[ignore = "needs macOS 26.4+ with MCL"]
fn powerui_reads_on_this_mac() {
    let mut p = PowerUi::new();
    assert!(p.supported());
    assert!(p.available().unwrap().contains(&80));
    let l = p.limit().unwrap();
    assert!((80..=100).contains(&l));
}
```

- [ ] **Step 5: Run** `cargo test -p eighty-hw native` (PASS), and on the M2 Max `cargo test -p eighty-hw native -- --ignored` (PASS).

- [ ] **Step 6: Commit** — `feat(hw): read and set macOS's native charge limit through PowerUI`

---

### Task 2: Policy, config and protocol for Native mode

**Files:**
- Modify: `crates/eighty-core/src/policy.rs`, `crates/eighty-core/src/config.rs`, `crates/eighty-core/src/proto.rs`

**Interfaces:**
- Produces:
  - `ControlMode::{Hold, Native, Unsupported}`. `Cycle` is deleted, along with its policy branch and its tests (`cycle_mode_table`, `cycle_lid_closed_reports_not_enforced`).
  - `Decision { outputs, activity, clear_override, native_limit: Option<u8> }`. `Some` only in Native mode.
  - `Settings::with_native_limit(self, upper: u8, lower: Option<u8>, available: &[u8]) -> Result<Settings, String>`. It keeps `lower` unchanged and returns the two refusal texts from Global Constraints.
  - `Config { settings, accept_other_limiters, native_original: Option<u8> }`, with `#[serde(default)]` so old files load.
  - `pub struct NativeState { pub available: Vec<u8>, pub limit: Option<u8>, pub enabled: Option<bool> }`, and `DoctorReport.native: Option<NativeState>` (`#[serde(default)]`).

Native-mode decisions (no override): `native_limit = Some(s.upper)`, `inhibit = false`, `discharge = false`, `light = None`. Activity: not on AC → `OnBattery`; `percent > upper` → `Discharging { to: upper }` (macOS does it); `percent == upper` → `Holding`; otherwise → `Charging { to: upper }`. `FullCharge` → `native_limit = Some(100)`, `Charging { to: 100 }`, `light = None`. `DischargeTo(n)` → today's `DischargeTo` rules with `hold = false`, `light = None`, and `native_limit = Some(s.upper)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn native_mode_table() {
    let d = |p, ext| decide(&s(), Override::None, &i(p, ext, false, Native), &prev(false, false));
    for (p, ext, act) in [(85, true, Discharging { to: 80 }), (80, true, Holding), (70, true, Charging { to: 80 }), (85, false, OnBattery)] {
        let r = d(p, ext);
        assert_eq!((r.activity, r.native_limit), (act, Some(80)), "{p} {ext}");
        assert_eq!(r.outputs, Outputs { inhibit: false, discharge: false, light: None });
    }
}
#[test] fn native_full_charge_asks_for_100() {
    let r = decide(&s(), Override::FullCharge, &i(80, true, false, Native), &prev(false, false));
    assert_eq!((r.native_limit, r.activity, r.outputs.light), (Some(100), Charging { to: 100 }, None));
}
#[test] fn native_discharge_to_uses_chie_and_keeps_the_limit() {
    let r = decide(&s(), Override::DischargeTo(70), &i(78, true, false, Native), &prev(false, false));
    assert_eq!((r.outputs.discharge, r.outputs.inhibit, r.native_limit), (true, false, Some(80)));
    let r = decide(&s(), Override::DischargeTo(70), &i(78, true, true, Native), &prev(false, true));
    assert!(!r.outputs.discharge); // lid closed
}
#[test] fn hold_mode_has_no_native_limit() {
    assert_eq!(decide(&s(), Override::None, &i(80, true, false, Hold), &prev(false, false)).native_limit, None);
}
#[test] fn native_limits_only_take_macos_steps() {
    let steps = [80, 85, 90, 95, 100];
    assert_eq!(Settings::default().with_native_limit(85, None, &steps).unwrap().upper, 85);
    assert_eq!(Settings::default().with_native_limit(70, None, &steps).unwrap_err(),
        "this Mac only supports a limit of 80, 85, 90, 95 or 100%");
    assert_eq!(Settings::default().with_native_limit(80, Some(75), &steps).unwrap_err(),
        "macOS decides when charging resumes on this Mac, so there is no lower limit");
}
#[test] fn old_config_without_native_original_loads() {
    assert_eq!(toml::from_str::<Config>("[settings]\nupper = 80\n").unwrap().native_original, None);
}
```

- [ ] **Step 2: Run** `cargo test -p eighty-core`, expect compile failures.
- [ ] **Step 3: Implement** the changes above. Fix every remaining `ControlMode::Cycle` reference in `eighty-core`.
- [ ] **Step 4: Run** `cargo test -p eighty-core` → PASS. (`eighty-helper` and `eighty-cli` won't compile until Tasks 3 and 6. That's expected; don't patch them here beyond what `cargo test -p eighty-core` needs.)
- [ ] **Step 5: Commit** — `feat(core): native mode policy replaces cycle mode`

---

### Task 3: Engine detection, apply and startup in Native mode

**Files:**
- Modify: `crates/eighty-helper/src/control.rs`, `crates/eighty-helper/src/engine.rs`

**Interfaces:**
- Consumes: `NativeLimit`, `FakeNative` (Task 1); `ControlMode::Native`, `Decision.native_limit`, `Config.native_original` (Task 2).
- Produces:
  - `pub fn detect(smc: &mut dyn SmcIo, native: &mut dyn NativeLimit) -> Detection` and `detect_read_only(..)` with the same parameters.
  - `Detection` gains `native_steps: Vec<u8>` (empty unless Native).
  - `Engine<S, B, Y, N: NativeLimit>` and `Deps { .., native: N }`.
  - The rig gets `Rig::native()`: a FakeSmc with only `CHIE` and `ACLC` plus `FakeNative::new(&[80, 85, 90, 95, 100], 100)`. `Rig::cycle()` is deleted, along with cycle-only engine tests (the sleep-warning tests and the cycle half of `battery_reading_from_before_a_switch_is_marked_stale`; rewrite that test on `Rig::hold()` with auto-discharge).

Behaviour:
- `detect`: `CHTE` usable → Hold. Else `native.supported()` and `available()` is `Ok` → Native with those steps. Else Unsupported.
- Startup in Native mode: if `config.native_original` is `None`, read `native.limit()` and save it there before the first write. If `config.settings.upper` isn't in the steps (an old config, e.g. 70), keep the config file, use the nearest step at or above it for this run, and show the warning `limit 70% isn't available on this Mac; using 80%`.
- Each tick in Native mode: after `decide`, if `d.native_limit` differs from the last value eighty wrote (or nothing has been written yet), call `set_limit`. On error, set `Health::WriteFailed { key: "MCL".into(), error }`, restore the SMC keys, and set `Paused`. `SmcControl::apply` doesn't write `ACLC` in Native mode, and only writes `CHIE` there.
- Delete `sleep_warning` and the code that sets it.

- [ ] **Step 1: Write the failing tests** (engine tests module):

```rust
#[test] fn native_falls_back_to_unsupported_without_mcl() {
    let r = Rig::native_with(FakeNative::unsupported()); r.set(80, false, true, 0, Some(900));
    assert_eq!(r.engine().status().health, Health::Unsupported);
}
#[test] fn native_startup_records_the_original_and_sets_the_limit() {
    let r = Rig::native(); r.set(85, false, true, -700, Some(900));
    let e = r.engine();
    assert_eq!(e.status().mode, ControlMode::Native);
    assert_eq!((r.native.limit_now(), r.native.writes()), (80, vec![80]));
    assert_eq!(r.store.load().0.native_original, Some(100));
    assert_eq!(e.status().activity, Activity::Discharging { to: 80 });
    assert_eq!(r.smc.value("ACLC"), Some(vec![0])); // never written
    assert_eq!(r.smc.value("CHIE"), Some(vec![0]));
}
#[test] fn native_startup_reapplies_the_configured_limit() {
    let r = Rig::native(); r.set(90, true, true, 1000, Some(900));
    r.native.set_externally(100); // e.g. left at 100 by a crash during `charge full`
    r.write_config("[settings]\nupper = 85\n");
    r.engine();
    assert_eq!(r.native.limit_now(), 85);
}
#[test] fn native_charge_full_lifts_the_limit_until_unplug() {
    let r = Rig::native(); r.set(80, false, true, 0, Some(900));
    let mut e = r.engine();
    e.request(0, Request::ChargeFull);
    assert_eq!(r.native.limit_now(), 100);
    r.set(80, false, false, -500, Some(1_000)); e.handle(Event::Tick);
    assert_eq!((r.native.limit_now(), e.status().override_), (80, Override::None));
}
#[test] fn native_set_limit_failure_is_loud() {
    let r = Rig::native(); r.native.fail_calls(true); r.set(80, false, true, 0, Some(900));
    let st = r.engine().status();
    assert!(matches!(st.health, Health::WriteFailed { ref key, .. } if key == "MCL"), "{:?}", st.health);
}
#[test] fn native_old_limit_outside_the_steps_uses_the_next_step_up() {
    let r = Rig::native(); r.write_config("[settings]\nupper = 70\nlower = 65\n"); r.set(75, true, true, 900, Some(900));
    let e = r.engine();
    assert_eq!(r.native.limit_now(), 80);
    assert!(e.status().warnings.iter().any(|w| w == "limit 70% isn't available on this Mac; using 80%"));
}
```

(`Rig::native_with`, `Rig::write_config`, and the `native` field on `Rig` are test helpers to add.)

- [ ] **Step 2: Run** `cargo test -p eighty-helper native`, expect failures.
- [ ] **Step 3: Implement** the behaviour above.
- [ ] **Step 4: Run** `cargo test -p eighty-helper` → PASS. Every existing hold-mode test still passes unchanged.
- [ ] **Step 5: Commit** — `feat(helper): native mode holds the limit through macOS's Charge Limit`

---

### Task 4: Engine requests, adoption and the native watchdog

**Files:**
- Modify: `crates/eighty-helper/src/engine.rs`

**Interfaces:**
- Consumes: Task 3's engine; `Settings::with_native_limit` (Task 2).
- Produces: `Request::SetLimits` and `Request::SetAutoDischarge` behaviour in Native mode, a `native_warning: Option<String>` in the status warnings, and `DoctorReport.native`.

Behaviour:
- `SetLimits` in Native mode validates with `with_native_limit(upper, lower, &detection.native_steps)`. `SetAutoDischarge` in Native mode returns the error `macOS brings the charge down to the limit on this Mac; it can't be turned off`.
- **Adoption:** each Native tick, before `decide`, read `native.limit()`. If it differs from the last value eighty wrote and the override isn't `FullCharge`, save `upper = read value`, log `native limit changed outside eighty: now <n>%`, and treat it as written.
- **Watchdog:** in Native mode the SMC watchdog doesn't run. Instead, on a fresh reading on AC (`update_time` ≥ the last native write or adoption + `SETTLE_SECS`; record that time when `set_limit` succeeds or a change is adopted) with `percent > upper`, `is_charging` and `amperage_ma > 50`, set `native_warning` to `macOS is charging past <upper>% (it does this now and then to calibrate the battery); eighty can't stop it`. Clear it on the first fresh reading that isn't charging. Never a fault, never a restore. While the warning is set, status activity is `Charging { to: 100 }` rather than the policy's `Discharging`, so the State line doesn't contradict the battery.
- `Doctor` fills `native` from `available()`, `limit()` and `enabled()` (errors become `None`).

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn native_adopts_a_limit_changed_in_system_settings() {
    let r = Rig::native(); r.set(80, false, true, 0, Some(900));
    let mut e = r.engine();
    r.native.set_externally(90); r.clock.set(1_060); e.handle(Event::Tick);
    assert_eq!((e.status().settings.upper, r.store.load().0.settings.upper), (90, 90));
    assert_eq!(e.status().health, Health::Ok);
    assert_eq!(r.native.writes(), vec![80]); // adopted, not overwritten
}
#[test] fn native_charging_past_the_limit_warns_without_faulting() {
    let r = Rig::native(); r.set(80, false, true, 0, Some(900));
    let mut e = r.engine();
    r.clock.set(1_100); r.set(83, true, true, 2000, Some(1_090)); e.handle(Event::Tick);
    let st = e.status();
    assert_eq!(st.health, Health::Ok);
    assert!(st.warnings.iter().any(|w| w.starts_with("macOS is charging past 80%")), "{:?}", st.warnings);
    assert_eq!(st.activity, Activity::Charging { to: 100 });
    r.clock.set(1_200); r.set(83, false, true, -600, Some(1_190)); e.handle(Event::Tick);
    assert!(e.status().warnings.is_empty());
}
#[test] fn native_limit_requests_follow_macos_steps() {
    let r = Rig::native(); r.set(80, false, true, 0, Some(900));
    let mut e = r.engine();
    assert!(matches!(e.request(0, Request::SetLimits { upper: 70, lower: None }), Response::Error(m) if m.starts_with("this Mac only supports")));
    assert!(matches!(e.request(0, Request::SetLimits { upper: 85, lower: None }), Response::Status(_)));
    assert_eq!(r.native.limit_now(), 85);
    assert!(matches!(e.request(0, Request::SetAutoDischarge { on: false }), Response::Error(_)));
}
#[test] fn native_doctor_reports_the_native_limit() {
    let r = Rig::native(); r.set(80, false, true, 0, Some(900));
    let Response::Doctor(d) = r.engine().request(0, Request::Doctor) else { panic!() };
    assert_eq!(d.native, Some(NativeState { available: vec![80, 85, 90, 95, 100], limit: Some(80), enabled: Some(true) }));
}
```

(`FakeNative::enabled()` returns `limit < 100`.)

- [ ] **Step 2: Run** `cargo test -p eighty-helper native`, expect failures.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p eighty-helper` → PASS.
- [ ] **Step 5: Commit** — `feat(helper): adopt System Settings changes and warn when macOS charges past the limit`

---

### Task 5: Wiring and uninstall restore

**Files:**
- Modify: `crates/eighty-helper/src/main.rs`, `crates/eighty-helper/Cargo.toml` (only if a feature flag is needed)

**Interfaces:**
- Consumes: `PowerUi` (Task 1), `Store`, `Config.native_original`.
- Produces: `pub fn restore_native(native: &mut dyn NativeLimit, store: &Store) -> Result<Option<u8>, String>` in `crates/eighty-helper/src/control.rs`. It sets the limit to `native_original` when that is `Some` and MCL is supported, and returns what it wrote. With no recorded original, or no MCL, it does nothing and returns `Ok(None)`.

Behaviour:
- `run_helper` builds `PowerUi::new()` on the engine thread and passes it in `Deps`.
- `--restore` runs `restore_all` (unchanged) and then `restore_native`. It prints `restore: native limit back to <n>%`, or `restore: native limit left as is`. A `restore_native` error makes `--restore` exit non-zero, so `eighty uninstall` reports it the way it does today. `emergency_restore` stays SMC-only.

- [ ] **Step 1: Write the failing tests** (control.rs tests):

```rust
#[test] fn restore_native_puts_back_the_original() {
    let dir = tempfile::tempdir().unwrap(); let store = Store::new(dir.path().into());
    store.save(&Config { native_original: Some(95), ..Config::default() }).unwrap();
    let mut n = FakeNative::new(&[80, 85, 90, 95, 100], 80);
    assert_eq!(restore_native(&mut n, &store), Ok(Some(95)));
    assert_eq!(n.limit_now(), 95);
}
#[test] fn restore_native_without_a_record_leaves_the_limit() {
    let dir = tempfile::tempdir().unwrap(); let store = Store::new(dir.path().into());
    let mut n = FakeNative::new(&[80, 85, 90, 95, 100], 80);
    assert_eq!(restore_native(&mut n, &store), Ok(None));
    assert_eq!(n.writes(), Vec::<u8>::new());
}
```

- [ ] **Step 2: Run** `cargo test -p eighty-helper restore_native`, expect failures.
- [ ] **Step 3: Implement** `restore_native`, then the wiring.
- [ ] **Step 4: Run** `cargo build --release && cargo test` → PASS.
- [ ] **Step 5: Commit** — `feat(helper): uninstall puts back the user's own native limit`

---

### Task 6: CLI text and docs

**Files:**
- Modify: `crates/eighty-cli/src/render.rs`, `README.md`

**Interfaces:**
- Consumes: `ControlMode::Native`, `DoctorReport.native` (Task 2).

Exact text in Native mode:
- `Limit     80% (held by macOS)`. No "resumes below", no auto-discharge part.
- `Mode      native (macOS Charge Limit) · MagSafe light: macOS`
- Doctor line: `Native    80% · steps 80, 85, 90, 95, 100`. With `limit: None` it reads `Native    unreadable · steps …`.
- Hold mode output is unchanged. Delete the `Cycle` texts and their tests.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn native_status_text() {
    let s = Status { mode: ControlMode::Native, activity: Activity::Holding, charging: false, amperage_ma: 0, ..st() };
    let out = render_status(&s);
    assert!(out.contains("Limit     80% (held by macOS)\n"), "{out}");
    assert!(out.contains("Mode      native (macOS Charge Limit) · MagSafe light: macOS\n"), "{out}");
}
#[test] fn doctor_shows_the_native_limit() { /* DoctorReport with native: Some(NativeState{..80..}) → contains "Native    80% · steps 80, 85, 90, 95, 100" */ }
```

- [ ] **Step 2: Run** `cargo test -p eighty-cli`, expect failures.
- [ ] **Step 3: Implement.** Then update the README:
  - The modes list becomes Hold and Native. Native: "Macs without a stop-charging key, for example M2 Max on macOS 27: eighty sets macOS's own Charge Limit (80, 85, 90 or 95%), and macOS holds it."
  - `eighty limit`: say that on Native Macs only 80/85/90/95/100 work and there is no lower limit.
  - Setup: in Native mode, leave the macOS Charge Limit to eighty (eighty sets it, and a change in System Settings becomes eighty's limit). In Hold mode it stays at 100%.
  - The pre-release status line drops the cycle-mode caveat.
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets` → PASS, no warnings.
- [ ] **Step 5: Commit** — `feat(cli): show native mode in status and doctor`

---

### Task 7: Acceptance on both Macs

**Files:**
- Create: `docs/testing/plan2b-acceptance.md`

Macs: **M2 Max** (Native) and **M5 Pro** (Hold, regression only). Record the result, `eighty status` and the relevant log lines per step, as in `plan2-acceptance.md`. Before installing on the M2, note the macOS Charge Limit currently shown in System Settings.

- [ ] **Step 1: Install (M2).** `sudo eighty install`. Expected: `Mode native (macOS Charge Limit)`, and System Settings → Battery shows 80%. **Boot-before-login check:** reboot, then compare the helper's first `native limit` log line with Finder's start time (spec §5.6 item 1).
- [ ] **Step 2: Limits (M2).** `eighty limit 85` → System Settings shows 85%. `eighty limit 70` and `eighty limit 80 75` → the exact refusal texts. Then set 80% back in System Settings → the log shows `native limit changed outside eighty: now 80%`, and status says 80%.
- [ ] **Step 3: Hold (M2).** On AC at or above the limit for 10+ minutes: not charging, light green. Above the limit: `Discharging to 80%` with negative amperage and `CHIE=00`.
- [ ] **Step 4: Overrides (M2).** `eighty charge full` → System Settings shows no limit and it charges. Unplug → the limit is back to 80%. `eighty discharge <current − 2>` → `CHIE=08`, then back to holding.
- [ ] **Step 5: Sleep and reboot (M2).** Leave it plugged in overnight with the lid closed and record the % in the morning (spec §5.6 item 3). Reboot and record whether the limit survived before the helper re-applied it: check the first tick's log (item 2).
- [ ] **Step 6: Uninstall (M2).** `sudo eighty uninstall` → prints `restore: native limit back to <original>%`, and System Settings shows the value noted before install.
- [ ] **Step 7: Regression (M5).** Install, run `eighty status` (Hold, unchanged text), then `eighty limit 70` (accepted, holds at 70), then uninstall.
- [ ] **Step 8: Write up and commit** — `docs: add plan 2b acceptance results`
