# Plan 3 hardware acceptance (calibration)

Task 10 of [Plan 3](../superpowers/plans/2026-11-22-eighty-03-calibration.md). Design: [calibration design](../superpowers/specs/2026-11-22-eighty-calibration-design.md). The owner runs this on each Mac in native mode. A full calibration takes several hours, so plan an afternoon per Mac.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | | native | Not run |
| M2 Max (Mac14,6) | 27.2 | native | Not run |

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
