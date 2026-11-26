# Plan 4 hardware acceptance (overheat protection)

## Lid-closed CHIE check

Task 1 of Plan 4. Decides `HEAT_PAUSE_LID_CLOSED`: whether holding `CHIE=08` with the lid closed keeps the Mac awake and the external displays on.

Owner instructions (M5 Pro):

1. Build with `cargo build --release`.
2. Connect the LG displays and AC. Charge must be between 30 and 95%.
3. Close the lid and wait for the external displays to show the desktop.
4. From a terminal on an external display, run `sudo ./target/release/eighty-probe --chie-soak 300`.
5. Paste the full output under Results.

Each line shows time (UTC), charge, ExternalConnected, amperage, `CHIE` and lid state, every 30 s. If the Mac sleeps, it shows as a gap of more than 60 s between lines.

**Pass** means no gap over 60 s, the displays stay on and `CHIE` ends `00`. On pass, `HEAT_PAUSE_LID_CLOSED = true`; on fail, `false`.

### Results

| Date | Mac | Result | Decision |
|---|---|---|---|
| 2026-11-26 | M5 Pro (Mac17,8), macOS 27.2.1, 2 LG displays, lid closed | Soak looked like a PASS, but the real pause FAILED (see below) | `HEAT_PAUSE_LID_CLOSED = false` |

```
09:04:18Z charge=87% external=Yes amperage=-104mA CHIE=08 lid=closed
09:04:49Z charge=87% external=Yes amperage=-379mA CHIE=08 lid=closed
09:05:19Z charge=87% external=Yes amperage=-379mA CHIE=08 lid=closed
09:05:49Z charge=87% external=Yes amperage=-317mA CHIE=08 lid=closed
09:06:19Z charge=87% external=Yes amperage=-317mA CHIE=08 lid=closed
09:06:49Z charge=87% external=Yes amperage=-322mA CHIE=08 lid=closed
09:07:19Z charge=87% external=Yes amperage=-322mA CHIE=08 lid=closed
09:07:49Z charge=87% external=Yes amperage=-302mA CHIE=08 lid=closed
09:10:51Z charge=87% external=Yes amperage=-1654mA CHIE=08 lid=closed
09:11:21Z charge=87% external=Yes amperage=-1654mA CHIE=08 lid=closed
09:11:49Z charge=87% external=Yes amperage=-463mA CHIE=00 lid=closed
```

Notes:
- The 3-minute gap (09:07:49 → 09:10:51Z) was macOS **idle sleep**. In `pmset -g log`: display off at 12:07:25 local (no input since 12:03:31), then `Entering Sleep state due to 'Idle Sleep' … Using AC (Charge:87%)` at 12:08:06, then a keyboard wake at 12:10:40. The same idle sleep happened at 11:52 and 12:02, before the test, with no `CHIE`. AC settings: `displaysleep 10`, `sleep 1`.
- macOS reported "Using AC" and `external=Yes` throughout with `CHIE=08`. The adapter was never seen as gone, so `CHIE` didn't trigger a sleep.
- `CHIE` stayed `08` across the sleep and was restored to `00` at the end.
- Limitation: `eighty uninstall` had put the Charge Limit back to 80% (the value before eighty), so at 87% macOS was already draining. The negative amperage is therefore not proof that `CHIE` stopped charging. Checks 2 and 7 below test the pause itself.

**Correction (2026-11-26, during the checklist):** the soak was not a valid test. macOS was already running from the battery (87% above the 80% limit), so `CHIE=08` changed nothing macOS could see. In a real heat pause the battery is *charging*, and turning `CHIE` on makes macOS report `ExternalConnected = No`. With the lid closed, macOS then treats the Mac as unpowered and goes into clamshell sleep. From `pmset -g log` (local time):

- 12:34:49: check 2 pause starts. 12:34:50: display off. 12:35:08: `Entering Sleep state due to 'Clamshell Sleep' … Using AC (Charge:85%)`.
- 12:41:03: pause starts again. 12:41:21: `Clamshell Sleep`.
- Each later wake: the helper turned `CHIE` back on, and the Mac slept again about 10 s later (around 10 cycles between 09:41 and 09:57Z in the helper log).

Decision: `HEAT_PAUSE_LID_CLOSED = false`. A lid-closed heat pause would need the clamshell-levels work (keeping the Mac awake), which is the next plan.

## Checklist

Task 7 of Plan 4. Design: [overheat design](../superpowers/specs/2026-11-26-eighty-overheat-design.md). The owner runs this on each Mac, the M5 Pro first, then the M2 Max. Both run in native mode, so a pause means `CHIE=08` (battery `discharging`).

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 27.2.1 | native | PASS 2026-11-26 (after the lid-closed fix) |
| M2 Max (Mac14,6) | | native | |

### Setup

- Build with `cargo build --release`, then `sudo ./target/release/eighty install`. Use `./target/release/eighty` for every command below.
- Keep the Mac on AC. Read the log with `eighty logs` (`/Library/Logs/eighty/helper.log`); overheat lines start with `overheat: `. Read the keys with `sudo ./target/release/eighty-probe` or `eighty status --json`. Record times in UTC.
- The testing form `eighty overheat <pause> [resume] --testing` is hidden. It allows a pause down to 20 °C and a resume down to 15 °C (resume at most pause − 2), so a pause can be triggered on a cool battery. A pause starts only when the temperature is at or above the pause value, so pick the pause at or below the `Temp` reading, rounded down, and the resume at pause − 2 (it must be at most pause − 2, and below the reading so the pause holds). For example, at 31.4 °C run `eighty overheat 31 29 --testing`.
- The heat settings are in `/Library/Application Support/eighty/heat.toml`.

### Checks

| # | Check | Result | Evidence |
|---|---|---|---|
| 1 | `eighty status` and `eighty doctor` | PASS | Status has a `Temp` line such as `Temp      31.4 °C` (right under `Battery`). Doctor has `Overheat  on · pause 35 °C · resume 32 °C`. If the Mac reports no temperature, status shows `Temp      n/a` and doctor warns `no battery temperature reported; overheat protection is inactive on this Mac` |
| 2 | Testing pause. On AC with the battery charging below the limit, run `eighty overheat <current> <current-2> --testing` (current = the whole °C in `Temp`, rounded down, e.g. `31 29` at 31.4 °C) | PASS | Within about 2 min, the State line reads `Paused: battery hot (36.1 °C), resumes at N °C` (with the real values). `Battery` shows `discharging N mA`. `CHIE=08`. Log has `overheat: paused`. Status warns `overheat test thresholds in use (pause 31 °C); run `eighty overheat 35` to return to normal` |
| 3 | Resume. Run `eighty overheat 35` | PASS | Within one tick, `CHIE=00` and charging resumes (`Battery` shows `charging N mA`). The `Paused` State line is gone and doctor shows the default `pause 35 °C · resume 32 °C` again. Log has `overheat: resumed`. The test-thresholds warning is gone. Note: with resume 32, the pause ends only if the battery is at or below 32 °C. If it is warmer, use `eighty overheat off` to end it (or `eighty overheat 35 33`) |
| 4 | At the limit and not charging (charge at the limit, `not charging`), run a testing pause as in check 2 | PASS | No pause starts and there is no drain: `CHIE=00`, `Battery` stays `not charging`, no `overheat: paused` line (a pause needs the battery to report charging). Then `eighty overheat 35` |
| 5 | Restart during a pause. Start a pause as in check 2, then `sudo launchctl kickstart -k system/io.github.mabujaber.eighty.helper` | PASS | After the restart the log has `eighty-helper <version> starting` and `overheat: resuming the pause from before the restart`, and the State line still reads `Paused: battery hot (...)`. `heat.toml` was there across the restart. `CHIE=08` again within about 2 min. Then `eighty overheat 35` |
| 6 | Sleep during a pause. Start a pause, then close the lid (or `pmset sleepnow`) and wake the Mac | PASS | Log has `sleep` and `wake`. After wake the State line reads `Paused: battery hot (...)` again and `CHIE=08`. No `CHIE` write while asleep. Then `eighty overheat 35` |
| 7 | Lid closed with external displays during a testing pause. Start a pause as in check 2, then close the lid with the displays connected | PASS (after fix; FAIL with `true`) | Depends on `HEAT_PAUSE_LID_CLOSED` (`false`, see the correction above). `true`: the pause stays on, `CHIE=08`, the displays stay on and the Mac stays awake. `false`: the pause is not held, `CHIE=00`, charging carries on, and the State line reads `Battery hot (36.1 °C), can't pause charging with the lid closed` |
| 8 | `eighty overheat off` during a pause | PASS | The pause ends at once: `CHIE=00`, State line no longer says `Paused`, doctor shows `Overheat  off`. Then `eighty overheat on` |
| 9 | `sudo eighty uninstall` during a pause | PASS | `CHIE=00`, macOS's Charge Limit is back to what it was before eighty, `heat.toml` is gone (`ls "/Library/Application Support/eighty/"`) |

### Extra checks (do if time allows)

| Check | Result | Evidence |
|---|---|---|
| Native floor: start a testing pause at a charge a few points above 20%, let it drain | not run (M5 was at 85–97%; covered by unit and engine tests) | At 20% the pause ends. State line reads `Battery hot, charging anyway: charge reached 20%`. No new pause until the charge reaches 30% or the temperature is at or below resume |
| `eighty overheat 50`, `eighty overheat 38 37` | not run on hardware (covered by tests) | Refused, with the allowed range. Settings unchanged |

### Results

Record each Mac's result here, with the build commit, the date and anything that needed a fix.

**M5 Pro (Mac17,8), macOS 27.2.1, 2026-11-26, builds 7f8e06b then 1007009. Checks run 09:34–10:19Z by Claude from the CLI; the owner did sudo and lid steps.**

- 1: `Temp      26.8 °C`; doctor `Overheat  on · pause 35 °C · resume 32 °C`.
- 2: `charge full` made the battery charge (85%, +1826 mA), then `eighty overheat 26 24 --testing` at 26.7 °C. It went straight to `Paused: battery hot (26.7 °C), resumes at 24 °C` with the testing warning, and was discharging within about 10 s (raw `Amperage` about −2.7 A, `IsCharging=0`). Log: `overheat: paused at 26.7 °C`. JSON `overheat.note = paused`. `charge full` stayed in place, so heat won over it. Raw IORegistry showed `ExternalConnected = No` while `CHIE` was on, and eighty's latch kept "on AC".
- 3: `eighty overheat 35` → `overheat: resumed at 26.9 °C`, the warning disappeared, and the battery was charging again.
- 4: under `charge auto` at 85% (macOS draining toward 80%, not charging), `eighty overheat 27 25 --testing` at 27.4 °C → no pause over 2 ticks; State stayed `Discharging to 80%`.
- 5: `launchctl kickstart -k` during a pause → `overheat: resuming the pause from before the restart`.
- 6: during the build with lid-closed protection on, the helper log shows `sleep:` → `discharge=false` and `wake:` → `discharge=true` about 10 times.
- 7: with `HEAT_PAUSE_LID_CLOSED = true` (7f8e06b), every pause with the lid closed caused `Clamshell Sleep` (see the correction above). With `false` (1007009), closing the lid during a pause: `CHIE` went off, the State line read `Battery hot (28.3 °C), can't pause charging with the lid closed`, the battery kept charging, and there was no sleep (only the normal display switch at lid close).
- 8: `eighty overheat off` during a pause → `overheat: off`, then `discharge=false` in the same second; doctor `Overheat  off`.
- 9: `sudo eighty uninstall` during a pause → `CHIE=00`, Charge Limit back to 80% (the value before eighty), `heat.toml` gone, helper gone.
- Note: `config.toml` still holds the testing thresholds (pause 28, `testing = true`) after uninstall without `--purge`. Run `eighty overheat 35` after the next install.

