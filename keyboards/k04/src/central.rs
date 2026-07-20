#![no_main]
#![no_std]

mod layer_names;

use rmk::macros::rmk_central;

#[rmk_central]
mod keyboard_central {
    #[Overwritten(ChipInit)]
    fn usb_only_chip_init() {
        let mut config = ::embassy_nrf::config::Config::default();
        config.dcdc.reg0_voltage = Some(::embassy_nrf::config::Reg0Voltage::_3V3);
        config.dcdc.reg0 = true;
        config.dcdc.reg1 = true;
        let p = ::embassy_nrf::init(config);

        ::embassy_nrf::pac::CLOCK.tasks_hfclkstart().write_value(1);
        while ::embassy_nrf::pac::CLOCK.events_hfclkstarted().read() != 1 {}
    }

    #[Overwritten(Entry)]
    async fn initialization_probe_entry() {
        use ::rmk::core_traits::Runnable;

        let mut usb_transport = ::rmk::usb::UsbTransport::new(driver, rmk_config.device_config);
        usb_transport.run().await;
    }
}
