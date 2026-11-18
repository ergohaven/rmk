#!/usr/bin/env python3
"""Run Phenom's production LED timeout logic with a deterministic host clock."""
from pathlib import Path
import os
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / "keyboards/phenom/src/layer_led.rs").read_text()
subscriptions = re.search(r"subscribe = \[(.*?)\]", source, re.S).group(1)
assert {"KeyboardEvent", "PointingEvent", "InputActivityEvent"} <= set(re.findall(r"\w+", subscriptions))


def function(name):
    start = source.index(f"    async fn {name}(") if f"    async fn {name}(" in source else source.index(f"    fn {name}(")
    opening = source.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


harness = r'''
#![allow(dead_code)]
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
static NOW: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Instant(u64);
impl Instant { fn now() -> Self { Self(NOW.load(Ordering::Relaxed)) } }
struct Duration(u64);
impl Duration { fn from_secs(value: u64) -> Self { Self(value * 1000) } }
impl std::ops::Add<Duration> for Instant {
    type Output = Self;
    fn add(self, rhs: Duration) -> Self { Self(self.0 + rhs.0) }
}
mod module_settings {
    use super::*;
    static TIMEOUT: AtomicU8 = AtomicU8::new(10);
    pub fn led_timeout_sec() -> u8 { TIMEOUT.load(Ordering::Relaxed) }
    pub fn set_timeout(value: u8) { TIMEOUT.store(value, Ordering::Relaxed); }
}
#[derive(Clone, Copy)]
struct InputActivityEvent;
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
fn publish_event(_: InputActivityEvent) { PUBLISHED.fetch_add(1, Ordering::Relaxed); }
#[derive(Clone, Copy)]
pub(crate) enum KeyboardEventSource { Key(()), RotaryEncoder(()), Touch(()), Macro }
struct KeyboardEvent { source: KeyboardEventSource }
struct AxisEvent { value: i16 }
struct PointingEvent { axes: [AxisEvent; 3] }
#[derive(Clone, Copy, PartialEq)]
enum UsbState { Enabled, Suspended }
struct ConnectionStatus { usb: UsbState }
struct ConnectionStatusChangeEvent(ConnectionStatus);
mod rmk { pub mod event { pub(crate) use super::super::KeyboardEventSource; } }
struct LayerLed { layer: u8, dark_at: Option<Instant>, dark: bool, suspended: bool, lit: bool }
impl LayerLed { async fn render(&mut self) { self.lit = !self.dark && !self.suspended; } }
fn run(future: impl std::future::Future<Output = ()>) {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_ready());
}
'''
harness += "\nimpl LayerLed {\n" + "\n".join(function(name) for name in ["arm_timeout", "on_keyboard_event", "on_pointing_event", "on_input_activity_event", "on_connection_status_change_event", "on_deadline"]) + "\n}\n"
harness += r'''
#[test]
fn every_layer_expires_and_activity_restarts_and_wakes_it() {
    for layer in [0, 1, 4, 15] {
        let mut led = LayerLed { layer, dark_at: None, dark: true, suspended: false, lit: false };
        module_settings::set_timeout(10);
        NOW.store(1000, Ordering::Relaxed);
        run(led.on_input_activity_event(InputActivityEvent));
        assert!(!led.dark, "input wakes layer {layer}");
        assert_eq!(led.dark_at, Some(Instant(11000)), "layer {layer}");
        NOW.store(9000, Ordering::Relaxed);
        led.arm_timeout();
        assert_eq!(led.dark_at, Some(Instant(19000)), "input postpones expiry");
        NOW.store(19000, Ordering::Relaxed);
        run(led.on_deadline());
        assert!(led.dark && !led.lit && led.dark_at.is_none(), "layer {layer} expires");
        for source in [KeyboardEventSource::Key(()), KeyboardEventSource::RotaryEncoder(()), KeyboardEventSource::Touch(())] {
            let before = PUBLISHED.load(Ordering::Relaxed);
            run(led.on_keyboard_event(KeyboardEvent { source }));
            assert_eq!(PUBLISHED.load(Ordering::Relaxed), before + 1);
        }
        let before = PUBLISHED.load(Ordering::Relaxed);
        run(led.on_keyboard_event(KeyboardEvent { source: KeyboardEventSource::Macro }));
        assert_eq!(PUBLISHED.load(Ordering::Relaxed), before);
        for (motion, expected) in [(0, before), (1, before + 1)] {
            run(led.on_pointing_event(PointingEvent { axes: [AxisEvent { value: motion }, AxisEvent { value: 0 }, AxisEvent { value: 0 }] }));
            assert_eq!(PUBLISHED.load(Ordering::Relaxed), expected);
        }
        run(led.on_connection_status_change_event(ConnectionStatusChangeEvent(ConnectionStatus { usb: UsbState::Suspended })));
        NOW.store(20000, Ordering::Relaxed);
        run(led.on_input_activity_event(InputActivityEvent));
        assert_eq!(led.dark_at, Some(Instant(30000)));
        assert!(!led.lit, "input must not override USB suspend");
        run(led.on_connection_status_change_event(ConnectionStatusChangeEvent(ConnectionStatus { usb: UsbState::Enabled })));
        assert!(led.lit, "activity during suspend refreshed the timer");
        module_settings::set_timeout(0);
        led.arm_timeout();
        assert_eq!(led.dark_at, None, "zero disables timeout on every layer");
    }
}
'''
with tempfile.TemporaryDirectory(prefix="phenom-led-activity-") as directory:
    path = Path(directory)
    (path / "test.rs").write_text(harness)
    toolchain = os.environ.get("RMK_STABLE_TOOLCHAIN") or re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
    subprocess.run(["rustup", "run", toolchain, "rustc", "--edition=2021", "--test", str(path / "test.rs"), "-o", str(path / "test")], check=True)
    subprocess.run([str(path / "test")], check=True)
