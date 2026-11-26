# eighty Plan 4 — Overheat Protection and Battery Temperature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While the battery is at or above the pause temperature and charging on AC, eighty stops charging: `CHTE` in hold mode, or `CHIE = 08` in native mode, with a 20% floor. It resumes at the resume temperature, and `status`/`doctor` show the temperature.

**Architecture:** A new pure module, `eighty-core::overheat`, owns the settings (`OverheatConfig`), a persisted latch (`HeatState`), the latch update `heat_step()` and `apply_heat()`. `apply_heat()` rewrites a `Decision` from the unchanged `decide()`, so heat is a layer on top of the existing policy rather than a change to it. The helper engine runs `heat_step` each tick before calibration and `decide`, then applies the result. It saves the latch to `heat.toml` and reports it in `Status`. Calibration learns `heat_paused`, so its hold clock and give-up clock stop during a pause. The CLI adds `eighty overheat …` and the new status and doctor lines.

**Tech Stack:** Rust stable (edition 2021), existing workspace crates. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-26-eighty-overheat-design.md` (parent: `docs/superpowers/specs/2026-11-16-eighty-design.md`).

## Global Constraints

- Defaults: `enabled = true`, `pause_c = 35`, `resume_c = 32`. Whole °C (`u8`).
- Ranges: `pause_c` 30–45; `resume_c` from 25 up to `pause_c − 2`. With `testing = true` (hidden, §Task 7): `pause_c` 20–45 and `resume_c` from 15. `eighty overheat <pause>` without a resume value uses `pause − 3`.
- Enter the pause only when: enabled, a reading exists, `temperature ≥ pause_c`, on AC, **and the battery reports charging**. Leave it when `temperature ≤ resume_c`, the reading is missing, or protection is off.
- Native floor: `HEAT_FLOOR = 20` (%). A pause ends at or below it, `floor_hit` is set, and no new pause starts until charge ≥ `HEAT_REARM = 30` or `temperature ≤ resume_c`.
- Hold mode pause: `inhibit = true`, `discharge` unchanged, light `Green`. Native mode pause: `discharge = true`, `inhibit = false`, light `None`, `native_limit` unchanged.
- No heat `CHIE` while asleep. The existing `self.sleeping` rule after `decide` already covers it, provided `apply_heat` runs before that rule.
- No heat writes during a fault. The existing early returns before `decide` already cover it.
- Lid closed in native mode: governed by the single constant `HEAT_PAUSE_LID_CLOSED`, which Task 1 sets. `discharge <n>`, auto-discharge and calibration step 1 keep today's lid rule regardless.
- Status text, exactly:
  - `Temp 31.4 °C` / `Temp n/a`
  - `Paused: battery hot (36.1 °C), resumes at 32 °C`
  - `Battery hot (36.1 °C), can't pause charging with the lid closed`
  - `Battery hot (36.1 °C), can't pause charging on this Mac`
  - `Battery hot, charging anyway: charge reached 20%`
- Doctor line: `Overheat on · pause 35 °C · resume 32 °C` or `Overheat off`. Doctor warning when on and no reading: `no battery temperature reported; overheat protection is inactive on this Mac`.
- Log lines start with `overheat: `.
- New `Status`/`Config`/`CalInputs` fields are `#[serde(default)]` where serialized, so an older CLI/helper pair still parses. `Activity` gets **no** new variant.
- `sudo eighty uninstall` deletes `/Library/Application Support/eighty/heat.toml`, as it does `calibration.toml`.
- Commits: conventional style, **no** `Co-Authored-By` trailer.

## Review Focus

1. **Battery held at its limit while hot (native, 80%, not charging):** no `CHIE`, no drain. Pinned by `no_pause_when_not_charging` (Task 2) and `engine_hot_at_limit_does_nothing` (Task 5).
2. **The pause lasting until the floor:** `CHIE` goes off at 20%, it doesn't start again at 21–29% while still hot, and it does start again at 30%. Pinned by `floor_ends_pause_and_rearms_at_30` (Task 2) and `engine_heat_floor` (Task 5).
3. **Helper restart at 33 °C during a pause:** the pause continues and `CHIE` is written again after the startup clear. Pinned by `engine_heat_latch_survives_restart` (Task 5).
4. **The temperature disappears mid-pause** (a macOS update drops the field): the pause ends, `CHIE` goes off and doctor warns. Pinned by `missing_reading_clears` (Task 2) and `engine_missing_temperature_ends_pause` (Task 5).
5. **A heat pause during calibration step 3 that lasts longer than 24 h:** the calibration isn't abandoned and the hold clock doesn't count. Pinned by `heat_pause_freezes_hold_and_give_up` (Task 3).

---

## File Structure

```
crates/eighty-probe/src/main.rs           + --chie-soak <secs>
crates/eighty-probe/src/soak.rs           NEW: CHIE soak loop (restores via guard)
crates/eighty-core/src/overheat.rs        NEW: OverheatConfig, HeatState, HeatInputs, HeatNote, heat_step, apply_heat
crates/eighty-core/src/lib.rs             + pub mod overheat
crates/eighty-core/src/config.rs          + Config.overheat
crates/eighty-core/src/calibration.rs     + CalInputs.heat_paused, CalWait::Heat
crates/eighty-core/src/proto.rs           + requests, OverheatStatus, Status fields
crates/eighty-core/src/paths.rs           + HEAT_FILE
crates/eighty-helper/src/store.rs         + load/save heat
crates/eighty-helper/src/engine.rs        integration
crates/eighty-cli/src/main.rs             overheat command
crates/eighty-cli/src/render.rs           Temp/heat/doctor lines
crates/eighty-cli/src/install.rs          uninstall deletes HEAT_FILE
docs/testing/plan4-acceptance.md          NEW
docs/superpowers/specs/*.md               notes
```

---

### Task 1: CHIE soak tool and the lid-closed hardware check

**Files:**
- Create: `crates/eighty-probe/src/soak.rs`
- Modify: `crates/eighty-probe/src/main.rs`, `crates/eighty-probe/src/lib.rs`
- Create: `docs/testing/plan4-acceptance.md` (section "Lid-closed CHIE check")

**Interfaces:**
- Produces: `eighty-probe --chie-soak <SECS>` (root only, like `--write-test`), and the value of `HEAT_PAUSE_LID_CLOSED` used in Task 2.

- [ ] **Step 1: Write the failing test.** In `soak.rs`, write `soak_writes_08_then_restores` against the probe's existing fake SMC (as in `writes.rs` tests). Call `run_soak(&mut smc, Duration::ZERO, Duration::ZERO, &mut |_| {})` with `CHIE` starting at `[0]`. Assert the write sequence is `[("CHIE",[8]), ("CHIE",[0])]` and the final value is `[0]`. Also write `soak_restores_original_nonzero`: start at `[0x10]`, final value `[0x10]`.
- [ ] **Step 2: Run** `cargo test -p eighty-probe soak`. Expected: FAIL (`run_soak` not defined).
- [ ] **Step 3: Implement** `pub fn run_soak(smc: &mut dyn SmcIo, total: Duration, every: Duration, sample: &mut dyn FnMut(Duration)) -> anyhow::Result<()>` in `soak.rs`. It writes through `RestoreGuard::write_tracked` (`guard.rs`), whose `Drop` restores the original value on return or error. For Ctrl-C, a SIGINT handler (`libc::signal`) sets an `AtomicBool` that the loop checks each second, so the loop ends and the guard drops instead of the process dying. Then it writes `08` and calls `sample(elapsed)` every `every` until `total`, then restores. In `main.rs`, `--chie-soak <SECS>` (conflicts with `--write-test`, same root check) samples every 30 s. Each sample prints one line with the wall-clock time, charge %, `ExternalConnected`, amperage, `CHIE` value and the lid state (`eighty_hw::system::lid_closed()`). If the Mac sleeps, it shows up as a gap of more than 60 s between lines.
- [ ] **Step 4: Run** `cargo test -p eighty-probe` and `cargo clippy -p eighty-probe -- -D warnings`. Expected: PASS, no warnings.
- [ ] **Step 5: Commit** `feat(probe): add --chie-soak for the lid-closed overheat check`.
- [ ] **Step 6 (owner, M5 Pro):** connect the LG displays and AC, with charge between 30 and 95%. Close the lid, wait for the external displays to show the desktop, then run `sudo ./target/release/eighty-probe --chie-soak 300` from an external-display terminal. Record the output in `docs/testing/plan4-acceptance.md`. **Pass** means no gap over 60 s, the displays stay on and `CHIE` ends `00`.
- [ ] **Step 7: Record the decision.** On pass, `HEAT_PAUSE_LID_CLOSED = true`. On fail, `false`. Add a dated note under spec §6.4 with the result. Commit `docs: record lid-closed CHIE check (plan 4)`.

---

### Task 2: `overheat` core module (settings, latch, apply)

**Files:**
- Create: `crates/eighty-core/src/overheat.rs`
- Modify: `crates/eighty-core/src/lib.rs`, `crates/eighty-core/src/config.rs`

**Interfaces:**
- Consumes: `policy::{Decision, ControlMode, Light, Outputs}`.
- Produces:
  ```rust
  pub const HEAT_FLOOR: u8 = 20;
  pub const HEAT_REARM: u8 = 30;
  pub const HEAT_PAUSE_LID_CLOSED: bool; // value from Task 1
  #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)] #[serde(default)]
  pub struct OverheatConfig { pub enabled: bool, pub pause_c: u8, pub resume_c: u8, pub testing: bool }
  impl Default for OverheatConfig            // true, 35, 32, false
  impl OverheatConfig {
      pub fn validate(&self) -> Result<(), String>;
      pub fn with_thresholds(self, pause_c: u8, resume_c: Option<u8>, testing: bool) -> Result<OverheatConfig, String>;
  }
  #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
  pub struct HeatState { pub paused: bool, pub floor_hit: bool }
  pub struct HeatInputs { pub temperature_c: Option<f32>, pub percent: u8, pub external: bool, pub charging: bool, pub mode: ControlMode }
  pub fn heat_step(cfg: &OverheatConfig, prev: HeatState, i: &HeatInputs) -> HeatState;
  #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)] #[serde(rename_all = "snake_case")]
  pub enum HeatNote { Paused, LidClosed, Unable, ChargingAtFloor }
  pub fn apply_heat(d: Decision, h: HeatState, mode: ControlMode, external: bool, lid_closed: bool, can_discharge: bool) -> (Decision, Option<HeatNote>);
  // config.rs: Config gains `#[serde(default)] pub overheat: OverheatConfig`
  ```

- [ ] **Step 1: Write the failing tests** in `overheat.rs` (`cfg()` is the default config; `hi(t, pct, ext, chg, mode)` builds `HeatInputs`):
  ```rust
  #[test] fn defaults() { assert_eq!(OverheatConfig::default(), OverheatConfig { enabled: true, pause_c: 35, resume_c: 32, testing: false }); }
  #[test] fn thresholds_validate() {
      let d = OverheatConfig::default();
      assert_eq!(d.with_thresholds(38, None, false).unwrap().resume_c, 35);
      assert!(d.with_thresholds(29, None, false).is_err());
      assert!(d.with_thresholds(46, None, false).is_err());
      assert!(d.with_thresholds(35, Some(34), false).is_err());   // > pause − 2
      assert!(d.with_thresholds(30, Some(24), false).is_err());   // < 25
      assert_eq!(d.with_thresholds(22, Some(20), true).unwrap(), OverheatConfig { enabled: true, pause_c: 22, resume_c: 20, testing: true });
      assert!(d.with_thresholds(19, None, true).is_err());
      assert!(!d.with_thresholds(36, None, false).unwrap().testing);
  }
  #[test] fn old_config_without_overheat_loads_defaults() { assert_eq!(toml::from_str::<Config>("[settings]\nupper = 80\n").unwrap().overheat, OverheatConfig::default()); }
  #[test] fn enters_at_pause_when_charging_on_ac() { assert!(heat_step(&cfg(), HeatState::default(), &hi(Some(35.0), 60, true, true, Native)).paused); }
  #[test] fn no_pause_when_not_charging() { assert!(!heat_step(&cfg(), HeatState::default(), &hi(Some(40.0), 80, true, false, Native)).paused); }
  #[test] fn no_pause_on_battery() { assert!(!heat_step(&cfg(), HeatState::default(), &hi(Some(40.0), 60, false, true, Native)).paused); }
  #[test] fn stays_paused_between_thresholds_even_not_charging() {
      let p = HeatState { paused: true, floor_hit: false };
      assert!(heat_step(&cfg(), p, &hi(Some(33.0), 60, true, false, Native)).paused);
      assert!(!heat_step(&cfg(), p, &hi(Some(32.0), 60, true, false, Native)).paused);
  }
  #[test] fn missing_reading_clears() { assert_eq!(heat_step(&cfg(), HeatState { paused: true, floor_hit: true }, &hi(None, 60, true, true, Native)), HeatState::default()); }
  #[test] fn disabled_clears() { let c = OverheatConfig { enabled: false, ..cfg() }; assert_eq!(heat_step(&c, HeatState { paused: true, floor_hit: false }, &hi(Some(40.0), 60, true, true, Native)), HeatState::default()); }
  #[test] fn floor_ends_pause_and_rearms_at_30() {
      let s = heat_step(&cfg(), HeatState { paused: true, floor_hit: false }, &hi(Some(37.0), 20, true, false, Native));
      assert_eq!(s, HeatState { paused: false, floor_hit: true });
      assert_eq!(heat_step(&cfg(), s, &hi(Some(37.0), 29, true, true, Native)), s);
      assert_eq!(heat_step(&cfg(), s, &hi(Some(37.0), 30, true, true, Native)), HeatState { paused: true, floor_hit: false });
      assert_eq!(heat_step(&cfg(), s, &hi(Some(32.0), 25, true, true, Native)), HeatState::default());
  }
  #[test] fn hold_mode_has_no_floor() { assert!(heat_step(&cfg(), HeatState { paused: true, floor_hit: false }, &hi(Some(37.0), 15, true, false, Hold)).paused); }
  ```
  `apply_heat` tests. `dec(inhibit, discharge, light, native_limit)` builds a `Decision` with `activity: Charging { to: 80 }`, and `P` is `HeatState { paused: true, floor_hit: false }`:
  ```rust
  #[test] fn hold_pause_inhibits_green() { let (d, n) = apply_heat(dec(false, false, Some(Orange), None), P, Hold, true, false, true); assert_eq!(d.outputs, Outputs { inhibit: true, discharge: false, light: Some(Green) }); assert_eq!(n, Some(HeatNote::Paused)); }
  #[test] fn native_pause_discharges_keeps_limit() { let (d, n) = apply_heat(dec(false, false, None, Some(100)), P, Native, true, false, true); assert_eq!(d.outputs, Outputs { inhibit: false, discharge: true, light: None }); assert_eq!(d.native_limit, Some(100)); assert_eq!(n, Some(HeatNote::Paused)); }
  #[test] fn native_lid_closed_follows_constant() { let (d, n) = apply_heat(dec(false, false, None, Some(80)), P, Native, true, true, true); if HEAT_PAUSE_LID_CLOSED { assert!(d.outputs.discharge); assert_eq!(n, Some(HeatNote::Paused)); } else { assert!(!d.outputs.discharge); assert_eq!(n, Some(HeatNote::LidClosed)); } }
  #[test] fn native_without_chie_is_unable() { let (d, n) = apply_heat(dec(false, false, None, Some(80)), P, Native, true, false, false); assert!(!d.outputs.discharge); assert_eq!(n, Some(HeatNote::Unable)); }
  #[test] fn already_discharging_unchanged() { let d0 = dec(true, true, Some(Green), None); let (d, n) = apply_heat(d0.clone(), P, Hold, true, false, true); assert_eq!((d.outputs, n), (d0.outputs, None)); }
  #[test] fn on_battery_or_unsupported_unchanged() { assert_eq!(apply_heat(dec(false, false, None, Some(80)), P, Native, false, false, true).1, None); assert_eq!(apply_heat(dec(false, false, None, None), P, Unsupported, true, false, true).1, None); }
  #[test] fn floor_hit_reports_charging_at_floor() { let (d, n) = apply_heat(dec(false, false, None, Some(80)), HeatState { paused: false, floor_hit: true }, Native, true, false, true); assert!(!d.outputs.discharge); assert_eq!(n, Some(HeatNote::ChargingAtFloor)); }
  ```
  (`Decision` needs `#[derive(Clone, Debug, PartialEq)]` if it doesn't have it yet. Add it in `policy.rs`.)
- [ ] **Step 2: Run** `cargo test -p eighty-core overheat`. Expected: FAIL (module missing).
- [ ] **Step 3: Implement** `overheat.rs` and `Config.overheat`. Order of rules in `heat_step`: disabled, or `None`, or `t ≤ resume_c` → `HeatState::default()`. Then `floor_hit`: below `HEAT_REARM` return unchanged; at or above it, clear `floor_hit` and continue. Then `paused` with `mode == Native && percent ≤ HEAT_FLOOR` → `{false, true}`. Then `paused` → unchanged. Otherwise enter if `t ≥ pause_c && external && charging`. Comparisons use `f32::from(u8)`. In `apply_heat`, return `(d, None)` when `!external` or `mode == Unsupported`. Return `(d, None)` when not paused and `!floor_hit`. A paused state whose decision already discharges also returns `(d, None)`.
- [ ] **Step 4: Run** `cargo test -p eighty-core` and `cargo clippy --workspace -- -D warnings`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): add overheat latch and decision layer`.

---

### Task 3: Calibration ignores time spent in a heat pause

**Files:**
- Modify: `crates/eighty-core/src/calibration.rs`

**Interfaces:**
- Produces: `CalInputs { …, pub heat_paused: bool }` and `CalWait::Heat`. `waiting()` returns `Some(CalWait::Heat)` in steps Charge/Hold while `heat_paused` (ahead of PlugIn/LidClosed).

- [ ] **Step 1: Write the failing tests:**
  - `heat_pause_freezes_hold_and_give_up`: step Hold, `held_secs = 600`, `last_tick = Some((t, true))`. Advance with `heat_paused: true` at `t + 90_000` on AC. Assert `held_secs == 600`, the outcome is `Continue` (not `Abandoned`), and `last_progress_at` moved forward by 90_000.
  - `heat_pause_no_kick_in_charge_step`: step Charge, settled, on AC, 60%, not charging, 600 s since step start, `heat_paused: true`. Assert the outcome isn't `KickCharging`.
  - `heat_pause_in_discharge_step_is_ignored`: step Discharge, `heat_paused: true`, percent drops. Assert progress counts as usual.
  - `waiting_reports_heat`: assert `waiting(&c_charge, &i_heat) == Some(CalWait::Heat)`.

  Update every existing `CalInputs` literal with `heat_paused: false`.
- [ ] **Step 2: Run** `cargo test -p eighty-core calibration`. Expected: FAIL.
- [ ] **Step 3: Implement.** At the top of `advance`, when `heat_paused` and the step isn't `Discharge`: add `dt` since `last_tick` to `last_progress_at` (so the give-up clock stands still), skip the hold clock, update `last_tick`, and return `Continue`. Downstream, add `CalWait::Heat => "waiting for the battery to cool"` wherever `CalWait` is matched.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: PASS (the engine and CLI compile with `heat_paused: false` placeholders until Task 5).
- [ ] **Step 5: Commit** `feat(core): pause calibration clocks during a heat pause`.

---

### Task 4: Protocol, paths and store

**Files:**
- Modify: `crates/eighty-core/src/proto.rs`, `crates/eighty-core/src/paths.rs`, `crates/eighty-helper/src/store.rs`, `crates/eighty-cli/src/install.rs`

**Interfaces:**
- Produces:
  ```rust
  // proto.rs
  Request::SetOverheat { on: bool }
  Request::SetOverheatThresholds { pause_c: u8, resume_c: Option<u8>, testing: bool }   // mutating
  #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
  pub struct OverheatStatus { pub config: OverheatConfig, pub note: Option<HeatNote> }
  Status { …, #[serde(default)] pub temperature_c: Option<f32>, #[serde(default)] pub overheat: Option<OverheatStatus> }
  // paths.rs
  pub const HEAT_FILE: &str = "/Library/Application Support/eighty/heat.toml";
  // store.rs
  pub fn load_heat(&self) -> HeatState;            // missing → default; unreadable/invalid → delete file, default
  pub fn save_heat(&self, h: &HeatState) -> io::Result<()>;   // write_atomic("heat.toml")
  ```
- [ ] **Step 1: Write the failing tests:**
  - `status_without_overheat_fields_parses`: JSON from an older helper → `temperature_c == None`, `overheat == None`.
  - `heat_state_roundtrip`: save `{paused: true, floor_hit: false}`, then load the same.
  - `invalid_heat_file_is_default`: write `"garbage = ["`, then `load_heat() == HeatState::default()` and the file is gone.
  - `uninstall_removes_heat_file`: `UNINSTALL_FILES.contains(&HEAT_FILE)`.
- [ ] **Step 2: Run** `cargo test --workspace`. Expected: FAIL.
- [ ] **Step 3: Implement.** Mutating requests go through `is_mutating` unchanged, since only Status/Doctor are read-only. Fill the new `Status` fields with `None` in the engine until Task 5.
- [ ] **Step 4: Run** `cargo test --workspace` and clippy. Expected: PASS.
- [ ] **Step 5: Commit** `feat: overheat requests, status fields and heat.toml store`.

---

### Task 5: Engine integration

**Files:**
- Modify: `crates/eighty-helper/src/engine.rs`

**Interfaces:**
- Consumes: Tasks 2–4.
- Produces: `Engine.heat: HeatState`, `Engine.heat_note: Option<HeatNote>`. `Status.temperature_c` comes from the snapshot, and `Status.overheat` is `Some` always. Two new request handlers, plus a status warning while `testing` is set: `overheat test thresholds in use (pause N °C); run \`eighty overheat 35\` to return to normal`.

- [ ] **Step 1: Write the failing tests** (add `Rig::set_temp(&self, t: Option<f32>)`):
  - `engine_heat_native_sets_chie`: native, `set(60, true, true, 1500, Some(900))`, `set_temp(Some(36.0))`, tick → `CHIE == [8]` and `status().overheat.unwrap().note == Some(HeatNote::Paused)`. The log has a line starting `overheat: paused`.
  - `engine_heat_hold_inhibits`: hold, 60% charging, 36 °C → `CHTE == [1,0,0,0]`, `CHIE == [0]`.
  - `engine_heat_resumes`: after the pause, `set_temp(Some(32.0))`, tick → `CHIE == [0]`, note `None`, log `overheat: resumed`.
  - `engine_hot_at_limit_does_nothing`: native, `set(80, false, true, 0, ..)`, 40 °C → `CHIE == [0]`.
  - `engine_heat_latch_survives_restart`: pause, then drop the engine and set 33 °C. A new `r.engine()` gives `CHIE == [8]` after `Event::Tick`, and `r.store.load_heat().paused`.
  - `engine_heat_floor`: paused native, `set(20, false, true, -500, ..)`, tick → `CHIE == [0]`, note `ChargingAtFloor`. At 25% charging, still `[0]`. At 30% charging, `[8]`.
  - `engine_heat_not_while_asleep`: paused, then `sleep(&mut e)` → `CHIE == [0]`.
  - `engine_heat_pauses_charge_full`: native, `Request::ChargeFull` on AC at 90% charging, then 36 °C → native limit 100 kept, `CHIE == [8]`, `override_ == FullCharge`.
  - `engine_heat_not_during_conflict`: native paused, then make another writer change `CHIE` to `[0x10]`. No `CHIE` write after the conflict.
  - `engine_missing_temperature_ends_pause`: paused, then `set_temp(None)` → `CHIE == [0]`.
  - `overheat_off_ends_pause`: paused, `Request::SetOverheat { on: false }` → `CHIE == [0]` right after the request, and the config is saved with `enabled == false`.
  - `overheat_thresholds_request_validates`: `SetOverheatThresholds { pause_c: 29, resume_c: None, testing: false }` → `Response::Error`. `{ pause_c: 22, resume_c: Some(20), testing: true }` → saved, and the status warnings contain the testing warning.
  - `calibration_hold_frozen_by_heat`: in calibration step 3 native, hot and paused, advance the clock 2 h → `calibration.held_secs` unchanged.
- [ ] **Step 2: Run** `cargo test -p eighty-helper heat overheat`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - `start()`: `heat = store.load_heat()`.
  - `tick()`, after `native_prepare` and before `maybe_start_scheduled`: compute `heat_step` from the snapshot. Use `charging: snap.is_charging` and `external: snap.external_connected`, which is the latched value while our `CHIE` is on. On change, log `overheat: paused at 36.1 °C`, `overheat: resumed at 32.0 °C`, `overheat: charge reached 20%, charging while hot` or `overheat: off`, and call `save_heat` (log a save failure, don't fault).
  - `advance_calibration` passes `heat_paused: self.heat.paused && snap.external_connected`.
  - After `decide`, and **before** the `self.sleeping` rule, run `apply_heat(d, self.heat, mode, external, lid_closed, self.control.can_discharge())`, then store `heat_note`. Clear `heat_note` in every early-return (fault) path.
  - `SetOverheat`/`SetOverheatThresholds`: validate, save config, then run `heat_step` again immediately, so turning it off ends the pause in the same request. Follow the existing apply-after-save pattern of `SetLimits`.
- [ ] **Step 4: Run** `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(helper): pause charging when the battery is hot`.

---

### Task 6: CLI command and rendering

**Files:**
- Modify: `crates/eighty-cli/src/main.rs`, `crates/eighty-cli/src/render.rs`

**Interfaces:**
- Consumes: Task 4 requests and `Status` fields.
- Produces: `eighty overheat on|off` and `eighty overheat <PAUSE> [RESUME] [--testing]` (`--testing` is `#[arg(long, hide = true)]`).

- [ ] **Step 1: Write the failing tests** in `render.rs` (and a clap parse test in `main.rs`):
  - `temp_line`: `temperature_c: Some(31.44)` → the output contains `Temp     31.4 °C` (use the existing `line()` padding). `None` → `Temp     n/a`.
  - `heat_paused_state_line`: note `Paused`, 36.1 °C, resume 32 → the State line is `Paused: battery hot (36.1 °C), resumes at 32 °C`.
  - `heat_lid_closed_line`, `heat_unable_line` and `heat_floor_line`: the exact copy from Global Constraints. The floor line uses `HEAT_FLOOR`.
  - `heat_during_calibration_appends`: calibrating step 2 with note `Paused` → the State line ends with `, paused: battery hot (36.1 °C)`.
  - `doctor_overheat_line_and_warning`: on with no temp → contains `Overheat on · pause 35 °C · resume 32 °C`, and `no battery temperature reported; overheat protection is inactive on this Mac`. Off → `Overheat off` and no warning.
  - `older_helper_without_overheat_renders`: `overheat: None` → no Temp line and no panic.
  - `parse_overheat_args`: `["overheat","on"]`, `["overheat","38"]`, `["overheat","38","34"]` and `["overheat","22","20","--testing"]` map to the right requests. `["overheat","hot"]` is an error.
- [ ] **Step 2: Run** `cargo test -p eighty-cli`. Expected: FAIL.
- [ ] **Step 3: Implement.** Model the clap shape on `Calibrate`: an `Overheat { words }` arg that parses `on`, `off`, `<u8> [u8]` and `--testing`. The Temp line goes directly after the Battery line, and only when `overheat.is_some()`.
- [ ] **Step 4: Run** `cargo test --workspace` and clippy. Expected: PASS.
- [ ] **Step 5: Commit** `feat(cli): overheat command, temperature and heat status`.

---

### Task 7: Acceptance doc, spec notes, owner hardware run

**Files:**
- Modify: `docs/testing/plan4-acceptance.md`, `docs/superpowers/specs/2026-11-16-eighty-design.md` (§2 overheat line, §5.3 open question → resolved with a link to the overheat design, and the plan-order note), `README.md` (the `overheat` command)

- [ ] **Step 1: Write the checklist** in `plan4-acceptance.md`. Each check states the exact command and the expected `status` output. Use `./target/release/eighty` and `sudo ./target/release/eighty install`.
  1. `status` shows `Temp xx.x °C`, and `doctor` shows the overheat line.
  2. Testing pause: on AC, charging below the limit, run `eighty overheat <current+1> <current-1> --testing`. Within about 2 min, `status` shows the `Paused` line, the battery shows `discharging` (native) or `not charging` (hold), and the log has `overheat: paused`.
  3. Resume: `eighty overheat 35`. Within one tick, `CHIE` is off, charging resumes and the testing warning is gone.
  4. At the limit and not charging: the testing pause causes no drain.
  5. Restart during a pause: `sudo launchctl kickstart -k system/io.github.mabujaber.eighty.helper` → the pause continues.
  6. Sleep during a pause: the lid closes (or `pmset sleepnow`), and on wake the pause is back.
  7. Lid closed with external displays during a testing pause: behaviour matches `HEAT_PAUSE_LID_CLOSED`.
  8. `eighty overheat off` during a pause → it ends at once.
  9. Uninstall during a pause → `CHIE` is 00, the Charge Limit is restored and `heat.toml` is gone.
- [ ] **Step 2: Update the spec notes and README.** Commit `docs: plan 4 acceptance checklist and spec notes`.
- [ ] **Step 3 (owner):** run the checklist on the M5 Pro, then the M2 Max. Record the results in the doc and commit them.
