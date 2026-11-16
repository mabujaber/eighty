# eighty Plan 1 — Hardware Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `eighty-probe`, a CLI that reports which SMC keys exist on this Mac and, run with sudo, which of them actually stop charging, force discharge, and change the MagSafe light. Then run it on both of the owner's Macs (M5 Pro on macOS 26.6.2, and an M2 Pro/Max) and commit the reports.

**Architecture:** A Cargo workspace with two crates. `eighty-hw` is the hardware library: raw SMC access over IOKit, battery state from the `AppleSmartBattery` IORegistry entry, and fakes for tests. Later plans reuse it in the helper. `eighty-probe` (lib + bin) holds the candidate-key catalog, a read-only probe, a write-test engine that always restores original values, and a text/JSON report. All probe logic runs against the traits `SmcIo` and `BatterySource`, so it is unit-tested with fakes. Only thin FFI layers touch hardware.

**Tech Stack:** Rust (stable, edition 2021), hand-written IOKit FFI, `core-foundation` 0.10, `mach2` 0.4, `libc` 0.2, `thiserror` 2, `anyhow` 1, `serde`/`serde_json` 1, `clap` 4.5 (derive), `ctrlc` 3.4.

**Spec:** `docs/superpowers/specs/2026-11-16-eighty-design.md` (§5.2 hardware backends and MagSafe light, §8 safety requirements, §10 risks). This plan implements the spec's "first milestone is a probe spike". The helper, CLI, menu-bar app and packaging are Plans 2+, written after this report exists.

## Global Constraints

- Apple Silicon only. Development machine: Apple M5 Pro, macOS 26.6.2 (build 25G83). Second probe machine: the owner's M2 Pro/Max.
- Never perform an SMC write unless running as root **and** `--write-test` was passed. The default run is read-only.
- Every value written is restored to the value read before the write. That holds on success, on error, on Ctrl-C and on panic.
- Forced-discharge tests only run when charge ≥ 20%.
- The library never exposes "write key X = Y" beyond the probe's fixed catalog (spec §4 anti-goal). `SmcIo::write` is `pub` in `eighty-hw` for the probe and helper only, and `eighty-probe` writes only catalog keys and values.
- No shelling out to `smc`, `ioreg` or `pmset`. Use IOKit directly (spec §5.3).
- Commit messages: conventional style (`feat:`, `test:`, `docs:`). No `Co-Authored-By` trailers.

## Review Focus

1. **Ctrl-C or panic during a write test.** Expected: every key the test changed is back to its original value before the process exits. *Pinned in Task 5 (`guard_restores_on_drop`, `cancel_mid_settle_restores`).*
2. **Mac on battery, at 100%, or not currently charging when an inhibit test runs.** Expected: verdict `CannotVerify` with the reason, not a false "no effect". *Pinned in Task 5 (`inhibit_cannot_verify_when_not_charging`).*
3. **`--write-test` without sudo.** Expected: exit code 2 with a message naming `sudo eighty-probe --write-test`, before any SMC connection is opened for writing. *Pinned in Task 6 (`write_test_requires_root`).*
4. **A key exists but has a different size than the catalog value** (e.g. `CHTE` is 4 bytes, so a 1-byte write would be wrong). Expected: verdict `SizeMismatch` with no write attempted. *Pinned in Task 5 (`size_mismatch_skips_write`).*
5. **Write rejected with `kIOReturnNotPrivileged` (0xE00002C1)**, the macOS 27 behavior from battery#474. Expected: verdict `Blocked`, distinct from `Missing`. *Pinned in Task 2 (`maps_not_privileged`) and Task 5 (`not_privileged_is_blocked`).*

---

## File Structure

```
Cargo.toml                         workspace (members: crates/*)
crates/eighty-hw/
  Cargo.toml                       features: fake
  src/lib.rs                       re-exports
  src/smc/mod.rs                   SmcIo trait, SmcError, KeyInfo
  src/smc/key.rs                   FourCC, hex()
  src/smc/ffi.rs                   SmcParam repr(C), IOKit externs, selectors
  src/smc/conn.rs                  SmcConnection: SmcIo over IOKit
  src/smc/fake.rs                  FakeSmc (cfg test / feature "fake")
  src/battery/mod.rs               BatterySnapshot, BatterySource, BatteryError
  src/battery/parse.rs             Prop, parse_snapshot()
  src/battery/ioreg.rs             IoRegBattery: BatterySource over IORegistry
  src/battery/fake.rs              FakeBattery (cfg test / feature "fake")
crates/eighty-probe/
  Cargo.toml                       dev-dep eighty-hw with feature "fake"
  src/lib.rs
  src/catalog.rs                   Role, Candidate, CANDIDATES, READ_ONLY_KEYS
  src/reads.rs                     probe_reads()
  src/guard.rs                     RestoreGuard
  src/writes.rs                    run_write_tests(), Verdict, WriteResult
  src/machine.rs                   MachineInfo via sysctlbyname
  src/report.rs                    ProbeReport, render_text()
  src/main.rs                      clap args, root check, ctrl-c, output
docs/probe/                        committed probe reports
```

---

### Task 1: Workspace, FourCC, SMC trait and fake

**Files:**
- Create: `Cargo.toml`, `.gitignore` (`/target`), `crates/eighty-hw/Cargo.toml`, `crates/eighty-hw/src/lib.rs`, `src/smc/mod.rs`, `src/smc/key.rs`, `src/smc/fake.rs`
- Test: inline `#[cfg(test)]` modules in `key.rs` and `fake.rs`

**Interfaces:**
- Produces:
  - `pub struct FourCC(pub u32)`: `FourCC::new(s: &str) -> Result<FourCC, SmcError>` (exactly 4 ASCII bytes, big-endian packed), `fn as_str(&self) -> String`, `impl Display`, `Copy + Eq + Hash + Debug`.
  - `pub fn hex(bytes: &[u8]) -> String` gives lowercase hex with no separators (`[1,0,0,0]` → `"01000000"`).
  - `pub struct KeyInfo { pub size: u32, pub data_type: FourCC, pub attributes: u8 }`
  - `pub enum SmcError { InvalidKey(String), KeyNotFound(String), NotPrivileged, SizeMismatch { key: String, expected: u32, got: usize }, ServiceNotFound, IoKit(i32) }` (thiserror)
  - `pub trait SmcIo { fn key_info(&mut self, key: FourCC) -> Result<KeyInfo, SmcError>; fn read(&mut self, key: FourCC) -> Result<Vec<u8>, SmcError>; fn write(&mut self, key: FourCC, bytes: &[u8]) -> Result<(), SmcError>; }` The `write` contract: if `bytes.len() != key_info.size`, return `SizeMismatch` without writing.
  - `FakeSmc` (behind `#[cfg(any(test, feature = "fake"))]`): `FakeSmc::new() -> FakeSmc`, `fn insert(&self, key: &str, data_type: &str, value: &[u8], mode: WriteMode)`, `fn value(&self, key: &str) -> Option<Vec<u8>>`, `fn writes(&self) -> Vec<(String, Vec<u8>)>`, `Clone` (state shared via `Rc<RefCell<…>>` so tests can observe it after handing a clone to the code under test). `pub enum WriteMode { Accept, NotPrivileged }`.

- [ ] **Step 1: Write failing tests**

```rust
// key.rs
#[test] fn fourcc_roundtrip() {
    let k = FourCC::new("CHTE").unwrap();
    assert_eq!(k.0, 0x4348_5445);
    assert_eq!(k.as_str(), "CHTE");
}
#[test] fn fourcc_rejects_bad_length() {
    assert!(matches!(FourCC::new("CH0"), Err(SmcError::InvalidKey(_))));
    assert!(matches!(FourCC::new("CH0BX"), Err(SmcError::InvalidKey(_))));
}
#[test] fn hex_formats() { assert_eq!(hex(&[1, 0, 0, 0]), "01000000"); assert_eq!(hex(&[]), ""); }

// fake.rs
#[test] fn fake_missing_key() {
    let mut s = FakeSmc::new();
    assert!(matches!(s.read(FourCC::new("CH0B").unwrap()), Err(SmcError::KeyNotFound(_))));
}
#[test] fn fake_size_mismatch_does_not_write() {
    let mut s = FakeSmc::new();
    s.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
    let r = s.write(FourCC::new("CHTE").unwrap(), &[1]);
    assert!(matches!(r, Err(SmcError::SizeMismatch { expected: 4, got: 1, .. })));
    assert!(s.writes().is_empty());
}
#[test] fn fake_not_privileged() {
    let mut s = FakeSmc::new();
    s.insert("CHIB", "ui8 ", &[0], WriteMode::NotPrivileged);
    assert!(matches!(s.write(FourCC::new("CHIB").unwrap(), &[1]), Err(SmcError::NotPrivileged)));
    assert_eq!(s.value("CHIB"), Some(vec![0]));
}
#[test] fn fake_clone_shares_state() {
    let s = FakeSmc::new();
    s.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
    let mut c = s.clone();
    c.write(FourCC::new("ACLC").unwrap(), &[3]).unwrap();
    assert_eq!(s.value("ACLC"), Some(vec![3]));
    assert_eq!(s.writes(), vec![("ACLC".to_string(), vec![3])]);
}
```

- [ ] **Step 2: Run `cargo test -p eighty-hw`.** Expected: compile failure (types not defined).
- [ ] **Step 3: Implement** the workspace, the `fake` feature in `eighty-hw/Cargo.toml`, and the types above.
- [ ] **Step 4: Run `cargo test -p eighty-hw`.** Expected: 7 tests pass.
- [ ] **Step 5: Commit** `feat(hw): add SMC key types, SmcIo trait and fake`

---

### Task 2: Real SMC connection over IOKit

**Files:**
- Create: `crates/eighty-hw/src/smc/ffi.rs`, `crates/eighty-hw/src/smc/conn.rs`
- Test: inline tests in `ffi.rs` and `conn.rs`

**Interfaces:**
- Consumes: `FourCC`, `KeyInfo`, `SmcError`, `SmcIo` (Task 1).
- Produces: `pub struct SmcConnection`: `SmcConnection::open() -> Result<SmcConnection, SmcError>`, `impl SmcIo`, `impl Drop` (calls `IOServiceClose`). `pub(crate) fn map_kern_return(kr: i32) -> SmcError`.

FFI values the implementer must use:
- `#[repr(C)] SmcParam` = `{ key: u32, vers: {u8,u8,u8,u8,u16}, p_limit: {u16,u16,u32,u32,u32}, key_info: {data_size: u32, data_type: u32, data_attributes: u8}, result: u8, status: u8, data8: u8, data32: u32, bytes: [u8; 32] }`. Its size must be **80**.
- Service name `"AppleSMC"`, `IOServiceOpen` type `0`, task from `mach2::traps::mach_task_self()`, main port `0`.
- `IOConnectCallStructMethod(conn, 2 /*kSMCHandleYPCEvent*/, &in, 80, &mut out, &mut 80)`.
- `data8` commands: `9` = read key info, `5` = read bytes, `6` = write bytes. Each read/write first calls key info to get `data_size`.
- `out.result == 132` (0x84) means `KeyNotFound`. Treat `data_size == 0` as `KeyNotFound` too (this is the smc tool's `no data` case).
- `map_kern_return`: `0xE00002C1u32 as i32` → `NotPrivileged`, anything else → `IoKit(kr)`.
- `#[link(name = "IOKit", kind = "framework")]`.

- [ ] **Step 1: Write failing tests**

```rust
// ffi.rs
#[test] fn smc_param_is_80_bytes() { assert_eq!(std::mem::size_of::<SmcParam>(), 80); }

// conn.rs
#[test] fn maps_not_privileged() {
    assert!(matches!(map_kern_return(0xE00002C1u32 as i32), SmcError::NotPrivileged));
    assert!(matches!(map_kern_return(-536870206), SmcError::IoKit(-536870206)));
}
#[test] #[ignore = "hardware"] fn reads_key_count_on_real_mac() {
    let mut c = SmcConnection::open().unwrap();
    let info = c.key_info(FourCC::new("#KEY").unwrap()).unwrap();
    assert_eq!(info.size, 4);
    let n = c.read(FourCC::new("#KEY").unwrap()).unwrap();
    assert!(u32::from_be_bytes(n.try_into().unwrap()) > 100);
}
#[test] #[ignore = "hardware"] fn missing_key_is_not_found() {
    let mut c = SmcConnection::open().unwrap();
    assert!(matches!(c.read(FourCC::new("ZZZZ").unwrap()), Err(SmcError::KeyNotFound(_))));
}
```

- [ ] **Step 2: Run `cargo test -p eighty-hw`.** Expected: compile failure.
- [ ] **Step 3: Implement** `ffi.rs` and `conn.rs` with the values above. `write` checks the length against `key_info.size` before calling IOKit (same contract as the fake).
- [ ] **Step 4: Run `cargo test -p eighty-hw && cargo test -p eighty-hw -- --ignored`.** Expected: all pass on the owner's Mac, without sudo (reads don't need root).
- [ ] **Step 5: Commit** `feat(hw): add IOKit SMC connection`

---

### Task 3: Battery snapshot from IORegistry

**Files:**
- Create: `crates/eighty-hw/src/battery/{mod.rs,parse.rs,ioreg.rs,fake.rs}`
- Test: inline tests in `parse.rs`, `ioreg.rs`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq, serde::Serialize)] pub struct BatterySnapshot { pub percent: u8, pub is_charging: bool, pub external_connected: bool, pub amperage_ma: i32, pub temperature_c: f32, pub cycle_count: u32 }`. Add `serde` as a dependency of `eighty-hw`.
  - `pub enum Prop { Int(i64), Bool(bool) }`
  - `pub fn parse_snapshot(props: &HashMap<String, Prop>) -> Result<BatterySnapshot, BatteryError>`
  - `pub enum BatteryError { NotFound, MissingProp(&'static str), IoKit(i32) }`
  - `pub trait BatterySource { fn snapshot(&mut self) -> Result<BatterySnapshot, BatteryError>; }`
  - `pub struct IoRegBattery;` `impl BatterySource`
  - `FakeBattery` (cfg test / feature fake): `FakeBattery::new(f: impl FnMut() -> BatterySnapshot + 'static)`, `impl BatterySource`.

Parsing rules:
- `percent` = `CurrentCapacity` if `MaxCapacity == 100`, else `round(CurrentCapacity * 100 / MaxCapacity)`, clamped to 0–100.
- `IsCharging` and `ExternalConnected` are booleans.
- `Amperage` may arrive as a large unsigned value. Take the low 32 bits as `i32` (`v as i32`).
- `Temperature` is in hundredths of a °C.
- `CycleCount` → `u32`.
- A missing required key → `MissingProp("<Name>")`.

- [ ] **Step 1: Write failing tests**

```rust
fn props(pairs: &[(&str, Prop)]) -> HashMap<String, Prop> { /* build map */ }
#[test] fn parses_apple_silicon_percent() {
    let s = parse_snapshot(&props(&[("CurrentCapacity", Prop::Int(81)), ("MaxCapacity", Prop::Int(100)),
        ("IsCharging", Prop::Bool(false)), ("ExternalConnected", Prop::Bool(true)),
        ("Amperage", Prop::Int(-100)), ("Temperature", Prop::Int(3050)), ("CycleCount", Prop::Int(42))])).unwrap();
    assert_eq!(s, BatterySnapshot { percent: 81, is_charging: false, external_connected: true,
        amperage_ma: -100, temperature_c: 30.5, cycle_count: 42 });
}
#[test] fn computes_percent_from_mah() { /* CurrentCapacity 4000, MaxCapacity 5000 → percent 80 */ }
#[test] fn missing_prop_is_error() {
    let r = parse_snapshot(&props(&[("CurrentCapacity", Prop::Int(50))]));
    assert!(matches!(r, Err(BatteryError::MissingProp(_))));
}
#[test] #[ignore = "hardware"] fn reads_real_battery() {
    let s = IoRegBattery.snapshot().unwrap();
    assert!(s.percent <= 100 && s.temperature_c > 0.0 && s.temperature_c < 80.0);
}
```
Add `amperage_wraps_u32`: `Prop::Int(4294967196)` parses to `amperage_ma == -100`.

- [ ] **Step 2: Run `cargo test -p eighty-hw`.** Expected: compile failure.
- [ ] **Step 3: Implement.** `ioreg.rs` uses `IOServiceGetMatchingService(0, IOServiceMatching("AppleSmartBattery"))` and `IORegistryEntryCreateCFProperties`. It converts the CFDictionary to `HashMap<String, Prop>` (CFNumber → `Int`, CFBoolean → `Bool`, other types skipped) and calls `parse_snapshot`.
- [ ] **Step 4: Run `cargo test -p eighty-hw && cargo test -p eighty-hw -- --ignored`.** Expected: all pass.
- [ ] **Step 5: Commit** `feat(hw): read battery snapshot from IORegistry`

---

### Task 4: Candidate catalog and read-only probe

**Files:**
- Create: `crates/eighty-probe/Cargo.toml`, `src/lib.rs`, `src/catalog.rs`, `src/reads.rs`
- Test: inline tests in `catalog.rs`, `reads.rs`

**Interfaces:**
- Consumes: `SmcIo`, `FourCC`, `hex`, `FakeSmc`, `WriteMode` (dev-dep `eighty-hw` with `features = ["fake"]`).
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Serialize)] pub enum Role { InhibitCharging, ForceDischarge, Light }`
  - `pub struct Candidate { pub backend: &'static str, pub role: Role, pub key: &'static str, pub on: &'static [u8], pub off: &'static [u8] }`
  - `pub const CANDIDATES: &[Candidate]`, exactly these rows in this order (spec §5.2 and battery#482):

    | backend | role | key | on | off |
    |---|---|---|---|---|
    | legacy | InhibitCharging | CH0B | `[0x02]` | `[0x00]` |
    | legacy | InhibitCharging | CH0C | `[0x02]` | `[0x00]` |
    | legacy | ForceDischarge | CH0I | `[0x01]` | `[0x00]` |
    | tahoe | InhibitCharging | CHTE | `[0x01,0,0,0]` | `[0,0,0,0]` |
    | tahoe | ForceDischarge | CHIE | `[0x08]` | `[0x00]` |
    | tahoe | ForceDischarge | CH0J | `[0x01]` | `[0x00]` |
    | magsafe | Light | ACLC | `[0x03]` (green) | `[0x04]` (orange) |

    For `Light`, "on"/"off" mean green/orange. The restore value is always the original read, not `off`.
  - `pub const READ_ONLY_KEYS: &[&str] = &["bfD0", "bfE0", "bfF0", "CHIB"];` These are probed for presence and value only, because their write semantics are unknown.
  - `#[derive(Serialize)] pub struct KeyReading { pub key: String, pub status: ReadStatus }`
  - `#[derive(Serialize)] pub enum ReadStatus { Present { size: u32, data_type: String, value_hex: String }, Missing, Error(String) }`
  - `pub fn probe_reads(smc: &mut dyn SmcIo) -> Vec<KeyReading>` reads every catalog key, then every `READ_ONLY_KEYS` key, deduplicated, in that order.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn catalog_has_spec_keys() {
    let keys: Vec<_> = CANDIDATES.iter().map(|c| c.key).collect();
    assert_eq!(keys, ["CH0B", "CH0C", "CH0I", "CHTE", "CHIE", "CH0J", "ACLC"]);
    assert!(CANDIDATES.iter().all(|c| c.on.len() == c.off.len()));
}
#[test] fn reads_present_and_missing() {
    let smc = FakeSmc::new();
    smc.insert("CHTE", "ui32", &[0, 0, 0, 0], WriteMode::Accept);
    smc.insert("ACLC", "ui8 ", &[0], WriteMode::Accept);
    let r = probe_reads(&mut smc.clone());
    assert_eq!(r.len(), 11);
    let chte = r.iter().find(|k| k.key == "CHTE").unwrap();
    assert!(matches!(&chte.status, ReadStatus::Present { size: 4, value_hex, .. } if value_hex == "00000000"));
    assert!(matches!(r.iter().find(|k| k.key == "CH0B").unwrap().status, ReadStatus::Missing));
    assert!(smc.writes().is_empty());
}
```

- [ ] **Step 2: Run `cargo test -p eighty-probe`.** Expected: compile failure.
- [ ] **Step 3: Implement** `catalog.rs` and `reads.rs` (`KeyNotFound` → `Missing`, other errors → `Error(e.to_string())`).
- [ ] **Step 4: Run `cargo test -p eighty-probe`.** Expected: pass.
- [ ] **Step 5: Commit** `feat(probe): add candidate catalog and read-only probe`

---

### Task 5: Write-test engine with guaranteed restore

**Files:**
- Create: `crates/eighty-probe/src/guard.rs`, `crates/eighty-probe/src/writes.rs`
- Test: inline tests in both

**Interfaces:**
- Consumes: `SmcIo`, `BatterySource`, `BatterySnapshot`, `FakeSmc`, `FakeBattery`, `CANDIDATES`, `Candidate`, `Role`.
- Produces:
  - `pub struct RestoreGuard<'a> { … }`: `RestoreGuard::new(smc: &'a mut dyn SmcIo) -> Self`, `fn write_tracked(&mut self, key: FourCC, value: &[u8]) -> Result<(), SmcError>` (reads and records the original the first time a key is touched, then writes), `fn restore_all(&mut self) -> Vec<(String, Result<(), SmcError>)>` (writes back the originals in reverse order, verifies each by read-back, and clears the list), `impl Drop` (calls `restore_all`).
  - `#[derive(Debug, PartialEq, Serialize)] pub enum Verdict { Verified, NoEffect, WriteAccepted, Blocked, Missing, SizeMismatch { expected: u32, got: usize }, CannotVerify(String), WriteFailed(String), RestoreFailed(String) }`
  - `#[derive(Serialize)] pub struct WriteResult { pub backend: String, pub key: String, pub role: Role, pub verdict: Verdict, pub before: Option<BatterySnapshot>, pub after: Option<BatterySnapshot> }`
  - `pub struct WriteTestConfig { pub settle: Duration, pub min_discharge_percent: u8 }` with `Default` = `{ settle: 15s, min_discharge_percent: 20 }`
  - `pub fn run_write_tests(smc: &mut dyn SmcIo, battery: &mut dyn BatterySource, cfg: &WriteTestConfig, cancel: &AtomicBool, sleep: &mut dyn FnMut(Duration)) -> Vec<WriteResult>`

Per-candidate algorithm. The tests pin it, but the order matters, so it is stated here:
1. If `cancel` is set, stop and return the results so far.
2. `key_info`: `KeyNotFound` → `Missing`. If the size ≠ `on.len()` → `SizeMismatch` and skip.
3. Preconditions from `battery.snapshot()`:
   - InhibitCharging needs `external_connected && is_charging && percent < 100`, otherwise `CannotVerify("<reason>")`.
   - ForceDischarge needs `external_connected && percent >= min_discharge_percent`, otherwise `CannotVerify`.
   - Light needs `external_connected`.
4. `guard.write_tracked(key, on)`: `NotPrivileged` → `Blocked`; other errors → `WriteFailed`.
5. Wait `cfg.settle` by calling `sleep` in chunks of ≤ 250 ms and checking `cancel` between chunks. For Light, write `on`, wait, write `off`, wait.
6. Take the after-snapshot.
   - InhibitCharging → `Verified` if `!after.is_charging`, otherwise `NoEffect`.
   - ForceDischarge → `Verified` if `after.amperage_ma < 0`, otherwise `NoEffect`.
   - Light → `WriteAccepted`.
7. `guard.restore_all()`. Any failure overrides the verdict with `RestoreFailed`.

Each candidate gets its own guard, so one candidate's change never overlaps the next.

- [ ] **Step 1: Write failing tests**

```rust
fn snap(pct: u8, charging: bool, ext: bool, amp: i32) -> BatterySnapshot { /* temp 30.0, cycles 1 */ }
fn no_sleep() -> impl FnMut(Duration) { |_| {} }

#[test] fn guard_restores_on_drop() {
    let smc = FakeSmc::new(); smc.insert("CHTE", "ui32", &[0,0,0,0], WriteMode::Accept);
    { let mut c = smc.clone(); let mut g = RestoreGuard::new(&mut c);
      g.write_tracked(FourCC::new("CHTE").unwrap(), &[1,0,0,0]).unwrap(); }
    assert_eq!(smc.value("CHTE"), Some(vec![0,0,0,0]));
}
#[test] fn inhibit_verified_when_charging_stops() {
    // FakeSmc with CHTE; FakeBattery closure reads smc.value("CHTE"): [1,0,0,0] → snap(70,false,true,0), else snap(70,true,true,1500)
    // assert verdict for CHTE == Verified, CH0B == Missing, CHTE restored to [0,0,0,0]
}
#[test] fn inhibit_no_effect_when_battery_unchanged() { /* battery always charging → NoEffect */ }
#[test] fn inhibit_cannot_verify_when_not_charging() {
    // battery snap(100,false,true,0) → CHTE verdict matches CannotVerify(_); smc.writes() has no CHTE entry
}
#[test] fn discharge_skipped_below_floor() { /* snap(15,true,true,1000), CHIE present → CannotVerify, no write */ }
#[test] fn discharge_verified_on_negative_amperage() { /* CHIE==[8] → amp -900 → Verified, restored to [0] */ }
#[test] fn size_mismatch_skips_write() {
    // CHTE inserted as 1-byte "ui8 " → SizeMismatch { expected: 1, got: 4 }; no writes
}
#[test] fn not_privileged_is_blocked() { /* CHTE WriteMode::NotPrivileged → Blocked, value unchanged */ }
#[test] fn light_writes_green_then_orange_then_restores() {
    // ACLC original [0] → writes() == [("ACLC",[3]),("ACLC",[4]),("ACLC",[0])], verdict WriteAccepted
}
#[test] fn cancel_mid_settle_restores() {
    // sleep closure sets cancel=true on first call → CHTE restored to original, results stop after CHTE
}
```

- [ ] **Step 2: Run `cargo test -p eighty-probe`.** Expected: compile failure.
- [ ] **Step 3: Implement** `guard.rs` and `writes.rs` using the interfaces and algorithm above.
- [ ] **Step 4: Run `cargo test -p eighty-probe`.** Expected: all pass.
- [ ] **Step 5: Commit** `feat(probe): add write-test engine with guaranteed restore`

---

### Task 6: Report, machine info and CLI

**Files:**
- Create: `crates/eighty-probe/src/machine.rs`, `src/report.rs`, `src/main.rs`
- Test: inline tests in `report.rs`, `main.rs`

**Interfaces:**
- Consumes: `probe_reads`, `run_write_tests`, `WriteTestConfig`, `SmcConnection`, `IoRegBattery`.
- Produces:
  - `#[derive(Serialize)] pub struct MachineInfo { pub model: String, pub chip: String, pub macos: String, pub build: String }` with `MachineInfo::current() -> MachineInfo`, using `sysctlbyname` for `hw.model`, `machdep.cpu.brand_string`, `kern.osproductversion` and `kern.osversion` (`"unknown"` on failure).
  - `#[derive(Serialize)] pub struct ProbeReport { pub tool_version: String, pub machine: MachineInfo, pub battery: Option<BatterySnapshot>, pub reads: Vec<KeyReading>, pub writes: Option<Vec<WriteResult>> }`
  - `pub fn render_text(r: &ProbeReport) -> String`
  - `pub fn check_write_permission(write_test: bool, euid: u32) -> Result<(), String>`
  - CLI: `eighty-probe [--write-test] [--settle-secs <N, default 15>] [--json <PATH>]`
    - Prints `render_text` to stdout and optionally writes pretty JSON to the path.
    - Exits 2 on the permission error.
    - Installs a `ctrlc` handler that only sets the `cancel` flag.
    - Before the Light test, prints `Watch the MagSafe light: GREEN for N s, then ORANGE for N s.`

`render_text` layout, one line per item:
- header: `eighty-probe <ver> — <chip>, macOS <macos> (<build>), <model>`
- battery: `Battery: <pct>% charging=<bool> plugged=<bool> <amp> mA <temp> °C`
- reads: `READ  <KEY>  present <size>B <type> = <hex>` or `READ  <KEY>  missing`
- writes: `WRITE <KEY>  <backend>/<role>  <Verdict>`
- final line: `Summary: inhibit=<first Verified InhibitCharging key or "none">, discharge=<… or "none">, light=<ACLC if WriteAccepted else "none">`

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn write_test_requires_root() {
    let e = check_write_permission(true, 501).unwrap_err();
    assert!(e.contains("sudo eighty-probe --write-test"));
    assert!(check_write_permission(true, 0).is_ok());
    assert!(check_write_permission(false, 501).is_ok());
}
#[test] fn renders_summary_line() {
    // build a ProbeReport with writes: CHTE Verified (InhibitCharging), CHIE NoEffect, CH0J Verified, ACLC WriteAccepted
    let t = render_text(&r);
    assert!(t.contains("READ  CHTE  present 4B ui32 = 00000000"));
    assert!(t.contains("READ  CH0B  missing"));
    assert!(t.ends_with("Summary: inhibit=CHTE, discharge=CH0J, light=ACLC\n"));
}
#[test] fn summary_none_when_nothing_works() { /* all Missing → "Summary: inhibit=none, discharge=none, light=none" */ }
```

- [ ] **Step 2: Run `cargo test -p eighty-probe`.** Expected: compile failure.
- [ ] **Step 3: Implement** `machine.rs`, `report.rs` and `main.rs`. In `main`, call `check_write_permission(args.write_test, libc::geteuid())` **before** opening `SmcConnection`.
- [ ] **Step 4: Run `cargo test --workspace && cargo clippy --workspace -- -D warnings`.** Expected: all pass, no warnings.
- [ ] **Step 5: Run `cargo run -p eighty-probe` (no sudo).** Expected: a header, a battery line and 11 READ lines. No WRITE lines. Summary shows `none` for all three.
- [ ] **Step 6: Commit** `feat(probe): add report and eighty-probe CLI`

---

### Task 7: Run the probe on both of the owner's Macs and record findings

Two machines are in scope:
- **Mac A:** M5 Pro, macOS 26.6.2 (this machine)
- **Mac B:** the owner's M2 Pro/Max at home. Its chip and macOS version are captured automatically in the report.

Comparing them shows whether the working keys differ across M-series generations (spec §10 risk). The owner runs the sudo steps (suggest `! <command>` in the Claude Code prompt). Each Mac stays on MagSafe for its whole run.

Report file names use `<slug>` = `<chip>-macos<version>`, lowercase, no spaces, e.g. `m5pro-macos26.6.2`.

- [ ] **Step 1: Build a release binary on Mac A.** Run `cargo build --release -p eighty-probe`. Both Macs are arm64, so the same binary runs on Mac B. Copy `target/release/eighty-probe` to Mac B via AirDrop/scp, or clone the repo and build there if Rust is installed.
- [ ] **Step 2: On each Mac, read-only run.** Run `./eighty-probe --json docs/probe/2026-11-16-<slug>-read.json`.
- [ ] **Step 3: On each Mac, write test (owner).** With the Mac plugged into MagSafe, charge between 20% and 95% and actively charging, run `sudo ./eighty-probe --write-test --json docs/probe/2026-11-16-<slug>-write.json`. The owner notes whether the light turned green, then orange.
- [ ] **Step 4: On each Mac, confirm the restore.** Re-run the read-only command. Every catalog key's value must match Step 2. If any differs, stop and fix the issue before continuing.
- [ ] **Step 5: Bring Mac B's two JSON files into `docs/probe/` on Mac A.**
- [ ] **Step 6: Write up** `docs/probe/README.md` with a table of one row per Mac: chip, macOS version/build, summary line, light observation. Then add:
  - **Agreement:** whether the two Macs agree on the working inhibit, discharge and light keys. If they disagree, list the per-machine backend order Plan 2's probe-at-startup must handle.
  - **Plan 2 backend:** which spec §5.2 backend(s) this implies for Plan 2.
  - **If either Mac shows `inhibit=none`:** record it, per spec §5.2 ("If no backend works… the project continues"). Plan 2 then covers investigating `bfD0`/`bfE0`/`bfF0` and the other routes on that machine.
- [ ] **Step 7: Commit** `docs(probe): record M5 Pro and M2 probe results`
