#![no_main]
#![no_std]

mod board;
mod layer_led;
// The K:04 module sources, shared as they are. Phenom leaves their LED and
// Qube parts unused, and does not declare their K:04-only features.
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/module_settings.rs"]
mod module_settings;
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/touchpad.rs"]
mod touchpad;
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/trackball.rs"]
mod trackball;

use rmk::macros::rmk_peripheral;

#[rmk_peripheral(id = 0)]
mod keyboard_peripheral {
    add_interrupt!(
        I2C0_IRQ => ::embassy_rp::i2c::InterruptHandler<::embassy_rp::peripherals::I2C0>;
        PIO0_IRQ_0 => ::embassy_rp::pio::InterruptHandler<::embassy_rp::peripherals::PIO0>;
        DMA_IRQ_0 => ::embassy_rp::dma::InterruptHandler<::embassy_rp::peripherals::DMA_CH0>;
    );

    #[register_processor]
    fn module_settings_sync() -> crate::module_settings::ModuleSettingsSync {
        crate::module_settings::ModuleSettingsSync::new()
    }

    #[register_processor]
    fn layer_led() -> crate::layer_led::LayerLed {
        crate::layer_led::LayerLed::new(p.PIO0, p.DMA_CH0, p.PIN_25, Irqs)
    }

    #[register_processor]
    fn trackball() -> crate::trackball::Trackball {
        crate::trackball::Trackball::new_peripheral(
            crate::trackball::new_trackball_from_pins(1, crate::board::TrackballPins::new(p.PIN_10, p.PIN_11, p.PIN_9)),
            1,
        )
    }

    #[register_processor]
    fn touchpad() -> crate::touchpad::Touchpad {
        let mut config = ::embassy_rp::i2c::Config::default();
        config.frequency = 400_000;
        let i2c = ::embassy_rp::i2c::I2c::new_async(p.I2C0, p.PIN_21, p.PIN_20, Irqs, config);
        crate::touchpad::Touchpad::new(3, i2c)
    }
}
