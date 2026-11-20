# Physical double-Reset bootloader entry

Phenom Full, Mini and Micro use the same `firmware.rs` startup hook in both physical-side binaries. A single startup arms a 200 ms window, then clears the marker and continues normal startup. A second physical Reset during that window clears the marker and enters the RP2040 ROM USB bootloader with both UF2 mass storage and PICOBOOT enabled. Connect USB to the half being put into bootloader mode.

Every normal startup, including a recovery reboot, therefore takes an additional **200 ms** before USB initialization and role election. The interval starts after HAL clock initialization, not at the electrical edge of the Reset button. This matches the timeout configured in Ergohaven QMK (`RP2040_BOOTLOADER_DOUBLE_TAP_RESET_TIMEOUT 200U`). Power-cycling is not a guaranteed double-Reset gesture. Matching that timeout is not a claim of identical end-to-end startup timing between the two firmware frameworks.

## Retention and Rust boundary

The Raspberry Pi Pico SDK explicitly documents that RP2040 SRAM survives assertion of RUN. Its RP2040 double-reset implementation stores a three-word token in uninitialized SRAM, waits 200 ms, and clears it before normal continuation or ROM entry. Its newer-chip implementation is different; do not transplant RP2350 retention assumptions to RP2040.

`double_reset.rs` reserves an aligned 12-byte `.uninit.phenom_double_reset` object through assembly. The linker chooses its address inside Phenom's configured RAM region; there is no hardcoded SRAM address. `cortex-m-rt` 0.7.6 collects `.uninit.*` into a separate NOLOAD section after the `.bss` zeroing range and before the heap. Phenom uses `flip-link`, which places the stack below the static regions; the linked marker must be outside that stack range too. The locked dependency graph enables only `device`, not `zero-init-ram` or `paint-stack`. Enabling whole-RAM initialization would break this contract and requires reconsidering this implementation.

Cold SRAM has unspecified contents. The first read uses ARM LDR instructions with initialized register outputs, not a Rust load of uninitialized storage, `MaybeUninit::assume_init`, or a volatile read of an uninitialized Rust integer. All three words must match; partial/nonmatching tokens take the normal startup path. Writes are volatile and followed by DSB before the window or ROM handoff. No WATCHDOG scratch register is used: the RP2040 register descriptions promise scratch retention through *soft reset*, not the physical RUN boundary.

This is a startup-only owner. It does not reserve or initialize a second watchdog, consume a peripheral singleton, change role recovery, alter the retained USB future, start USB early, or modify generic RMK boot policy. As in the SDK reference, an actual reset while the marker is armed is counted regardless of its cause. Ordinary recovery resets occur after expiry, when the marker is clear.

## Clock and startup ordering

The board-local `#[Overwritten(chip_init)]` preserves the default `Config` and single `embassy_rp::init` call, then runs the reset check. HAL initialization configures clocks, the RP2040 microsecond tick generator, and its hardware time driver before returning. `embassy_time::block_for` polls that timer synchronously: it does not depend on an async timer future being polled or another task running.

The macro emits this hook before USB initialization, storage/processors, the RMK watchdog, and the role-election entry. The executor's main task has already been entered; this deliberately blocking startup interval is **before concurrent firmware tasks begin**, not a new pre-init hook before Rust memory initialization. The ROM USB boot call consequently also sees initialized clocks.

## Focused regression

From the registered project, run through the managed storage wrapper:

```sh
agent-storage run --agent dash --project rmk -- python3 scripts/test_phenom_double_reset.py
agent-storage run --agent dash --project rmk -- python3 scripts/test_phenom_double_reset.py --mutation-controls
```

The host regression compiles the complete production decision helper verbatim, replacing only retained memory and the blocking delay with deterministic test adapters. It covers first startup, interrupted windows at 0/1/37/199 ms, expiration, later recovery reboots, clearing before ROM handoff, and partial/cold tokens. Source guards check the shared hook in both physical-side binaries, initialization ordering, blocking time source, retained section and ROM interface arguments. Optional mutations must fail assertions for a wrong interval, either omitted clear, and a partial-token match; production source is never mutated.

These are host decision/wiring checks, not electrical Reset, ROM USB enumeration, or ARM-link qualification. The combined firmware still needs the owning ARM compilation/link check, including the emitted `.uninit` region and its marker symbol. Matching updated images must be installed on both halves to provide this behavior on either side; flashing is not part of the automated regression.

## Primary sources

- [Pico SDK 2.2.0 double-reset implementation](https://github.com/raspberrypi/pico-sdk/blob/a1438dff1d38bd9c65dbd693f0e5db4b9ae91779/src/rp2_common/pico_bootsel_via_double_reset/pico_bootsel_via_double_reset.c), lines 20–23, 53–80 and 109–143. SHA-256: `d024f71bb7bee0b66bca80da4322d855b492fbee6398423a4c4d2f9e431269a9`.
- [RP2040 watchdog register definitions](https://github.com/raspberrypi/pico-sdk/blob/a1438dff1d38bd9c65dbd693f0e5db4b9ae91779/src/rp2040/hardware_regs/include/hardware/regs/watchdog.h), SCRATCH descriptions. SHA-256: `005873145c1b36e18e6b3226d450b3cbb7ca39ce72b47761687a85d7286a088a`.
- Lock-resolved [cortex-m-rt 0.7.6 linker](https://github.com/rust-embedded/cortex-m/blob/b1f693155ae45ec888d73c5f91bb5c6476ec61d1/cortex-m-rt/link.x.in), `.bss`, `.uninit`, heap/stack boundaries. SHA-256: `1f4391d246e460900c269f50fcd2a42f879ced305460a4b16cc6f9853bb3790b`. Its `src/lib.rs` startup zeroing respects those symbols unless `zero-init-ram` is enabled.
- Lock-resolved [embassy-rp 0.10.0 initialization](https://github.com/embassy-rs/embassy/blob/84444a19eb57d978e3c09fcdc8e60cdd7278eb03/embassy-rp/src/lib.rs), `init`; same revision `src/clocks.rs` (tick generation), `src/time_driver.rs` (`now` reads TIMER), and `src/rom_data/rp2040.rs` (`reset_to_usb_boot`). Initialization source SHA-256: `89e5a0e405562a5b8749ef5b48fb971ebe91351c0b257d00c868348038ecae8b`.
- Lock-resolved [embassy-time 0.5.1 blocking delay](https://github.com/embassy-rs/embassy/blob/50c6aac5b9b3b0af18f5a7d61725cc8e4d1f7acb/embassy-time/src/delay.rs), `block_for`. SHA-256: `0e45f41d70452fbb91aae20451177ec0130254894e462a06263c1e1a43e87aa6`.
- QMK parity reference: immutable checkout `8e4dbffc1028bf9c2f8fc29bb8df448fd34c0421`, `platforms/chibios/bootloaders/rp2040.c` (`__late_init`, clocks, retained marker, 200 ms). SHA-256: `45bee884142d3e3bcc911b14d941b5a2401cbfe037ca176f225c96670f5c6c51`. This checkout is reference-only and unchanged.
