#![no_main]
#![no_std]

mod battery_nrf;
#[cfg(classic_encoder_settings)]
#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
#[cfg(classic_encoder_settings)]
#[path = "../../common/encoder_device_settings.rs"]
mod encoder_device_settings;
#[cfg(classic_encoder_settings)]
#[path = "../../common/layer_names.rs"]
mod layer_names;
#[cfg(classic_encoder_settings)]
include!(concat!(env!("OUT_DIR"), "/qube_profile_generated.rs"));
#[cfg(velvet_pointing)]
#[allow(dead_code)]
#[path = "../../common/velvet_pointing.rs"]
mod velvet_pointing;

use rmk::macros::rmk_peripheral;

#[rmk_peripheral(id = 1)]
mod keyboard_peripheral {
    #[cfg(classic_encoder_settings)]
    #[register_processor(event)]
    fn encoder_settings_sync() -> crate::encoder_device_settings::EncoderSettingsSync {
        crate::encoder_device_settings::EncoderSettingsSync::new()
    }

    #[register_processor(event)]
    fn battery() -> crate::battery_nrf::SplitBattery {
        crate::battery_nrf::SplitBattery::new(p.SAADC, p.P0_31)
    }

    #[cfg(velvet_pointing)]
    #[register_processor(event)]
    fn pointing_settings() -> crate::velvet_pointing::VelvetPointingSettingsSync {
        crate::velvet_pointing::VelvetPointingSettingsSync::new()
    }
}
