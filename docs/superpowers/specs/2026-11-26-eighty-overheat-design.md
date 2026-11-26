# eighty — Overheat protection and battery temperature (Plan 4)

Date: 2026-11-26
Parent spec: `docs/superpowers/specs/2026-11-16-eighty-design.md` (§2 "Overheat protection" and "Show the battery temperature", §5.3 "Temperature for overheat protection")
Previous plan: `docs/superpowers/specs/2026-11-22-eighty-calibration-design.md` (merged as PR #3)

## 1. Intent

eighty aims to have every BattOpt feature, for free (parent spec §1–2). BattOpt has **overheat protection**: charging stops while the battery is hot, because charging a hot battery wears it fastest. The owner's question was simply: "why don't we stop charging when the temperature is high?" This plan adds that. It also shows the battery temperature, which the owner asked for on 2026-11-20.

Neither of the owner's Macs has run hot so far (M5 Pro 27.2–28.5 °C, M2 Max 24.0–26.4 °C), so this is for heavier use, warm rooms and eighty's other users. It doesn't fix a problem the owner sees today.

## 2. Goals

1. When the battery temperature reaches the **pause** threshold while the Mac is on the charger, the battery stops charging. Charging comes back once the battery cools to the **resume** threshold.
2. Works in both control modes. In **native** mode (macOS 27, both of the owner's Macs) it works by running the Mac from the battery (`CHIE = 08`). In **hold** mode (`CHTE`, macOS 26) it works by stopping charging (`CHTE` inhibit) without draining.
3. `eighty status` and `eighty doctor` show the temperature, e.g. `Temp 31.4 °C`, or `Temp n/a` when nothing reports it. `--json` includes it as well.
4. `status` says when charging is paused for heat, how hot the battery is, and when it will resume.
5. Commands to turn the feature on or off and to change the thresholds.

## 3. Non-goals

- **SMC temperature keys (`TB0T`/`TB1T`/`TB2T`) as a backup source.** *Claude's call:* not in this plan. The IORegistry reading is verified on both Macs under macOS 27 (parent spec §5.3), and a second, undocumented source adds probing on every model for little gain. If the IORegistry reading disappears on some Mac, protection is off and `doctor` says so (§6.6). The backup can come later.
- **Cooling the Mac**, e.g. limiting CPU or fans. eighty only controls charging.
- **Notifications.** These come with the menu-bar app. Until then, `status` and the log are the only signals.
- **Overheat protection while asleep.** Today's rule turns `CHIE` off before sleep, and that rule stays. A sleeping Mac makes little heat. If the battery is still hot on wake, the pause comes back.

## 4. Anti-goals

- **Never let the battery run flat to avoid heat.** A Mac that dies is worse than a warm battery. In native mode the pause drains the battery, so it has a floor (§6.3).
- **Never leave `CHIE` on by mistake.** Uninstall, a crash, a fault or turning the feature off must end with `CHIE` off and the normal limit in place, following today's restore rules.
- **No flapping.** Charging must not switch on and off every minute around one temperature. That's why there are separate pause and resume thresholds.

## 5. Approaches (owner picked 1, 2026-11-26)

1. **Run the Mac from the battery (`CHIE`) when hot (chosen).** Charging stops at once at any charge level. The battery drains slowly while it cools. It's the same on both Macs and reuses the `discharge` path that already exists. Hold mode uses `CHTE` instead, which stops charging without draining.
2. Lower macOS's Charge Limit to the nearest step (80/85/90/95). This only works above 80%, so it still needs option 1 below that, which means two mechanisms. macOS also drains down to the step it was given.
3. Only show the temperature and rely on macOS's own heat protection. Least work, but there's nothing the user can control.

## 6. Design

### 6.1 Settings

A new `[overheat]` section in the config file. All fields are optional and fall back to the defaults below. An old config without the section loads with protection on.

| Field | Default | Range |
|---|---|---|
| `enabled` | `true` | |
| `pause_c` | `35` | 30–45 °C, whole degrees |
| `resume_c` | `32` | 25 °C up to `pause_c − 2` |

*Claude's call:* the defaults (35 / 32) and the ranges. 35 °C is the top of Apple's stated ideal ambient range for Macs, and the battery itself runs a little above ambient. The ranges stop settings that would never fire or never let charging resume. The values stay whole degrees so the command reads naturally.

CLI (*Claude's call* on the command shape, following `auto-discharge on|off` and `limit <upper> [lower]`):

```
eighty overheat on|off
eighty overheat <pause> [resume]     # e.g. `eighty overheat 38 34`; resume defaults to pause − 3
```

Each one is a new IPC request that saves the config, like `SetLimits`.

### 6.2 When it pauses

The policy takes a new input, the battery temperature (`Option<f32>`), plus a persisted latch, `heat_paused: bool`.

- **Enter the pause** when `enabled`, the temperature is at or above `pause_c`, the Mac is on the charger, **and the battery is charging**. That means the reading reports it charging, or (hold mode) the policy would otherwise leave inhibit off. A battery already held at its limit isn't charging, so there's nothing to stop. Pausing there in native mode would drain it toward the floor and then charge it back up while hot. That's nearly a full cycle at high temperature, which is the opposite of the goal. *Claude's call, from the owner's "stop the charging when the temperature is high". Raised by the builder check.* Once the pause has started it stays on until the exit conditions below, even though the battery no longer reports charging (because `CHIE` or `CHTE` stopped it).
- **Leave the pause** when the temperature is at or below `resume_c`, or when protection is turned off.
- **No reading** (`None`): the latch is cleared and protection is inactive (§6.6).
- On battery the latch can stay set, but it changes nothing because there's no charging to stop. When the charger comes back and the battery is still hot, the pause applies straight away.

The temperature reading refreshes only about once a minute (parent spec §5.3). That is fine for heat, which changes slowly, so no extra polling is added.

### 6.3 What a pause does

**Hold mode:** inhibit on (`CHTE`), `CHIE` off. The battery holds where it is. The light follows today's rule (green while charging is inhibited).

**Native mode:** `CHIE = 08`. The Mac runs from the battery and the battery slowly drains. macOS's Charge Limit stays as it was. When the pause ends, `CHIE` goes off and macOS charges toward its limit as usual. This reuses the existing discharge path, so the existing safety rules apply. These are the sleep rule (`CHIE` off before sleep), the "effective external" latch (`CHIE` can hide the adapter on macOS 27.2), ownership by bit `0x08`, conflict detection and crash/uninstall restore.

**Floor (native mode only).** If the charge falls to **20%** during a pause, eighty turns `CHIE` off and lets the battery charge even though it's hot. It logs this, and `status` shows `battery hot, charging anyway: charge reached 20%`. It doesn't pause for heat again until the charge is back at **30%** or the battery has cooled below `resume_c`. This stops it from bouncing around 20%. *Claude's call:* 20% and 30%. Both are well above `HARD_FLOOR` (10%), and 20% is about where macOS starts warning about low battery.

### 6.4 Lid closed (owner's decision, 2026-11-26: test first, then decide)

Today eighty never forces `CHIE` with the lid closed, because cutting the adapter in clamshell can make macOS sleep. The owner usually works in clamshell with external displays, which is probably when the Mac is hottest. So:

- **First task of the plan (hardware check, owner-run on the M5 Pro with the LG displays):** with the lid closed on AC and the displays awake, set `CHIE = 08` for about 5 minutes. Record whether the Mac stays awake with the displays on, whether macOS still reports the adapter, and whether the battery drains. Then restore `CHIE`. The plan decides the tool. It could be a small `#[ignore]` hardware test or an `eighty-probe` option. The tool must restore `CHIE` on exit and on Ctrl-C.
- **Mac stays awake:** the overheat pause also applies with the lid closed. Only the heat pause gets this exception. `discharge <n>`, auto-discharge and calibration step 1 keep today's lid-closed rule.
- **Mac sleeps:** no heat pause with the lid closed in native mode. `status` shows `battery hot (36.1 °C), can't pause charging with the lid closed`. Hold mode isn't affected either way, because `CHTE` doesn't cut the adapter.
- The result goes in the acceptance doc and in this spec as a dated note. The code keeps the choice in one place (a constant or a mode-specific rule), not spread across the policy.
- *Update (2026-11-26):* **Mac sleeps → `HEAT_PAUSE_LID_CLOSED = false`.** The soak alone looked fine, because macOS was already running from the battery then. But real heat pauses during the acceptance run (battery charging, lid closed, 2 LG displays, M5 Pro / macOS 27.2.1) made macOS report `ExternalConnected = No` and enter `Clamshell Sleep` within about 20 s, on every wake. Lid-closed heat protection waits for clamshell levels (keeping the Mac awake), the next plan. Details in `docs/testing/plan4-acceptance.md`.

### 6.5 Overrides and calibration

Heat wins over everything that would charge:

| Running | During a heat pause |
|---|---|
| Normal limit | Paused as in §6.3 |
| `charge full` | Paused. The override stays in place and charging to 100% resumes after it cools. `status` shows both. |
| `discharge <n>` | Already not charging. Nothing changes. Once it reaches `n`, the heat pause takes over if the battery is still hot. |
| Calibration step 1 (discharge) | Already not charging. Nothing changes. |
| Calibration steps 2–3 (charge, hold) | Paused. Time spent in a heat pause doesn't count toward step 3's hour or toward the 24 h no-progress give-up. This works the same way the calibration already treats waiting. |
| Fault (Conflict, WriteFailed, …) | The fault's handling wins. Nothing gets written to `CHIE` during a fault. |

### 6.6 Temperature display and missing readings

- `status`: one new line, `Temp 31.4 °C` (one decimal), or `Temp n/a`. When paused for heat, the activity line reads `Paused: battery hot (36.1 °C), resumes at 32 °C`.
- `status --json`: `temperature_c` (number or null), `overheat` settings, and `heat_paused`. New fields use `#[serde(default)]` so older CLIs and helpers keep working.
  *Update (2026-11-26):* `--json` reports the pause as `overheat.note` (`paused`, `lid_closed`, `unable`, `charging_at_floor`) instead of a separate `heat_paused` field.
- `doctor`: shows the temperature and the settings (`Overheat: on, pause 35 °C, resume 32 °C`). If no temperature is reported, it warns: `no battery temperature reported; overheat protection is inactive on this Mac`.

### 6.7 Restore and persistence

- `heat_paused` and the floor latch are saved with the helper's state, and they **still count after a crash or reboot**. *Claude's call:* if the helper restarts at 33 °C (between resume and pause) during a pause, the pause continues. Otherwise the battery would charge while still warm, until it reached `pause_c` again. A latch that's stale is harmless, because the next reading at or below `resume_c` clears it.
- On helper start, a leftover `CHIE` is cleared exactly as today and logged. Then the policy runs with the restored latches, so a pause that continues puts `CHIE` back on (native mode, lid rule permitting).
- `eighty overheat off` during a pause ends it straight away (§6.2).
- Uninstall restores as today: `CHIE` off and the Charge Limit put back.

## 7. Testing

- **Unit (policy, pure):** enter and leave at the thresholds, both modes, on battery, no reading, the floor and its 30% re-arm, each override row in §6.5, the fault row, and lid closed both ways (§6.4 constant).
- **Engine:** the persisted latch across restart; the calibration clock not counting during a heat pause; `overheat off` ending a pause; the restore paths.
- **CLI:** parsing and validating `overheat`, plus rendering the temperature, `n/a`, the pause line and the doctor warning.
- **Hardware (owner):** first the lid-closed check in §6.4. Then an acceptance doc `docs/testing/plan4-acceptance.md`. A real 35 °C battery is hard to produce on demand, so the main check temporarily sets `pause_c` to just above the current reading (e.g. `eighty overheat 30 28` with `pause_c` clamped for tests, or a test-only lower bound). Check that charging stops within a couple of minutes, the temperature line is correct, and charging resumes once the threshold goes back up. The plan decides how to allow a low threshold for testing without exposing it in the normal range.

## 8. Left to the builder

- Where the latch lives (in `Override`, a separate field in the engine state, or the policy inputs) and how `Activity` grows (e.g. `Activity::HeatPaused`). Changing `Activity` breaks an older CLI reading a newer helper, the same as the known calibration minor, so add it in a way that degrades gracefully if that's cheap.
- The exact wording of the log lines.
- The tool for the §6.4 hardware check and the testing threshold in §7.
- Updating the parent spec: the §2 overheat line, the §5.3 "open question for Plan 4", and the plan order note.
