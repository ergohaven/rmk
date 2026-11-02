#![no_main]
#![no_std]

mod battery_level;
mod battery_nrf;
mod board;
#[path = "qube_layer_led.rs"]
mod layer_led;
mod module_settings;
mod touchpad;
mod trackball;
#[cfg(feature = "right_uf2_cleanup")]
mod uf2_usb_cleanup;

use rmk::macros::rmk_peripheral;

#[rmk_peripheral(id = 1)]
mod keyboard_peripheral {
    add_interrupt!(
        TWISPI1 => ::embassy_nrf::twim::InterruptHandler<::embassy_nrf::peripherals::TWISPI1>;
    );

    #[register_processor]
    fn layer_led() -> crate::layer_led::LayerLed {
        let mut config = ::embassy_nrf::pwm::Config::default();
        config.prescaler = ::embassy_nrf::pwm::Prescaler::Div1;
        config.max_duty = 20;
        config.sequence_load = ::embassy_nrf::pwm::SequenceLoad::Common;
        let led = ::embassy_nrf::pwm::SequencePwm::new_1ch(p.PWM0, p.P0_30, config).unwrap();
        crate::layer_led::LayerLed::new(led, 1)
    }

    #[register_processor]
    fn module_settings_sync() -> crate::module_settings::ModuleSettingsSync {
        crate::module_settings::ModuleSettingsSync::new()
    }

    #[register_processor]
    fn trackball() -> crate::trackball::Trackball {
        crate::trackball::Trackball::new_peripheral(
            crate::trackball::new_trackball_from_pins(
                1,
                crate::board::TrackballPins::new(p.P0_01, p.P0_00, p.P0_05, p.P1_09),
            ),
            1,
        )
    }

    #[register_processor]
    fn touchpad() -> crate::touchpad::Touchpad {
        static TOUCH_TWIM_TX_BUF: ::static_cell::StaticCell<[u8; 4]> = ::static_cell::StaticCell::new();
        let mut config = ::embassy_nrf::twim::Config::default();
        config.frequency = ::embassy_nrf::twim::Frequency::K400;
        config.sda_pullup = true;
        config.scl_pullup = true;
        let i2c = ::embassy_nrf::twim::Twim::new(
            p.TWISPI1,
            Irqs,
            p.P0_24,
            p.P0_13,
            config,
            TOUCH_TWIM_TX_BUF.init([0; 4]),
        );
        crate::touchpad::Touchpad::new(3, i2c)
    }

    #[register_processor]
    fn battery() -> crate::battery_nrf::K04Battery {
        crate::battery_nrf::K04Battery::new(p.SAADC, p.P0_31)
    }
}
