//! OP36 battery (ZMK op36.dtsi: AIN7/P0_31, 1564k/2370k).
//!
//! Stock RMK `battery_adc_pin` is not used: its NrfAdc path calls
//! `calibrate().await`, which can hang under SoftDevice. This reader skips
//! calibrate, times out samples, and re-publishes BatteryStatusEvent every 2s
//! so the split loop can forward.

use embassy_futures::select::select;
use embassy_nrf::interrupt;
use embassy_nrf::interrupt::InterruptExt;
use embassy_nrf::peripherals::{P0_31, SAADC};
use embassy_nrf::saadc::{self, Input as _, Saadc};
use embassy_nrf::{bind_interrupts, Peri};
use embassy_time::{with_timeout, Duration, Timer};
use rmk::core_traits::Runnable;
use rmk::event::{EventSubscriber, PeripheralBatteryRefreshEvent, SubscribableEvent};
use rmk::input_device::battery::publish_battery_status;
use rmk::types::battery::{BatteryStatus, ChargeState};

bind_interrupts!(struct SaadcIrqs {
    SAADC => saadc::InterruptHandler;
});

const MEASURED: i32 = 1564;
const TOTAL: i32 = 2370;

fn percent(val: u16) -> u8 {
    let val = val as i32;
    if val > 4755 * MEASURED / TOTAL {
        100
    } else if val < 4055 * MEASURED / TOTAL {
        0
    } else {
        ((val * TOTAL / MEASURED - 4055) / 7) as u8
    }
}

pub struct Op36Battery {
    saadc: Saadc<'static, 1>,
}

impl Op36Battery {
    pub fn new(saadc: Peri<'static, SAADC>, pin: Peri<'static, P0_31>) -> Self {
        interrupt::SAADC.set_priority(interrupt::Priority::P3);
        let channel = saadc::ChannelConfig::single_ended(pin.degrade_saadc());
        Self {
            saadc: Saadc::new(saadc, SaadcIrqs, saadc::Config::default(), [channel]),
        }
    }

    async fn publish_sample(&mut self) {
        let mut buf = [0i16; 1];
        let level = match with_timeout(Duration::from_millis(200), self.saadc.sample(&mut buf)).await {
            Ok(()) => {
                let raw = if buf[0] < 0 { 0 } else { buf[0] as u16 };
                percent(raw)
            }
            Err(_) => 0,
        };
        publish_battery_status(BatteryStatus::Available {
            charge_state: ChargeState::Unknown,
            level: Some(level),
        });
    }
}


impl Runnable for Op36Battery {
    async fn run(&mut self) -> ! {
        Timer::after(Duration::from_millis(1000)).await;
        let mut refresh_sub = PeripheralBatteryRefreshEvent::subscriber();
        loop {
            self.publish_sample().await;
            let _ = select(Timer::after(Duration::from_secs(2)), refresh_sub.next_event()).await;
        }
    }
}
