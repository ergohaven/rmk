#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_nrf::{bind_interrupts, nvmc::Nvmc, peripherals, usb};
use nrf_mpsl as _;
use rmk::config::{BehaviorConfig, DeviceConfig, PositionalConfig, StorageConfig};
use rmk::core_traits::Runnable;
use rmk::types::action::{EncoderAction, KeyAction};
use rmk::KeymapData;
use {defmt_rtt as _, panic_probe as _};

const ROW: usize = 10;
const COL: usize = 6;
const NUM_LAYER: usize = 16;
const NUM_ENCODER: usize = 2;
const DEFAULT_KEYS: [[[KeyAction; COL]; ROW]; NUM_LAYER] = [[[rmk::k!(No); COL]; ROW]; NUM_LAYER];
const DEFAULT_ENCODERS: [[EncoderAction; NUM_ENCODER]; NUM_LAYER] =
    [[rmk::encoder!(rmk::k!(No), rmk::k!(No)); NUM_ENCODER]; NUM_LAYER];

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

    let flash = rmk::storage::async_flash_wrapper(Nvmc::new(p.NVMC));
    let storage_config = StorageConfig {
        num_sectors: 32,
        start_addr: 0,
        clear_storage: false,
        clear_layout: true,
    };
    let mut behavior_config = BehaviorConfig::default();
    let positional_config = PositionalConfig::<ROW, COL>::default();
    let mut keymap_data = KeymapData::new_with_encoder(DEFAULT_KEYS, DEFAULT_ENCODERS);
    let (_keymap, _storage) = rmk::initialize_keymap_and_storage(
        &mut keymap_data,
        flash,
        &storage_config,
        &mut behavior_config,
        &positional_config,
    )
    .await;

    let driver = usb::Driver::new(p.USBD, Irqs, usb::vbus_detect::HardwareVbusDetect::new(Irqs));
    let device_config = DeviceConfig {
        vid: 0xE126,
        pid: 0x0071,
        manufacturer: "Ergohaven",
        product_name: "K:04 RMK storage probe",
        serial_number: "diag-rmk-storage-1c285672",
    };
    let mut transport = rmk::usb::UsbTransport::new(driver, device_config);
    transport.run().await;
}
