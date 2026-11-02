#![no_main]
#![no_std]

mod battery_nrf;
#[path = "../../common/ble_host_power.rs"]
mod ble_host_power;
#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
#[allow(dead_code)]
#[path = "../../common/layer_names.rs"]
mod layer_names;
#[path = "../../common/velvet_device_settings.rs"]
mod velvet_device_settings;
#[allow(dead_code)]
#[path = "../../common/velvet_pointing.rs"]
mod velvet_pointing;

const DEFAULT_LAYER_NAMES: [&str; 16] = default_layer_names::STANDARD_WITH_MOUSE;

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    #[register_processor]
    fn battery() -> crate::battery_nrf::SplitBattery {
        crate::battery_nrf::SplitBattery::new(p.SAADC, p.P0_31)
    }

    #[register_processor]
    fn pointing_mode() -> crate::velvet_pointing::VelvetPointingMode {
        crate::velvet_pointing::VelvetPointingMode::new()
    }

    #[register_processor]
    fn pointing_settings() -> crate::velvet_pointing::VelvetPointingSettingsSync {
        crate::velvet_pointing::VelvetPointingSettingsSync::new()
    }

    #[register_processor]
    fn settings_broadcast() -> crate::velvet_device_settings::VelvetSettingsBroadcast {
        crate::velvet_device_settings::VelvetSettingsBroadcast::new()
    }

    #[register_processor]
    fn ergohaven_user_keys() -> ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys {
        ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys::new()
    }
}
