mod board;
#[path = "../../common/default_layer_names.rs"]
mod default_layer_names;
mod double_reset;
mod layer_led;
// The K:04 module sources, shared as they are. Phenom leaves their LED and
// Qube parts unused, and does not declare their K:04-only features.
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/layer_names.rs"]
mod layer_names;
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/module_settings.rs"]
mod module_settings;
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/touchpad.rs"]
mod touchpad;
#[allow(dead_code, unexpected_cfgs)]
#[path = "../../k04/src/trackball.rs"]
mod trackball;

const DEFAULT_LAYER_NAMES: [&str; 16] = default_layer_names::PHENOM;

mod role;

macro_rules! phenom {
    ($($side:tt)*) => {
        #[rmk::macros::rmk_central($($side)*)]
        mod keyboard {
            #[Overwritten(chip_init)]
            fn init_chip() {
                let config = ::embassy_rp::config::Config::default();
                let mut p = ::embassy_rp::init(config);
                crate::double_reset::check();
            }

            add_interrupt!(
                I2C0_IRQ => ::embassy_rp::i2c::InterruptHandler<::embassy_rp::peripherals::I2C0>;
                PIO0_IRQ_0 => ::embassy_rp::pio::InterruptHandler<::embassy_rp::peripherals::PIO0>;
            );

            #[register_processor]
            fn module_settings_sync() -> crate::module_settings::ModuleSettingsSync {
                crate::module_settings::ModuleSettingsSync::new()
            }

            // DMA_CH1 serves the storage flash, and DMA_IRQ_0 is bound for both
            // channels already.
            #[register_processor]
            fn layer_led() -> crate::layer_led::LayerLed {
                crate::layer_led::LayerLed::new(p.PIO0, p.DMA_CH0, p.PIN_25, Irqs)
            }


            // The peripheral role never allocates keyboard or pointing-processor subscribers.
            #[Overwritten(keyboard)]
            fn defer_keyboard() {}

            #[Overwritten(entry)]
            async fn run() {
                use ::rmk::core_traits::Runnable;
                use ::embassy_futures::select::{select, Either};
                let local = crate::role::matrix_config(crate::IS_RIGHT, ROW, COL);
                let remote = crate::role::matrix_config(!crate::IS_RIGHT, ROW, COL);
                let mut encoder = encoder_0;
                use ::rmk::types::protocol::rynk::ergohaven::FirmwareTarget;
                let host_service = host_service.with_firmware_target(if crate::IS_RIGHT {
                    FirmwareTarget::Peripheral(0)
                } else {
                    FirmwareTarget::Central
                });
                let mut usb_transport = ::rmk::usb::UsbTransport::new(driver, rmk_config.device_config)
                    .with_host_service(&host_service);
                {
                    // Keep this future pinned through election and the whole primary session.
                    let mut usb = ::core::pin::pin!(usb_transport.run());
                    let primary = match select(usb.as_mut(), crate::role::primary(
                        ::rmk::usb::wait_for_usb_configuration(),
                        ::embassy_time::Timer::after_secs(2),
                    )).await {
                        Either::First(_) => unreachable!(),
                        Either::Second(primary) => primary,
                    };
                    if primary {
                        let mut keyboard = ::rmk::keyboard::Keyboard::new(&keymap);
                        let mut pointing = ::rmk::input_device::pointing::QubePointingModeProcessor::new(&keymap);
                        let mut broadcast = crate::layer_names::ModuleSettingsBroadcast::new();
                        let mut wpm = ::rmk::processor::builtin::wpm::WpmProcessor::new();
                        let mut config = ::embassy_rp::i2c::Config::default();
                        config.frequency = 400_000;
                        let i2c = ::embassy_rp::i2c::I2c::new_async(p.I2C0, p.PIN_21, p.PIN_20, Irqs, config);
                        let (mut trackball, mut touchpad) = crate::modules(i2c,
                            crate::board::TrackballPins::new(p.PIN_10, p.PIN_11, p.PIN_9), true);
                        ::rmk::join_all!(
                            usb.as_mut(),
                            ::rmk::run_all!(matrix, encoder, storage, keyboard, pointing, broadcast,
                                wpm, module_settings_sync, layer_led, trackball, touchpad, watchdog_runner),
                            ::rmk::split::central::run_peripheral_manager(0, uart0, remote),
                        ).await;
                        unreachable!();
                    }
                }
                // Embassy requires disable after cancellation before any USB reuse.
                usb_transport.disable().await;
                let mut config = ::embassy_rp::i2c::Config::default();
                config.frequency = 400_000;
                let i2c = ::embassy_rp::i2c::I2c::new_async(p.I2C0, p.PIN_21, p.PIN_20, Irqs, config);
                let (mut trackball, mut touchpad) = crate::modules(i2c,
                    crate::board::TrackballPins::new(p.PIN_10, p.PIN_11, p.PIN_9), false);
                // Subscribe before the split service can publish its first status.
                use ::rmk::event::{EventSubscriber, SubscribableEvent};
                let mut peer_status = ::rmk::event::ConnectionStatusChangeEvent::subscriber();
                let recovery = async {
                    let connected = async {
                        loop {
                            let status = peer_status.next_event().await.0;
                            if matches!(status.usb, ::rmk::types::connection::UsbState::Configured
                                | ::rmk::types::connection::UsbState::Suspended) { break; }
                        }
                    };
                    if matches!(select(connected, ::embassy_time::Timer::after_secs(3)).await, Either::Second(())) {
                        // Retry election after a slow host or a missing primary, as QMK does.
                        ::cortex_m::peripheral::SCB::sys_reset();
                    }
                    ::core::future::pending::<()>().await;
                };
                ::rmk::join_all!(
                    recovery,
                    crate::run_peripheral_matrix(&mut matrix, local),
                    ::rmk::run_all!(encoder, module_settings_sync, layer_led, trackball, touchpad, watchdog_runner),
                    ::rmk::split::peripheral::run_rmk_split_peripheral(uart0),
                ).await;
            }
        }
    };
}

fn modules(
    i2c: board::TouchpadI2c,
    pins: board::TrackballPins,
    primary: bool,
) -> (trackball::Trackball, touchpad::Touchpad) {
    let (ball_id, touch_id) = role::module_ids(IS_RIGHT);
    let sensor = trackball::new_trackball_from_pins(ball_id, pins);
    let ball = if primary {
        trackball::Trackball::new_central(sensor, ball_id)
    } else {
        trackball::Trackball::new_peripheral(sensor, ball_id)
    };
    (ball, touchpad::Touchpad::new(touch_id, i2c))
}

async fn run_peripheral_matrix<M: rmk::input_device::InputDevice<Event = rmk::event::KeyboardEvent>>(
    matrix: &mut M,
    local: rmk::split::PeripheralMatrixConfig,
) -> ! {
    loop {
        if let Some(event) = role::local_matrix_event(matrix.read_event().await, local) {
            rmk::event::publish_event_async(event).await;
        }
        embassy_futures::yield_now().await;
    }
}
