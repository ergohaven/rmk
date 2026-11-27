#![no_main]
#![no_std]

// One UF2 for both halves: the physical hand is read from GP29 at boot and
// the firmware adapts at run time. See firmware.rs.
include!("firmware.rs");

phenom!(runtime_hand = true);
