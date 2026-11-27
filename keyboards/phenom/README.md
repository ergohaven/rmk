# Phenom series (universal UF2)

One UF2 — built from a single `central` binary — flashes onto **both halves** of a
Phenom / Phenom Mini / Phenom Micro split. The firmware reads the half's identity
from the hand-detect pin (GP29; high = left half, low = right half — the same pin
QMK uses for ergohaven boards via `SPLIT_HAND_PIN`) before any hardware is initialised,
and adjusts everything hand-dependent at run time:

- physical key positions and the link row offset (left 0..4, right 5..9);
- the encoder id (left = 0, right = 1);
- the trackball / touchpad module ids (left = 0/2, right = 1/3);
- which half forwards events over the serial link (the non-USB-primary half).

Either half can become the USB primary: at startup each half waits up to two seconds
for USB enumeration. The enumerated half runs the keyboard and host configuration
service; the other forwards input over the serial link. A peripheral that receives
no primary status retries startup after three seconds. Cable changes that remove power
restart this election.

Flashing: **the same UF2 on both halves** — see the build instructions below. Each
half keeps its own saved keymap and settings. The current USB primary loads its local
configuration and sends live module and lighting settings to the other half; these
updates do not copy its saved configuration into the other half's flash.

To enter ROM USB UF2 mode, press the physical Reset button twice within the 200 ms
startup window on the half connected to USB. A normal boot waits an extra 200 ms before
USB initialization and role election. See [physical double-Reset](DOUBLE_RESET.md) for
the retention/startup contract and focused regression.

The factory base-layer indicator is off (black). Loading a saved configuration preserves its
chosen colors, including an explicitly enabled base-layer indicator; select a color
in Entropy if you want one for an existing configuration.

Selected touchpads retain fast polling during normal input inactivity and do not enter
the sensor's LP1/LP2 modes. Disabling a module still suspends its touchpad, and the
shared driver's explicit sleep-event handling is unchanged. This does not add automatic
USB host-suspend propagation to the wired Phenom build. K:04 retains its battery-saving policy.

## Building

```bash
# Phenom Mini (profile keyboard_mini.toml)
export KEYBOARD_TOML_PATH="$PWD/keyboard_mini.toml"
export VIAL_JSON_PATH="$PWD/vial_mini.json"
cargo build --release --bin central

# Full Phenom
export KEYBOARD_TOML_PATH="$PWD/keyboard.toml"
export VIAL_JSON_PATH="$PWD/vial.json"
cargo build --release --bin central

# Phenom Micro
export KEYBOARD_TOML_PATH="$PWD/keyboard_micro.toml"
export VIAL_JSON_PATH="$PWD/vial_micro.json"
cargo build --release --bin central
```

The `central` binary is self-contained: it never expects a compile-time peripheral.
The UF2 is produced with the standard RP2040 path (objcopy `-O binary`, then
`bin2uf2.py` with base `0x10000000` and family `0xE48BFF56`).
