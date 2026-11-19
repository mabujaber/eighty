# Plan 2b hardware acceptance

Task 7 of [Plan 2b](../superpowers/plans/2026-11-19-eighty-02b-native-mode.md). The helper and CLI were run on real Macs, with the owner watching the MagSafe light.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 26.6.2 (25G83) | hold (`CHTE` + `CHIE`), regression only | **Passed**, 2026-11-19 |
| M2 Max (Mac14,6) | 27.2 (26B123) | native (macOS Charge Limit) | Not run yet |
| M5 Pro (Mac17,8) | 27.2.1 (26B131) | native (macOS Charge Limit) | In progress, 2026-11-19 |

## M5 Pro, hold mode regression

Build: `feat/helper` at `01c5439`. Setup: macOS Charge Limit 100%, Optimized Charging off. Limits 80% / 75%, auto-discharge on, config kept from the plan 2 run (no `native_original`). The test started at 93% on AC with the lid closed (external displays). Times are UTC.

| Step | Check | Result | Evidence |
|---|---|---|---|
| — | First install used a stale `target/release` (built 2026-11-17, before native mode) | Redone | The old helper's log has no `native steps` field and the binaries had no PowerUI strings. Rebuilt with `cargo build --release` and reinstalled |
| Install | `sudo eighty install` over the old helper, then `eighty status` | Pass | 07:51:34 the old helper logged `shutting down; restoring charging`. 07:51:35 the new one logged `mode Hold, light true, keys [CHTE Usable, CHIE Usable, ACLC Usable], native steps []`. Status text unchanged: `Mode hold (CHTE) · MagSafe light: yes`, `Lid closed: holding at 93%, discharges to 80% when the lid opens` |
| — | Hold mode never touches macOS's Charge Limit | Pass | No PowerUI calls in the log (`native steps []`, no "Charge Limit was N% before eighty" line). The config never gained `native_original` |
| — | Battery line after a switch (finding H) | Pass | Status showed `updating…` until macOS's next battery reading, about 80 s after startup, then `93% · not charging · on AC`, 0 mA |
| — | `eighty doctor` | Pass | `Limiters none`, `Native 100% · steps 80, 85, 90, 95, 100`, `Health ok`. The helper (root) reads the Charge Limit through PowerUI |
| 7 | `eighty limit 80 7` | Pass | Refused: "lower limit must be between 10 and the upper limit 80 (got 7)" (a typo, but the right refusal) |
| 7 | `eighty limit 80 70` | Pass | Accepted, `Limit 80% (resumes below 70%)`. A lower limit still works in hold mode |
| 7 | `eighty limit 70` | Pass | Accepted, `Limit 70% (resumes below 65%)`, `discharges to 70% when the lid opens`. No native-mode refusal |
| 7 | Lid opens above the limit | Pass | 08:02:09 sleep, 08:02:15 wake (the usual short sleep/wake when the lid opens), and in the same second `outputs inhibit=true discharge=true light=Some(Green)`, `activity Discharging { to: 70 }`. Light green (owner). Status `discharging 566 mA · on AC` while ioreg said `ExternalConnected = No`: bug A's fix held, no flapping |
| 6 | macOS Charge Limit set to 80%, then back to 100%, while discharging | Pass | Doctor read `Native 80%` at 08:07 and `100%` at 08:10. Throughout: `Discharging to 70%` (463–1014 mA), `Health ok`, config `upper = 70`, no new log lines. Nothing adopted, no Conflict |
| 8 | `eighty limit 80`, then `sudo eighty uninstall` | Pass | 08:11:22 `Discharging { to: 80 }`. 08:11:34 `shutting down; restoring charging`. Printed `restore: native limit left as is` and `charging restored; eighty uninstalled`. Helper, plist, process and socket gone; config and log kept. Probe at 08:12:35: `CHTE=00000000`, `CHIE=00`, `ACLC=04`. Charging at +3.7 A from 93% |

Not run (code paths unchanged since plan 2, or optional): sleep, reboot, permissions, `kill -9`, `charge full`, `discharge <n>`.

## M5 Pro, native mode (macOS 27.2.1)

Updating this M5 from 26.6.2 to 27.2.1 removed `CHTE`, so it can no longer use hold mode. The read-only and `--write-test` probe runs showed `CHTE` missing and `CHIE` Verified. The light test ended in `RestoreFailed` (ACLC read back `04`), but it had read `03` again by the next check, so it looks like a timing issue. Nobody watched the light during that run, so the green/orange light check on 27.2.1 is unconfirmed (native mode leaves the light to macOS). `cargo test -p eighty-hw native -- --ignored` passed (`powerui_reads_on_this_mac`, no root). Build: `feat/helper` at `1f70f1e`, with `target/release` from `01c5439` (only docs changed since). Before the run, System Settings showed a Charge Limit of 100%. No other limiter was installed. Times are UTC.

| Step | Check | Result | Evidence |
|---|---|---|---|
| 1 | `sudo eighty install`, `eighty status`, `eighty doctor` | Pass | 09:40:52 log: `mode Native, light false, keys [CHTE Missing, CHIE Usable, ACLC Usable], native steps [80, 85, 90, 95, 100]`, `native: macOS's Charge Limit was 100% before eighty`, `native: set macOS's Charge Limit to 80%`. Status: `Mode native (macOS Charge Limit) · MagSafe light: macOS`, `Limit 80% (held by macOS)`. Doctor: `Native 80%`, `Health ok`, `Limiters none`. Config gained `native_original = 100` |
| 3 | Above the limit | Pass | At 100% on AC: `State Discharging to 80%`. ioreg at 09:41:44 showed `InstantAmperage` −603 mA with `ExternalConnected = Yes`, and the probe read `CHIE=00` and `ACLC=03`, so the discharge came from macOS, not from eighty. Status still showed 0 mA until macOS's next battery reading |
| 2 | `eighty limit 85` | Pass | 09:42:45 `native: set macOS's Charge Limit to 85%`, `Discharging { to: 85 }`. Status `Limit 85% (held by macOS)`, `discharging 869 mA · on AC` |
| 2 | `eighty limit 70` | Pass | Refused: "this Mac only supports a limit of 80, 85, 90, 95 or 100%" |
| 2 | `eighty limit 80 75` | Pass | Refused: "macOS decides when charging resumes on this Mac, so there is no lower limit" |
| 2 | Set 80% in System Settings | Pass | 09:44:52 `native: macOS reports 80%, not eighty's 85%; checking again next tick`. 09:45:52 `native limit changed outside eighty: now 80%`. Status `Limit 80%` and `Warning Charge Limit changed in System Settings: now 80%.` Config `upper = 80` |
| 2 | A brief change: 90% in System Settings, then back to 80% before the next check | Pass | 09:48:52 `native: macOS reports 90%, not eighty's 80%; checking again next tick`. The owner set 80% again. After that: no adoption and no write by eighty (no further native lines). Doctor `Native 80%`, `Health ok`, status `Limit 80%` |
| 2 | Three changes without a steady value (90 → 85 → 95 → 90, each right after the helper logged the previous one) | Pass | 09:54:52 `macOS reports 90%, not eighty's 80%`. 09:55:52 `macOS reported 90%, now 85%`. 09:56:52 `macOS reported 85%, now 95%`. 09:57:52 `native: macOS's reading keeps changing (now 90%); setting eighty's 80% again`, `native: set macOS's Charge Limit to 80%`. Doctor `Native 80%`, `Health ok`, config `upper = 80` (nothing adopted) |
| 1 | Reboot (owner restart, FileVault on) | Pass, with a small margin | 09:59:47 `shutting down; restoring charging` (no PowerUI write at shutdown). Kernel boot 10:00:05. The helper started at 10:01:01 and in the same second logged `native: set macOS's Charge Limit to 80%`, before Finder started (10:01:03). loginwindow started at 10:01:00, so the helper ran about when the session started, not long before it. No `NativeUnavailable`: PowerUI answered on the first try. After boot: doctor `Native 80%`, `Health ok`, status `Limit 80% (held by macOS)` |
| 5 | Does macOS keep the limit across a reboot without the helper? | Yes, macOS keeps it | `sudo launchctl disable system/io.github.mabujaber.eighty.helper`. 10:10:07 the helper stopped (`shutting down`). Kernel boot 10:10:25. The helper did not run after boot (no process, no new log lines). System Settings showed a Charge Limit of 80% (owner). So eighty's write at boot only backs up what macOS has already stored |
| 5 | Re-enable the helper and reboot | Pass | `sudo launchctl enable …`, reboot. At 10:15:08 the helper started, then `native: set macOS's Charge Limit to 80%`. Status at 10:25: 96%, `discharging 438 mA · on AC`, doctor `Native 80%`, `Health ok` |
| — | `sudo -u nobody eighty limit 85` / `status` | Pass | Refused: "permission denied: only an admin (sudo) or the logged-in user can change eighty". Status works. The CLI was copied to `/tmp/eighty-cli` first, because `nobody` can't get into the home folder (`drwxr-x---`) |
| 4 | `eighty charge full` | Pass | 10:29:14 `native: set macOS's Charge Limit to 100%`, `Charging { to: 100 }`. Doctor `Native 100%`. Status `96% · charging 3630 mA`, `Override charge to 100% until unplugged`. Minor: status still says `Limit 80% (held by macOS)` while macOS's limit is 100% (wording only) |
| 4 | Unplug | Pass | 10:31:14 `override FullCharge finished`, `native: set macOS's Charge Limit to 80%`, `OnBattery`. Doctor `Native 80%`, `Health ok`. Status has no Override line, `Limit 80%`. Config unchanged (`upper = 80`) |
| 4 | `eighty discharge 94` on battery (charger not plugged in by mistake) | Note | The CLI was run on battery. No override was logged and none was active after plugging in (the helper logged `Discharging { to: 80 }` at 10:34:43 with `CHIE=00`). The command had to be run again on AC. Check what the CLI prints for `discharge` on battery |
| 4 | `eighty discharge 94` on AC, at 96% | Pass | 10:35:16 `outputs inhibit=false discharge=true`, `Discharging { to: 94 }`. Probe `CHIE=08`. Status `Override discharge to 94%`. The shown % lagged the actual charge: 20 min later it still showed 96% while `AppleRawCurrentCapacity` had dropped from 8212 to 7935 mAh (−552 mA) |
| — | Sleep during the discharge override | Pass | 11:01:00 `sleep: macOS is going to sleep`, `outputs … discharge=false` (CHIE `00` while asleep). 11:04:22 woke (HID), `outputs … discharge=true`. The override survived (status still `Override discharge to 94%`, at 95%). It slept again 11:05:31–11:07:55, and the same thing happened |
| 4 | Discharge reaches 94% | Pass | 11:08:06 `override DischargeTo(94) finished`, `outputs … discharge=false`, `activity Discharging { to: 80 }`. Probe `CHIE=00`. ioreg `InstantAmperage` −621 mA with `ExternalConnected = Yes`, so macOS took over and carries on to its 80% Charge Limit. Status: `94%`, `Discharging to 80%`, no Override line |

## M2 Max, native mode

Not run yet. Before starting:

- `git pull` and `cargo build --release`. Check that the build has native mode: `strings target/release/eighty-helper | grep -c PowerUI` is not 0.
- Note the macOS Charge Limit shown in System Settings → Battery: ____%.
- Uninstall any other limiter (`battery`, including `/Applications/battery.app`).

| Step | Check | Expected |
|---|---|---|
| 1 | `sudo eighty install`, `eighty status`, `eighty doctor` | `Mode native (macOS Charge Limit) · MagSafe light: macOS`, `Limit 80% (held by macOS)`. Log: `native: macOS's Charge Limit was N% before eighty`. System Settings shows 80%. The config gains `native_original = N` |
| 1 | Reboot, before logging in | The helper's first `native: set macOS's Charge Limit to 80%` line is before Finder's start time (§5.6 item 1). If PowerUI isn't reachable yet, status shows `couldn't reach macOS's Charge Limit (PowerUI)` and it retries each tick |
| 5 | Reboot: does the limit survive? | The helper sets the limit again at boot, so test it with the helper off: `sudo launchctl disable system/io.github.mabujaber.eighty.helper`, reboot, read System Settings (80% means macOS stores it), then `sudo launchctl enable …` and reboot again (§5.6 item 2) |
| 2 | `eighty limit 85` | System Settings shows 85% |
| 2 | `eighty limit 70` | Refused: "this Mac only supports a limit of 80, 85, 90, 95 or 100%" |
| 2 | `eighty limit 80 75` | Refused: "macOS decides when charging resumes on this Mac, so there is no lower limit" |
| 2 | Set 80% in System Settings | Log: `native limit changed outside eighty: now 80%`. Status shows 80% and the "Charge Limit changed in System Settings" line. Config `upper = 80` |
| 2 | Change it twice quickly, or for less than 40 s | A one-tick blip is ignored. After three changes without a steady value, eighty sets its limit back |
| 3 | On AC at or above the limit for 10+ minutes | Not charging, light green (set by macOS) |
| 3 | Above the limit | `Discharging to 80%`, negative amperage, `CHIE=00` (macOS does it, not eighty) |
| 4 | `eighty charge full` | System Settings shows no limit (100%), and it charges |
| 4 | Unplug | The limit goes back to 80% |
| 4 | `eighty discharge <current − 2>` | `CHIE=08` while it discharges, then back to Holding with `CHIE=00` |
| 5 | Plugged in overnight, lid closed | Record the % in the morning (§5.6 item 3) |
| — | If macOS charges past the limit (calibration) | Status shows the warning and doesn't claim Holding (§5.6 item 5) |
| — | `sudo -u nobody eighty limit 85` | Permission denied; `sudo -u nobody eighty status` works |
| 6 | `sudo eighty uninstall` | Prints `restore: native limit back to N%` (the value noted above). System Settings shows it. Probe: `CHIE=00` |
