# eighty — Design

Date: 2026-11-16
Repo: https://github.com/mabujaber/eighty

## 1. Intent

`eighty` is a free, open-source battery charge limiter for Apple Silicon Macs, written in Rust. It keeps the battery at or below a user-set limit (default 80%) to slow battery wear on Macs that live on the charger.

**Why it exists:**
- [actuallymentor/battery](https://github.com/actuallymentor/battery), the popular free option, has been unreliable across recent macOS updates. The legacy SMC keys it used to write (`CH0B`, `CH0C`, `CH0I`) read `no data` on recent builds (issues #474, #482, #460, #461, #477, #487), and charging silently went to 100%. *Correction (2026-11-16):* the version installed on the owner's M5 Pro (macOS 26.6.2) did hold the battery at 80% and set the MagSafe light green, through the Tahoe-era `CHTE` key. So `battery` can work on current macOS. Its remaining problems are polling (§5.4), broad passwordless sudo (§4), and an uninstall path that can leave a Mac unable to charge (§7).
- [js4jiang5/BattOpt](https://github.com/js4jiang5/BattOpt) works on current macOS but puts some features (clamshell mode, zone awareness, smart resume calibration) behind a paid Pro tier.
- The owner wants one tool, released publicly, that has all of these features for free and that keeps working across macOS updates.

## 2. Goals

1. Hold the battery at the configured limit on the owner's machine (Apple M5 Pro, macOS 26.6.2) and on Apple Silicon Macs running current macOS generally.
2. Ship a **CLI** (`eighty`) and a **menu-bar app** (Eighty.app). Both control the same background helper and share the same state.
3. Include every Mac feature BattOpt offers, free and Pro:
   - **Charge limiter** with upper and lower limits: stop charging at the upper limit, start again when the charge falls below the lower limit. Defaults: 80 / 75 (builder's call, see §9).
   - **Discharge to limit:** if the charge is above the upper limit while plugged in, run from the battery (adapter inhibited) down to the limit, as `battery` does. *Claude's call:* this runs automatically by default, can be turned off in settings, and is also available on demand via `discharge <n>`. With the lid closed (clamshell), eighty never forces a discharge, because cutting the adapter can make macOS sleep. It holds the current charge instead and resumes discharging when the lid opens.
   - **Calibration:** a full cycle (discharge to ~15%, charge to 100%, hold briefly, return to the limit). It can be run by hand or on a schedule.
   - **Smart resume calibration:** a calibration interrupted by unplugging, sleep or reboot continues from the step it reached instead of restarting or being abandoned.
   - **Overheat protection:** pause charging when the battery temperature goes above a threshold, and resume when it cools.
   - **Sleep, shutdown and reboot persistence** (owner's decision, two separate cases):
     - **Sleep:** the limit keeps being enforced as normal. If the charge is at or above the limit, charging stays blocked. If it is below the limit and charging, the helper schedules short wakes (e.g. `IOPMSchedulePowerEvent` dark wakes) to check progress and blocks charging when the limit is reached. *Claude's interpretation of "keep blocking the charging as normal"; the builder picks the wake interval.* If the firmware turns out to enforce the limit on its own (see §5.2), use that and skip the wakes.
     - **Shutdown:** accepted that eighty can't act while the Mac is off. The Mac may charge past the limit. At boot (before login) the helper re-applies the limit. If the charge went over while off, it discharges down to the limit (per the discharge rule) and the status shows what happened, so it isn't silent.
   - **Clamshell mode support:** limits keep working with the lid closed on an external display, including on wake events where the helper might otherwise miss the transition.
   - **Zones:** named profiles (Home / Work / Travel, user-editable) with their own limits, picked by the current Wi-Fi network name (up to 4 per zone, as in BattOpt). Travel can mean "charge to 100%".
   - **MagSafe charging light** (owner's requirement): the light on the MagSafe connector shows eighty's state, not macOS's guess.
     - **Green** when the battery has reached the limit (80% or whatever the user set) and is being held there.
     - **Orange** while charging toward the limit, as normal.
     - While discharging down to the limit, the light shows green, since the limit is already reached. *Claude's call.*
     - During calibration's charge-to-100% step, the light is left to macOS.
     - Uninstalling, or the helper stopping, hands the light back to macOS (system control).
     - Only Macs with a MagSafe 3 port have this light. Charging over USB-C has no light, and that isn't treated as an error.
4. Release publicly through **Homebrew**: a cask for the signed and notarized app, which also installs the CLI.
5. **Fail loudly.** If eighty can't control charging on a machine (unknown firmware, keys missing, write rejected), it says so in the CLI, the menu bar and a notification. It never lets the battery quietly reach 100% while showing "limited".

## 3. Non-goals

- Intel Macs and macOS releases older than what current Apple Silicon firmware needs. The minimum is the builder's call, but macOS 14+ is a reasonable floor.
- BattOpt's Android phone monitoring ("Andish") features.
- Bootcamp/Windows support (not relevant on Apple Silicon).
- A paid tier, accounts, telemetry, or licensing of any kind.
- A Mac App Store release. A privileged helper writing SMC keys won't pass review.

## 4. Anti-goals

- **No copying of BattOpt's code or assets.** BattOpt's code is MIT-licensed and may be read to learn its approach, but eighty is written from scratch. Its brand name and logo are proprietary and must not be used. No cracking or bypassing of anyone's licensing.
- **No broad passwordless sudo.** `battery` puts NOPASSWD sudoers entries on a user-writable binary path, which its issue #471 flagged as a security hole. eighty must not do this. Privilege lives only in a signed helper with a narrow, authenticated interface.
- **No silent failure.** A tool that reports "limited to 80%" while charging to 100% is worse than no tool.
- **No arbitrary SMC writes.** The helper exposes only intent-level operations ("inhibit charging", "allow charging", "force discharge on/off"). It never offers "write key X = Y" to callers.

## 5. Architecture (owner-approved shape)

### 5.1 Components

```
┌────────────┐   ┌──────────────┐
│ eighty CLI │   │ Eighty.app   │  (menu bar, runs as user)
└─────┬──────┘   └──────┬───────┘
      │   XPC / local socket (authenticated)
      └────────┬────────┘
        ┌──────▼────────┐
        │ eighty helper │  LaunchDaemon, runs as root, starts at boot
        │  - policy loop│
        │  - SMC backend│
        └──────┬────────┘
               │ IOKit (AppleSMC) / fallback backends
            Hardware
```

- **Helper (root daemon):** the only component that touches hardware. It owns the policy loop: it reads charge, temperature, power source, Wi-Fi network and calibration state, then decides whether to charge, hold or discharge. It persists config and state to disk (e.g. `/Library/Application Support/eighty/`). It is registered via `SMAppService.daemon(plistName:)` from the signed app bundle (macOS 13+ API), so the user approves it once in System Settings → Login Items.
- **CLI:** a thin client. It sends commands to the helper and prints status. It works without the menu-bar app running. Command surface (builder may refine names): `status`, `limit <upper> [lower]`, `charge on|off` (temporary override), `discharge <n>`, `calibrate [now|schedule …|cancel]`, `zone list|add|edit|remove`, `doctor` (prints firmware/macOS info and which hardware control backend is working), `logs`.
- **Menu-bar app:** shows the current charge, the limit, the state (charging / holding / discharging / calibrating / overheated / **unsupported**) and the active zone. It offers quick toggles and a settings window. It is also the installer for the helper.

### 5.2 Hardware control backends

This is the riskiest part and the reason the project exists. Which SMC keys control charging varies by firmware and macOS version, and Apple has been changing them in 2026. Design for this explicitly:

- Define a `ChargeControl` trait with operations such as `inhibit_charging`, `allow_charging`, `force_discharge(bool)`, `read_state`, and `probe() -> Capability`.
- Implement backends, then probe at startup and pick the first that **verifiably** works: write a value, read it back, and confirm the expected change in IOKit's charging state.
  - **Legacy SMC:** `CH0B`/`CH0C` (inhibit), `CH0I` (discharge).
  - **Tahoe-era SMC:** `CHTE` (inhibit, 4 bytes), `CHIE` / `CH0J` (discharge).
  - **Firmware charge-limit registers:** `bfD0`/`bfE0`/`bfF0` (upper limit / lower limit / enable), reported in battery#482.
  - **PowerUI / MCL** (private `PowerUISmartChargeClient`, `setMCLLimit:`), reported in battery#474 as the route on macOS 27, where direct SMC writes return `kIOReturnNotPrivileged`. It only supports fixed steps (80/85/90/95/100), so features that need arbitrary limits degrade, and that degradation must be shown to the user. *Claude's call:* macOS 27 isn't testable yet, so v1 reserves this backend's slot in the trait and probe order but doesn't implement it. Work on it starts once macOS 27 betas can be tested.
- **If no backend works** (on the owner's Mac or any other): the project continues (owner's decision). eighty reports "unsupported on this firmware/macOS" through `doctor`, the CLI and the menu bar. It does nothing to charging, and it keeps probing on each launch and after OS updates so it picks up a working path when one appears (e.g. a new backend in a later release).
- **MagSafe light:** the SMC key `ACLC` (seen still readable on newer builds in battery#482) controls the light. `battery` uses 0 = system control, 3 = green, 4 = orange. The probe spike confirms which values work on the owner's Mac. The helper exposes this as intent-level operations (`set_light(Green|Orange|System)`), never as raw key writes.
- **First milestone is a probe spike on the owner's M5 Pro / macOS 26.6.2:** find which keys exist, which accept writes from a root process, and which actually change charging behavior. The backend order and the default key set come from that result, not from this document.
- **Probe results:** see `docs/probe/README.md`. On the owner's M5 Pro / macOS 26.6.2 (2026-11-16): `CHTE` = `01000000` stops charging, `CHIE` = `08` makes the Mac run from the battery while plugged in, `ACLC` = `03` sets the light green and `04` orange. `CH0J` is blocked for root, and `CH0B`/`CH0C`/`CH0I` are missing. The Tahoe-era backend comes first in the probe order. The M4 Pro Max result is still pending.
- Prior art to check against: `charlie0129/batt` (Go; reported working on Sequoia 15.8 in battery#482), `anarchy89/batterycharging`, and `battery`'s PRs #469 and #486.

### 5.3 Reading battery state

Use IOKit's `AppleSmartBattery` registry entry (`CurrentCapacity`, `MaxCapacity`, `IsCharging`, `ExternalConnected`, `Temperature`, cycle count, etc.) and `IOPSCopyPowerSourcesInfo` for power-source changes. Don't shell out to `pmset` / `ioreg` / `smc` binaries.

**macOS only refreshes these readings about once a minute.** `UpdateTime` (Unix seconds) says when the readings were last refreshed. Anything that checks the effect of a write, such as backend verification or "charging stopped" checks, must wait for a reading whose `UpdateTime` is after the write plus a settle time. Reading straight after a write returns stale data. That's how the first probe run wrongly reported `CHTE` as having no effect.

### 5.4 Events, not polling

`battery` polls every 60 s, which overshoots the limit and misses sleep transitions. The helper instead reacts to:
- power-source and battery capacity change notifications (IOKit / IOPS)
- system sleep/wake (`IORegisterForSystemPower`). Before sleep, if the charge is at or above the limit, inhibit charging so the battery doesn't charge to 100% while the helper is suspended.
- Wi-Fi network changes (for zones)
- a slow fallback timer as a safety net

### 5.5 Coexistence with macOS's own features and other limiters

Recent macOS (26.4+) has a native charge limit and Optimized Battery Charging. BattOpt tells users to set the native limit to 100% and turn off Optimized Charging. eighty's `doctor` and first-run flow detect a conflicting native setting and tell the user what to change. Whether eighty can set these itself is left to the builder.

**Other charge limiters write the same keys.** On the owner's Mac, `battery`'s LaunchAgent (`com.battery.app`) kept running after `brew uninstall` and held `CHTE` at `01`, so the Mac stayed at 80% even with the native limit at 100%. eighty must:
- detect other limiters at first run and in `doctor`: `battery` (`~/Library/LaunchAgents/battery.plist`, `/usr/local/co.palokaj.battery`, `/etc/sudoers.d/battery`), BattOpt, `batt`, and similar tools. It warns about them and doesn't start its policy loop until the user removes them or confirms.
- read `CHTE`/`CHIE` before writing and not assume it's the only writer. If a value changes when eighty didn't write it, the helper reports a conflict and doesn't fight over the key.

## 6. Release bar (owner's decision)

v1 ships via Homebrew after it has been tested on **2–5 Apple Silicon Macs** the owner will test on, across several M-series generations where possible. Other machines rely on the startup probe plus fail-loudly. Issue templates should ask for `eighty doctor` output.

## 7. Constraints

- **Language:** Rust (owner's choice). Use `objc2` / `objc2-foundation` / `objc2-app-kit` for Objective-C and AppKit, `core-foundation` / `io-kit-sys` (or hand-written FFI) for IOKit. The menu-bar UI should use native AppKit through `objc2` (builder's call, see §9).
- **Hardware:** Apple Silicon only. The development machine is an M5 Pro on macOS 26.6.2.
- **Distribution:** Homebrew cask in a tap the owner controls (e.g. `mabujaber/homebrew-tap`), pointing at a GitHub Release `.dmg` or `.zip`. The owner has an Apple Developer account. The app and helper must be signed with a Developer ID and notarized, because `SMAppService` daemons need a signed bundle. Release builds go through GitHub Actions with signing secrets.
- **License:** MIT (matching the prior art). Builder's call, but confirm with the owner before release.
- **Uninstall:** `eighty uninstall` and the cask's `uninstall`/`zap` stanzas must unregister the helper and **restore normal charging** (re-enable charging, clear inhibit/discharge keys, and return the MagSafe light to system control). Otherwise a removed tool could leave a Mac unable to charge. A plain `brew uninstall` must do this too, not only `eighty uninstall`. The cask's `uninstall` stanza stops the helper and runs the restore step before files are deleted. `battery` got this wrong on the owner's Mac: `brew uninstall` removed the app but left its LaunchAgent running with charging switched off.

## 8. Safety requirements

- On helper crash, exit or uninstall, leave the machine in a charging-allowed state wherever possible. On startup, recover from a stale inhibit or discharge state.
- Never allow forced discharge to take the battery below a hard floor (e.g. 10%) except during an explicit calibration step, and stop calibration discharge at its floor.
- The IPC interface authenticates callers (XPC code-signing requirement or peer-credential check). Any local user may read status. Changing settings requires the console user or an admin (builder's call on the exact rule).

## 9. Left to the builder (calls Claude made, not the owner)

- Default lower limit (75) and whether the lower limit is optional.
- CLI command names and flags beyond the shape in §5.1.
- IPC mechanism: XPC via `objc2` versus a root-owned Unix socket with peer-credential checks.
- Menu-bar UI toolkit: native AppKit via `objc2` is preferred over Tauri/web views for size and native feel, but the builder may choose.
- Config file format (TOML suggested) and exact paths.
- Overheat threshold default (e.g. 35 °C pause / 32 °C resume) and calibration floor values.
- Wi-Fi network name access: on macOS 14+ reading the network name needs Location Services permission. The menu-bar app requests it and passes the network name to the helper. The builder confirms the exact API.
- Minimum supported macOS version.
- Crate/workspace layout (suggested: `eighty-core`, `eighty-helper`, `eighty-cli`, `eighty-app`).

## 10. Open risks

- **Hardware control may not be possible on future macOS.** Apple is tightening SMC write access (battery#474). The probe-first design and the PowerUI backend reduce this risk without removing it. The fail-loudly rule keeps a lost control path from becoming silent harm.
- Private APIs (PowerUI) can change without notice and are not usable on the App Store. That's acceptable for a Homebrew release.
- Firmware differences between M-series generations mean "works on the owner's M5 Pro" does not guarantee it works on an M1. Issue templates should ask for `eighty doctor` output.
