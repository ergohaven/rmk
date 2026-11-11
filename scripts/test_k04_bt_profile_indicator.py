#!/usr/bin/env python3
"""Run the standalone K:04 display decision and color helpers on the host.

Hardware event delivery and physical LED appearance still need device checks.
"""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def block(source, marker):
    start = source.index(marker)
    opening = source.index("{", start)
    depth = 0
    for pos in range(opening, len(source)):
        depth += (source[pos] == "{") - (source[pos] == "}")
        if depth == 0:
            return source[start:pos + 1]
    raise AssertionError(marker)


PRELUDE = r'''
#![allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb { r: u8, g: u8, b: u8 }
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Instant(u64);
struct Duration(u64);
impl Instant { fn duration_since(self, start: Self) -> Duration { Duration(self.0 - start.0) } }
impl Duration { fn as_millis(self) -> u64 { self.0 } }
#[derive(Clone, Copy, PartialEq, Eq)] enum BleState { Advertising, Connected, Inactive, Sleeping }
#[derive(Clone, Copy, PartialEq, Eq)] enum SplitConnectionState { Searching, Connected }
#[derive(Clone, Copy, PartialEq, Eq)] enum ConnectionType { Ble, Usb }
#[derive(Clone, Copy)] enum UsbState { Configured, Suspended, Disconnected }
#[derive(Clone, Copy)] struct ConnectionStatus { preferred: ConnectionType, usb: UsbState }
#[derive(Clone, Copy)] enum Overlay { Profile(u8), HostConnected, SplitConnected, Battery(u8), LowBattery }
#[derive(Clone, Copy)] struct TimedOverlay { kind: Overlay, ends: Instant }
mod module_settings {
    use super::*;
    pub static BRIGHTNESS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(255);
    pub fn led_brightness() -> u8 { BRIGHTNESS.load(std::sync::atomic::Ordering::Relaxed) }
    pub fn charge_indicator_enabled() -> bool { true }
    pub fn layer_color(_: u8) -> Rgb { Rgb { r: 13, g: 17, b: 19 } }
    pub fn bt_profile_color(profile: u8) -> Rgb {
        [Rgb {r:255,g:0,b:0}, Rgb {r:0,g:255,b:0}, Rgb {r:0,g:0,b:255},
         Rgb {r:255,g:80,b:0}, Rgb {r:80,g:0,b:255}][profile as usize]
    }
}
struct LayerLed {
    sleeping: bool, split_state: SplitConnectionState, indicator_phase_started: Instant,
    overlay: Option<TimedOverlay>, current_layer: Option<u8>, layer_deadline: Option<Instant>,
    connection_status: Option<ConnectionStatus>, ble_state: BleState,
    ble_profile: u8,
}
fn led() -> LayerLed { LayerLed { sleeping: false, split_state: SplitConnectionState::Connected,
    indicator_phase_started: Instant(0), overlay: None, current_layer: Some(0),
    layer_deadline: None, connection_status: None, ble_state: BleState::Advertising,
    ble_profile: 0 } }
'''

TESTS = r'''
#[test]
fn profile_blink_and_existing_priorities() {
    for brightness in [0, 128, 255] {
        module_settings::BRIGHTNESS.store(brightness, std::sync::atomic::Ordering::Relaxed);
        for profile in 0..5 {
                let mut led = led(); led.ble_profile = profile;
                for (ms, on) in [(0,true),(179,true),(180,false),(359,false),(360,true)] {
                    let raw = module_settings::bt_profile_color(profile);
                    let expected = if on { Rgb {
                        r: (u16::from(raw.r)*u16::from(brightness)/255) as u8,
                        g: (u16::from(raw.g)*u16::from(brightness)/255) as u8,
                        b: (u16::from(raw.b)*u16::from(brightness)/255) as u8,
                    } } else { Rgb {r:0,g:0,b:0} };
                    assert_eq!(led.display_color(Instant(ms), (false,false)), (expected,5));
                }
                led.sleeping = true;
                assert_eq!(led.display_color(Instant(0), (false,false)).1, 0);
                led.sleeping = false; led.split_state = SplitConnectionState::Searching;
                assert_eq!(led.display_color(Instant(0), (false,false)).1, 1);
                led.split_state = SplitConnectionState::Connected;
                led.overlay = Some(TimedOverlay {kind: Overlay::Profile(profile), ends: Instant(1000)});
                assert_eq!(led.display_color(Instant(200), (false,false)).1, 2);
                led.overlay = None;
                assert_eq!(led.display_color(Instant(0), (true,true)).1, 3);
                led.connection_status = Some(ConnectionStatus {preferred: ConnectionType::Usb, usb: UsbState::Configured});
                assert_eq!(led.display_color(Instant(0), (false,false)).1, 6);
                led.connection_status = None; led.ble_state = BleState::Connected;
                assert_eq!(led.display_color(Instant(0), (false,false)).1, 6);
        }
    }
}
'''

source = (ROOT / 'keyboards/k04/src/layer_led.rs').read_text()
helpers = [block(source, match.group(0)) for match in re.finditer(r'^fn (?!indicator_duration)[a-z_]+\(', source, re.M)]
constants = re.findall(r'^const STATUS_BLINK_[^\n]+', source, re.M)
harness = '\n'.join([PRELUDE, *constants, *helpers, 'impl LayerLed {',
    block(source, 'fn display_color('), block(source, 'fn layer_color('), '}', TESTS])
with tempfile.TemporaryDirectory(prefix='k04-bt-indicator-') as directory:
    path = Path(directory) / 'indicator.rs'
    binary = Path(directory) / 'indicator'
    path.write_text(harness)
    subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
