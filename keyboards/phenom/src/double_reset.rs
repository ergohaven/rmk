//! Phenom-only RP2040 physical double-Reset entry. See ../DOUBLE_RESET.md.

const MAGIC: [u32; 3] = [0x4d4b_4850, 0x8ba9_2f16, 0xd301_c6e5];
const WINDOW_MS: u64 = 200;

trait ResetWindow {
    fn read_marker(&mut self) -> [u32; 3];
    fn write_marker(&mut self, marker: [u32; 3]);
    fn wait_ms(&mut self, millis: u64);
}

fn bootloader_requested(window: &mut impl ResetWindow) -> bool {
    if window.read_marker() == MAGIC {
        window.write_marker([0; 3]);
        return true;
    }
    window.write_marker(MAGIC);
    window.wait_ms(WINDOW_MS);
    window.write_marker([0; 3]);
    false
}

#[cfg(target_arch = "arm")]
pub fn check() {
    if bootloader_requested(&mut rp2040::Window) {
        embassy_rp::rom_data::reset_to_usb_boot(0, 0);
        // The ROM API is typed as returning (), but must not resume startup.
        loop {
            cortex_m::asm::wfi();
        }
    }
}

#[cfg(target_arch = "arm")]
mod rp2040 {
    use core::arch::{asm, global_asm};
    use core::ptr::{addr_of_mut, write_volatile};

    // Assembly owns the raw bytes: cold SRAM is not an initialized Rust value.
    // cortex-m-rt reserves .uninit outside .bss and before the heap.
    global_asm!(
        ".pushsection .uninit.phenom_double_reset,\"aw\",%nobits",
        ".balign 4",
        ".global PHENOM_DOUBLE_RESET_MARKER",
        ".type PHENOM_DOUBLE_RESET_MARKER,%object",
        "PHENOM_DOUBLE_RESET_MARKER:",
        ".space 12",
        ".size PHENOM_DOUBLE_RESET_MARKER,12",
        ".popsection",
    );

    extern "C" {
        static mut PHENOM_DOUBLE_RESET_MARKER: [u32; 3];
    }

    pub(super) struct Window;

    impl super::ResetWindow for Window {
        fn read_marker(&mut self) -> [u32; 3] {
            let (first, second, third);
            // SAFETY: the linker reserves this aligned 12-byte SRAM region.
            // LDR produces initialized register outputs for every bit pattern;
            // no Rust load/assume_init reads uninitialized retained storage.
            unsafe {
                asm!(
                    "ldr r1, [r0, #0]",
                    "ldr r2, [r0, #4]",
                    "ldr r3, [r0, #8]",
                    in("r0") addr_of_mut!(PHENOM_DOUBLE_RESET_MARKER),
                    out("r1") first,
                    out("r2") second,
                    out("r3") third,
                    options(nostack, preserves_flags),
                );
            }
            [first, second, third]
        }

        fn write_marker(&mut self, marker: [u32; 3]) {
            // SAFETY: startup is the sole owner, before any other tasks run.
            unsafe {
                write_volatile(addr_of_mut!(PHENOM_DOUBLE_RESET_MARKER), marker);
            }
            cortex_m::asm::dsb();
        }

        fn wait_ms(&mut self, millis: u64) {
            // HAL init has started the hardware timer. Do not await a timer:
            // USB, role election and the RMK watchdog have not started yet.
            embassy_time::block_for(embassy_time::Duration::from_millis(millis));
        }
    }
}
