# Ergohaven K:04 Series

One firmware crate for K:04, K:04 Mini, and K:04 Micro in both Standalone and
Qube topologies. Module settings, pointing devices, battery reader, layer
names, dependencies, and build logic are shared. Connection topology is still
selected at compile time by the binary and profile.

- Standalone: `central` is the left half with a local matrix; `peripheral` is
  the right half.
- Qube: `qube` is the matrix-less USB dongle; `left` and `right` are BLE
  peripherals.

Each topology and model keeps its own matrix, factory keymap, Vial definition,
Product ID, Vial keyboard ID, storage, and UF2 artifact.

| Topology | Profile | Keyboard config | Vial config | Matrix | Product ID |
|----------|---------|-----------------|-------------|--------|------------|
| Standalone | K:04 | `keyboard.toml` | `vial.json` | 10×6 | `0x0074` |
| Standalone | Mini | `keyboard_mini.toml` | `vial_mini.json` | 8×6 | `0x0075` |
| Standalone | Micro | `keyboard_micro.toml` | `vial_micro.json` | 8×6 | `0x0076` |
| Qube | K:04 | `keyboard_qube.toml` | `vial_qube.json` | 10×6 | `0x0071` |
| Qube | Mini | `keyboard_qube_mini.toml` | `vial_qube_mini.json` | 8×6 | `0x0072` |
| Qube | Micro | `keyboard_qube_micro.toml` | `vial_qube_micro.json` | 8×6 | `0x0073` |

## Build

```sh
KEYBOARD_TOML_PATH="$PWD/keyboard.toml" \
VIAL_JSON_PATH="$PWD/vial.json" \
CARGO_TARGET_DIR=target/k04 \
cargo build --release --bin central --bin peripheral --bin hardreset --features production_v30g

KEYBOARD_TOML_PATH="$PWD/keyboard_mini.toml" \
VIAL_JSON_PATH="$PWD/vial_mini.json" \
CARGO_TARGET_DIR=target/mini \
cargo build --release --bin central --bin peripheral --bin hardreset --features production_v30g

KEYBOARD_TOML_PATH="$PWD/keyboard_micro.toml" \
VIAL_JSON_PATH="$PWD/vial_micro.json" \
CARGO_TARGET_DIR=target/micro \
cargo build --release --bin central --bin peripheral --bin hardreset --features production_v30g
```

Qube K:04:

```sh
KEYBOARD_TOML_PATH="$PWD/keyboard_qube.toml" \
VIAL_JSON_PATH="$PWD/vial_qube.json" \
CARGO_TARGET_DIR=target/qube/k04/dongle \
cargo build --release --bin qube --no-default-features --features qube

KEYBOARD_TOML_PATH="$PWD/keyboard_qube.toml" \
VIAL_JSON_PATH="$PWD/vial_qube.json" \
CARGO_TARGET_DIR=target/qube/k04/halves \
cargo build --release --bin left --bin right --no-default-features --features qube-half
```

Use the matching `*_mini` or `*_micro` pair for the other Qube models.
`--no-default-features` keeps Qube's USB-log backend separate from the
Standalone `defmt` backend.

The repository build matrix builds all six profiles:

```sh
./scripts/build_k04_matrix.sh
```

## Battery

The halves use `src/battery_nrf.rs`, which samples `P0_31` without
`calibrate().await` and re-publishes `BatteryStatusEvent` periodically.

## USB power indication

On the base layer, a dim cyan LED means USB power is present. It does not
confirm that the battery is connected, charging, or full. This applies to
K:04, Mini, and Micro, both standalone and with Qube. The battery percentage
does not change this color; the former yellow/green charging indication is
no longer used. Host-status pulses interrupt the power or layer colour and
restore it between pulse groups. A short dark separator after each host pulse
keeps the rhythm visible even when a custom layer colour matches the pulse.

| State | LED indication while awake |
| --- | --- |
| Host connected through the active USB/Bluetooth transport | White for one second on connection, then power/layer colour |
| Reconnecting to a Bluetooth host | One short white pulse every 1.5 seconds |
| No host connection and no advertising attempt | Two short white pulses every 3 seconds |
| Pairing with a new Bluetooth host | Purple pulse every 300 ms |
| Split link searching | Three amber pulses every 2 seconds, overriding host/power indication |
| USB power present, charge state unknown | Dim cyan on the base layer between host-status pulses |
| Sleeping | Off |

An available USB host suppresses background Bluetooth advertising indications
when USB is the active transport. Qube halves use the host status forwarded by
the dongle; their own USB cable only establishes local power presence.
Restoring a split link flashes white only when a host transport is also ready.

The existing `Charge indicator` toggle (QSID 331) enables this USB power
indication. Its label and stored bit are retained for compatibility with
configurators that discover it by that name. It controls the cyan power colour;
connection patterns remain visible with it disabled. Brightness scales every
indicator, including the connection pulses.

For charging, leave each half's physical power switch ON, as required by the
[K:04 guide](https://docs.eh.works/device/k04/#зарядка-мониторинг-заряда).
The firmware reads USB VBUS but has no configured input for the physical
switch or charger status, so it cannot distinguish actual charging from USB
power alone. The battery protocol therefore reports an unknown charge state
while USB power is present. Battery levels remain based on unplugged
measurements to avoid false full readings from charging voltage; a percentage
shown while plugged in is the last unplugged measurement. Battery-level query
colours describe that estimate, not charger completion. Actual charging and
charged indications require a verified charger-status input.

Run `python3 scripts/test_k04_usb_power_indicator.py` from the repository root
to check both LED implementations on the host. It exercises their USB color
branches, brightness scaling, layer/VBUS/toggle gates, host/split priority, and
unknown charge-state reporting; it does not verify
the physical switch, charge current, or the LED's appearance on hardware.

## Qube display

K:04, Mini, and Micro Qube dongles expose a **Qube display** settings group
in compatible Entropy versions (Modules page):

| QSID | Setting | Values | Default |
|------|---------|--------|---------|
| 336 | Display brightness | 0–100 percent (0 turns the backlight off) | 100 |
| 337 | Display background | 0: Black, 1: Blue | Black |
| 338 | Display text size | 0: Compact, 1: Large | Large |

Brightness uses 16 kHz PWM on the display backlight pin and is independent of
the halves' LED brightness. Sleep turns the backlight off; wake restores the
selected brightness. Background and text-size changes redraw the screen.
The large size includes Cyrillic layer and media text.

These settings persist on the dongle. Existing v1–v5 settings load with the
new display defaults, preserving layer names and module settings. Qube saves
version 6 using unused palette/encoder padding bits, keeping the existing
224-byte capacity and layer-name offsets. Firmware predating this format
cannot restore that settings blob after a downgrade: module settings reset,
layer names may appear garbled, and the next Vial change saves the reset values.
Export the layout/settings before downgrading and restore them afterwards.
Standalone profiles continue using version 5 and do not advertise the display QSIDs.

Run the host-side Vial/storage regression test with:

```sh
python3 scripts/test_qube_display_settings.py
```
