#!/usr/bin/env python3
"""Host probe of the verbatim USB indicator branches in both K:04 LED owners.

Exercises color, brightness, and the layer/VBUS/setting gates. Earlier LED
priorities and physical charging/switch detection are outside this probe.
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
                                (Rgb { r: 0, g: (180u16 * u16::from(brightness) / 255) as u8,
                                    b: brightness }, 3)
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
if failed:
    raise SystemExit(f"USB power indicator failed: {', '.join(failed)}")
