# Plan 2b hardware acceptance

Task 7 of [Plan 2b](../superpowers/plans/2026-11-19-eighty-02b-native-mode.md). The helper and CLI were run on real Macs, with the owner watching the MagSafe light.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 26.6.2 (25G83) | hold (`CHTE` + `CHIE`), regression only | **Passed**, 2026-11-19 |
| M2 Max (Mac14,6) | 27.2 (26B123) | native (macOS Charge Limit) | Not run yet |

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
