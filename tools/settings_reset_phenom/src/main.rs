//! Phenom RP2040 settings-only eraser. No USB application, split, DMA or core1 tasks.
#![no_std]
#![no_main]
mod reset;
use cortex_m_rt::entry;
use embassy_rp::flash::{Blocking, Flash};

fn stop() -> ! {
    loop { cortex_m::asm::nop(); }
}

#[entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());
    // Core1 stays in boot ROM; no multicore::spawn_core1 and no DMA is started.
    // embassy-rp additionally checks core0, guards interrupts and calls the
    // .data.ram_func erase trampoline while XIP is unavailable, restoring XIP
    // from a RAM copy of boot2 before returning and before readback.
    cortex_m::interrupt::disable();
    let mut flash = Flash::<_, Blocking, { reset::FLASH_SIZE as usize }>::new_blocking(p.FLASH);
    let ok = reset::reset_settings(|from, to| {
        if flash.blocking_erase(from, to).is_err() { return false; }
        // Volatile reads after the driver's XIP cache flush; never trust a cached
        // successful erase or merely the return value of the ROM erase routine.
        let mut address = 0x10000000 + from;
        while address < 0x10000000 + to {
            if unsafe { core::ptr::read_volatile(address as *const u32) } != u32::MAX {
                return false;
            }
            address += 4;
        }
        true
    });
    if !ok { stop(); }
    // BOOTSEL appears again only on complete success; no reboot-erase loop.
    embassy_rp::rom_data::reset_to_usb_boot(0, 0);
    stop()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A failure must not masquerade as successful return to BOOTSEL.
    stop()
}
