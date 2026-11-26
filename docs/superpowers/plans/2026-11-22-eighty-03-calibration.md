# eighty Plan 3 — Calibration with Smart Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `eighty calibrate` runs a full battery cycle (15% → 100% → 1 h hold → back to the limit) inside the helper, resumes it after any interruption, gives up after 24 h without progress, and can run on an `every N days` schedule.

**Architecture:** The cycle is a pure state machine in `eighty-core::calibration` (`advance()`), unit-tested without hardware. `decide()` gains `Override::Calibrate(CalStep)` to pick the outputs and native limit for each step. The helper engine owns one `Option<Calibration>` (the only source of truth; `override_` stays `None` while calibrating), calls `advance()` each tick, persists it to `calibration.toml` next to `config.toml`, and starts scheduled runs from `config.calibration`. The CLI adds `eighty calibrate [cancel | schedule every <N> days [at HH:MM] | schedule off]` and status lines.

**Tech Stack:** Rust stable (edition 2021), existing workspace crates, `chrono` 0.4 (new: `default-features = false, features = ["std"]` in `eighty-core`; add `"clock"` in `eighty-helper` and `eighty-cli`), `serde`, `toml`, `clap` 4.5.

**Spec:** `docs/superpowers/specs/2026-11-22-eighty-calibration-design.md` (parent: `docs/superpowers/specs/2026-11-16-eighty-design.md`).

## Global Constraints

- Calibration low point `CAL_LOW = 15` (%). `HARD_FLOOR = 10` is untouched and has no calibration exception.
- Hold: `CAL_HOLD_SECS = 3600`, counted only while on AC; falling below `CAL_HOLD_MIN = 95` (%) in the hold step goes back to the charge step and resets the hold clock.
- Give up: `CAL_GIVE_UP_SECS = 86_400` without progress → cancel, restore the user's limit, delete state, log `calibration abandoned: <reason>`, show the reason in `status` warnings until the next mutating request. Doesn't set `last_calibrated`.
- Progress = step 1: charge went down; step 2: charge went up; step 3: hold clock advanced; any step change.
- Step 2 "charging really started": `CAL_KICK_SECS = 300`. On a settled reading, on AC, below 100%, not charging, and ≥ 300 s since the step began or the last kick → re-send the native limit (native) and log it.
- "Full" = 100% **and** `FullyCharged == Some(true)`; if `FullyCharged` is `None`: 100% **and** on AC **and** not charging.
- Native step 1 keeps macOS's Charge Limit at the **lowest available step below 100** (today 80); steps 2–3 at 100.
- No forced discharge with the lid closed or while asleep (existing rules apply unchanged).
- A fault pauses calibration in its step; in native mode the user's limit is put back on macOS's Charge Limit (best effort) while faulted.
- While calibrating: `charge full|auto` and `discharge <n>` are refused with exactly `a calibration is running; run \`eighty calibrate cancel\` first`; `limit` is accepted (applies at the end); `doctor --retry` keeps the calibration; Charge Limit changes in System Settings aren't adopted.
- Schedule: `every <N> days [at HH:MM]`, 7 ≤ N ≤ 90, default time `10:00`, local time. First run = N days after it's set; afterwards N days after `last_calibrated`; a due date already consumed by a scheduled start is skipped by N days. Never starts while one runs.
- Every calibration event is logged with the prefix `calibration: `.
- `sudo eighty uninstall` always deletes `/Library/Application Support/eighty/calibration.toml` (even without `--purge`).
- New `Status` fields are `#[serde(default)]` so an older CLI/helper pair still parses.

## Review Focus

1. **Helper restart in the middle of step 2 or 3** (reboot, crash, reinstall): resumes the saved step; downtime doesn't count toward the hold; the 24 h give-up clock survives. Pinned by `resumes_saved_step_after_restart` (Task 6).
2. **Unplugged during sleep in the hold step:** time asleep counts only when on AC at both ticks around it. Pinned by `hold_clock_counts_only_ac_to_ac` (Task 2).
3. **A corrupt or foreign `calibration.toml`:** logged, deleted, normal limit; never a crash loop. Pinned by `invalid_calibration_file_is_dropped` (Task 5).
4. **`eighty limit 85` or a System Settings change during step 2:** the Charge Limit stays 100 until the end, the new limit is saved and applies after finishing; nothing is adopted. Pinned by `limit_during_calibration_applies_after` (Task 6).
5. **Started on battery and left draining to 12%:** step 1 finishes at ≤ 15% and step 2 waits for the charger; no `CHIE` write at ≤ 10%. Pinned by `discharge_step_done_on_battery_below_low` (Task 2) and `calibrate_never_discharges_at_floor` (Task 3).

---

## File Structure

```
crates/eighty-hw/src/battery/mod.rs        + BatterySnapshot.fully_charged
crates/eighty-hw/src/battery/parse.rs      parse FullyCharged
crates/eighty-hw/src/battery/fake.rs       (field default)
crates/eighty-core/Cargo.toml              + chrono
crates/eighty-core/src/lib.rs              + pub mod calibration, schedule
crates/eighty-core/src/calibration.rs      NEW: CalStep, Calibration, CalInputs, CalOutcome, CalWait, advance()
crates/eighty-core/src/schedule.rs         NEW: CalibrationConfig, parse_at, next_due()
crates/eighty-core/src/config.rs           + Config.calibration
crates/eighty-core/src/policy.rs           + Override::Calibrate, Activity::Calibrating, Inputs.native_lowest
crates/eighty-core/src/proto.rs            + requests, CalibrationStatus, ScheduleStatus, Status fields
crates/eighty-core/src/paths.rs            + CALIBRATION_FILE
crates/eighty-helper/src/store.rs          + load/save/clear calibration
crates/eighty-helper/src/engine.rs         integration (Tasks 6–7)
crates/eighty-cli/src/main.rs              calibrate command
crates/eighty-cli/src/schedule_args.rs     NEW: parse `schedule …` words
crates/eighty-cli/src/render.rs            calibration lines
crates/eighty-cli/src/install.rs           uninstall deletes CALIBRATION_FILE
docs/testing/plan3-acceptance.md           NEW: hardware checklist
docs/superpowers/specs/2026-11-16-eighty-design.md   scope note
README.md                                  calibrate commands
```

---

### Task 1: `FullyCharged` in the battery snapshot

**Files:**
- Modify: `crates/eighty-hw/src/battery/mod.rs` (struct), `crates/eighty-hw/src/battery/parse.rs`, every `BatterySnapshot { … }` literal (fake, engine tests' `Rig::with`, probe if any — `cargo build --workspace --tests` lists them)

**Interfaces:**
- Produces: `BatterySnapshot.fully_charged: Option<bool>` (top-level `FullyCharged` on `AppleSmartBattery`, `None` if absent).

- [x] **Step 1: Write failing tests** in `parse.rs` tests: `fully_charged_is_read` (props with `FullyCharged = Bool(true)` → `Some(true)`) and `fully_charged_missing_is_none`.
- [x] **Step 2:** `cargo test -p eighty-hw fully_charged` → FAIL (no field).
- [x] **Step 3:** Add the field (doc comment: "macOS sets this at the end of a charge; 100% is shown earlier") and `fully_charged: boolean(props, "FullyCharged").ok()`. Fill `fully_charged: None` in every other literal.
- [x] **Step 4:** `cargo test --workspace` → PASS.
- [x] **Step 5:** Commit `feat(hw): read FullyCharged`.

### Task 2: Calibration state machine (`eighty-core::calibration`)

**Files:**
- Create: `crates/eighty-core/src/calibration.rs`; Modify: `crates/eighty-core/src/lib.rs`

**Interfaces:**
- Produces:
```rust
pub const CAL_LOW: u8 = 15;
pub const CAL_HOLD_MIN: u8 = 95;
pub const CAL_HOLD_SECS: u64 = 3600;
pub const CAL_GIVE_UP_SECS: u64 = 86_400;
pub const CAL_KICK_SECS: u64 = 300;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalStep { Discharge, Charge, Hold }
impl CalStep { pub fn number(self) -> u8 } // 1, 2, 3 (of 4)

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalWait { PlugIn, LidClosed, Fault }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Calibration {
    pub step: CalStep,
    pub started_at: u64,
    pub scheduled: bool,
    pub step_started_at: u64,
    pub held_secs: u64,
    pub last_progress_at: u64,
    /// Lowest charge seen in Discharge, highest in Charge: progress is beating it.
    pub best_percent: u8,
    /// Previous tick: (time, on AC). `None` after a load, so downtime never counts.
    pub last_tick: Option<(u64, bool)>,
    pub last_kick_at: Option<u64>,
}
impl Calibration { pub fn new(now: u64, percent: u8, scheduled: bool) -> Calibration }

pub struct CalInputs { pub now: u64, pub percent: u8, pub external: bool, pub charging: bool,
    pub fully_charged: Option<bool>, pub lid_closed: bool, pub settled: bool, pub faulted: bool }

#[derive(Debug, PartialEq)]
pub enum CalOutcome { Continue, StepChanged(CalStep), KickCharging, Finished, Abandoned(String) }

pub fn advance(c: &mut Calibration, i: &CalInputs) -> CalOutcome;
pub fn waiting(c: &Calibration, i: &CalInputs) -> Option<CalWait>;
```

`advance` order: (1) hold clock: in `Hold`, if `last_tick == Some((t, true))`, `i.external` and not faulted, add `now - t`; then `last_tick = Some((now, external))`. (2) If faulted: only the give-up check. (3) Step logic: Discharge → `Charge` at `percent <= CAL_LOW`; Charge → `Hold` when full (Global Constraints), else `KickCharging` per `CAL_KICK_SECS` (sets `last_kick_at`); Hold → `Charge` if `percent < CAL_HOLD_MIN` (reset `held_secs`), `Finished` at `held_secs >= CAL_HOLD_SECS`. Every step change sets `step_started_at`, `best_percent = percent`, `last_progress_at = now`. (4) Give-up: `now - last_progress_at >= CAL_GIVE_UP_SECS` → `Abandoned(format!("no progress in step {}/4 for 24 h{}", n, wait))` where `wait` is `": waiting for the charger"`, `": lid closed"`, `": eighty is in a fault"` or empty. `waiting`: Fault if faulted; PlugIn if `!external` and step is Charge/Hold; LidClosed if `lid_closed && external` and step is Discharge; else None.

- [x] **Step 1: Write failing tests** (one per row; `CalInputs` built by a test helper `at(now, pct, ext)` with `settled: true`, others false/None):
  - `discharge_progresses_and_finishes_at_low`: new at 60 → 50 gives `Continue` and `last_progress_at` updated → 15 gives `StepChanged(Charge)`.
  - `discharge_step_done_on_battery_below_low`: at 12% on battery → `StepChanged(Charge)`; next tick on battery → `waiting == Some(PlugIn)`.
  - `charge_needs_fully_charged`: 100% with `fully_charged: Some(false)` → `Continue`; `Some(true)` → `StepChanged(Hold)`; `None` + on AC + not charging → `StepChanged(Hold)`; `None` + charging → `Continue`.
  - `charge_kicks_when_not_charging`: Charge at 40% on AC, not charging, settled, 301 s after step start → `KickCharging`; same again 100 s later → `Continue`; not settled → `Continue`.
  - `hold_clock_counts_only_ac_to_ac`: ticks (t0, AC), (t0+1800, battery), (t0+3600, AC) → `held_secs == 0`; then (t0+5400, AC) → 1800.
  - `hold_drop_goes_back_to_charge`: Hold with `held_secs = 1200`, 94% → `StepChanged(Charge)`, `held_secs == 0`.
  - `hold_finishes_after_an_hour_on_ac`: 61 ticks of 60 s on AC at 100% → last outcome `Finished`.
  - `gives_up_after_24h_without_progress`: Charge, on battery, `now = last_progress_at + 86_400` → `Abandoned("no progress in step 2/4 for 24 h: waiting for the charger")`.
  - `fault_only_checks_give_up`: faulted at 10% in Discharge → `Continue` (no step change), and at +86_400 → `Abandoned(... ": eighty is in a fault")`.
  - `calibration_roundtrips_toml`: `toml::to_string` → `from_str` equal.
- [x] **Step 2:** `cargo test -p eighty-core calibration` → FAIL (module missing).
- [x] **Step 3:** Implement `crates/eighty-core/src/calibration.rs` as specified; `pub mod calibration;` in `lib.rs`.
- [x] **Step 4:** `cargo test -p eighty-core calibration` → PASS.
- [x] **Step 5:** Commit `feat(core): calibration state machine`.

### Task 3: Policy outputs for calibration steps

**Files:**
- Modify: `crates/eighty-core/src/policy.rs` (+ every `Inputs { … }` literal in helper)

**Interfaces:**
- Consumes: `CalStep`, `CAL_LOW` (Task 2).
- Produces: `Override::Calibrate(CalStep)`; `Activity::Calibrating { step: CalStep }`; `Inputs.native_lowest: u8` (lowest native step below 100; callers pass 80 when unknown).

`decide` for `Override::Calibrate`: never sets `clear_override`. **Discharge:** exactly today's `DischargeTo(CAL_LOW)` outputs (lid closed → hold/LidClosed, floor guard, else discharge) but `native_limit: nl(i.native_lowest)` and activity `Calibrating { step }` whenever on AC with the lid open, `OnBattery` off AC, `LidClosed` with the lid closed on AC. **Charge/Hold:** same outputs as `FullCharge` (`native_limit: nl(100)`), activity `Calibrating { step }`, and not cleared by unplugging.

- [x] **Step 1: Write failing tests:**
  - `calibrate_discharge_step_native_limit_is_lowest`: Native, 60%, AC, lid open, `native_lowest: 80` → `outputs.discharge == true`, `native_limit == Some(80)`, `activity == Calibrating{Discharge}`, `!clear_override`.
  - `calibrate_discharge_lid_closed_holds`: Hold mode, 60%, AC, lid closed → `discharge == false`, `inhibit == true`, `activity == LidClosed`.
  - `calibrate_never_discharges_at_floor`: Discharge step at 10% on AC → `discharge == false`.
  - `calibrate_charge_and_hold_lift_to_100`: Native, Charge and Hold, on battery → `native_limit == Some(100)`, `!clear_override`; Hold mode → `inhibit == false`, `light == Some(Light::System)`.
- [x] **Step 2:** `cargo test -p eighty-core calibrate_` → FAIL.
- [x] **Step 3:** Implement; add `native_lowest` to `Inputs` and fix all literals (`native_lowest: 80` in existing tests).
- [x] **Step 4:** `cargo test --workspace` → PASS (existing policy/engine tests unchanged).
- [x] **Step 5:** Commit `feat(core): policy for calibration steps`.

### Task 4: Schedule config and next due date (`eighty-core::schedule`)

**Files:**
- Create: `crates/eighty-core/src/schedule.rs`; Modify: `crates/eighty-core/src/config.rs`, `lib.rs`, `crates/eighty-core/Cargo.toml` (chrono), `crates/eighty-helper/src/store.rs` (validate on load)

**Interfaces:**
- Produces:
```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CalibrationConfig {
    pub every_days: Option<u16>,
    pub at: Option<String>,             // "HH:MM"; None = 10:00
    pub set_at: Option<u64>,            // when the schedule was set (Unix s)
    pub last_calibrated: Option<u64>,   // when a calibration last finished
    pub last_scheduled_due: Option<u64>,// the due time a scheduled start consumed
}
impl CalibrationConfig { pub fn validate(&self) -> Result<(), String> }
pub fn parse_at(s: &str) -> Result<NaiveTime, String>;
pub fn next_due(c: &CalibrationConfig, to_local: impl Fn(u64) -> NaiveDateTime,
                from_local: impl Fn(NaiveDateTime) -> u64) -> Option<u64>;
```
`Config` gains `#[serde(default)] pub calibration: CalibrationConfig`. `Store::load` also runs `c.calibration.validate()` (an invalid schedule → defaults + warning, as for settings).

`next_due`: `None` if `every_days` is `None`. Anchor = `last_calibrated.or(set_at)?`; due = `from_local(to_local(anchor).date() + N days, at)`; while `last_scheduled_due.is_some_and(|d| due <= d)`, add N days (local). Messages: `"calibration schedule must be every 7 to 90 days (got {n})"`, `"time must be HH:MM, e.g. 10:00 (got {s})"`.

- [x] **Step 1: Write failing tests** (with UTC closures for determinism):
  - `first_due_is_n_days_after_set`: set 2026-11-22 15:00, every 30 at default → 2026-12-22 10:00.
  - `after_a_calibration_counts_from_it`: last_calibrated 2026-12-01 18:00 → 2026-12-31 10:00.
  - `consumed_due_is_skipped`: due 2026-12-22 10:00 with `last_scheduled_due` = that → 2027-01-21 10:00.
  - `off_means_no_due`: `every_days: None` → `None`.
  - `validate_ranges`: 6 and 91 rejected, 7 and 90 accepted; `at: "25:00"` rejected with the exact message.
  - In `store.rs`: `invalid_schedule_falls_back_with_warning` (`[calibration]\nevery_days = 3\n`).
- [x] **Step 2:** `cargo test -p eighty-core schedule` → FAIL.
- [x] **Step 3:** Implement; add `chrono = { version = "0.4", default-features = false, features = ["std"] }` to `eighty-core`.
- [x] **Step 4:** `cargo test --workspace` → PASS.
- [x] **Step 5:** Commit `feat(core): calibration schedule`.

### Task 5: Protocol, paths and the saved calibration file

**Files:**
- Modify: `crates/eighty-core/src/proto.rs`, `crates/eighty-core/src/paths.rs`, `crates/eighty-helper/src/store.rs`

**Interfaces:**
- Produces:
```rust
// proto.rs
Request::Calibrate, Request::CalibrateCancel,
Request::SetCalibrationSchedule { every_days: Option<u16>, at: Option<String> } // every_days None = off
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CalibrationStatus { pub step: CalStep, pub held_secs: u64, pub waiting: Option<CalWait>,
    pub scheduled: bool, pub started_at: u64 }
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ScheduleStatus { pub every_days: Option<u16>, pub at: String,
    pub next_due: Option<u64>, pub last_calibrated: Option<u64>, pub waiting: Option<String> }
// Status gains (all #[serde(default)]):
pub calibration: Option<CalibrationStatus>, pub schedule: Option<ScheduleStatus>,
// paths.rs
pub const CALIBRATION_FILE: &str = "/Library/Application Support/eighty/calibration.toml";
// store.rs
impl Store {
    pub fn load_calibration(&self) -> (Option<Calibration>, Option<String>); // missing: (None, None); invalid: deletes it, (None, Some(warning))
    pub fn save_calibration(&self, c: &Calibration) -> io::Result<()>;      // atomic, like save()
    pub fn clear_calibration(&self);                                       // ignore NotFound
}
```
- [x] **Step 1: Write failing tests:** `old_status_json_without_calibration_parses` (a `Status` JSON lacking the new fields deserializes, both `None`); `calibration_requests_are_mutating`; store: `calibration_save_load_clear`, `invalid_calibration_file_is_dropped` (garbage → `(None, Some(w))`, `w` contains `calibration.toml`, file gone).
- [x] **Step 2:** `cargo test --workspace calibration` → FAIL.
- [x] **Step 3:** Implement. Extract the atomic write in `store.rs` into one private `write_atomic(&self, name, text)` used by both saves. Warning text: `"ignoring {path}: {reason}; calibration dropped"`.
- [x] **Step 4:** `cargo test --workspace` → PASS.
- [x] **Step 5:** Commit `feat: calibration protocol and saved state`.

### Task 6: Engine — start, cancel, refusals, resume

**Files:**
- Modify: `crates/eighty-helper/src/engine.rs` (fields, `start`, `request`, `native_wanted`, `check_native_limit`, `status`, `tick`'s `decide` call)

**Interfaces:**
- Consumes: Tasks 2–5.
- Produces (private): fields `calibration: Option<Calibration>`, `cal_wait: Option<CalWait>`, `cal_saved_at: u64`, `cal_notice: Option<String>`; `fn start_calibration(&mut self, scheduled: bool) -> Result<(), String>`; `fn end_calibration(&mut self, why: &str)` (clear field, `store.clear_calibration()`, log `calibration: {why}`); `fn native_lowest(&self) -> u8` (min of `detection.native_steps` below 100, else 80).

Behaviour:
- `start`: after `store.load()`, `store.load_calibration()`; a warning is logged; a loaded one gets `last_tick = None` and logs `calibration: resuming step {n}/4`.
- `start_calibration` refusals, in order, exact texts: Unsupported → `Health::Unsupported.message()`; health not Ok → `self.health.message()`; native and `!control.can_discharge()` → `"this Mac can't run from the battery while plugged in, so calibration isn't available"`; already running → `"a calibration is already running (step {n}/4)"`. On success: `override_ = None`, `clear_native_candidate()`, save the file, log `calibration: started (step 1/4, discharging to 15%)` (`scheduled` adds `, scheduled`).
- `CalibrateCancel` with none running → `Error("no calibration is running")`; else `end_calibration("cancelled")`, tick.
- `ChargeFull`, `ChargeAuto`, `DischargeTo` while calibrating → `Error("a calibration is running; run `eighty calibrate cancel` first")`.
- Any mutating request clears `cal_notice`.
- `tick` passes `ov = calibration.map(|c| Override::Calibrate(c.step)).unwrap_or(override_)` and `native_lowest` to `decide`.
- `native_wanted`: Discharge → `native_lowest()`; Charge/Hold → 100. `check_native_limit`: no adoption while calibrating (same early return as `FullCharge`).
- `status()`: `calibration` from the field + `cal_wait`; `cal_notice` appended to `warnings`.

- [x] **Step 1: Write failing tests** (existing `Rig`/`req` helpers):
  - `calibrate_starts_and_discharges_native`: native rig, 60% on AC → `Calibrate` → `CHIE == [8]`, native limit 80, `status().calibration.step == Discharge`, file exists.
  - `calibrate_refusals`: unsupported rig; native rig whose CHIE is blocked (`native_blocked_chie()`); second `Calibrate` → exact texts above.
  - `calibrate_allowed_on_battery`: 60% on battery → `Status`, `CHIE == [0]`.
  - `overrides_refused_while_calibrating`: `ChargeFull`, `ChargeAuto`, `DischargeTo{50}` → exact error.
  - `cancel_restores_limit_and_deletes_state`: after start, `CalibrateCancel` → `CHIE == [0]`, native limit back to 80 (user's), file gone; second cancel → `"no calibration is running"`.
  - `resumes_saved_step_after_restart`: start, force step Charge via saved file, build a new engine from the same rig → `status().calibration.step == Charge`, native limit 100.
  - `limit_during_calibration_applies_after`: during Charge, `SetLimits{85}` → saved `upper == 85`, native limit still 100; a System Settings read of 90 for two ticks isn't adopted.
  - `retry_keeps_calibration`: during Discharge, `ClearFault` → still calibrating.
- [x] **Step 2:** `cargo test -p eighty-helper calibrat` → FAIL.
- [x] **Step 3:** Implement.
- [x] **Step 4:** `cargo test --workspace` → PASS.
- [x] **Step 5:** Commit `feat(helper): start, cancel and resume calibration`.

### Task 7: Engine — advancing, finishing, giving up, faults, schedule

**Files:**
- Modify: `crates/eighty-helper/src/engine.rs` (`tick`, new `fn advance_calibration(&mut self, snap: Option<&BatterySnapshot>, faulted: bool)`, `fn maybe_start_scheduled(&mut self)`), `crates/eighty-helper/Cargo.toml` (`chrono` with `clock`)

**Interfaces:**
- Consumes: Tasks 2–6.

Behaviour:
- `advance_calibration` builds `CalInputs` (`settled = !reading_stale() && (mode != Native || native_reading_settled(snap))`, `lid_closed` from `system`), calls `advance`, stores `cal_wait = waiting(..)`, then: `StepChanged(s)` → log `calibration: step {n}/4: {discharging to 15% | charging to 100% | holding at 100% for 1 h}`, save; `KickCharging` → log `calibration: charging hasn't started; asking macOS again`, `native_written = None`; `Finished` → `config.calibration.last_calibrated = Some(now)` (save config), `end_calibration("finished; back to the {upper}% limit")`; `Abandoned(r)` → `cal_notice = Some(format!("calibration abandoned: {r}"))`, `end_calibration(&format!("abandoned: {r}"))`; `Continue` → save if `now - cal_saved_at >= 300`.
- Called in `tick` (a) with `faulted: false` just before `decide` on the normal path; (b) with `faulted: true` at the top of the `!may_control()` branch, in the battery-error branch (no snapshot: give-up check only), and when `native_prepare`/`write_native` fail. In (b), native mode with a key fault (not `NativeUnavailable`): if `native_written != Some(settings().upper)`, call `write_native(settings().upper)` once, ignoring errors.
- `maybe_start_scheduled` (each tick, before advancing): if no calibration and `next_due(&config.calibration, local, local_inv)` ≤ now → `start_calibration(true)`; on success set `last_scheduled_due = Some(due)` and save config; on error keep `schedule waiting` text (`"scheduled calibration waiting: {err}"`) for `ScheduleStatus.waiting`. Local conversion via `chrono::Local` in the helper, passed as closures.
- `SetCalibrationSchedule`: validate (Task 4); `Some(n)` → `every_days = Some(n)`, `at`, `set_at = Some(now)`, `last_scheduled_due = None`; `None` → `every_days = None`. Save config. Log `calibration: schedule every {n} days at {at}` / `calibration: schedule off`.
- `status().schedule`: `Some` when `every_days` or `last_calibrated` is set.

- [x] **Step 1: Write failing tests** (fake clock):
  - `full_cycle_native`: start at 20% on AC; set 15% → step Charge, limit 100; set 100% + `fully_charged: Some(true)` → Hold; 61 ticks → finished: limit back to 80, file gone, `last_calibrated` saved.
  - `kick_resends_native_limit`: Charge, 40%, AC, not charging, settled, +301 s → FakeNative records a second `set_limit(100)`.
  - `abandons_after_24h`: Charge on battery, clock +86_400 → `calibration == None`, warnings contain `"calibration abandoned: no progress in step 2/4 for 24 h: waiting for the charger"`, limit 80; next `SetLimits` clears the warning.
  - `fault_puts_user_limit_back`: native, Charge step (limit 100), inject a CHIE conflict → native limit 80, still calibrating; `ClearFault` → back to 100.
  - `schedule_fires_once`: `SetCalibrationSchedule{Some(7), None}`; clock +7 days → started with `scheduled: true`, `last_scheduled_due` saved; cancel, next tick → not restarted.
  - `scheduled_start_waits_when_blocked`: due while health is a Conflict → not started, `status().schedule.waiting` starts with `"scheduled calibration waiting:"`.
- [x] **Step 2:** `cargo test -p eighty-helper calibrat` → FAIL.
- [x] **Step 3:** Implement.
- [x] **Step 4:** `cargo test --workspace` → PASS; `cargo clippy --workspace --all-targets -- -D warnings` → clean.
- [x] **Step 5:** Commit `feat(helper): run, finish, abandon and schedule calibrations`.

### Task 8: CLI — command, status lines, uninstall

**Files:**
- Create: `crates/eighty-cli/src/schedule_args.rs`; Modify: `crates/eighty-cli/src/main.rs`, `lib.rs`, `render.rs`, `install.rs`, `crates/eighty-cli/Cargo.toml` (`chrono` with `clock`)

**Interfaces:**
- Produces: `pub fn parse_schedule(words: &[String]) -> Result<Request, String>` accepting `["off"]` and `["every", N, "days"]` / `[…, "at", "HH:MM"]`. Error: ``usage: eighty calibrate schedule every <N> days [at HH:MM] | off``.
- Clap: `Calibrate { #[command(subcommand)] action: Option<CalAction> }`, `CalAction::Cancel`, `CalAction::Schedule { #[arg(trailing_var_arg = true, required = true)] words: Vec<String> }`. No action → `Request::Calibrate`. Help text: "Run a full calibration cycle now (15% → 100% → 1 h → limit)".

Render (local time via chrono, `YYYY-MM-DD HH:MM`):
```
State     Calibrating: step 1/4, discharging to 15% (now 42%)
State     Calibrating: step 2/4, charging to 100% (now 63%)
State     Calibrating: step 3/4, holding at 100% (35 of 60 min)
State     Calibrating: step 2/4, paused: plug in to continue      (waiting PlugIn)
          … paused: lid closed, macOS may charge up to 80% meanwhile   (LidClosed, native; "lid closed" in hold mode)
          … paused: eighty isn't limiting (see above)                (Fault)
Calibrate Last 2026-11-22 · next 2026-12-22 10:00 (every 30 days)
Calibrate next 2026-12-22 10:00 (every 30 days) · scheduled calibration waiting: …
```
`--json` needs nothing new (fields are in `Status`).

- [x] **Step 1: Write failing tests:** `parse_schedule_forms` (`every 30 days`, `every 30 days at 21:30`, `off`, and `every 30` → usage error); render: `renders_calibration_steps` (the three step lines and the PlugIn pause), `renders_schedule_line`; install: `uninstall_paths_include_calibration_file` (make the list of always-removed files a `const` slice and assert it contains `CALIBRATION_FILE`).
- [x] **Step 2:** `cargo test -p eighty-cli` → FAIL.
- [x] **Step 3:** Implement; `uninstall` removes `CALIBRATION_FILE` alongside the plist and socket.
- [x] **Step 4:** `cargo test --workspace` → PASS.
- [x] **Step 5:** Commit `feat(cli): eighty calibrate`.

### Task 9: Docs and hardware acceptance checklist

**Files:**
- Create: `docs/testing/plan3-acceptance.md`; Modify: `README.md`, `docs/superpowers/specs/2026-11-16-eighty-design.md`, this plan (tick boxes)

- [x] **Step 1:** Parent spec: under §2 add a dated note (2026-11-22): Zones dropped; sleep dark wakes dropped for native mode (link the acceptance note); plan order now calibration → overheat → clamshell levels → Homebrew → menu-bar app; link the calibration design. Update the "Scope split" bullets in `docs/superpowers/plans/2026-11-17-eighty-02-helper-cli.md` header with the same order.
- [x] **Step 2:** README: `eighty calibrate`, `cancel`, `schedule every <N> days [at HH:MM]`, `schedule off`, one line on what a calibration does and that it takes several hours on the charger with the lid open.
- [x] **Step 3:** `docs/testing/plan3-acceptance.md` — owner-run, native mode, each Mac: (1) `sudo eighty install` of this build; (2) `eighty calibrate` at ≥ 40% on AC, lid open → `CHIE=08`, Charge Limit 80, status step 1; (3) unplug for 5 min during step 1 → keeps going; (4) `sudo reboot` during step 1 or 2 → resumes the same step (log `calibration: resuming`); (5) at 15% → step 2, Charge Limit 100, charging within 5 min; record when `FullyCharged` flips vs 100% shown (`ioreg -rn AppleSmartBattery | grep FullyCharged`); (6) unplug in step 3 → paused, hold clock stops; (7) finish → limit back to 80, macOS discharges to it, `Last calibrated` shown; (8) second run: `cancel` mid-step 1 → `CHIE=00`, limit 80; (9) `schedule every 7 days` → next date shown 7 days out; `schedule off`; (10) `sudo eighty uninstall` during a run → charging restored, `calibration.toml` gone. Each row: Check | Result | Evidence (log lines, times UTC).
- [x] **Step 4:** `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → PASS/clean.
- [x] **Step 5:** Commit `docs: calibration commands, acceptance checklist, scope note`.

### Task 10: Hardware acceptance (owner-driven)

- [ ] Run `docs/testing/plan3-acceptance.md` on the M5 Pro, then the M2 Max; record results and any fixes there. If `FullyCharged` never flips, switch step 2's rule to the `None` fallback (100% + AC + not charging) and note it in the spec §7.6.
