//! RP2040 hardware behind the K:04 module code that Phenom shares:
//! `trackball.rs`, `touchpad.rs`, `module_settings.rs` and `layer_names.rs`.

// The central and the peripheral link different subsets of these modules.
#![allow(dead_code)]

use embassy_rp::gpio::{Flex, Input, Level, Output};
use embassy_rp::peripherals::{I2C0, PIN_10, PIN_11, PIN_9};
use embassy_rp::Peri;

pub type TrackballSck = Output<'static>;
pub type TrackballSdio = Flex<'static>;
pub type TrackballCs = Output<'static>;
pub type TrackballMotion = Input<'static>;
pub type TouchpadI2c = embassy_rp::i2c::I2c<'static, I2C0, embassy_rp::i2c::Async>;

/// Factory "Ball axis" (left, right) and "Touch axis" (left, right) settings.
///
/// Phenom modules sit rotated against the K:04 ones. The ball axes keep the
/// QMK firmware's default orientation. The left touch axis comes from testing
/// on hardware; QMK turns the right touchpad 180° from the left one.
pub const DEFAULT_BALL_AXIS: [u8; 2] = [2, 2];
pub const DEFAULT_TOUCH_AXIS: [u8; 2] = [3, 1];

/// The PMW3610 pins of the module slot, the same on both halves. The slot has
/// no MOTION line, so the trackball polls the sensor.
pub struct TrackballPins {
    pub sck: TrackballSck,
    pub sdio: TrackballSdio,
    pub cs: TrackballCs,
    pub motion: Option<TrackballMotion>,
}

impl TrackballPins {
    pub fn new(sck: Peri<'static, PIN_10>, sdio: Peri<'static, PIN_11>, cs: Peri<'static, PIN_9>) -> Self {
        Self {
            sck: Output::new(sck, Level::High),
            sdio: Flex::new(sdio),
            cs: Output::new(cs, Level::High),
            motion: None,
        }
    }
}

/// Phenom has no battery and is always powered over the cable.
pub fn vbus_detected() -> bool {
    true
}

/// The serial split link runs at one fixed rate, so there is no pointing
/// cadence to switch to.
pub fn set_split_link_pointing(_peripheral_id: usize, _pointing: bool) {}

/// The serial split link keeps up with trackball reports at the local rate.
pub const PACE_SPLIT_TRACKBALL: bool = false;

/// The encoder pins are free whatever module sits in the slot, and there is no
/// battery to save, so both encoders always run, as in the QMK firmware.
pub const PARK_UNSELECTED_ENCODERS: bool = false;
