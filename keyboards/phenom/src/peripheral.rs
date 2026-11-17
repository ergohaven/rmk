#![no_main]
#![no_std]

// Keep the historical binary name for the physical right half.
const IS_RIGHT: bool = true;
include!("firmware.rs");
phenom!(peripheral = 0);
