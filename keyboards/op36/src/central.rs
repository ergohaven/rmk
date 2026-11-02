#![no_main]
#![no_std]

mod battery_nrf;
#[path = "../../common/ble_host_power.rs"]
mod ble_host_power;
#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
#[path = "../../common/layer_names.rs"]
mod layer_names;

const DEFAULT_LAYER_NAMES: [&str; 16] = default_layer_names::STANDARD_NO_MOUSE;

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    #[register_processor]
    fn battery() -> crate::battery_nrf::Op36Battery {
        crate::battery_nrf::Op36Battery::new(p.SAADC, p.P0_31)
    }

    #[register_processor]
    fn ergohaven_user_keys() -> ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys {
        ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys::new()
    }
}
