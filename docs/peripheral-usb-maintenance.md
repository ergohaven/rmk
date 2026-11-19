# Peripheral USB maintenance over Rynk

A directly USB-connected peripheral can identify itself, unlock using its own
physical keys and enter its UF2 bootloader. It retains its fixed split role:
plugging in USB does not make it a primary/central or add keyboard/mouse HID.
The central or Qube dongle does not need to be connected for maintenance.

This is an unreleased addition to Ergohaven Rynk wire revision **3**. Released
v0.3.0 images use revision 2 and do not contain this service. Updating Entropy
alone cannot add it to installed firmware: the first installation on a
peripheral requires the existing BOOT/Reset procedure. A matching Entropy
implementation is tracked separately in
[entropy#275](https://github.com/ergohaven/entropy/pull/275).

## Enabled profiles and target identity

| Profile | Central target | Maintenance targets |
| --- | --- | --- |
| K:04, Mini, Micro standalone | Left USB/BLE half | Right: `Peripheral(0)` |
| K:04, Mini, Micro Qube | Qube dongle | Left: `Peripheral(0)`, right: `Peripheral(1)` |
| K:03, Imperial44, OP36 standalone | Left USB/BLE half | Right: `Peripheral(0)` |
| Velvet standalone | Right USB/BLE half | Left: `Peripheral(0)` |
| K:03, Imperial44, OP36, Velvet Qube | Qube dongle | Left: `Peripheral(0)`, right: `Peripheral(1)` |

`GetFirmwareTarget` (`0x7E60`) returns `Standalone`, `Central` or
`Peripheral(index)`. The index names the zero-based `[[split.peripheral]]`
entry. The model UID, USB serial and target do not identify a pair of
physical halves.

Phenom, Mini and Micro can become primary over USB on either half. The
normal Rynk service reports the directly connected image target
(`Central` on the left, `Peripheral(0)` on the right) and uses the normal
keyboard unlock challenge. The maintenance-only service applies to the
fixed-role peripherals in the table.

`GetErgohavenInfo` retains its existing payload. `FIRMWARE_TARGET` (bit 13)
advertises the new query. `USB_MAINTENANCE` (bit 14) tells the host this interface
serves maintenance only. Normal keyboard services advertise `FIRMWARE_TARGET`
without `USB_MAINTENANCE`. Clients must negotiate revision 3 and inspect these
capabilities before selecting the editor or updater UI. An app supporting only
revision 2 must not treat a maintenance endpoint as a complete keyboard.

## USB and commands

Maintenance reuses Rynk's vendor bulk interface, serial prefix, framing and
WinUSB descriptors. It is USB-only; it does not add a BLE host service, change
split messages, relay firmware through the central or transfer firmware bytes
while the application firmware is running.

The service accepts only:

- `GetVersion`, `GetCapabilities`, `GetDeviceInfo`, `GetErgohavenInfo` and
  `GetFirmwareTarget` and `GetBoardDefinition`;
- `GetLockStatus`, `UnlockPoll` and `Lock`;
- `BootloaderJump`, only after physical unlocking.

Everything else returns `UnknownCmd`; topic-range requests are dropped. There
are no unsolicited topics. Capabilities describe the local matrix with zero
layers and no storage, macro or bulk-editing support. Identity uses the same
model UID, definition hash and board firmware version as the central image.

## Physical unlock

Each native profile opts in with local matrix positions, before split offsets:

```toml
[[split.peripheral]]
# Other board fields...
usb_maintenance_unlock_keys = [[2, 1], [2, 2]]
```

Right peripherals use the factory **J + K** positions. Left peripherals use
**D + F**. These physical positions remain the challenge after key remapping.
`GetLockStatus` reports them in local coordinates. Entropy should map them
through the target profile's matrix offsets for display, and poll `UnlockPoll`
while the user holds the indicated keys. No BOOT/Reset button is needed once
this firmware is installed; physical key confirmation is still required.

A separate scanner subscriber runs even without a central connection. Empty,
duplicate and out-of-bounds challenges are rejected by configuration resolution.
A lost scanner event clears held-key state and revokes authorization. `Lock`,
a USB bus reset/deconfiguration/disconnect or reboot requires a new unlock. Closing
an application handle alone does not relock the USB session; the app should send
`Lock` before closing. Maintenance
never inherits `[host].insecure` or the central's unlocked state.

Only the explicitly opted-in native profiles change. Vial profiles and
peripherals without `usb_maintenance_unlock_keys` keep their existing USB path.
Code generation currently accepts RP2040 ROM boot and nRF52840 Adafruit
bootloaders, and requires USB hardware configuration.

## Entropy integration and verification

1. Discover the directly connected Rynk USB component and read its wire revision,
   model UID, version and firmware target. Show a maintenance flow when bit 14
   is set, instead of attempting keymap/settings initialization.
2. Select the model/topology archive and the image matching the reported target.
   Classic standalone and Qube profiles share a USB product ID and UID.
   Read the definition's `entropy.firmwareUpdate.asset` to distinguish them.
   Unlock locally, then send `BootloaderJump`. A successful reset can disconnect
   USB before the acknowledgement arrives.
3. Match the newly appearing bootloader on the same physical USB port, perform
   the existing UF2 board/family checks, then write and sync the selected image.
   Runtime target identity is not proof of a bootloader's identity after reset.
4. After the component reboots, establish a fresh Rynk session and require the
   same model UID and target plus the expected installed firmware version.
   A copied UF2 alone is not version verification.
5. Repeat for the remaining components of the same firmware release. Keep the
   primary/dongle until last when the app's update guide needs it for backup.

The updater needs an Entropy follow-up to implement this maintenance path and
revision 3 negotiation. The existing draft's manual peripheral step remains
necessary until that app work lands. This firmware PR does not perform a
hardware update or establish USB enumeration, electrical behavior, physical
unlock or a successful UF2 transfer on real devices.
