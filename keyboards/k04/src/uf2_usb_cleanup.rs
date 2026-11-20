//! Remove USB/POWER state left behind when the UF2 bootloader jumps directly
//! into the split-peripheral application.
//!
//! Legacy right-half builds do not own a USB driver; stale bootloader USB/POWER
//! interrupts can dispatch into their default handler when VBUS disappears.
//! Native Rynk builds now initialize a maintenance USB driver after this cleanup.
//! Both paths first clear the bootloader's state, as a hardware Reset would.

use core::ptr::{read_volatile, write_volatile};

const POWER_BASE: usize = 0x4000_0000;
const POWER_EVENTS_USBDETECTED: usize = POWER_BASE + 0x11c;
const POWER_EVENTS_USBREMOVED: usize = POWER_BASE + 0x120;
const POWER_EVENTS_USBPWRRDY: usize = POWER_BASE + 0x124;
const POWER_INTENCLR: usize = POWER_BASE + 0x308;
const POWER_USB_INTERRUPT_MASK: u32 = (1 << 7) | (1 << 8) | (1 << 9);

const USBD_BASE: usize = 0x4002_7000;
const USBD_EVENTS_USBRESET: usize = USBD_BASE + 0x100;
const USBD_EVENTS_STARTED: usize = USBD_BASE + 0x104;
const USBD_EVENTS_ENDEPIN0: usize = USBD_BASE + 0x108;
const USBD_EVENTS_EP0DATADONE: usize = USBD_BASE + 0x128;
const USBD_EVENTS_ENDISOIN: usize = USBD_BASE + 0x12c;
const USBD_EVENTS_ENDEPOUT0: usize = USBD_BASE + 0x130;
const USBD_EVENTS_ENDISOOUT: usize = USBD_BASE + 0x150;
const USBD_EVENTS_SOF: usize = USBD_BASE + 0x154;
const USBD_EVENTS_USBEVENT: usize = USBD_BASE + 0x158;
const USBD_EVENTS_EP0SETUP: usize = USBD_BASE + 0x15c;
const USBD_EVENTS_EPDATA: usize = USBD_BASE + 0x160;
const USBD_SHORTS: usize = USBD_BASE + 0x200;
const USBD_INTENCLR: usize = USBD_BASE + 0x308;
const USBD_EVENTCAUSE: usize = USBD_BASE + 0x400;
const USBD_ENABLE: usize = USBD_BASE + 0x500;
const USBD_USBPULLUP: usize = USBD_BASE + 0x504;

const NVIC_ICER0: usize = 0xe000_e180;
const NVIC_ICER1: usize = NVIC_ICER0 + 4;
const NVIC_ICPR0: usize = 0xe000_e280;
const NVIC_ICPR1: usize = NVIC_ICPR0 + 4;
const CLOCK_POWER_IRQ_BIT: u32 = 1 << 0;
const USBD_IRQ_BIT_IN_BANK1: u32 = 1 << (39 - 32);

/// Runs before `.bss`/`.data` initialization, before a stale USB interrupt can
/// pre-empt the application. Only raw peripheral accesses are allowed here.
#[cortex_m_rt::pre_init]
unsafe fn clear_uf2_usb_state() {
    // Stop any bootloader-owned USB interrupt source before touching events.
    write_volatile(USBD_INTENCLR as *mut u32, u32::MAX);
    write_volatile(POWER_INTENCLR as *mut u32, POWER_USB_INTERRUPT_MASK);
    write_volatile(NVIC_ICER1 as *mut u32, USBD_IRQ_BIT_IN_BANK1);

    // Disconnect the USB pull-up and fully disable the peripheral. The right
    // split half either stays USB-free (legacy) or initializes its own driver.
    write_volatile(USBD_USBPULLUP as *mut u32, 0);
    write_volatile(USBD_SHORTS as *mut u32, 0);
    write_volatile(USBD_ENABLE as *mut u32, 0);

    // Clear all USBD event latches that the bootloader may have left set.
    write_volatile(USBD_EVENTS_USBRESET as *mut u32, 0);
    write_volatile(USBD_EVENTS_STARTED as *mut u32, 0);
    for endpoint in 0..8 {
        write_volatile((USBD_EVENTS_ENDEPIN0 + endpoint * 4) as *mut u32, 0);
        write_volatile((USBD_EVENTS_ENDEPOUT0 + endpoint * 4) as *mut u32, 0);
    }
    write_volatile(USBD_EVENTS_EP0DATADONE as *mut u32, 0);
    write_volatile(USBD_EVENTS_ENDISOIN as *mut u32, 0);
    write_volatile(USBD_EVENTS_ENDISOOUT as *mut u32, 0);
    write_volatile(USBD_EVENTS_SOF as *mut u32, 0);
    write_volatile(USBD_EVENTS_USBEVENT as *mut u32, 0);
    write_volatile(USBD_EVENTS_EP0SETUP as *mut u32, 0);
    write_volatile(USBD_EVENTS_EPDATA as *mut u32, 0);
    write_volatile(USBD_EVENTCAUSE as *mut u32, u32::MAX);

    // POWER/CLOCK is shared with the radio stack, so only clear its USB
    // sources and pending bit. Its NVIC enable state is left for MPSL to own.
    write_volatile(POWER_EVENTS_USBDETECTED as *mut u32, 0);
    write_volatile(POWER_EVENTS_USBREMOVED as *mut u32, 0);
    write_volatile(POWER_EVENTS_USBPWRRDY as *mut u32, 0);
    write_volatile(NVIC_ICPR1 as *mut u32, USBD_IRQ_BIT_IN_BANK1);
    write_volatile(NVIC_ICPR0 as *mut u32, CLOCK_POWER_IRQ_BIT);

    // Complete all MMIO writes before cortex-m-rt initializes RAM and enters
    // the async executor.
    cortex_m::asm::dsb();
    cortex_m::asm::isb();

    // Prevent the compiler from treating the cleanup as write-only dead code
    // across LTO. Volatile writes already provide the required side effects;
    // this read is a final ordering point for the disabled USBD register.
    let _ = read_volatile(USBD_ENABLE as *const u32);
}
