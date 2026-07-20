#![no_main]
#![no_std]

mod battery_nrf;
mod layer_led;
mod layer_names;
mod module_settings;
mod touchpad;
mod trackball;

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    add_interrupt!(
        TWISPI1 => ::embassy_nrf::twim::InterruptHandler<::embassy_nrf::peripherals::TWISPI1>;
    );

    // Hardware diagnostic: keep the production K:04 matrix, storage, Vial,
    // USB descriptor and linker layout, but do not initialize or run the BLE
    // stack. This isolates the common BLE central backend from the base USB
    // runtime without changing production firmware.
    #[Overwritten(ChipInit)]
    fn usb_only_chip_init() {
        let mut config = ::embassy_nrf::config::Config::default();
        config.dcdc.reg0_voltage = Some(::embassy_nrf::config::Reg0Voltage::_3V3);
        config.dcdc.reg0 = true;
        config.dcdc.reg1 = true;
        let p = ::embassy_nrf::init(config);
    }

    #[Overwritten(Entry)]
    async fn usb_only_entry() {
        use ::rmk::core_traits::Runnable;

        let mut usb_transport = ::rmk::usb::UsbTransport::new(driver, rmk_config.device_config);
        ::rmk::run_all!(matrix, storage, keyboard, host_service, usb_transport)
    }

    #[register_processor(event)]
    fn layer_led() -> crate::layer_led::LayerLed {
        let mut config = ::embassy_nrf::pwm::Config::default();
        config.prescaler = ::embassy_nrf::pwm::Prescaler::Div1;
        config.max_duty = 20;
        config.sequence_load = ::embassy_nrf::pwm::SequenceLoad::Common;
        let led = ::embassy_nrf::pwm::SequencePwm::new_1ch(p.PWM0, p.P0_30, config).unwrap();
        crate::layer_led::LayerLed::new(led)
    }

    #[register_processor(event)]
    fn module_settings_sync() -> crate::module_settings::ModuleSettingsSync {
        crate::module_settings::ModuleSettingsSync::new()
    }

    #[register_processor(poll)]
    fn ergohaven_user_keys() -> ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys {
        ::rmk::processor::builtin::ergohaven::ErgohavenUserKeys::new()
    }

    #[register_processor(poll)]
    fn trackball() -> crate::trackball::Trackball {
        crate::trackball::Trackball::new(
            crate::trackball::new_trackball_from_pins(0, p.P0_01, p.P0_00, p.P0_05, p.P1_09),
            0,
        )
    }

    #[register_processor(poll)]
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
        crate::touchpad::Touchpad::new(2, i2c)
    }

    #[register_processor(event)]
    fn battery() -> crate::battery_nrf::K04Battery {
        crate::battery_nrf::K04Battery::new(p.SAADC, p.P0_31)
    }

    #[register_processor(poll)]
    fn pointing_processor() -> ::rmk::input_device::pointing::QubePointingModeProcessor<'static> {
        ::rmk::input_device::pointing::QubePointingModeProcessor::new(&keymap)
    }
}
