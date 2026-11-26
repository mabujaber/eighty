# eighty

Keep your Apple Silicon Mac's battery at 80%, for free.

eighty is a charge limiter written in Rust. A small root helper stops charging at your limit, can run the Mac from battery while it is plugged in to bring the charge down, and turns the MagSafe light green when the limit is reached. It always hands charging back to macOS when it stops or is uninstalled.

> **Status: pre-release.** Hold mode passes hardware acceptance on an M5 Pro running macOS 26.6.2 ([results](docs/testing/plan2-acceptance.md)). Native mode passes hardware acceptance on an M2 Max running macOS 27.2 ([results](docs/testing/plan2b-acceptance.md)). The menu-bar app, signed builds and Homebrew come later. For now you build from source.

## Install

You need Rust (stable) and an Apple Silicon Mac.

```sh
git clone https://github.com/mabujaber/eighty.git
cd eighty
cargo build --release
sudo ./target/release/eighty install
./target/release/eighty status
```

`install` copies the helper to `/Library/PrivilegedHelperTools/` and starts it as a LaunchDaemon, so it runs at boot before anyone logs in. It does not put `eighty` on your `PATH`. Run it from `target/release`, or link it yourself:

```sh
sudo ln -s "$PWD/target/release/eighty" /usr/local/bin/eighty
```

Turn off **Optimized Battery Charging** in System Settings → Battery so it does not fight eighty.

In hold mode, leave the macOS Charge Limit at 100%. In native mode, leave it to eighty: eighty sets it. While eighty is running and in control, a change you make in System Settings → Battery → Charge Limit becomes eighty's limit. A change made while eighty is stopped, while it is waiting for macOS, or during `eighty charge full` doesn't: eighty sets its own limit again. `eighty uninstall` puts back the limit you had before eighty first took control.

## Use

```text
$ eighty status
Battery   80% · not charging · on AC
Limit     80% (resumes below 75%) · auto-discharge on
State     Holding at 80%
Mode      hold (CHTE) · MagSafe light: yes
```

| Command | What it does |
|---|---|
| `eighty status [--json]` | Battery, limits and what the helper is doing |
| `eighty limit <upper> [lower]` | Set the limit (20–100). Charging resumes below `lower`, which defaults to `upper − 5`. In native mode only 80, 85, 90, 95 and 100 (no limit) work, and there is no lower limit |
| `eighty auto-discharge on\|off` | When the battery is above the limit on AC, run from battery until it is back at the limit (hold mode only; in native mode macOS always does it, so `on` changes nothing and `off` is refused) |
| `eighty charge full` | Charge to 100% once. It ends when you unplug. `eighty charge auto` ends it now |
| `eighty discharge <percent>` | Run from battery on AC down to `percent`, then hold. Needs the charger plugged in |
| `eighty calibrate` | Run a full calibration now (see below) |
| `eighty calibrate cancel` | Stop a calibration and go back to your limit |
| `eighty calibrate schedule every <N> days [at HH:MM]` | Run a calibration every N days (7–90) at that time (default 10:00). The first run is N days after you turn it on, or after the last calibration if that is later. While `charge full` or `discharge` runs, a due run waits for it |
| `eighty calibrate schedule off` | Turn the schedule off |
| `eighty overheat on\|off` | Turn overheat protection on or off (on by default). When the battery reaches the pause temperature while charging on AC, eighty pauses charging, and it resumes when the battery cools. In native mode this runs the Mac from battery meanwhile, down to a 20% floor |
| `eighty overheat <pause> [resume]` | Set the temperatures in °C. Pause 30–45 (default 35), resume 25 up to pause − 2 (default pause − 3, so 32). `eighty status` shows `Temp` and, while paused, why |
| `eighty doctor [--retry]` | Check SMC support and helper health. `--retry` clears a recorded fault |
| `eighty logs` | The last lines of the helper log |
| `sudo eighty uninstall [--purge]` | Stop the helper, restore normal charging and remove it. `--purge` also deletes settings and logs |

A calibration runs the battery through one full cycle: down to 15%, up to 100%, an hour at full, then back to your limit. It helps the battery gauge relearn its capacity. It takes several hours, so keep the Mac on the charger with the lid open. Closing the lid, sleeping or rebooting pauses it, and it carries on from the step it reached. Unplugging pauses steps 2 and 3 (charging and the hour at full); step 1 keeps going on battery. While it runs, `eighty status` shows the step. A Charge Limit changed in System Settings during a calibration is set back, and a fault puts your limit back on macOS's Charge Limit until it clears. Details: [calibration design](docs/superpowers/specs/2026-11-22-eighty-calibration-design.md).

Changing settings needs an admin (`sudo`) or the user logged in at the console. Anyone can read the status.

## How it works

The helper talks to the SMC directly. It does not shell out to other tools. It checks the battery once a minute and reacts at once to plug, unplug, sleep and wake.

| Key | Role | Values |
|---|---|---|
| `CHTE` | Stop charging | `01000000` stop, `00000000` allow |
| `CHIE` | Run from battery on AC | `08` on, `00` off |
| `ACLC` | MagSafe light | `03` green, `04` orange, `00` macOS decides |

There are two modes, chosen from what the Mac supports:

- **Hold** (`CHTE` works, for example M5 Pro on macOS 26): stop charging at the limit and hold there. Auto-discharge brings a higher charge down to the limit.
- **Native** (no stop-charging key, for example M2 Max on macOS 27): eighty sets macOS's own Charge Limit (80, 85, 90 or 95%), and macOS holds it. macOS brings a higher charge down to the limit itself and sets the MagSafe light. There is no lower limit. If macOS charges past the limit (it does this to calibrate), `eighty status` shows a warning. eighty can't stop it.

Safety rules:

- It never discharges below 10%, and never discharges with the lid closed.
- At sleep it switches discharge off, and switches it back on at wake.
- If something else changes these keys (another limiter, or macOS), eighty reports a Conflict, switches its own discharge off and stops writing. `eighty doctor --retry` takes control again.
- If writes don't take effect on two fresh readings in a row, it reports a fault and restores normal charging.
- On shutdown, crash recovery and uninstall in hold mode, it restores `CHTE=00000000`, `CHIE=00` and `ACLC=00`, so a removed or stopped eighty never leaves a Mac unable to charge.

In native mode:

- eighty writes `CHIE` only for an explicit `eighty discharge <percent>` and for step 1 of a calibration (down to 15%). The 10% floor, lid-closed and sleep rules above still apply to both.
- On shutdown, crash recovery and faults, it writes `CHIE=00`. Otherwise it leaves macOS's Charge Limit as it is, because macOS enforces it and keeps holding while eighty is stopped. The exception is a calibration that lifted the limit: a fault, or stopping the helper mid-calibration, puts your limit back (the calibration carries on once eighty runs again). A shutdown or reboot during steps 2–3 can't do this, because macOS has already closed the Charge Limit service by then, so macOS keeps 100% until eighty starts after boot and resumes. With another limiter installed, eighty doesn't touch the Charge Limit at all unless a calibration is loaded.
- `eighty uninstall` also puts back the limit you had before eighty first took control.

Files:

- Settings: `/Library/Application Support/eighty/config.toml`
- Running calibration: `/Library/Application Support/eighty/calibration.toml` (only while one runs)
- Overheat settings: `/Library/Application Support/eighty/heat.toml`
- Log: `/Library/Logs/eighty/helper.log`
- Socket: `/var/run/eighty.sock`

## Other charge limiters

Run only one limiter at a time. Other tools write the same keys. Uninstall them fully before installing eighty. For [battery](https://github.com/actuallymentor/battery), run `battery uninstall` before `brew uninstall`, or its LaunchAgent keeps running. Then remove the app with `brew uninstall --cask --zap battery`: at every login it reinstalls its CLI as root. If eighty finds another limiter installed, it stays paused and `eighty doctor` names the limiter. `eighty doctor --accept-other-limiters` makes eighty run anyway.

## Check your Mac

`eighty-probe` reports which keys exist on your Mac. `sudo eighty-probe --write-test` checks that they work, and restores every value afterwards. Results so far are in [docs/probe](docs/probe/README.md).

## License

MIT
