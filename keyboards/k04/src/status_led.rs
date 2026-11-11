//! Shared patterns for standalone and Qube-half status LEDs.

use super::{BleAdvertisingMode, BleState, ConnectionStatus, Rgb};

/// A short host-status pulse; the caller restores power/layer colour between pulses.
pub(super) fn host_indicator(status: ConnectionStatus, mode: BleAdvertisingMode, elapsed_ms: u64) -> Option<Rgb> {
    if status.decide_active().is_some() {
        return None;
    }
    let (on, separator, color) = if status.ble.state == BleState::Advertising {
        match mode {
            BleAdvertisingMode::Pairing => {
                let phase = elapsed_ms % 300;
                (phase < 100, phase < 150, Rgb { r: 180, g: 0, b: 255 })
            }
            BleAdvertisingMode::Reconnecting => {
                let phase = elapsed_ms % 1_500;
                (phase < 120, phase < 200, Rgb { r: 255, g: 255, b: 255 })
            }
        }
    } else {
        let phase = elapsed_ms % 3_000;
        (
            phase < 100 || (200..300).contains(&phase),
            phase < 400,
            Rgb { r: 255, g: 255, b: 255 },
        )
    };
    if on {
        Some(color)
    } else if separator {
        Some(Rgb { r: 0, g: 0, b: 0 })
    } else {
        None
    }
}

pub(super) fn split_search_color(elapsed_ms: u64) -> Rgb {
    let phase = elapsed_ms % 2_000;
    if phase < 500 && phase % 200 < 100 {
        Rgb { r: 255, g: 180, b: 0 }
    } else {
        Rgb { r: 0, g: 0, b: 0 }
    }
}
