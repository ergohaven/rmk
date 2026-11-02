//! Built-in processor module for RMK
//!
//! This module contains built-in processor implementations for output devices.

pub mod battery_led;
#[cfg(feature = "_dfu")]
pub mod dfu_led;
#[cfg(feature = "_ble")]
pub mod ergohaven;
pub mod led_indicator;
pub mod wpm;
