//! Ergohaven split battery (AIN7/P0_31, 1564k/2370k).
//!
//! Stock RMK `battery_adc_pin` is not used: its NrfAdc path calls
//! `calibrate().await`, which can hang under SoftDevice and leave the dongle
//! on "??" forever. This reader skips calibrate, times out samples, and
//! re-publishes BatteryStatusEvent periodically so the split loop can forward.

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

use crate::battery_level::BatteryLevelTracker;

bind_interrupts!(struct SaadcIrqs {
    SAADC => saadc::InterruptHandler;
});

const EMPTY_MV: i32 = 3300;
const FULL_MV: i32 = 4100;
// Calibrated from K:04 Qube halves: raw ~= 2520 at 3.150 V and 2600 at 3.281 V.
const RAW_PER_MV_NUM: i32 = 4;
const RAW_PER_MV_DEN: i32 = 5;
const CHARGING_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
const DISCHARGING_SAMPLE_INTERVAL: Duration = Duration::from_secs(15);

fn percent(val: u16) -> u8 {
    let val = val as i32;
    let empty = EMPTY_MV * RAW_PER_MV_NUM / RAW_PER_MV_DEN;
    let full = FULL_MV * RAW_PER_MV_NUM / RAW_PER_MV_DEN;

    if val >= full {
        100
    } else if val <= empty {
        0
    } else {
        (((val - empty) * 100) / (full - empty)) as u8
    }
}

pub struct K04Battery {
    saadc: Saadc<'static, 1>,
    level: BatteryLevelTracker,
}

impl K04Battery {
    pub fn new(saadc: Peri<'static, SAADC>, pin: Peri<'static, P0_31>) -> Self {
        interrupt::SAADC.set_priority(interrupt::Priority::P3);
        let channel = saadc::ChannelConfig::single_ended(pin.degrade_saadc());
        Self {
            saadc: Saadc::new(saadc, SaadcIrqs, saadc::Config::default(), [channel]),
            level: BatteryLevelTracker::default(),
        }
    }

    async fn read_raw(&mut self) -> Option<u16> {
        let mut buf = [0i16; 1];
        with_timeout(Duration::from_millis(200), self.saadc.sample(&mut buf))
            .await
            .ok()?;
        Some(buf[0].max(0) as u16)
    }

    async fn sample_raw(&mut self) -> Option<u16> {
        // The first conversion after idle can be biased by the SAADC sample
        // capacitor. Discard it, then let the high-impedance divider settle.
        self.read_raw().await?;
        Timer::after(Duration::from_millis(30)).await;

        let mut sum = 0u32;
        let mut count = 0u32;

        for _ in 0..3 {
            if let Some(raw) = self.read_raw().await {
                sum += raw as u32;
                count += 1;
            }
            Timer::after(Duration::from_millis(30)).await;
        }

        (count > 0).then_some((sum / count) as u16)
    }

    async fn publish_sample(&mut self) {
        let charge_state = current_charge_state();
        let charging = charge_state == ChargeState::Charging;
        let measured = if charging {
            // VBUS only tells us that charging power is present. The charger
            // raises terminal voltage toward 4.1 V before the cell is full,
            // so publishing that sample would create a false 100% reading.
            None
        } else {
            self.sample_raw().await.map(percent)
        };
        let level = self.level.observe(measured, charging);
        let status = if charging || level.is_some() {
            BatteryStatus::Available { charge_state, level }
        } else {
            BatteryStatus::Unavailable
        };
        // A Qube half answers every central connection status change with this
        // cache, so publishing around it would show the dongle "??".
        publish_battery_status(status);
    }
}

/// Raw nRF USB regulator status used by the v15 startup/LED diagnostics.
pub(crate) fn usb_power_status() -> (bool, bool) {
    let status = embassy_nrf::pac::POWER.usbregstatus().read();
    (status.vbusdetect(), status.outputrdy())
}

pub(crate) fn usb_powered() -> bool {
    // Restore the v14 charging semantics. On the split peripheral OUTPUTRDY
    // may remain low after the v16 bootloader-USBD cleanup even though VBUS is
    // physically present. VBUSDETECT is the authoritative cable signal; the
    // v16 cleanup removes the stale UF2 interrupt state that originally made
    // the right-side indication misleading.
    usb_power_status().0
}

/// Repeated briefly after boot so an RTT client attached after UF2 can verify
/// that no bootloader-owned USB interrupt source remains on the right half.
#[cfg(feature = "right_uf2_cleanup")]
pub(crate) fn log_uf2_usb_cleanup_state_v16() {
    let power = embassy_nrf::pac::POWER;
    let usbd = embassy_nrf::pac::USBD;
    let usb = power.usbregstatus().read();
    defmt::info!(
        "[UF2_USB_CLEANUP_V16] usbd_en={} usbd_int=0x{:08x} power_usb_int=0x{:03x} vbus={} outputrdy={} resetreas=0x{:08x}",
        usbd.enable().read().enable(),
        usbd.inten().read().0,
        power.intenset().read().0 & 0x380,
        usb.vbusdetect(),
        usb.outputrdy(),
        power.resetreas().read().0
    );
}

fn current_charge_state() -> ChargeState {
    if usb_powered() {
        ChargeState::Charging
    } else {
        ChargeState::Discharging
    }
}

impl Runnable for K04Battery {
    async fn run(&mut self) -> ! {
        Timer::after(Duration::from_millis(1000)).await;
        let mut refresh_sub = PeripheralBatteryRefreshEvent::subscriber();
        loop {
            self.publish_sample().await;
            let interval = match current_charge_state() {
                ChargeState::Charging => CHARGING_SAMPLE_INTERVAL,
                _ => DISCHARGING_SAMPLE_INTERVAL,
            };
            let _ = select(Timer::after(interval), refresh_sub.next_event()).await;
        }
    }
}
