# Plan 2 hardware acceptance

Task 12 of [Plan 2](../superpowers/plans/2026-11-17-eighty-02-helper-cli.md). The helper and CLI were run on real Macs, with the owner watching the MagSafe light.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 26.6.2 (25G83) | hold (`CHTE` + `CHIE`) | **Passed**, 2026-11-17/18 |
| M2 Max (Mac14,6) | 27.2 (26B123) | cycle (`CHIE` only) | **Passed**, 2026-11-18, with design findings G–I |

## M5 Pro, hold mode

Setup: macOS Charge Limit 100%, Optimized Charging off. Limits 80% / 75%, auto-discharge on. The test started at 100% on AC. Times are UTC.

| Step | Check | Result | Evidence |
|---|---|---|---|
| Install | `sudo eighty install`, then `eighty status` | Pass | Status shows `Mode hold (CHTE) · MagSafe light: yes` |
| 2 | Auto-discharge with the lid closed (external displays) | Pass | No discharge with the lid closed, as the spec requires. Status said "Holding" at 100% (bug C) |
| 2 | Lid opens above the limit | Pass | Discharge started within one 60 s tick |
| 2 | Auto-discharge 100% → 80% | Pass, after fixes A and B | About 2 h 20 min at 0.6–1 A. At 80% the outputs went to inhibit on, discharge off, light green |
| — | Reinstall mid-discharge | Pass | The old helper restored the keys on shutdown. The new one resumed "Discharging to 80%" within 1 s |
| 1 | Hold at the limit on AC | Pass | 80%, not charging, 0 mA after a fresh reading. Light green |
| 4b | Unplug, then replug during hold | Pass | On battery: light left alone, `CHTE` kept. On replug: Holding, light green |
| 3 | `eighty charge full` | Pass | Inhibit off, light handed to macOS (orange). Unplugging ended the override. After replug: Holding |
| 3 | `eighty discharge 78` | Pass | Discharged to 78%, the override finished, then Holding. Stays at 78% because it is inside the 75–80 band |
| 4.1 | Lid-closed sleep on AC at the limit | Pass | 15 min of sleep with a maintenance dark wake. 77% throughout, no Conflict after wake. macOS did not change `CHTE`/`CHIE` across sleep |
| 4.2 | Lid-open sleep while discharging | Pass | Discharge was forced off at sleep (20:08:20) and resumed in the same second as wake (20:23:29). 77% before and after |
| 5a | `kill -9` mid-discharge | Pass | launchd restarted the helper immediately. It restored the keys on startup and lost the override (overrides are in memory, as expected). At 76% it charged to 80% |
| 5b | Reboot | Pass | Boot 05:26:05. The helper started at 05:26:34, before Finder and Dock, and came up Holding at 80% with the light green |
| 6 | macOS Charge Limit set to 80%, then back to 100% | Pass | No Conflict either way. eighty kept holding at 80%, and macOS did not write the keys eighty controls. `eighty doctor --retry` was not needed |
| 7 | Permissions | Pass | `sudo -u nobody eighty limit 70` → "permission denied: only an admin (sudo) or the logged-in user can change eighty". `sudo -u nobody eighty status` works |
| 8 | `sudo eighty uninstall` | Pass | Helper, plist and socket removed. Config and log kept (`--purge` removes them). The probe read `CHTE=00000000`, `CHIE=00`, `ACLC=04` (macOS's own orange). The battery charged past 80% at +2 A |

### Bugs found and fixed

| Bug | What happened | Fix |
|---|---|---|
| A | While `CHIE=08` is set, macOS reports `ExternalConnected`, `ExternalChargeCapable` and `AppleRawExternalConnected` as No, so the helper thought the Mac was unplugged and stopped discharging. The discharge flapped every second | `a0009e0`. While it is discharging, the helper keeps trusting that AC is connected. After discharge stops, it ignores a lagging "unplugged" reading until a fresh one arrives, for at most three ticks (`5535b46`, `c71a783`) |
| B | The firmware set an extra bit in `CHIE` (`0x18`), which the helper reported as a Conflict, leaving the Mac running from battery on AC | `118e0a7`. Ownership of `CHIE` is judged by bit `0x08` only |
| C | With the lid closed above the limit, status said "Holding at 80%" | `760eb3b`, `424108a`. It now says "Lid closed: holding at N%, discharges to 80% when the lid opens" |
| D | "Holding at" showed the limit, not the current charge | `7064ef2` |
| E | The helper did not log sleep and wake | `59710ca`. Real sleep/wake lines appear in the log since then |
| F | Right after a crash, status briefly said "on battery" | `b97719b` |

Earlier fixes from the same run: the watchdog now needs two failing readings in a row (`b9c6662`), and a Conflict while discharging switches the discharge off (`6247814`).

### Notes

- The installed process is `io.github.mabujaber.eighty.helper`. To kill it for a crash test, use `sudo pkill -9 -f io.github.mabujaber.eighty.helper`.
- Changing the Charge Limit in System Settings caused a sleep/wake of a few seconds in the helper log. It did no harm.
- Known limit: a Mac that starts unplugged with a stale `CHIE=08` above the limit may resume auto-discharge and show "on AC" until it reaches the target. The drain matches a normal unplugged drain, and the 10% floor and lid rule still apply.

## M2 Max, cycle mode

Setup: macOS Charge Limit 100%, Optimized Charging off. Limits 80% / 75% (the defaults). The test started at 81% on AC. Times are UTC.

| Step | Check | Result | Evidence |
|---|---|---|---|
| Install | `sudo eighty install`, then `eighty status` | Pass | Log: `mode Cycle`, `CHTE` Missing, `CHIE` and `ACLC` Usable. Status shows `Mode cycle (CHIE only) · MagSafe light: yes` |
| 1, 2 | Discharge above the upper limit, charge below the lower one | Pass | It started at 81%, so steps 1 and 2 ran as one. Three full cycles, 15:10–17:48: discharge 80 → 75% in about 43 min at −0.8 A with the light green, charge 75 → 80% in about 7 min at +4.9 A with the light orange. Bug A's fix held: macOS reported `ExternalConnected = No` while `CHIE=08`, and status kept saying "on AC" |
| 3 | `eighty charge full`, then unplug and replug | Pass | 18:30:01 `Charging to 100%`, light handed to macOS. 18:30:19 unplug: `override FullCharge finished`. 18:30:26 replug: `Charging to 80%`, light orange |
| 3 | `eighty discharge 75` | Pass | 18:31:04 `Discharging to 75%`. 18:42:28 `override DischargeTo(75) finished`, then `Charging to 80%`. The target equalled the lower limit, so only the log tells the override apart from the normal cycle |
| 8 (mid-test) | `sudo eighty uninstall`, then reinstall | Pass | 18:47:40 `shutting down; restoring charging`. Probe: `CHIE=00`, `ACLC=04`. Charging at +4.4 A from 80%. Reinstalled at 18:49:52 and resumed `Discharging to 75%` |
| 7 | Permissions | Pass | `sudo -u nobody eighty limit 70` → "permission denied: only an admin (sudo) or the logged-in user can change eighty". `sudo -u nobody eighty status` works |
| 5a | `kill -9` while discharging | Pass | Killed three times (18:50:41, 18:50:52, 18:52:06). launchd restarted the helper each time within a second, and it resumed `Discharging to 75%` with the light green |
| 5b | Reboot | Pass | Clean shutdown at 18:54:48 restored charging. Boot 18:55:10. Helper started 18:55:56, before Finder and Dock (18:56:01), and came up `Discharging to 75%` |
| 4.2 | Lid-open sleep while discharging (Apple menu → Sleep) | Pass, see G | Sleep 19:01:42 at 80%: discharge off, light orange, the cycle-mode warning logged. Wake 19:22:53 at 94%: discharge on in the same second. Status showed the warning until the charge came back down |
| 4.3 | Does macOS change `CHIE` across sleep? | No | No Conflict after either wake. `CHIE` read `08` after wake |
| 4.1 | Lid-closed sleep, no external display | Pass, see G | Lid closed 20:39:27 at 89%: discharge off, `activity LidClosed`. Opened 21:21:56 (42 min) at 99%: discharge on |
| 4b, 6 | Watchdog across replug, macOS Charge Limit conflict | n/a | Both test `CHTE`, which this Mac doesn't have |
| 8 | Final `sudo eighty uninstall` | Pass | 21:23:50 `shutting down; restoring charging`. Helper, plist and socket removed. Probe: `CHIE=00`, `ACLC=04` (macOS's own value, as on the M5). Charging at +0.8 A from 99%, "finishing charge" |

### Findings

None of these is a bug in the code: the helper did what the spec asks every time. They are problems with the spec's cycle mode, or gaps left for later. H and I are fixed. G is redesigned as native mode (spec §5.6) and not built yet.

| Finding | What happened | Proposal |
|---|---|---|
| G | **Cycle mode wears the battery and can't hold the limit during sleep.** Plugged in and awake, the Mac runs from its battery about 85% of the time, about one equivalent full cycle per 16 hours on AC. Asleep, it charges at full rate: 80 → 94% in 21 min, and 89 → 99% in 42 min. Overnight on AC it reaches 100%, then eighty runs it on battery down to 75% the next morning. Before eighty was installed, this Mac sat at 80% and 0 mA with no limiter running, which points to macOS 27's own Charge Limit holding it still | Native mode, spec §5.6: on Macs without `CHTE`, eighty drives macOS's own Charge Limit (80/85/90/95) and drops cycle mode. Setting it through PowerUI was verified on this Mac the same day |
| H | **The Battery line contradicts the State line for up to a minute after a switch.** macOS refreshes the battery reading about once a minute, so status showed "discharging 635 mA" next to `Charging to 100%`, and "charging 3685 mA" next to `Discharging to 75%` | Fixed: status shows `updating…` until a reading from `SETTLE_SECS` after the switch arrives |
| I | **The other-limiter check missed the `battery` app.** Its CLI, LaunchAgent and sudoers file were gone, but `/Applications/battery.app` (Homebrew cask) was still a login item. At each login it tries to reinstall its CLI as root through an admin password prompt. eighty doesn't look for the app, so it reported no other limiter | Fixed: `/Applications/battery.app` now counts as the `battery` limiter, and the README says to run `brew uninstall --cask --zap battery` |

### Notes

- The Mac was left with eighty uninstalled. The recommended setting for this Mac until G is done: macOS Charge Limit at 80%.
- The override log records when an override finishes, but not when it starts. The request shows up only as a change in `outputs` and `activity`.
