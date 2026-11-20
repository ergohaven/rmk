// Offsets, not XIP addresses. Current full Phenom: rp2040 default + 32 sectors.
pub const START: u32 = 0x100000;
pub const END: u32 = 0x120000;
pub const PAGE_SIZE: u32 = 4096;
pub const FLASH_SIZE: u32 = 0x200000;
const ATTEMPTS: u8 = 3;

pub const fn valid_range(start: u32, end: u32, capacity: u32, code_end: u32) -> bool {
    start < end && end <= capacity && start >= code_end
        && start % PAGE_SIZE == 0 && end % PAGE_SIZE == 0
}

// The linker caps ALL reset code/load data at START, including BOOT2.
const _: () = assert!(valid_range(START, END, FLASH_SIZE, START));

// true only after every page was erased AND read back successfully.
// The callback is the real blocking RP2040 driver plus post-XIP verification.
pub fn reset_settings(mut erase_and_verify: impl FnMut(u32, u32) -> bool) -> bool {
    let mut addr = START;
    while addr < END {
        let mut attempt = 0;
        loop {
            if erase_and_verify(addr, addr + PAGE_SIZE) { break; }
            attempt += 1;
            if attempt == ATTEMPTS { return false; }
        }
        addr += PAGE_SIZE;
    }
    true
}
