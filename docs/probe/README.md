# Hardware probe results

`eighty-probe` checks which SMC keys control charging on a Mac. These results set the backend order and key values for the eighty helper (Plan 2).

- **Read-only run:** `eighty-probe --json <file>`
- **Write test:** `sudo eighty-probe --write-test --json <file>`. Every value is restored afterwards.

## Summary

| Mac | macOS | Stop charging | Run from battery on AC | MagSafe light | Files |
|---|---|---|---|---|---|
| M5 Pro (Mac17,8) | 26.6.2 (25G83) | `CHTE` ✅ | `CHIE` ✅ | `ACLC` ✅ | `2026-11-16-m5pro-macos26.6.2-*.json` |
| M2 Max (Mac14,6) | 27.2 (26B123) | ❌ none | `CHIE` ✅ | `ACLC` ✅ | `2026-11-17-m2max-macos27.2-*.json` |

## M5 Pro, macOS 26.6.2 (2026-11-16)

| Key | Backend | Role | Value written | Result | Evidence |
|---|---|---|---|---|---|
| `CH0B` | legacy | stop charging | — | missing | key doesn't exist |
| `CH0C` | legacy | stop charging | — | missing | key doesn't exist |
| `CH0I` | legacy | run from battery | — | missing | key doesn't exist |
| `CHTE` | tahoe | stop charging | `01000000` (off: `00000000`) | **Verified** | charging at 1197 mA → not charging, 0 mA |
| `CHIE` | tahoe | run from battery | `08` (off: `00`) | **Verified** | charging at 1272 mA → discharging at −410 mA |
| `CH0J` | tahoe | run from battery | — | blocked | `kIOReturnNotPrivileged` even as root |
| `ACLC` | magsafe | light | `03` green, `04` orange | **Works** | owner saw green, then orange |

Read-only keys: `bfD0` = `0000`, `CHIB` = `00`, and `bfE0`/`bfF0` are missing. The firmware charge-limit registers aren't usable here.

All keys were back to `CHTE=00000000`, `CHIE=00` and `ACLC=04` after the run. The battery was at 99% during the test: the tool allowed that then, but the write test now refuses above 95%.

### Notes from this run

1. **The first read-only report is not a clean baseline.** At the time, a leftover `battery` LaunchAgent (`com.battery.app`, still running after `brew uninstall`) was holding `CHTE` at `01`. The Mac stayed at 80% even with macOS's own Charge Limit at 100%. `battery uninstall` removed it, and charging came back right away.
2. **The first write test was run unplugged.** Every test came back `CannotVerify`, because the probe needs the Mac to be charging.
3. **The second write test used the default 15-second wait.** `CHTE` was reported as `NoEffect` because macOS only refreshes battery readings about once a minute. The "after" reading was identical to the "before" one (charging, 1448 mA). With `--settle-secs 75`, `CHTE` and `CHIE` were both verified. The probe now waits for a reading whose `UpdateTime` is at least the settle time after the write, so the default works.

## M2 Max, macOS 27.2 (2026-11-17)

| Key | Backend | Role | Value written | Result | Evidence |
|---|---|---|---|---|---|
| `CH0B` | legacy | stop charging | — | missing | key doesn't exist |
| `CH0C` | legacy | stop charging | — | missing | key doesn't exist |
| `CH0I` | legacy | run from battery | — | missing | key doesn't exist |
| `CHTE` | tahoe | stop charging | — | missing | key doesn't exist |
| `CHIE` | tahoe | run from battery | `08` (off: `00`) | **Verified** | charging at 2732 mA → discharging at −421 mA (fresh reading, 72 s after the write) |
| `CH0J` | tahoe | run from battery | — | blocked | `kIOReturnNotPrivileged` even as root |
| `ACLC` | magsafe | light | `03` green, `04` orange | **Works** | owner saw green, then orange |

Read-only keys: `CHIB` = `00`. `bfD0`/`bfE0`/`bfF0` exist but are **blocked even for root** (`kIOReturnNotPrivileged`). On the M5 they were empty or missing.

After the run, the Mac was charging again at 2871 mA. A read-only run afterwards confirmed `CHIE=00` and `ACLC=04`, the same as before the test.

### Notes from this run

1. **No key can stop charging on macOS 27.2.** `CHTE` and the legacy keys are gone, and the firmware limit registers are blocked. eighty can still hold a limit with `CHIE` alone: turn it on at the upper limit, so the Mac runs from its battery, and off at the lower limit, so the charger takes over again. The battery cycles gently between the two limits; it doesn't sit still the way it does with `CHTE`. The other option on macOS 27 is Apple's own charge limit (PowerUI / MCL, battery#474), which only supports fixed steps.
2. **macOS 27 doesn't report `Temperature`** in `AppleSmartBattery`. The probe originally failed to read the battery at all, until temperature (and cycle count, as a precaution) became optional (`1e32ead`). eighty's overheat protection needs another temperature source on macOS 27.
3. **A `battery` leftover was here too.** `brew uninstall` removed its files, but `com.battery.app` was still registered with launchd. It wasn't running (exit `-15`). `launchctl remove com.battery.app` cleared it.

## Comparison

| | M5 Pro / macOS 26.6.2 | M2 Max / macOS 27.2 |
|---|---|---|
| Stop charging | `CHTE` | — |
| Run from battery on AC | `CHIE` | `CHIE` |
| MagSafe light | `ACLC` | `ACLC` |
| Firmware limit keys (`bf*`) | empty or missing | blocked for root |
| Battery temperature | reported | **not reported** |

**For Plan 2:** `CHIE` works on both Macs, so it's the backbone that works on both. Where `CHTE` exists, use it to hold the battery still at the limit. Where it doesn't, alternate `CHIE` on and off between the upper and lower limits. `eighty doctor` should say which mode is active.

**Agreement:** the two Macs agree on the run-from-battery key (`CHIE`) and the light (`ACLC`). They disagree on stopping charging: `CHTE` on the M5 / macOS 26, none on the M2 / macOS 27. The startup probe in Plan 2 tries `CHTE` first, then falls back to the `CHIE`-only mode.

**`inhibit=none` on the M2 (spec §5.2):** the project continues. Plan 2 also investigates the other routes on macOS 27: whether `bfD0`/`bfE0`/`bfF0` can be reached another way, and Apple's own charge limit (PowerUI / MCL).

## How to run on another Mac

1. Copy `target/release/eighty-probe` over (AirDrop is fine).
2. Make sure no other charge limiter is running: `launchctl list | grep -i batt`.
3. In System Settings → Battery → Charging, set Charge Limit to **100%** and turn **Optimized Battery Charging** off.
4. Plug into MagSafe. The Mac must be **charging** and between **20% and 95%**.
5. Run `./eighty-probe --json <date>-<chip>-macos<ver>-read.json`.
6. Run `sudo ./eighty-probe --write-test --json <date>-<chip>-macos<ver>-write.json`. It takes about 5–8 minutes. Watch the MagSafe light at the end (`ACLC`): green, then orange, then back to how it was.
7. Add a row to the summary table above.
