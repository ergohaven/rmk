#![no_main]
#![no_std]

//! Ergohaven Qube dongle — USB HID central + ST7789 status screen.
//!
//! Build: `cargo make uf2-qube`

#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
#[allow(dead_code)]
#[path = "../../common/layer_names.rs"]
mod layer_names;
mod qube_display;
#[cfg(classic_encoder_settings)]
#[path = "../../common/encoder_device_settings.rs"]
mod encoder_device_settings;
#[cfg(velvet_pointing)]
#[path = "../../common/velvet_device_settings.rs"]
mod velvet_device_settings;
#[cfg(velvet_pointing)]
#[allow(dead_code)]
#[path = "../../common/velvet_pointing.rs"]
mod velvet_pointing;

include!(concat!(env!("OUT_DIR"), "/qube_profile_generated.rs"));

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    #[cfg(classic_encoder_settings)]
    #[register_processor(event)]
    fn encoder_settings_sync() -> crate::encoder_device_settings::EncoderSettingsSync {
        crate::encoder_device_settings::EncoderSettingsSync::new()
    }

    add_interrupt! {
        SPIM3 => ::embassy_nrf::spim::InterruptHandler<::embassy_nrf::peripherals::SPI3>;
    }

    #[register_processor(event)]
    fn display_processor() -> crate::qube_display::DongleScreen<Irqs> {
        crate::qube_display::create_processor(
            p.SPI3, p.P1_11, p.P1_10, p.P1_13, p.P0_28, p.P0_03, p.P0_02, Irqs,
        )
    }

    #[cfg(velvet_pointing)]
    #[register_processor(event)]
    fn pointing_mode() -> crate::velvet_pointing::VelvetPointingMode {
        crate::velvet_pointing::VelvetPointingMode::new()
    }

    #[cfg(velvet_pointing)]
    #[register_processor(event)]
    fn settings_broadcast() -> crate::velvet_device_settings::VelvetSettingsBroadcast {
        crate::velvet_device_settings::VelvetSettingsBroadcast::new()
    }

    #[register_processor(poll)]
    fn ergohaven_user_keys() -> ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys {
        ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys::new()
    }
}
