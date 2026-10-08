#![no_main]
#![no_std]

mod battery_nrf;
#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
#[path = "../../common/encoder_device_settings.rs"]
mod encoder_device_settings;
#[path = "../../common/layer_names.rs"]
mod layer_names;

const DEFAULT_LAYER_NAMES: [&str; 16] = default_layer_names::STANDARD_NO_MOUSE;
const ENCODER_COUNT: usize = 6;
const ENCODER_SETTING_KEYS: &[u16] = &[
    200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 340, 341, 342, 343, 344, 345, 346,
    347, 348, 349, 350, 351,
];

use rmk::macros::rmk_peripheral;

#[rmk_peripheral(id = 0)]
mod keyboard_peripheral {
    #[register_processor(event)]
    fn encoder_settings_sync() -> crate::encoder_device_settings::EncoderSettingsSync {
        crate::encoder_device_settings::EncoderSettingsSync::new()
    }

    #[register_processor(event)]
    fn battery() -> crate::battery_nrf::SplitBattery {
        crate::battery_nrf::SplitBattery::new(p.SAADC, p.P0_31)
    }
}
