# Plan 2 hardware acceptance

Task 12 of [Plan 2](../superpowers/plans/2026-11-17-eighty-02-helper-cli.md). The helper and CLI were run on real Macs, with the owner watching the MagSafe light.

| Mac | macOS | Mode | Status |
|---|---|---|---|
| M5 Pro (Mac17,8) | 26.6.2 (25G83) | hold (`CHTE` + `CHIE`) | **Passed**, 2026-11-17/18 |
| M2 Max (Mac14,6) | 27.2 (26B123) | cycle (`CHIE` only) | Pending |

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

Pending.
