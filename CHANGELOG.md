# Changelog

## v0.4

More language and remapping actions, clearer pointing controls and everyday typing fixes.

- Universal Symbols ANSI/ISO selection across the current keyboard range, with dedicated Ж, Э and Ё actions.
- Shared EN/RU helpers: one English word, tap/hold language switching and language store/revert around macros; delayed return from English punctuation fallback.
- LNEXT/LPREV layer cycling, skipping unassigned destinations.
- Persistent Magic remapping, including Ctrl/Cmd swap, restore and toggle.
- K:04-family automatic mouse-layer timeouts up to 3000 ms and left/right/both Normal-mode actions.
- Fixed single-key Ctrl+Space input switching on macOS and premature save-completion reporting.
- Consistent factory layer names, unnamed unused layers and factory Left Alt, preserving custom names and saved assignments.

Export your layout, update Entropy and flash all matching parts. Factory layout
changes do not replace saved keymaps. Official downloads are RMK/Rynk only;
manual Vial configurations remain available. Phenom RMK downloads are paused;
Phenom, Phenom Mini and Phenom Micro use official QMK/Vial firmware instead.

Thanks to @IgorArkhipov and @D19ital.

[Full changelog](https://github.com/ergohaven/rmk/compare/v0.3.0...v0.4)

## v0.3.0 (local release candidate)

Default firmware moves from Vial to Rynk with Ergohaven wire revision 2 and
adds the wired Phenom Series. Separate Vial standalone variants remain for
K:03, K:04 Micro, Imperial44, OP36 and Velvet; full-size K:04 Vial is excluded
from production. The official matrix has 22 profiles / 51 keyboard UF2 roles.

### Features

- Added Phenom, Phenom Mini and Phenom Micro with the K:04 module stack and per-half layer LEDs
- Added Rynk Ergohaven commands, staged writes and a 4096-byte macro pool (2048 bytes on Vial)
- Added Qube display brightness, background and font options, 64-byte media fields and scrolling titles
- Added persistent sticky-mode keys, PC/macOS Universal Symbols mode, numero and model-specific ISO geometry
- Added Universal actions in extended Vial macros and standalone K:04-family BT0–BT4 discovery names

### Improvements

- Integrated upstream RMK through `a0ccc40f0933366b5a076562104071f1e3281bb0`
- Improved K:04 Qube radio scheduling while retaining its USB-only computer connection and BLE half links
- Coalesced settings/macro writes, bounded flash waits and adopted the upstream Nordic flash-lock fix

### Fixes

- Preserved host-selected standalone K:04 BLE intervals, selected-profile security admission and GATT subscriptions
- Restored legacy K:04-family LED colors and timing; the intermediate cyan USB-only indication is superseded
- Fixed modifier-assisted pointing, held drags, touchpad holds, unbound-key auto-layer deactivation and cadence-aware acceleration
- Fixed local pointing wake activity, macOS Caps Lock report ordering and modifier-bearing shortcuts
- Made Fast typing window enable/disable inherited Flow Tap while retaining explicit per-key overrides
- Restored independently rebooted wired peripheral state/settings/layer synchronization and Phenom module behavior

### Compatibility and verification

Export layouts **before** flashing: the transition from v0.2.0 changes storage
and clears settings and host/split pairing on every part. Update Entropy,
flash both halves plus the Qube dongle where applicable, re-pair and import.
Protocol/feature changes can reset storage again; nonzero QSID 27 in a restored
backup now enables default Flow Tap. Back up before downgrading Qube display
settings too. Phenom starts with both modules set to `None`; QMK settings do
not carry over. USB LED indication uses cached battery state, not a measured
charging-current/completion signal.

Release preparation performs source/profile checks only. Fresh candidate CI,
firmware builds, pure Rust parser execution, binary/archive verification and
hardware regression remain pending. The original v0.2.0 tag is `a06f0506`;
its refreshed published assets used `347cf14f`, still version 0.2.0. Their
classic factory-layout/task-join fixes are already shipped, not new here.

See [complete release notes, contributor credits and flashing caveats](docs/releases/v0.3.0.md).

## v0.1.10

Adds an auto-layer option across the whole K:04 line. Both version lines move to `0.1.10`: the standalone K:04 Series from `v0.1.9`, and the K:04 Qube profiles from `v0.1.8`. VIA and USB advertise `0x0110`.

### Features

- Added the `Auto layer deactivate on key` setting (QSID 335): when enabled, pressing any non-mouse key immediately drops the pointing auto layer instead of waiting for the inactivity timeout. Mouse keys, repeated keycodes, layer switches, macros, and the pointer-mode keys leave the layer intact
- Advertised the new setting in all six K:04 Vial definitions, so configurators can expose it without a client-side change

### Notes

- Disabled by default; with the option off, the auto layer behaves exactly as in `v0.1.9`, including held keys suppressing the timeout
- Stored in the last free bit of the existing auto-flags byte, so the module settings storage layout and version are unchanged and no migration is needed
- Only a layer this feature turned on is dropped on a keypress. Typing leaves it alone when it was already active when the pointer moved (held by `MO`/`TG`), once a layer key such as `MO`/`TG`/`TO` names it, and once something else switched it off and it was turned back on. The inactivity timeout still applies as in `v0.1.9`

## v0.1.9

Stable release for the standalone K:04 Series: K:04, K:04 Mini, and K:04 Micro. K:04 Qube profiles and all other keyboards remain on v0.1.8.

### Fixes

- Deferred host-BLE idle and disconnect power transitions while USB is the active output, fixing both K:04 halves freezing after the two-minute idle boundary on wired connections

### Verification

- Reproduced the v0.1.8 wired freeze on physical K:04 hardware, eliminated it with the host-power A/B build, and confirmed the final active-USB guard with a second hardware test

## v0.1.9-rc.2

Corrective release candidate for the standalone K:04 Series after review of `v0.1.9-rc.1`.

- CI now builds K:04, K:04 Mini, and K:04 Micro with the validated `production_v22` feature set
- Restored live left/right Ball DPI settings while retaining 600 CPI as the production default
- Applied USB `bcdDevice = 0x0109` to the actual device descriptor
- Enabled right-half split liveness recovery in non-diagnostic production builds
- Removed a non-portable atomic read-modify-write from the generic BLE path
- Queued each native i16 mouse delta once and moved HID chunking into the transport writers, preventing one extreme event from filling the report queue
- Restored the pre-v22 4/8 ms cadence and i8 event width for K:04 Qube halves
- Added regression coverage for the production workflow, USB BCD, split recovery, Qube isolation, live DPI, and extreme motion deltas

## v0.1.9-rc.1

Release candidate for the standalone K:04 Series: K:04, K:04 Mini, and K:04 Micro. K:04 Qube profiles and all other keyboards remain on v0.1.8.

The embedded Vial definition carries the full `0.1.9-rc.1` identity. VIA and USB fields that support only numeric versions advertise `0.1.9` / `0x0109`.

### Features

- Added the `production_v22` build profile for all six standalone K:04 Series half images, with diagnostic features excluded at compile time and release symbols stripped
- Added accumulated full-width `i16` motion transport from the right half so lowering the split report rate never discards trackball distance
- Added generation-aware split lifecycle state and recovery so the left half can reset, tear down stale resources, and reconnect to the still-running right half
- Added gated RTT instrumentation and regression helpers for split timing, HID queues, PMW3610 health, radio strength, USB state, and movement preservation; none are enabled in production images

### Improvements

- Set right-half motion reports to 15 ms, exactly two 7.5 ms split radio windows, while retaining 4 ms local trackball sampling
- Paced BLE HID mouse reports at 7.5 ms and merged queued relative motion with proportional vector-preserving chunking, retaining button ordering and total movement
- Configured the K:04 Series radio for +8 dBm and 1M PHY, and enabled the E73 REG1 DC/DC path with REG0 disabled
- Configured PMW3610 for 600 CPI, Smart Mode, software axis swapping, and verified register setup on K:04, Mini, and Micro
- Refreshed bonded-host connection parameters through an Apple-compatible 15 ms request followed by 7.5 ms when accepted, with a 15 ms/latency-zero fallback
- Preserved the v0.1.8 two-minute sleep timeout and host power policy

### Fixes

- Removed the persistent right-trackball latency caused by sending split movement faster than the BLE link could drain its notification queue
- Reduced weak-signal right-trackball microfreezes without clipping or losing accumulated motion
- Recovered PMW3610 from failed or implausible reads and added fallback reads after motion gaps
- Fixed split recovery after resetting the left half, including stale channel/resource cleanup and advertising/HCI race handling
- Cleared stale USB, POWER, and NVIC state left by the UF2 bootloader on the right half so disconnecting USB cannot leave it locked up
- Corrected VBUS/charging indication and made live split-searching state take priority over stale connected LED state

### Verification

- Passed all four motion pacing and movement-preservation tests, the RMK feature test matrix, and production builds for both halves of K:04, K:04 Mini, and K:04 Micro
- Validated all six UF2 images as nRF52840 firmware starting at `0x00026000`
- Physically validated K:04 right-trackball behavior at 20 cm and 60 cm; K:04 Mini and K:04 Micro still require model-specific hardware regression testing

## v0.1.8

### Features

- Added two-stage host BLE power saving to all seven supported Ergohaven standalone profiles—K:03, K:04, K:04 Mini, K:04 Micro, Imperial44, OP36, and Velvet—with low-duty host link parameters after two minutes, a fixed host disconnect after 30 minutes, and fast bonded wake that retains the first supported input event
- Advertised firmware-native Repeat/Again and Fork key-override capabilities to Entropy for all 14 supported Ergohaven standalone and Qube profiles

### Improvements

- Aligned the K:04 left-bracket key geometry with the surrounding row in Vial
- Expanded Vial Key Override capacity from 8 to 32 slots across every supported Ergohaven profile
- Updated the embedded firmware and package identity to v0.1.8 for all 14 supported Ergohaven standalone and Qube profiles

### Fixes

- Hardened bonded BLE wake through encryption, fast reconnect, local HID suspend, safe split-radio scheduling, stale-peer recovery, and fail-closed recovery from stalled HID notifications
- Released temporary wake-input observers immediately after reconnect so their queues cannot block a key release and leave the last key repeating
- Preserved queued input across sleeping reconnects and restored bonded reconnect and macro layer actions
- Prevented K:04 from reporting a false full-battery state while charging

## v0.1.8-rc.5

### Improvements

- Advertised firmware-native Repeat/Again support so Entropy can expose it independently from QMK Alt Repeat slots

### Verification

- Kept every production Ergohaven profile at 32 Morse / Tap Dance entries

## v0.1.8-rc.4

### Features

- Added two-stage host BLE power saving for standalone K:04, K:04 Mini, and K:04 Micro: low-duty connection parameters after two minutes, then a configurable full disconnect after 10 minutes to 5 hours
- Preserved the first keyboard, encoder, trackball, or touchpad event while reconnecting after idle sleep

### Fixes

- Applied host disconnect timeout changes immediately and migrated existing v0.1.7 settings to the new 30-minute default
- Kept wake-key press and release reports ordered while a sleeping bonded host reconnects, without blocking later keyboard input

## v0.1.7

### Features

- Added firmware-native Universal Symbols and Russian letters to dynamic Combo and Tap Dance actions
- Added persistent per-combo activation layers configurable from Entropy
- Advertised exact firmware update package identities for all 14 supported Ergohaven keyboard and Qube profiles

### Fixes

- Suspended K:04 trackball and touchpad modules during automatic sleep while preserving their selected type and settings after wake
- Stabilized repeated macro saves, split BLE traffic, pointing-mode changes, and K:04 thumb-cluster geometry

### Removed

- Removed RMK firmware and release builds for the standalone Trackball Mini v3.0, Mini v3.1, and Royale devices

## v0.1.5-rc.1

### Features

- Added modular firmware-native Universal Symbols for EN/RU punctuation, autonomous layout controls, PC/macOS mappings, and optional Entropy Layout Sync
- Advertised Universal Symbols support through the Ergohaven native key-action capability protocol and enabled it for every bundled Ergohaven keyboard profile
- Added firmware-native Russian `х`, `б`, `ю`, and `ъ` actions that behave like regular shifted letter keys while the Russian layout is active

## v0.1.4

### Features

- Added unified Velvet UI firmware for Standalone and Qube, with the right half as the Standalone central and optional PMW3610 trackball support
- Added persistent Velvet trackball enable and Mouse auto-layer timeout controls for Entropy
- Added left/right battery telemetry, live Qube display data, and consistent factory layouts across the unified Ergohaven profiles
- Embedded the Ergohaven firmware version `0.1.4` in every released keyboard definition and exposed it through VIA `id_firmware_version`

### Fixes

- Fixed Velvet startup and split-runtime panics caused by exhausted settings and layer subscribers
- Fixed PMW3610 report starvation, idle jitter, auto-layer timeouts, and stale-motion accumulation
- Fixed split BLE framing, link cadence, HCI command serialization, reconnect synchronization, and wake responsiveness
- Restored RP2040/Pico split compatibility by avoiding unsupported ARMv6-M atomic read-modify-write operations
- Fixed Qube display redraws blocking pointer reports; Shift and Command indicators now update atomically without cursor freezes
- Fixed K:04 encoder detents and USB layer indication, classic keyboard defaults, and live split status reporting

## v0.1.3

### Features

- Added complete K:04 Series firmware for K:04, Mini, and Micro in standalone and Qube configurations
- Added left/right battery telemetry and Qube live display data across the complete K:04 Series
- Added module-aware pointing settings, configurable encoder steps, and factory-enabled touchpad acceleration and gestures for the K:04 Series
- Embedded the Ergohaven manufacturer and firmware version `0.1.3` in the released K:04 and trackball definitions and exposed the same version through VIA `id_firmware_version`

### Fixes

- Fixed split wake latency after idle while retaining the power-saving connection interval
- Fixed excessive idle polling for trackball and touchpad modules
- Fixed BLE Vial report framing, host-session responsiveness, discovery compatibility, and split battery updates
- Fixed K:04 settings persistence, Layer LED writes, touch gestures, and Qube pointing runtime parity
