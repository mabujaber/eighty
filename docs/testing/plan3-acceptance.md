# Plan 3 hardware acceptance (calibration)

Task 10 of [Plan 3](../superpowers/plans/2026-11-22-eighty-03-calibration.md). Design: [calibration design](../superpowers/specs/2026-11-22-eighty-calibration-design.md). The owner runs this on each Mac in native mode. A full calibration takes several hours, so plan an afternoon per Mac.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 27.2.1 (26B131) | native | In progress, 2026-11-25 (build `a54049e`) |
| M2 Max (Mac14,6) | 27.2 (26B123) | native | **Passed**, 2026-11-22 to 2026-11-23 (build `89d6a71`), one finding (check 4) |

## Setup

- Build this branch with `cargo build --release`. Run the rows in order.
- Keep the Mac on the charger with the lid open, except where a row says otherwise.
- Read the log with `eighty logs`. Read the keys with `sudo eighty-probe` or `eighty status --json`. Record times in UTC.
- Calibration lines in the log start with `calibration: `.

## Checks

| # | Check | Result | Evidence |
|---|---|---|---|
| 1 | `sudo eighty install` of this build, then `eighty status` | | Status shows `Mode native`. Log shows the new helper started |
| 2 | `eighty calibrate` at 40% or more on AC, lid open | | Log: `calibration: started (step 1/4, discharging to 15%)`. `CHIE=08`. macOS Charge Limit is 80. `eighty status` State line: `Calibrating: step 1/4, discharging to 15% (now N%)` |
| 3 | Unplug for 5 minutes during step 1, then plug in | | Calibration keeps going: the battery drains anyway. State line still says step 1/4 and the percentage falls. No `paused` text |
| 4 | `sudo reboot` during step 1 or 2 | | After boot the log has `calibration: resuming step N/4` with the same N as before the reboot. `eighty status` shows the same step. `calibration.toml` was there during the reboot |
| 5 | The charge reaches 15% | | Log: `calibration: step 2/4: charging to 100%`. macOS Charge Limit is 100. Charging starts within 5 minutes (if not, the log has `calibration: charging hasn't started; asking macOS again`). Note the time the percentage showed 100 and the time `FullyCharged` flipped to `Yes` (`ioreg -rn AppleSmartBattery \| grep FullyCharged`). If it never flips, see Task 10 in the plan |
| 6 | Unplug during step 3 (the hold) | | Log: `calibration: step 3/4: holding at 100% for 1 h`. After unplugging, the State line says `paused: plug in to continue` and the `N of 60 min` count stops. After plugging in, it counts again |
| 7 | Let it finish | | Log: `calibration: finished; back to the 80% limit`. macOS Charge Limit is back to 80 and macOS discharges toward it. `eighty status` shows `Calibrate Last <today>`. `CHIE=00` |
| 8 | Second run: `eighty calibrate`, then `eighty calibrate cancel` during step 1 | | Log: `calibration: cancelled`. `CHIE=00`. Charge Limit 80. `calibration.toml` is gone. Nothing starts again right after |
| 9 | `eighty calibrate schedule every 7 days`, then `eighty calibrate schedule off` | | Log: `calibration: schedule every 7 days at 10:00`. `eighty status` shows `next` 7 days out at 10:00 (the time defaults to 10:00). After `off`, log: `calibration: schedule off` and the Calibrate line has no `next` |
| 10 | Start a calibration, then `sudo eighty uninstall` during it | | Charging restored (`CHIE=00`, `CHTE` unchanged, Charge Limit back to what it was before eighty). `/Library/Application Support/eighty/calibration.toml` is gone |

## Extra checks (do if time allows)

| Check | Result | Evidence |
|---|---|---|
| In step 2, change the Charge Limit in System Settings | | Log: `calibration: macOS's Charge Limit changed to N%; setting 100% again for the calibration`. The change is not adopted as eighty's limit |
| Close the lid during step 1 | | State line: `paused: lid closed, macOS may charge up to 80% meanwhile` |
| `eighty calibrate schedule every 7 days at 21:30` | | Next date is 7 days out at 21:30 |

## Results

Record each Mac's result here, with the build commit, the date and anything that needed a fix.

### M2 Max (Mac14,6), macOS 27.2 (26B123), native mode

Build: `feat/calibration` at `89d6a71`. Installed helper SHA-256 matches `target/release/eighty-helper` (`0d2fb020…`). No other limiter. Times are UTC, 2026-11-22.

| # | Result | Evidence |
|---|---|---|
| 1 | Pass | 20:41:09 `eighty-helper 0.1.0 starting`, `mode Native, light false, keys [CHTE Missing, CHIE Usable, ACLC Usable], native steps [80, 85, 90, 95, 100]`, `native: set macOS's Charge Limit to 80%`. Status `Mode native (macOS Charge Limit)`, `Holding at 80%` |
| 2 | Pass | Started at 80% on AC, lid open. 20:42:31 `calibration: started (step 1/4, discharging to 15%)`, `outputs inhibit=false discharge=true`, `activity Calibrating { step: Discharge }`. Status `Calibrating: step 1/4, discharging to 15% (now 80%)`. Doctor `Native 80%`, `Health ok`. Probe `CHIE=08`, `ACLC=04`. `calibration.toml` present (`step = "discharge"`). 20:43:33 ioreg `InstantAmperage` −1015 mA, `ExternalConnected = No`; raw charge 5867 → 5813 mAh in about a minute |
| 3 | Pass | Unplugged 20:50:30 to about 20:55:50. State stayed `Calibrating: step 1/4, discharging to 15% (now 80%)`, no `paused`; no new log lines. Raw charge 5867 (20:42) → 5729 (20:50) → 5634 mAh (20:55), −2753 mA after plugging back in. Notes: with `CHIE=08` macOS reports no adapter whether plugged or not (`pmset -g adapter`: "No adapter attached"), so status kept `on AC` while unplugged (the helper keeps the last trusted reading, by design). macOS's displayed 80% didn't move for 13 min while the raw charge fell about 3% (macOS lag, as in plan 2b) |
| 5 (start) | Pass | Step 1 drained 80% → 16% (displayed) from 20:42 to 02:08 on 2026-11-23, about 5 h 26 min at mostly −0.8 to −1.1 A, no pause or fault lines. 02:08:41 `calibration: step 2/4: charging to 100%`, `native: set macOS's Charge Limit to 100%`, `outputs inhibit=false discharge=false`. Doctor `Native 100%`, `Health ok`. Charging immediately: `IsCharging = Yes`, `ExternalConnected = Yes`, adapter 140 W seen again. No `charging hasn't started` line. The switch came at a displayed 16% (status shows macOS's lagging percentage) |
| 5 | Pass | Charging 16% → 100% (displayed) from 02:08:41 to 03:45:54, about 1 h 37 min. `FullyCharged` stayed `No` at 100% for 15 min, then flipped to `Yes` (first seen 04:00:58, polling every 20 s). The helper logged `calibration: step 3/4: holding at 100% for 1 h` at 04:00:49. Status `Calibrating: step 3/4, holding at 100% (0 of 60 min)`, `100% · not charging · on AC` |
| 6 | Pass (second run) | First run: nobody unplugged during the hold (04:00:49 to 05:01:12). Final run: 14:51:52 `calibration: step 3/4: holding at 100% for 1 h`; count 0 → 5 min on AC. Unplugged about 14:58: status `step 3/4, paused: plug in to continue` at 14:58:05 and 14:59:39 (the paused line shows no minute count). The Mac also slept 14:58:03 → 15:05:56 (log `sleep` / `wake`), which pauses the hold too. Plugged in about 15:06: `holding at 100% (5 of 60 min)` at 15:06:18, `6 of 60` at 15:07:18. The count stopped at 5 for about 8 min and went on counting after plugging in. Earlier in the final run, an unplug at 14:40 during step 2 showed `step 2/4, paused: plug in to continue` and charging carried on after plugging in |
| 7 | Pass | 05:01:12 `calibration: finished; back to the 80% limit`, `native: set macOS's Charge Limit to 80%`, `activity Discharging { to: 80 }`. Doctor `Native 80%`, `Health ok`. Status `Discharging to 80%`, `Calibrate Last calibrated 2026-11-23`. `calibration.toml` gone; config gained `[calibration] last_calibrated`. Probe after check 8 (12:53): `CHIE=00`, `ACLC=04`. Whole run 20:42:31 → 05:01:12, about 8 h 19 min. macOS discharged 100% → 80% and the helper logged `activity Holding` at 06:59:35 |
| 8 | Pass | Run on battery at 16%. 12:52:50 `calibration: started (step 1/4, discharging to 15%)`, status `step 1/4 … (now 16%)`, `calibration.toml` present. 12:52:53 `calibration: cancelled`. Status `On battery`, `Limit 80%`; doctor `Native 80%`, `Health ok`; `calibration.toml` gone; nothing restarted. On battery no `outputs` line was logged (step 1 writes nothing on battery) |
| 9 | Pass | 12:52:56 `calibration: schedule every 7 days at 10:00`; status `Calibrate Last calibrated 2026-11-23 · next 2026-11-30 10:00 (every 7 days)`. 12:52:58 `calibration: schedule off`; status `Calibrate Last calibrated 2026-11-23` (no `next`) |
| 10 | Pass | Started at 15% on battery: 12:54:34 `calibration: started`, then at once `step 2/4: charging to 100%`, `native: set macOS's Charge Limit to 100%`; status `step 2/4, paused: plug in to continue`. `sudo eighty uninstall`: 12:55:29 `calibration: helper stopping; putting the 80% limit back until it resumes`, `set macOS's Charge Limit to 80%`, `eighty-helper stopped`. Printed `restore: wrote CHTE=00000000, CHIE=00, ACLC=00 to every key present`, `restore: native limit back to 100%` (the value before eighty), `charging restored; eighty uninstalled`. `calibration.toml` gone. Reinstall at 12:55:43 logged `macOS's Charge Limit was 100% before eighty`, confirming the restore, then `set … to 80%`, doctor `Native 80%`, `Health ok` |
| 4 | Pass, with a finding | Final run: started at 15% on battery, 12:57:01 `calibration: started`, then at once `step 2/4: charging to 100%`, `set macOS's Charge Limit to 100%`. Plugged in; owner rebooted during step 2. 13:54:33 `calibration: helper stopping; putting the 80% limit back until it resumes`, then **`native: couldn't set the Charge Limit back to 80%: Couldn’t communicate with a helper application.`** (PowerUI was already gone during shutdown). 13:55:22 `eighty-helper 0.1.0 starting`, `calibration: resuming step 2/4`, `set macOS's Charge Limit to 100%`. `calibration.toml` kept across the reboot. Status `Calibrating: step 2/4, charging to 100% (now 92%)`, doctor `Native 100%`, `Health ok` at 13:58. Finding: on a shutdown or reboot the limit isn't put back, so if eighty didn't start again macOS would keep 100% |

Final run after check 6 (2026-11-23, UTC). Unplugged at 100% at 15:38 (macOS power log) and used on battery. 17:17:07 `calibration: step 2/4: charging to 100%`: the charge fell below 95% during the hold, so the run went back to step 2 as designed. Plugged in at 56% at 21:15:21; in the same second `calibration: charging hasn't started; asking macOS again` and `set macOS's Charge Limit to 100%` (the stall check fired on hours of no progress while on battery: harmless but noisy). 22:40:46 `step 3/4: holding at 100% for 1 h` (hold clock reset). 23:41:17 `calibration: finished; back to the 80% limit`, `set macOS's Charge Limit to 80%`. Status `Discharging to 80%`, `Calibrate Last calibrated 2026-11-24` (local date), doctor `Native 80%`, `Health ok`, `calibration.toml` gone.

### M5 Pro (Mac17,8), macOS 27.2.1 (26B131), native mode

Build: `feat/calibration` at `a54049e` (same code as `89d6a71`; later commits are docs only). Installed helper SHA-256 matches `target/release/eighty-helper` (`d867cf86…`). No other limiter (`Limiters none`). Times are UTC, 2026-11-25. The Mac runs with two external LG displays.

| # | Result | Evidence |
|---|---|---|
| 1 | Pass | 09:46:45 `eighty-helper 0.1.0 starting`, `mode Native, light false, keys [CHTE Missing, CHIE Usable, ACLC Usable], native steps [80, 85, 90, 95, 100]`, `native: macOS's Charge Limit was 80% before eighty`, `set … to 80%`, `activity Holding`. Status `Mode native (macOS Charge Limit)`, `Holding at 80%`. Doctor `Limiters none`, `Native 80%`, `Health ok` |
| Extra: lid closed in step 1 | Pass | Started with the lid closed (clamshell). 09:47:02 `calibration: started (step 1/4, discharging to 15%)`, `activity LidClosed`. State `Calibrating: step 1/4, paused: lid closed, macOS may charge up to 80% meanwhile`. `calibration.toml` present (`step = "discharge"`, `best_percent = 80`) |
| 2 | Pass | Lid opened about 09:48:40. 09:48:45 `outputs inhibit=false discharge=true light=None`, `activity Calibrating { step: Discharge }`. State `Calibrating: step 1/4, discharging to 15% (now 80%)`. Probe `CHIE=08`, `ACLC=04`. `status --json` `native_limit: 80`. `pmset`: "No adapter attached", `AppleRawExternalConnected = No` (CHIE hides the adapter, as on the M2). Raw charge 6897 (09:48:59) → 6811 mAh (09:52:19), about −1.8 A |
| 3 | Pass | Unplugged about 09:56, plugged in about 09:59:45 (owner). While unplugged: State `step 1/4, discharging to 15% (now 80%)`, no `paused`, no new log lines; raw 6760 → 6734 mAh. After plugging in (09:59:52): same State, `CHIE=08`, raw 6696 mAh, discharging about 1 A. Status kept `on AC` throughout (last trusted reading, by design). The unplug was about 4 min, not 5 |
