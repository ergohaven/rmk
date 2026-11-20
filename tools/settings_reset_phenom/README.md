# Phenom RP2040 settings reset

Standalone reset utility for the hardware-approved full-size Phenom RP2040 configuration with 2 MiB flash and 32 settings sectors. This crate has its own workspace and locked embassy-rp 0.10.0 dependency; it does not depend on the repository's RMK crates

## Safety and scope

- Erases only settings offsets `[0x100000, 0x120000)`, corresponding to XIP addresses `[0x10100000, 0x10120000)`: 32 sectors of 4096 bytes
- Verifies every erased word, retries each sector at most three times and returns to BOOTSEL only after complete success; failure leaves the utility stopped
- Loading the UF2 replaces the application entry code and BOOT2 in the first flash sector. Reinstall the appropriate normal firmware after resetting
- Settings, including saved layout and sensor settings in this partition, are lost intentionally
- Not for nRF52840, QMK, DFU layouts, other models or different storage partitions. The existing `tools/settings_reset` remains the separate nRF utility
- This target branch does not contain `keyboards/phenom`; adding this standalone utility does not add or change normal Phenom firmware

## Build

Run from this directory with the Rust `thumbv6m-none-eabi` target installed

```sh
rustup target add thumbv6m-none-eabi
cargo build --release --locked
```

Convert the resulting ELF using `arm-none-eabi-objcopy` and Microsoft's UF2 converter (with its accompanying `uf2families.json`)

```sh
arm-none-eabi-objcopy -O binary target/thumbv6m-none-eabi/release/settings-reset-phenom settings_reset_phenom.bin
python3 /path/to/uf2conv.py settings_reset_phenom.bin --base 0x10000000 --family 0xE48BFF56 --output settings_reset_phenom.uf2
```

If `CARGO_TARGET_DIR` is set, use that directory instead of `target` above. Do not use the nRF utility's `0x26000` load base or `0xADA52840` family

## Focused host tests

The host harness imports the production erase-loop module without building the embedded entry point

```sh
rustc --edition=2021 --test tests/reset.rs -o /path/to/build-cache/phenom-reset-tests
/path/to/build-cache/phenom-reset-tests
```

## Use

Connect each half directly over USB, one at a time, enter RP2040 BOOTSEL and copy the same reset UF2 to its `RPI-RP2` drive. Successful verification returns the device to BOOTSEL; then install the normal firmware appropriate to that half. If it does not return, stop and investigate rather than assuming settings were erased
