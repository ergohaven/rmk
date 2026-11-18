#![no_main]
#![no_std]

// This binary always belongs to the physical left half, independent of USB role.
const IS_RIGHT: bool = false;
include!("firmware.rs");
phenom!();
