//! K:04 (nRF52840) hardware behind the module code that Phenom shares:
//! `trackball.rs`, `touchpad.rs`, `module_settings.rs` and `layer_names.rs`.

// Every K:04 binary links a different subset of these modules.
#![allow(dead_code)]

use embassy_nrf::gpio::{Flex, Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::Peri;
use rmk::split::ble::central::{set_split_link_profile, SplitLinkProfile};

pub type TrackballSck = Output<'static>;
pub type TrackballSdio = Flex<'static>;
pub type TrackballCs = Output<'static>;
pub type TrackballMotion = Input<'static>;
pub type TouchpadI2c = embassy_nrf::twim::Twim<'static>;

/// Factory "Ball axis" (left, right) and "Touch axis" (left, right) settings.
pub const DEFAULT_BALL_AXIS: [u8; 2] = [0, 0];
pub const DEFAULT_TOUCH_AXIS: [u8; 2] = [0, 0];

/// The PMW3610 pins of the K:04 module connector, the same on both halves.
pub struct TrackballPins {
    pub sck: TrackballSck,
    pub sdio: TrackballSdio,
    pub cs: TrackballCs,
    pub motion: Option<TrackballMotion>,
}

impl TrackballPins {
    pub fn new(
        sck: Peri<'static, embassy_nrf::peripherals::P0_01>,
        sdio: Peri<'static, embassy_nrf::peripherals::P0_00>,
        cs: Peri<'static, embassy_nrf::peripherals::P0_05>,
        motion: Peri<'static, embassy_nrf::peripherals::P1_09>,
    ) -> Self {
        Self {
            sck: Output::new(sck, Level::High, OutputDrive::Standard),
            sdio: Flex::new(sdio),
            cs: Output::new(cs, Level::High, OutputDrive::Standard),
            motion: Some(Input::new(motion, Pull::Up)),
        }
    }
}

/// VBUS transitions can reset the PMW3610, so the trackball re-checks its
/// registers whenever this changes.
pub fn vbus_detected() -> bool {
    // Same register as `battery_nrf::usb_power_status`, which the Qube binary
    // does not build.
    embassy_nrf::pac::POWER.usbregstatus().read().vbusdetect()
}

/// Switch the BLE split link of `peripheral_id` between the key-only and the
/// low-latency pointing cadence.
pub fn set_split_link_pointing(peripheral_id: usize, pointing: bool) {
    let profile = if pointing {
        SplitLinkProfile::Pointing
    } else {
        SplitLinkProfile::Keyboard
    };
    let _ = set_split_link_profile(peripheral_id, profile);
}

/// A peripheral trackball paces its reports to the BLE pointing connection
/// interval, see `trackball/motion_pacing.rs`.
pub const PACE_SPLIT_TRACKBALL: bool = true;

/// An encoder runs only while its module is selected, so a parked one
/// neither polls nor wakes the chip on battery.
pub const PARK_UNSELECTED_ENCODERS: bool = true;
