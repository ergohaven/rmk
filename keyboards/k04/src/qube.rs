#![no_main]
#![no_std]

//! K:04 Qube dongle — USB HID central + ST7789 status screen.
//!
//! Build: `cargo make uf2-qube`

mod board;
#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
mod layer_names;
mod media_label;
mod module_settings;
mod qube_display;

const DEFAULT_LAYER_NAMES: [&str; 16] = default_layer_names::STANDARD_WITH_MOUSE;

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    add_interrupt! {
        SPI2 => ::embassy_nrf::spim::InterruptHandler<::embassy_nrf::peripherals::SPI2>;
    }

    #[register_processor]
    fn display_processor() -> crate::qube_display::DongleScreen<Irqs> {
        crate::qube_display::create_processor(p.SPI2, p.P1_11, p.P1_10, p.P1_13, p.P0_28, p.P0_03, p.P0_02, Irqs)
    }

    #[register_processor]
    fn module_settings_broadcast() -> crate::layer_names::ModuleSettingsBroadcast {
        crate::layer_names::ModuleSettingsBroadcast::new()
    }

    #[register_processor]
    fn module_settings_sync() -> crate::module_settings::ModuleSettingsSync {
        crate::module_settings::ModuleSettingsSync::new()
    }

    #[register_processor]
    fn ergohaven_user_keys() -> ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys {
        ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys::new()
    }

    #[register_processor]
    fn pointing_processor() -> ::rmk::input_device::pointing::QubePointingModeProcessor<'static> {
        ::rmk::input_device::pointing::QubePointingModeProcessor::new(&keymap)
    }
}
