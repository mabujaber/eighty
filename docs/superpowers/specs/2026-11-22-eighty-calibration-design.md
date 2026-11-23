# eighty — Calibration with smart resume (Plan 3)

Date: 2026-11-22
Parent spec: `docs/superpowers/specs/2026-11-16-eighty-design.md` (§2 goal 3: "Calibration" and "Smart resume calibration")

## 1. Intent

eighty's goal is every feature of BattOpt, free (parent spec §1–2). BattOpt sells **calibration with smart resume** as a Pro feature. This plan adds it.

A calibration runs the battery through one full cycle: down to a low charge, up to 100%, a hold at full, then back to the limit. It helps the battery gauge relearn its capacity, which drifts on a Mac that sits at 80% for months. *Context:* the owner's M5 Pro (21 cycles, 102% health, never calibrated) doesn't need one today, so this is about the feature being there for eighty's users, not fixing a problem the owner has.

**Smart resume** means a calibration that gets interrupted (unplugged, lid closed, sleep, reboot, helper crash) continues from the step it reached. It doesn't start over, and it isn't silently dropped.

## 2. Plan order (owner's decisions, 2026-11-22)

1. **This plan: calibration + smart resume.**
2. Overheat protection, plus battery temperature in `status`/`doctor`.
3. Clamshell levels (BattOpt Pro parity): discharge and calibrate with the lid closed by keeping the Mac awake (level 1), optionally with the external display on (level 2).
4. Homebrew release (old Plan 5: signing, notarization, CI, tap).
5. Menu-bar app (old Plan 3: Eighty.app, `SMAppService`, notifications).

Dropped from scope:
- **Zones** (Wi-Fi profiles). Dropped by the owner.
- **Sleep dark wakes.** Native mode uses macOS's own Charge Limit, which macOS enforces while asleep. The overnight runs on both Macs held 80% through maintenance wakes. Charging *up to* the limit while asleep wasn't tested separately; the owner accepted it as covered by macOS (`docs/testing/plan2b-acceptance.md`, last section).
- **Clamshell sleep checks.** Done for holding the limit. The only open check is native mode with external displays, which needs no code. BattOpt's lid-closed discharge/calibration is item 3 above.

The parent spec gets a short note recording these changes (§2 and the Plan 2 scope note).

## 3. Goals

1. `eighty calibrate` starts a calibration now.
2. `eighty calibrate schedule …` runs one automatically on a schedule; `eighty calibrate schedule off` turns it off.
3. `eighty calibrate cancel` stops one and returns to the normal limit.
4. Smart resume across unplug, lid close, sleep, reboot, helper crash and helper reinstall.
5. `eighty status` shows the step, the progress and what it's waiting for. `--json` carries the same.
6. Works in both control modes: **native** (macOS 27, both of the owner's Macs today) and **hold** (`CHTE`, macOS 26).

## 4. Non-goals

- Triggering or controlling macOS's own calibration. There's no API. eighty keeps reporting it as today ("macOS is charging past 80% …").
- Notifications. Those come with the menu-bar app. Until then, `status` and the log are the only signals.
- Measuring the result (e.g. capacity before/after). Nice to have later; not needed for parity.

## 5. Anti-goals

- **A calibration must never leave the Mac stuck.** Cancel, uninstall, a fault, or a crash during any step ends with the normal limit and no `CHIE` discharge left on, exactly like today's restore rules.
- **Never below the hard floor.** The calibration low point (15%) is above `HARD_FLOOR` (10%), so the existing floor guard stays untouched and has no calibration exception.
- **No silent stall.** If a step can't make progress (e.g. it needs the charger and the Mac is unplugged), `status` says what it's waiting for.

## 6. Approaches (owner picked A, 2026-11-22)

- **A. Calibration inside the helper (chosen).** A new override that steps through the cycle by itself, with its step saved to disk so it resumes after any interruption, and the schedule kept by the helper. Most work, but it's the only option where smart resume survives a reboot, and it reuses the tested safety rules (lid, sleep, floor, crash recovery).
- B. A CLI script chaining `discharge 15` → `charge full` → wait → `charge auto`. Quick, but dies with the terminal or a reboot: no smart resume and no schedule, which drops the BattOpt feature this exists for.
- C. Trigger macOS's own calibration. No API, and it runs when macOS decides.

## 7. Design

### 7.1 The cycle

| Step | What eighty does | Done when |
|---|---|---|
| 1. Discharge | Runs the Mac from the battery down to **15%**. On AC: `CHIE = 08` (both modes; hold mode also keeps `CHTE` inhibit, as `discharge <n>` does today). On battery: nothing to do, the battery is draining anyway, and progress counts. | Charge ≤ 15% |
| 2. Charge | Lets the Mac charge to full. Native: Charge Limit → 100. Hold: inhibit off, light left to macOS (as `charge full` today). | Charge = 100% **and** macOS reports it full (see 7.6) |
| 3. Hold | Stays at full on the charger for **1 hour**. Native: Charge Limit stays 100. | 60 minutes of time on AC since step 3 began (time on battery doesn't count) |
| 4. Return | Puts the user's limit back. Native: macOS discharges down to it by itself. Hold: auto-discharge (if on) brings it down, as today. | Immediately: the calibration is recorded as finished when the limit is restored. |

On finish, eighty records `last_calibrated` (date) and, if a schedule is set, the next due date.

### 7.2 Interruptions (smart resume)

The current step and its progress (step, step start time, hold time accumulated) are saved to disk at every step change and every few minutes during step 3. On startup the helper loads it and carries on.

| Event | Step 1 (discharge) | Step 2 (charge) | Step 3 (hold) |
|---|---|---|---|
| Unplugged | Keeps going: the battery drains anyway | Pauses, "plug in to continue" | Pauses the hold clock, "plug in to continue" |
| Lid closed on AC | Pauses discharging (eighty never forces discharge with the lid closed); hold mode inhibits, native mode can't stop macOS charging toward its limit (see 7.3) | Continues | Continues |
| Asleep | `CHIE` off while asleep (existing rule), resumes on wake | Continues | Continues (time on AC while asleep counts) |
| Reboot / crash / reinstall | Resumes the saved step at startup, after the usual startup recovery | same | same |

A reboot or shutdown in steps 2–3 leaves macOS's Charge Limit at 100% while eighty is down: the helper tries to put the user's limit back when it stops, but macOS has already closed PowerUI by then (seen in acceptance check 4: "Couldn't communicate with a helper application"). The helper sets the right limit again when it resumes after boot. Restoring it from macOS's shutdown notice instead is possible but untested.

*Claude's calls (from the builder check):*
- **Step 3 after the charge drops.** If the charge falls below 95% during step 3 (e.g. unplugged and used), the calibration goes back to step 2 and the hold clock resets. "Hold at full" means at full.
- **Sleep in step 3.** Time asleep counts toward the hold only if the Mac was on AC both when it went to sleep and when it woke. The helper can't see what happened in between.

### 7.3 Native-mode gap during a paused step 1

In native mode nothing stops charging except macOS's Charge Limit. While step 1 is paused on AC (lid closed or asleep), macOS charges back toward the Charge Limit, and the discharge loses ground. *Claude's call:* during step 1, keep the Charge Limit at the **lowest step macOS offers** (today 80%), whatever the user's limit is, so a pause costs at most the way back up to 80. The step resumes from wherever the charge is. `status` shows "paused: lid closed, macOS may charge up to 80% meanwhile". No extra machinery.

### 7.4 What blocks a calibration

`eighty calibrate` is refused, with a clear message, when:
- the mode is Unsupported, or health isn't Ok (same rule as `discharge`);
- native mode can't discharge (`can_discharge()` false), since step 1 needs `CHIE`;
- a calibration is already running (message shows the current step).

It's **allowed on battery**: step 1 simply proceeds by draining. *Claude's call*, since requiring the charger to start a step that doesn't need it adds friction.

While a calibration runs:
- `charge full|auto` and `discharge <n>` are refused: "a calibration is running; `eighty calibrate cancel` first".
- `limit …` is accepted and saved. It takes effect at step 4.
- `doctor --retry` (`ClearFault`) clears a fault but keeps the calibration; it resumes once health is Ok.
- A fault (Conflict, WriteFailed, …) pauses the calibration in its step. It doesn't cancel it, and it isn't lost. `status` shows both the fault and "calibration paused". *Claude's call:* while paused by a fault, native mode puts the **user's limit** back on macOS's Charge Limit (a PowerUI call, separate from the SMC keys the fault concerns), so a fault never leaves the Mac uncapped at 100%. Resuming steps 2–3 sets 100 again. This needs a small new path: today's fault handling never touches the native limit. Time in a fault doesn't count toward the hold clock.
- Not adopted during a calibration: a Charge Limit changed in System Settings (as with `charge full` today). `doctor --retry` keeps the calibration (today it drops any override).
- `sudo eighty uninstall` cancels it and deletes the saved state before the normal restore.

### 7.4a Giving up (owner's decision, 2026-11-22)

If a calibration makes **no progress for 24 hours**, eighty abandons it: it cancels the calibration, puts the user's limit back, deletes the saved state, and logs `calibration abandoned: <reason>` (e.g. "no progress in step 2 for 24 h: still waiting for the charger"). `status` shows the abandon message until the next command. An abandoned run doesn't set `last_calibrated`, so a schedule tries again on its next due date.

Progress means: step 1, the charge went down; step 2, the charge went up or the step finished; step 3, the hold clock advanced. The saved state keeps the time of the last progress, so the 24 hours survive a reboot. Why: steps 2–3 lift macOS's Charge Limit to 100%, and a stalled run would otherwise leave the Mac uncapped for days, which is what eighty exists to prevent.

### 7.5 Schedule

*Claude's call:* `eighty calibrate schedule every <N> days [at HH:MM]`, default off, N between 7 and 90, the time defaulting to 10:00. When it's due, the helper starts the calibration at the first tick at or after that time. If the Mac is asleep or off then, it starts at the next tick after it's awake. `eighty calibrate schedule off` clears it. `status` shows `Next calibration: 2026-12-22 10:00`. The next due date counts from when the last calibration **finished**, so a late start doesn't bunch two together. Saved in `config.toml`; a missed schedule doesn't stack up (at most one pending).

**First run and changes (owner's decision, 2026-11-22).** The first automatic run is **N days after the schedule is set**, at the chosen time, never right away. `status` shows that date at once. Changing N later counts from `last_calibrated`, or from when the schedule was set if there's no calibration yet. So `config.toml` keeps `last_calibrated` plus the schedule's set date. An on-demand `eighty calibrate` counts as a calibration: the next scheduled one is N days after it finishes. A due run doesn't start while one is already running.

A scheduled start that hits a blocker (7.4) is retried each tick, and `status` says why it's waiting.

### 7.6 Detecting "full"

macOS shows 100% before the battery is truly full. Step 2 ends only when the charge reads 100% **and** `BatteryData.FullyCharged = 1` (seen on the M5: `"FullyCharged"=0` at 80%), or, if that field is missing, when the charge reads 100% and the battery isn't charging (`IsCharging = No`, amperage ≤ 0) on AC. `BatterySnapshot` gains an optional `fully_charged`. The builder confirms on hardware that `FullyCharged` flips at the end of a charge.

### 7.7 Where it lives

- **`eighty-core`:** `Override::Calibrate` (or a separate calibration state next to the override — builder's call), the cycle logic in the pure policy (`decide`) so it's unit-testable without hardware, `Activity::Calibrating { step, … }`, new `Request`s (`Calibrate`, `CalibrateCancel`, `SetCalibrationSchedule`), and `Status` fields for the calibration and the schedule.
- **`eighty-helper`:** saves/loads the calibration state, tracks AC time for step 3, starts scheduled runs, persists `last_calibrated`.
- **Saved state:** a separate file next to the config (e.g. `/Library/Application Support/eighty/calibration.toml`), so the user's `config.toml` stays settings only. It's written atomically like `config.toml`. An unreadable or invalid file is logged, ignored, and deleted (calibration dropped, limit restored) — never a crash loop. *Claude's call.*
- **`eighty-cli`:** `eighty calibrate [cancel | schedule every <N> days [at HH:MM] | schedule off]`, and status lines such as:
  ```
  Calibrating: step 1/4, discharging to 15% (now 42%)
  Calibrating: step 3/4, holding at 100% (35 of 60 min)
  Calibrating: step 2/4, paused: plug in to continue
  Last calibrated 2026-11-22 · next 2026-12-22 10:00
  ```

### 7.8 Checked against prior art (2026-11-22)

- **`battery` (actuallymentor):** has no calibration. Nothing to match.
- **BattOpt** (README and CHANGELOG; its code isn't public): discharge to 15% → charge to 100% → 1-hour rest → discharge to the limit; `calibrate` / `calibrate stop`; smart resume pauses on unplug and resumes on replug. This design matches all of that. Step 1 also keeps going on battery.
- **Schedule form (owner's decision):** keep `every <N> days [at HH:MM]` for this release. BattOpt's forms (fixed days of the month, e.g. the 6th and 21st at 21:30, or every N weeks on a weekday) can come in a later release.
- **Charging must actually start in step 2.** BattOpt's changelog fixed a macOS 27 bug where charging to 100% sometimes didn't start after the discharge to 15%. Step 2 checks, on a settled reading, that the battery is charging (or already full). If it isn't, it sets the Charge Limit to 100 again (native) or clears inhibit again (hold), and logs it. That counts toward the 24-hour give-up rule.
- **Calibration log:** every calibration start, step change, pause, resume, finish, cancel and abandon goes in the helper log with a `calibration:` prefix. A separate history file (BattOpt has one) is left for later.
- **Real vs shown percentage:** BattOpt can act on the raw percentage. eighty uses the shown percentage, as everywhere else.
- **Clamshell levels (BattOpt Pro):** BattOpt can discharge or calibrate with the lid closed by keeping the Mac awake (level 1), optionally with the external display on (level 2). This design keeps eighty's current rule: no forced discharge with the lid closed, so a docked Mac with the lid shut pauses in step 1. Lid-closed support is a separate item (see §2).

## 8. Testing

- Pure-policy unit tests for every step × (AC, battery, lid closed, asleep, fault) and each mode.
- Engine tests with the fake battery and fake clock: resume after restart in each step, uninstall clears state, refusals, a schedule firing once, a corrupt state file.
- Hardware acceptance on both Macs (`docs/testing/plan3-acceptance.md`): one full calibration in native mode on each, with an unplug and a reboot during it; `cancel` mid-step 1 restores `CHIE = 00` and the limit; hold mode on macOS 26 only if a Mac on 26 is available (neither of the owner's Macs is, today), otherwise covered by unit tests.

## 9. Left to the builder

- Exact type layout (calibration as an `Override` variant vs. separate state), file format of the saved state, status wording.
- How often to save during step 3 (every few minutes is enough; losing a few minutes of hold on a crash is fine).
- Whether step 4 also waits for the charge to come back down before printing "finished" — the parent behaviour (limit restored = finished) is the default.
