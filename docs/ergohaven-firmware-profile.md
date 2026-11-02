# Ergohaven Firmware Profile

K:04 Series is the reference profile for Ergohaven RMK firmware. Production
profiles share this software contract:

- 16 layers
- 32 combos, up to 4 keys each
- 32 Morse / Tap Dance entries, up to 8 patterns each
- 2048 bytes of macro storage over Vial; 4096 over Rynk, whose encoding takes
  up to twice the bytes, with a 4128-byte Rynk frame that holds the whole pool
- 32 forks / Vial Key Override slots
- 5 Bluetooth profiles
- Bluetooth reconnect and pairing windows of 60 seconds
- persistent layer names at QSID 200–215, up to 12 UTF-8 bytes each
- Universal Symbols, the `universal_symbols` RMK feature
- firmware version 0.2.0 and manufacturer `Ergohaven`

Phenom Series is wired and has no batteries, so the Bluetooth and battery
parts of the contract do not apply to it.

Every profile speaks Rynk with Ergohaven's commands (rmk feature
`ergohaven_rynk`, `[host] rynk_enabled = true`). Over Rynk the same QSIDs,
layer names, board definition and host data are served, writes need no unlock,
as over Vial, and keymap and macro writes reach flash after a quiet period or a
host commit.

Standard keyboard profiles expose the factory layer names `Base`,
`Navigation`, `Symbols`, and `Adjust`. Layer 4 is `Mouse` only on keyboards
that have the pointing layer there; otherwise it is `4`. Layers 5–15 use their
decimal indices. User-defined names remain authoritative.

Every Ergohaven profile compiles layers 5–15 as `No`, not `Transparent`, so
Entropy displays those factory layers as empty instead of inheriting keys from
lower layers. Layers 0–4 keep their existing keymaps. Velvet uses one firmware
for both hardware variants: the ordinary right thumb key remains in the matrix,
while an optional PMW3610 occupies that position on the pointing variant.
Layers 5 and 6 can still select Scroll and Sniper pointing modes, but their
factory keymaps contain only `No`.

Saved key and encoder actions on layers 0–4 use the main keymap records, and
layers 5–15 a separate tail namespace (`KeymapTailV3`, `EncoderTailV3`).

Split pairing uses a 30-second window. Standalone split centrals sleep after
120 seconds; powered Qube centrals use 900 seconds, K:04 Qube centrals 300.
K:04 Qube centrals reserve 3 ms per connection event and space the two half
links 3.75 ms apart, so the 7.5 ms trackball link keeps every event next to
the 15 ms link of the other half. They do not advertise to a BLE host: the PC
connects over USB.
Entropy capability metadata advertises separate half batteries on all split
devices and time/media live features only where Qube supplies them.

Hardware-specific differences remain in matrices, pins, encoder count,
pointing devices, displays, lighting, batteries, and split topology.

## USER keycode compatibility

Classic firmware keeps the established USER00–USER09 numeric mapping so saved
layouts remain valid. The names are standardized as `BT0`–`BT4`, `BT_NEXT`,
`BT_PREV`, `BT_CLR`, `BT_TOG`, and `BT_PEER`.

K:04 retains its established numeric mapping and its larger named USER set.
RMK's Ergohaven processor accepts both mappings. Renumbering either mapping
requires an explicit saved-layout migration and is outside this compatibility
stage.

## Storage and reset

All production nRF52840 profiles reserve the same 128 KiB settings partition
at `0xCC000–0xEC000`. Application linkers stop at `0xCC000`, preventing
firmware and settings from overlapping.

The first boot after upgrading from firmware before RMK 0.9 (Ergohaven v0.2.0
and earlier) erases the settings partition: the record format changed. The
stored schema hash covers the RMK version, its feature set and the configured
sizes, so a later change to any of them erases the settings again. Export the
layout in Entropy before flashing and import it afterwards.

Use `settings_reset.uf2` for keyboard halves and `settings_reset_qube.uf2` for
a Qube dongle. Both erase only the unified partition; separate files are
required because the application origins differ.

The one-time storage migration utility for the legacy `0xA0000–0xC0000`
partition applies only to firmware before RMK 0.9; newer firmware erases those
settings on first boot anyway.

The former Velvet UI profile used a separate USB identity (`0x00BF`) and a
different saved-keymap shape. Before its first upgrade to the unified Velvet
firmware (`0x00BE`), reset both halves with `settings_reset.uf2`, remove the
old Velvet UI Bluetooth pairing on the host, and pair again. Existing standard
Velvet installations already use the unified identity and do not need this
one-time reset.

Run `./scripts/check_ergohaven_profile.sh` to reject accidental profile drift.
