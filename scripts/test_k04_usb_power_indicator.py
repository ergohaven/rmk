#!/usr/bin/env python3
"""Host probes of K:04 USB gates, complete LED decisions, and charge reporting.

Hardware peripherals and transport snapshots are stubbed; colour decisions
are compiled from production source. Physical charging is not tested.
"""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def block(source: str, start: int) -> str:
    opening = source.index("{", start)
    depth = 0
    for pos in range(opening, len(source)):
        if source[pos] == "{":
            depth += 1
        elif source[pos] == "}":
            depth -= 1
            if depth == 0:
                return source[start:pos + 1]
    raise AssertionError("Unclosed Rust block")


PRELUDE = r'''
#![allow(dead_code)]
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb { r: u8, g: u8, b: u8 }
mod module_settings {
    use super::*;
    pub static ENABLED: AtomicBool = AtomicBool::new(true);
    pub static BRIGHTNESS: AtomicU8 = AtomicU8::new(255);
    pub fn charge_indicator_enabled() -> bool { ENABLED.load(Ordering::Relaxed) }
    pub fn led_brightness() -> u8 { BRIGHTNESS.load(Ordering::Relaxed) }
}
struct LayerLed { current_layer: Option<u8>, latest_battery: Option<u8> }
const FALLTHROUGH: (Rgb, u8) = (Rgb { r: 1, g: 2, b: 3 }, 255);
'''

TEST = r'''
#[test]
fn usb_power_does_not_claim_charging_or_full_battery() {
    for brightness in [0, 1, 128, 255] {
        module_settings::BRIGHTNESS.store(brightness, Ordering::Relaxed);
        for enabled in [false, true] {
            module_settings::ENABLED.store(enabled, Ordering::Relaxed);
            for layer in [None, Some(0), Some(1), Some(15)] {
                for vbus in [false, true] {
                    for output_ready in [false, true] {
                        for level in [None, Some(0), Some(74), Some(94), Some(95), Some(100)] {
                            let led = LayerLed { current_layer: layer, latest_battery: level };
                            let expected = if enabled && layer == Some(0) && vbus {
                                (Rgb { r: 0, g: (45u16 * u16::from(brightness) / 255) as u8,
                                    b: (64u16 * u16::from(brightness) / 255) as u8 }, 3)
                            } else {
                                FALLTHROUGH
                            };
                            assert_eq!(led.usb_indicator((vbus, output_ready)), expected,
                                "layer={layer:?}, VBUS={vbus}, OUTPUTRDY={output_ready}, \
                                 level={level:?}, enabled={enabled}, brightness={brightness}");
                        }
                    }
                }
            }
        }
    }
}
'''

failed = []
with tempfile.TemporaryDirectory(prefix="k04-usb-power-") as directory:
    for name in ("layer_led", "qube_layer_led"):
        source = (ROOT / f"keyboards/k04/src/{name}.rs").read_text()
        marker = "if self.current_layer == Some(0) && vbus_status.0"
        assert source.count(marker) == 1, name
        branch = block(source, source.index(marker))
        # Include the production color/scaling helpers, including the old
        # yellow/green branch so this probe also runs against the unfixed tree.
        helpers = []
        for function in ("color_cyan", "color_yellow", "color_green", "scale_color", "scale"):
            match = re.search(r"^fn " + function + r"\b", source, re.M)
            if match:
                helpers.append(block(source, match.start()))
        constants = re.findall(r"^const CHARGED_BATTERY_MIN:[^\n]+", source, re.M)
        harness = "\n".join([
            PRELUDE, *constants, *helpers,
            "impl LayerLed { fn usb_indicator(&self, vbus_status: (bool, bool)) -> (Rgb, u8) {",
            branch, "FALLTHROUGH } }", TEST,
        ])
        source_file = Path(directory) / f"{name}.rs"
        binary = Path(directory) / name
        source_file.write_text(harness)
        subprocess.run(["rustc", "--edition=2021", "--test", str(source_file), "-o", str(binary)], check=True)
        print(f"Testing {name}", flush=True)
        if subprocess.run([str(binary)]).returncode:
            failed.append(name)

# Compile each complete production display decision with hardware-free state.
DISPLAY_PRELUDE = r"""
#![allow(dead_code)]
extern crate self as defmt;
#[macro_export]
macro_rules! info { ($($arg:tt)*) => {}; }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb { r: u8, g: u8, b: u8 }
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Instant(u64);
struct Duration(u64);
impl Instant { fn duration_since(self, earlier: Self) -> Duration { Duration(self.0 - earlier.0) } }
impl Instant { fn now() -> Self { Self(0) } }
impl Duration { fn as_millis(self) -> u64 { self.0 } fn from_millis(ms: u64) -> Self { Self(ms) } }
impl std::ops::Add<Duration> for Instant { type Output = Self; fn add(self, rhs: Duration) -> Self { Self(self.0 + rhs.0) } }
#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitConnectionState { Idle, Searching, Connected }
#[derive(Clone, Copy, PartialEq, Eq)]
enum BleState { Inactive, Sleeping, Advertising, Connected }
#[derive(Clone, Copy, PartialEq, Eq)]
enum BleAdvertisingMode { Pairing, Reconnecting }
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionType { Usb, Ble }
#[derive(Clone, Copy, PartialEq, Eq)]
enum UsbState { Disabled, Enabled, Configured, Suspended }
#[derive(Clone, Copy)]
struct BleStatus { state: BleState }
#[derive(Clone, Copy)]
struct ConnectionStatus { usb: UsbState, ble: BleStatus, preferred: ConnectionType }
mod module_settings {
    use super::*;
    pub fn charge_indicator_enabled() -> bool { true }
    pub fn led_brightness() -> u8 { 255 }
    pub fn layer_color(_: u8) -> Rgb { Rgb { r: 12, g: 34, b: 56 } }
    pub fn bt_profile_color(_: u8) -> Rgb { Rgb { r: 1, g: 2, b: 3 } }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay { Profile(u8), HostConnected, Battery(u8), LowBattery }
#[derive(Clone, Copy)]
struct TimedOverlay { kind: Overlay, ends: Instant }
struct LayerLed {
    sleeping: bool, split_state: SplitConnectionState, current_layer: Option<u8>,
    indicator_phase_started: Instant, phase_started: Instant, overlay: Option<TimedOverlay>,
    connected_until: Option<Instant>, connection_status: Option<ConnectionStatus>,
    ble_state: BleState, ble_advertising_mode: BleAdvertisingMode, layer_deadline: Option<Instant>,
}
impl LayerLed {
    fn fixture() -> Self {
        Self { sleeping: false, split_state: SplitConnectionState::Connected, current_layer: Some(0),
            indicator_phase_started: Instant(0), phase_started: Instant(0), overlay: None,
            connected_until: None, connection_status: None, ble_state: BleState::Advertising,
            ble_advertising_mode: BleAdvertisingMode::Reconnecting, layer_deadline: None }
    }
    fn host(&mut self, usb: UsbState, ble: BleState) {
        self.ble_state = ble;
        self.connection_status = Some(ConnectionStatus { usb, ble: BleStatus { state: ble }, preferred: ConnectionType::Usb });
    }
}
"""
DISPLAY_TEST = r"""
#[test]
fn usb_power_does_not_hide_host_reconnection_or_pairing() {
    let mut led = LayerLed::fixture();
    led.host(UsbState::Disabled, BleState::Advertising);
    assert_eq!(led.display_color(Instant(10), (true, false)).0, Rgb { r: 255, g: 255, b: 255 });
    assert_eq!(led.display_color(Instant(500), (true, false)).0, Rgb { r: 0, g: 45, b: 64 });
    led.ble_advertising_mode = BleAdvertisingMode::Pairing;
    assert_eq!(led.display_color(Instant(10), (true, false)).0, Rgb { r: 180, g: 0, b: 255 });
}
#[test]
fn active_usb_suppresses_background_ble_advertising() {
    let mut led = LayerLed::fixture();
    led.host(UsbState::Configured, BleState::Advertising);
    assert_eq!(led.display_color(Instant(10), (true, true)).0, Rgb { r: 0, g: 45, b: 64 });
    led.host(UsbState::Suspended, BleState::Advertising);
    assert_eq!(led.display_color(Instant(10), (true, true)).0, Rgb { r: 0, g: 45, b: 64 });
}
#[test]
fn disconnected_has_two_pulses_with_background_between() {
    let mut led = LayerLed::fixture();
    led.host(UsbState::Disabled, BleState::Inactive);
    for ms in [10, 210] {
        assert_eq!(led.display_color(Instant(ms), (true, false)).0, Rgb { r: 255, g: 255, b: 255 });
    }
    assert_eq!(led.display_color(Instant(500), (true, false)).0, Rgb { r: 0, g: 45, b: 64 });
}
#[test]
fn host_pulses_have_dark_separators_even_on_a_matching_layer_colour() {
    let status = ConnectionStatus { usb: UsbState::Disabled, ble: BleStatus { state: BleState::Advertising }, preferred: ConnectionType::Usb };
    let off = Rgb { r: 0, g: 0, b: 0 };
    assert_eq!(status_led::host_indicator(status, BleAdvertisingMode::Reconnecting, 150), Some(off));
    assert_eq!(status_led::host_indicator(status, BleAdvertisingMode::Pairing, 120), Some(off));
}
#[test]
fn split_search_has_three_amber_pulses_and_sleep_is_dark() {
    let mut led = LayerLed::fixture();
    led.split_state = SplitConnectionState::Searching;
    for ms in [10, 210, 410] {
        assert_eq!(led.display_color(Instant(ms), (true, false)).0, Rgb { r: 255, g: 180, b: 0 });
    }
    for ms in [110, 310, 600] {
        assert_eq!(led.display_color(Instant(ms), (true, false)).0, Rgb { r: 0, g: 0, b: 0 });
    }
    led.sleeping = true;
    assert_eq!(led.display_color(Instant(10), (true, false)).0, Rgb { r: 0, g: 0, b: 0 });
}
#[test]
fn connected_usb_without_power_overlay_preserves_layer_color() {
    let mut led = LayerLed::fixture();
    led.host(UsbState::Configured, BleState::Advertising);
    led.current_layer = Some(2);
    assert_eq!(led.display_color(Instant(500), (true, true)).0, Rgb { r: 12, g: 34, b: 56 });
}
#[test]
fn split_recovery_only_confirms_connection_when_host_is_ready() {
    let mut led = LayerLed::fixture();
    led.split_state = SplitConnectionState::Searching;
    led.host(UsbState::Disabled, BleState::Inactive);
    led.transition_split(SplitConnectionState::Connected);
    assert_eq!(led.display_color(Instant(500), (true, false)).0, Rgb { r: 0, g: 45, b: 64 });
    led.transition_split(SplitConnectionState::Searching);
    led.host(UsbState::Configured, BleState::Inactive);
    led.transition_split(SplitConnectionState::Connected);
    assert_eq!(led.display_color(Instant(500), (true, true)).0, Rgb { r: 255, g: 255, b: 255 });
}
"""
with tempfile.TemporaryDirectory(prefix="k04-display-") as directory:
    for name in ("layer_led", "qube_layer_led"):
        source = (ROOT / f"keyboards/k04/src/{name}.rs").read_text()
        methods = [block(source, source.index("    fn display_color("))]
        split_method = "set_split_state" if name == "layer_led" else "apply_split_state"
        methods.append(block(source, source.index(f"    fn {split_method}(")))
        methods.append(f"fn transition_split(&mut self, state: SplitConnectionState) {{ self.{split_method}(state, Instant(0), 0); }}")
        if "    fn layer_color(" in source:
            methods.append(block(source, source.index("    fn layer_color(")))
            methods.extend(block(source, source.index(f"    fn {method}(")) for method in ("start_overlay", "expire_overlay"))
        helpers = [block(source, match.start()) for match in re.finditer(r"^fn (?!send_color\b)[a-z_]+\(", source, re.M)]
        constants = re.findall(r"^const (?:STATUS_BLINK|SPLIT_BLINK|INDICATOR_DURATION|CONNECTED_INDICATOR)[^\n]+", source, re.M)
        shared = ROOT / "keyboards/k04/src/status_led.rs"
        shared_mod = f'#[path = "{shared}"] mod status_led;' if shared.exists() else ""
        connection = (ROOT / "rmk-types/src/connection.rs").read_text()
        routing = block(connection, connection.rindex("impl ConnectionStatus {"))
        harness = "\n".join([DISPLAY_PRELUDE, routing, shared_mod, *constants,
                              "impl LayerLed {", *methods, "}", *helpers, DISPLAY_TEST])
        source_file = Path(directory) / f"{name}.rs"
        binary = Path(directory) / name
        source_file.write_text(harness)
        subprocess.run(["rustc", "--edition=2021", "--test", str(source_file), "-o", str(binary)], check=True)
        print(f"Testing complete {name} display priorities", flush=True)
        if subprocess.run([str(binary), "--test-threads=1"]).returncode:
            failed.append(name + " display")
if failed:
    raise SystemExit(f"K:04 status indication failed: {', '.join(failed)}")

# Exercise the production charge-state decision independently of LED colour.
source = (ROOT / 'keyboards/k04/src/battery_nrf.rs').read_text()
charge_state = block(source, source.index('fn current_charge_state('))
harness = r'''
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Debug, PartialEq, Eq)]
enum ChargeState { Discharging, Unknown }
static USB_POWERED: AtomicBool = AtomicBool::new(false);
fn usb_powered() -> bool { USB_POWERED.load(Ordering::Relaxed) }
#[test]
fn usb_power_does_not_claim_actual_charging() {
    USB_POWERED.store(true, Ordering::Relaxed);
    assert_eq!(current_charge_state(), ChargeState::Unknown);
    USB_POWERED.store(false, Ordering::Relaxed);
    assert_eq!(current_charge_state(), ChargeState::Discharging);
}
'''
with tempfile.TemporaryDirectory(prefix='k04-charge-state-') as directory:
    source_file = Path(directory) / 'charge_state.rs'
    binary = Path(directory) / 'charge_state'
    source_file.write_text(harness + charge_state)
    subprocess.run(['rustc', '--edition=2021', '--test', str(source_file), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
