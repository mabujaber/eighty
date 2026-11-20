# Plan 2b hardware acceptance

Task 7 of [Plan 2b](../superpowers/plans/2026-11-19-eighty-02b-native-mode.md). The helper and CLI were run on real Macs, with the owner watching the MagSafe light.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 26.6.2 (25G83) | hold (`CHTE` + `CHIE`), regression only | **Passed**, 2026-11-19 |
| M2 Max (Mac14,6) | 27.2 (26B123) | native (macOS Charge Limit) | **Passed**, 2026-11-19/20 |
| M5 Pro (Mac17,8) | 27.2.1 (26B131) | native (macOS Charge Limit) | **Passed**, 2026-11-19 to 2026-11-20 |

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
| 3 | On AC at 80% for 10+ minutes | Pass | 12:34 at 81%, −1369 mA, `CHIE=00` (macOS discharging). 12:37:39 `activity Holding`. 12:38 at 80%, −528 mA (last step / stale reading). From 12:39: 0 mA, `IsCharging = No`, `ExternalConnected = Yes`. Still `Holding at 80%` and `not charging · on AC` at 12:56 (about 19 min). Light green (owner) |
| 5 | Plugged in overnight, lid closed (no external displays) | Pass | 19:50 UTC: 80%, AC, `Holding at 80%`, `CHIE=00`. The helper logged `sleep` at 19:54:08 and `wake` at 06:50:17 (about 11 h), nothing in between: no activity change, no fault. macOS's power log for the night: 17 DarkWakes, every reading `Using AC (Charge:80%)` (44 of 44). Morning: 80%, `not charging · on AC`, `Holding at 80%`, doctor `Native 80%`, `Health ok`, `CHIE=00`, `ACLC=03` |
| 6 | `sudo eighty uninstall` | Pass | Printed `restore: wrote CHTE=00000000, CHIE=00, ACLC=00 to every key present`, `restore: native limit back to 100%`, `charging restored; eighty uninstalled`. 06:53:34 `shutting down; restoring charging`. Helper binary, LaunchDaemon plist, process and `/var/run/eighty.sock` gone; config and log kept. Probe: `CHTE` missing, `CHIE=00`, `ACLC=04` (charging). Next battery reading (06:54:17): charging at +4227 mA from 80%, `NotChargingReason = 0`. System Settings: Charge Limit 100% (owner), the value before eighty |
| — | After uninstall: `cargo build --release` at `235cc83`, `cargo test -p eighty-hw battery -- --ignored` | Pass | `reads_real_battery` and `reads_real_temperature` pass. The probe prints `27.2 °C` (was `temp n/a`) |

### Extra native-mode checks on the M5 Pro (2026-11-20)

Checks that neither Mac had run in native mode. Build: `feat/helper` at `e1866ee`, `cargo build --release` (includes the temperature change). Before install, eighty was uninstalled and macOS's Charge Limit was 100%. The config from the last uninstall was still there, with `native_original = 100`.

| Check | Result | Evidence |
|---|---|---|
| Install with a config left by an earlier uninstall | Pass, but see finding 1 | 06:57:27 started, `native: set macOS's Charge Limit to 80%`. No `Charge Limit was N% before eighty` line: the old `native_original = 100` was reused, not read again |
| Reinstall over a running install | Pass | 06:57:59 `shutting down`, 06:58:04 new helper, `set … 80%`. Config kept `native_original = 100` |
| `charge full`, then `charge auto` (still on AC) | Pass | 06:58:46 `set … 100%`, `Charging { to: 100 }`, doctor `Native 100%`. 06:58:49 `set … 80%`, `Discharging { to: 80 }`, doctor `Native 80%` |
| `auto-discharge off` / `on` | Pass | Both refused, exit 1: "macOS brings the charge down to the limit on this Mac; it can't be turned off". Config unchanged. Minor: the same message for `on` reads oddly |
| `logs`, `doctor --retry` | Pass | `logs` prints the last lines. `--retry` re-ran detection (06:58:52 `mode Native …`, `set … 80%`), `Health ok` |
| `kill -9` the helper while holding | Pass | launchd restarted it at 06:59:27 (new pid, no `shutting down` line). Doctor `Native 80%`, `Health ok`, `CHIE=00`, `native_original = 100` |
| `kill -9` during `discharge 81` (CHIE=08) | Pass | 06:59:47 `outputs … discharge=true`, probe `CHIE=08`. Killed. 07:00:37 restart: probe `CHIE=00`, override gone (in memory only), `Discharging to 80%` by macOS, `Health ok`. Nothing in the log says CHIE was cleared at startup |
| `discharge 79` on battery | Pass | Unplugged: printed `Discharging to 79%`, `Override discharge to 79%`, exit 0. 07:01:24 `outputs … discharge=true`, probe `CHIE=08`. Yesterday's "nothing happened" did not reproduce (its CLI output wasn't kept) |
| Plug in during that discharge | Pass (better than the known limitation) | With `CHIE=08`, macOS 27.2.1 still reported `ExternalConnected = Yes` and `AppleRawExternalConnected = Yes`, and status said `on AC`. On macOS 26 `CHIE=08` hid the adapter; on 27.2.1 it doesn't |
| `charge auto` ends a discharge override | Pass | 07:03:20 `outputs … discharge=false`, `Discharging { to: 80 }`, probe `CHIE=00` |
| Helper CPU and memory | Pass | 0.04 s CPU in 2 min 53 s, with commands sent during that time. `top`: 0.0% CPU, 3.6 MB, no idle wakeups |
| `sudo eighty uninstall` after all of the above | Pass | `restore: native limit back to 100%`, `charging restored; eighty uninstalled`. Helper, plist, process and socket gone. `CHIE=00`, charging at +3.6 A |
| Probe `--write-test`, light watched | Pass | `ACLC WriteAccepted`, no `RestoreFailed` (the light was already orange from charging, so the restore matched). The owner saw green for 15 s, then orange. `CHIE` gave `CannotVerify("battery is already discharging")`: the first battery reading was still from before the uninstall (CHIE was verified on 2026-11-19). The probe printed `28.1 °C` |
| macOS charges past the limit (calibration) | Didn't happen | Not seen on either Mac |
| Uninstall forgets the original (`921fdae`, release build) | Pass | Starting from a stale `native_original = 100` left by the old uninstall: install, then uninstall printed `restore: native limit back to 100%` and the config no longer had `native_original`. The owner set the Charge Limit to 90% in System Settings. Install at 07:16:40 logged `native: macOS's Charge Limit was 90% before eighty`, config `native_original = 90`. Uninstall printed `restore: native limit back to 90%`, System Settings 90% (owner), config without `native_original` |

### Findings and follow-ups (code)

Follow-ups found on 2026-11-19, all fixed in `4d0ab06`:

- Probe: ACLC restore reads back too early (`RestoreFailed`, read `04` after writing `03`). **Fixed:** the light is treated as firmware-owned. Its restore counts once the original value is written back, and the read-back is no longer judged.
- Status during `charge full` still says `Limit 80% (held by macOS)` while macOS's limit is 100%. **Fixed:** it now says `Limit 80% (lifted to 100% until unplugged)`.
- `eighty discharge 94` run on battery left no override and no log line. **Fixed:** it is refused with "plug in first: `discharge` runs the Mac from its battery while it's on the charger; on battery it's discharging already", like `charge full`.

Found in the extra checks above (2026-11-20), all fixed in the next commit:

1. **`native_original` survives uninstall.** Uninstall restores it but leaves it in the kept config. If the user changes the Charge Limit while eighty is uninstalled and then reinstalls, eighty keeps the old value and the next uninstall overwrites the user's newer choice. **Fixed:** `--restore` removes `native_original` from the config once the limit is back. If that save fails, the limit still counts as restored and the message says the record couldn't be cleared.
2. **Startup doesn't log a cleared CHIE.** After a crash with `CHIE=08`, the restart clears it silently. A line like `startup: CHIE was 08; cleared` would make crashes visible in the log. **Fixed:** startup logs `startup: CHIE was left discharging (after a crash or a fault); cleared it`.
3. **`auto-discharge on`** in native mode is refused with "can't be turned off". **Fixed:** `on` is accepted and changes nothing; `off` is still refused.
4. **CHIE no longer hides the adapter on macOS 27.2.1** (M5). The "effective external" latch is only needed on macOS 26. Keep it. **Done:** noted in the spec (§5.6).

Note: the `discharge 79` on battery row above was run on `e1866ee`, before `4d0ab06`. Since `4d0ab06`, `discharge` on battery is refused with "plug in first".

## M2 Max, native mode

Build: `feat/helper` at `98668dd` (includes the `AppleSmartBatteryPack` temperature change), `cargo build --release` before the run, `PowerUI` present in the helper. No other limiter installed (no `battery` CLI, no `/Applications/battery.app`). `cargo test -p eighty-hw battery -- --ignored` passed and the probe printed `24.0 °C`. Times are UTC.

| Step | Check | Result | Evidence |
|---|---|---|---|
| 1 | `sudo eighty install`, `eighty status`, `eighty doctor` | Pass | 17:13:41 log: `mode Native, light false, keys [CHTE Missing, CHIE Usable, ACLC Usable], native steps [80, 85, 90, 95, 100]`, `native: macOS's Charge Limit was 80% before eighty`, `set macOS's Charge Limit to 80%`. Status `Mode native (macOS Charge Limit) · MagSafe light: macOS`, `Limit 80% (held by macOS)`. Doctor `Native 80%`, `Health ok`, `Limiters none`. Config `native_original = 80` |
| — | Redo with a 100% starting value | Pass | The Charge Limit was already 80% (left there after the plan 2 run), so a restore would look like a no-op. `sudo eighty uninstall --purge` printed `restore: native limit back to 80%`. The owner set 100% in System Settings and reinstalled: 17:14:55 `native: macOS's Charge Limit was 100% before eighty`, `set … to 80%`. Config `native_original = 100`. System Settings showed 80% (owner) |
| 2 | `eighty limit 85` | Pass | 17:15:49 `native: set macOS's Charge Limit to 85%`. Status `Limit 85% (held by macOS)`, doctor `Native 85%`, `Health ok` |
| 2 | `eighty limit 70` | Pass | Refused: "this Mac only supports a limit of 80, 85, 90, 95 or 100%" |
| 2 | `eighty limit 80 75` | Pass | Refused: "macOS decides when charging resumes on this Mac, so there is no lower limit" |
| 2 | Set 80% in System Settings | Pass | 17:16:55 `native: macOS reports 80%, not eighty's 85%; checking again next tick`. 17:17:55 `native limit changed outside eighty: now 80%`. Status `Limit 80%` and `Warning Charge Limit changed in System Settings: now 80%.` Config `upper = 80` |
| 2 | A brief change: 90%, then back to 80% before the next check | Pass | 17:20:55 `macOS reports 90%, not eighty's 80%`. The owner set 80% again. No further native lines through 17:24, doctor `Native 80%`, `Health ok`, config `upper = 80` |
| 2 | Three changes without a steady value (90 → 85 → 95 → 90, each right after the helper logged the previous one) | Pass | 17:26:55 `reports 90%`. 17:27:31 `reported 90%, now 85%`. 17:27:55 `reported 85%, now 95%`. 17:28:31 `macOS's reading keeps changing (now 90%); setting eighty's 80% again`, `set … to 80%`. Doctor `Native 80%`, config `upper = 80`. Two of the checks ran at :31 rather than on the :55 tick, so a change in System Settings seems to trigger an early check (likely the short sleep/wake seen on the M5). No harm |
| — | `sudo -u nobody eighty limit 85` / `status` | Pass | Refused: "permission denied: only an admin (sudo) or the logged-in user can change eighty". Status works. The CLI was copied to `/tmp/eighty-cli` first |
| 3 | On AC at 80% for 10+ minutes | Pass | Plugged in at 80%: 17:29:08 `activity Holding`. Probe `CHIE=00`, `ACLC=03`. Sampled every 2 min, 17:31–17:39: `80% · not charging · on AC`, `Holding at 80%`, `InstantAmperage` 0, `IsCharging = No`, `ExternalConnected = Yes`, `CHIE=00`. Light green (owner). This is the fix for finding G: in cycle mode this Mac ran from its battery most of the time on AC |
| 4 | `eighty charge full` | Pass | 17:40:54 `native: set macOS's Charge Limit to 100%`, `Charging { to: 100 }`. System Settings 100%, light orange (owner). 17:47:43 status `83% · charging 3664 mA`, `Override charge to 100% until unplugged`, doctor `Native 100%`. Minor, as on the M5: status still says `Limit 80% (held by macOS)` |
| 4 | Unplug | Pass | 17:48:00 `override FullCharge finished`, `native: set macOS's Charge Limit to 80%`, `OnBattery`. Doctor `Native 80%`, `Health ok`, config `upper = 80`, no Override line |
| 3 | Above the limit (replugged at 83%) | Pass | 17:48:18 `Discharging { to: 80 }`. 17:49:37 status `83% · discharging 1083 mA · on AC`. ioreg `InstantAmperage` −660 mA with `ExternalConnected = Yes`, probe `CHIE=00`, `ACLC=03`: macOS discharges, not eighty. Light green (owner) |
| 4 | `eighty discharge 81` on AC, at 83% | Pass | 17:49:57 `outputs inhibit=false discharge=true`, `Discharging { to: 81 }`. Probe `CHIE=08`, status `Override discharge to 81%`, battery line `updating…` (finding H). 18:09:47 `override DischargeTo(81) finished`, `discharge=false`, `Discharging { to: 80 }`. Probe `CHIE=00`, `InstantAmperage` −779 mA with `ExternalConnected = Yes` (macOS carries on). 18:15:38 `activity Holding` |
| 1 | Reboot (owner restart) | Pass, with a small margin | 19:07:55 `shutting down; restoring charging`. Kernel boot 19:08:16, loginwindow 19:08:31. The helper started at 19:08:32 and in the same second logged `native: set macOS's Charge Limit to 80%`, before Finder and Dock (19:08:35). No `NativeUnavailable`. After boot: doctor `Native 80%`, `Health ok`, status `78% · charging`, `Charging to 80%`, then 19:10:46 `Holding` |
| 5 | Does macOS keep the limit across a reboot without the helper? | Yes, macOS keeps it | `sudo launchctl disable system/io.github.mabujaber.eighty.helper` (the running helper keeps running until the restart). 19:11:53 `shutting down`. Kernel boot 19:12:11. No helper process and no new log lines after boot. System Settings showed 80% (owner). ioreg: 80%, `InstantAmperage` 0, `IsCharging = No`, `pmset` `AC attached; not charging` |
| 5 | Re-enable the helper and reboot | Pass | `sudo launchctl enable …`, reboot. Kernel boot 19:14:44, loginwindow 19:15:03, helper 19:15:04 with `native: set macOS's Charge Limit to 80%` and `Holding`, Finder 19:15:07. Doctor `Native 80%`, `Health ok`. Right after boot ioreg showed +1760 mA, `IsCharging = Yes` at 80% while status said `Holding at 80%`. By 19:16:56 it read 0 mA, `IsCharging = No`, and it never went past 80%: a short top-up by macOS after boot (also seen with the helper disabled) |
| 5 | Plugged in overnight, lid closed | Pass | 19:23:00 `OnBattery`, 19:23:04 `Holding` (a 4 s unplug before the lid closed). Then only sleep/wake lines: short dark wakes 19:24–20:30, asleep 20:30:41 → 06:50:30 (about 10 h 20 min). No `Charging`, no Conflict, no native lines. Morning: `80% · not charging · on AC`, `Holding at 80%`, `InstantAmperage` 0, `IsCharging = No`, doctor `Native 80%`, `Health ok`, probe `CHIE=00`, `ACLC=03`, `24.9 °C`. In cycle mode this Mac reached 100% overnight (finding G) |
| — | macOS charges past the limit (calibration) | Didn't happen | Not seen during the run or overnight |
| 6 | `sudo eighty uninstall` | Pass | 06:51:28 `shutting down; restoring charging`, `eighty-helper stopped`. Printed `restore: native limit back to 100%` and `charging restored; eighty uninstalled`. System Settings 100% (owner). Helper, plist, process and socket gone; config and log kept. Probe: `CHTE` missing, `CHIE=00`, `ACLC=04`. 06:52:18 charging at +4288 mA from 81% |
| — | Battery temperature (`0b0b2c1`) | Pass | 2026-11-20. `AppleSmartBattery` has no top-level `Temperature` on macOS 27.2; the only reading is in the one `AppleSmartBatteryPack` entry's `BatteryData` (`Temperature`=2639, `VirtualTemperature`=2639), so the pack fallback is used. `cargo test -p eighty-hw battery -- --ignored`: `reads_real_temperature` and `reads_real_battery` pass. `cargo test --workspace`: 232 passed. Probe `26.4 °C`, `--json` `"temperature_c": 26.39`. Readings during the run: 24.0 °C at the start, 24.9 °C after the night on AC, 26.0–26.4 °C charging at about 3.4 A. Only the probe shows the temperature; `status` and `doctor` don't |

Notes:

- The `Charge Limit changed in System Settings: now 80%.` warning from 17:17 was still shown at 17:49 and cleared only at the reboot. **Fixed 2026-11-20:** it now clears 10 minutes after the change (or at the next `eighty limit`). A change to 100% ("no limit") still stays until the next `eighty limit`.
- Right after boot, status can say `Holding at 80%` for about a minute while macOS tops up at about 1.7 A. It stays within the limit, so it is not the §5.6 item 5 case.

### Follow-up fixes on the M2 Max (2026-11-20)

Build `4d0ab06`, installed at 07:01:48 over the kept config. Times are UTC.

| Check | Result | Evidence |
|---|---|---|
| `eighty discharge 70` on battery | Pass | Refused: "plug in first: `discharge` runs the Mac from its battery while it's on the charger; on battery it's discharging already". No override and no log line. (Run on AC first by mistake: accepted, as it should be, 07:02:18 and 07:05:08) |
| `eighty charge full` | Pass | 07:05:38 `native: set macOS's Charge Limit to 100%`. Status `Limit 85% (lifted to 100% until unplugged)`. Unplug at 07:06:51: `override FullCharge finished`, `set … to 85%` |
| The warning after a change in System Settings times out | Pass | Set 85%: 07:03:48 `macOS reports 85%…`, 07:04:48 `native limit changed outside eighty: now 85%`. Warning shown until at least 07:07:13. At 07:15:04 it was gone, with `Limit 85% (held by macOS)` and config `upper = 85` |
