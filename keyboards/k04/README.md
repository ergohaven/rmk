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
VIAL_JSON_PATH="$PWD/vial_universal.json" \
CARGO_TARGET_DIR=target/k04 \
cargo build --release --bin central --bin peripheral --bin hardreset --features production_v30g,universal_mac_iso

KEYBOARD_TOML_PATH="$PWD/keyboard_mini.toml" \
VIAL_JSON_PATH="$PWD/vial_mini.json" \
CARGO_TARGET_DIR=target/mini \
cargo build --release --bin central --bin peripheral --bin hardreset --features production_v30g

KEYBOARD_TOML_PATH="$PWD/keyboard_micro.toml" \
VIAL_JSON_PATH="$PWD/vial_micro_universal.json" \
CARGO_TARGET_DIR=target/micro \
cargo build --release --bin central --bin peripheral --bin hardreset --features production_v30g,universal_mac_iso
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

## Universal Symbols on macOS

Select Mac mode with the Universal Symbols macOS toggle; firmware remembers
this choice after a restart. Configure macOS input-source switching as
Control+Space and keep English and Russian as the two input sources.

For K:04 and Micro standalone builds, select ANSI or ISO in Entropy's
Live Integrations → Universal Symbols to match the keyboard type in macOS.
This changes universal backtick and tilde only, and preserves the encoder options.
Mini and Qube do not advertise this geometry option.

With Entropy closed, use the universal language toggle so firmware can track the
language. If the language changes elsewhere, use the universal sync key to
resynchronize it. Host layout synchronization requires Entropy to stay running.

## Battery

The halves use `src/battery_nrf.rs`, which samples `P0_31` without
`calibrate().await` and re-publishes `BatteryStatusEvent` periodically.

## Legacy LED indication

K:04, Mini, and Micro share these rules in standalone and Qube halves:

- Selecting BT0–BT4 shows that profile's configurable color for one second.
  Factory colors are red, blue, yellow, green, and magenta respectively.
- Cyan blinking means pairing with a new host; white blinking means reconnecting
  to a known host. Yellow blinking means a half is searching for its split peer.
- A host or split-peer connection shows green for one second.
- **BT Battery** shows each half's own available battery level for one second:
  0–20% red, 21–40% orange, 41–74% yellow, 75–100% green. If the local
  level is unknown, the request waits for a real local battery reading.
- At <=20%, a half without USB-C power shows a short red blink every two seconds
  (120 ms on). USB-C power suppresses this low-battery warning.
- On the base layer, USB-C indication is yellow below 95% (or when the level is
  unknown) and green at an available level of 95% or above. A non-base-layer
  color takes precedence over USB indication, including on a power-only cable.
- During sleep, a half without USB-C power turns its LED off. A powered half
  retains the appropriate indication, including non-base-layer colors.

The existing `Charge indicator` toggle (QSID 331), packed settings bit, profile
palette, and protocol schema are unchanged. Split search and one-second connection
success keep their existing overlay priority. Profile and battery-request overlays
are visible ahead of background host/USB indication. Both topologies now use the
same existing overlay/event/deadline owner rather than separate painters.

For charging, leave each half's physical power switch ON, as required by the
[K:04 guide](https://docs.eh.works/device/k04/#зарядка-мониторинг-заряда).
VBUS is only a power input: firmware has no physical-switch, charge-current or
charge-termination input. Battery filtering still uses trusted unplugged readings
and does not synthesize levels from elevated charging voltage. Thus the green
threshold is a display rule on the **available** level, not proof of charge
termination. A cold boot on USB may remain yellow; an older cached level may
remain unchanged until an unplugged measurement is available.

The LED's 10 ms timer stops during sleep without USB. Powered sleep keeps that
timer for blink/overlay expiry and cable removal. After cable insertion into an
unpowered sleeping half, the existing battery/event wake observes VBUS and resumes
indication; there is no new immediate cable-insertion interrupt. The battery task
still wakes independently (normally every 15 seconds without USB, plus sampling
time), so stopping this LED timer does not eliminate all MCU wakeups.

Host regressions (run through the project's managed build-cache wrapper):

```sh
python3 scripts/test_k04_usb_power_indicator.py
python3 scripts/test_k04_led_legacy.py
```

The first checks the USB decision branch, brightness, threshold and gates. The
second compiles the complete production LED state/event/render/poll/deadline
callbacks, settings-color functions, shared connection-state functions and selected
split-receive arms. It substitutes deterministic clock/snapshots, event transport
and PWM output. It does not test the generated forever-loop, physical BLE/USB
transport, switch, battery ADC, charge current or LED appearance on hardware.

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

## Bluetooth profile names

Host Bluetooth profiles advertise as `K:04 BT0` through `K:04 BT4`, with the
corresponding Mini or Micro model name. GAP Device Name uses the same selected
profile name. Profile labels are automatic; this does not add editable names
or change saved bonds, profile addresses, USB identity, or Qube split discovery.
The shared RMK `ble_profile_names` feature is enabled for this keyboard family.
Hosts may retain an older display name for an existing pairing; check new
pairings as well as switching between saved profiles during hardware testing.
