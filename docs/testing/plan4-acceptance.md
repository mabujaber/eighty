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
| | | | |

```
(paste output here)
```
