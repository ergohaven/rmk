#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_nrf::{bind_interrupts, peripherals, usb};
use nrf_mpsl as _;
use rmk::config::DeviceConfig;
use rmk::core_traits::Runnable;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut nrf_config = embassy_nrf::config::Config::default();
    nrf_config.dcdc.reg0_voltage = Some(embassy_nrf::config::Reg0Voltage::_3V3);
    nrf_config.dcdc.reg0 = true;
    nrf_config.dcdc.reg1 = true;
    let p = embassy_nrf::init(nrf_config);

    embassy_nrf::pac::CLOCK.tasks_hfclkstart().write_value(1);
    while embassy_nrf::pac::CLOCK.events_hfclkstarted().read() != 1 {}

    let driver = usb::Driver::new(p.USBD, Irqs, usb::vbus_detect::HardwareVbusDetect::new(Irqs));
    let device_config = DeviceConfig {
        vid: 0xE126,
        pid: 0x0071,
        manufacturer: "Ergohaven",
        product_name: "K:04 RMK USB transport probe",
        serial_number: "diag-rmk-usb-d56fbe5a",
    };
    let mut transport = rmk::usb::UsbTransport::new(driver, device_config);
    transport.run().await;
}
