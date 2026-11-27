#!/usr/bin/env python3
"""Exercise production module selection, storage and split settings on the host."""
import os
import re
import subprocess
import tempfile
from pathlib import Path
from test_k04_led_legacy import block

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / "keyboards/k04/src/layer_names.rs").read_text()
constants = source[source.index("const MODULE_SETTINGS_VERSION:"):source.index("// `VialCommand::BehaviorSettingQuery`")]
functions = ["module_selection_value", "module_set_module_selection", "serialize_module_settings",
             "deserialize_module_settings", "reset_module_settings", "ensure_module_settings_initialized",
             "module_byte", "module_set_byte", "pack_color", "unpack_color", "set_default_layer_color",
             "module_layer_color_index", "module_set_layer_color_index", "module_bt_profile_color_index",
             "module_set_bt_profile_color_index", "module_settings_sync_packet"]
harness = r'''#![allow(dead_code)]
use std::sync::atomic::{AtomicU8, Ordering};
mod board { @BOARD@ }
'''
harness += constants
harness += "\nstatic MODULE_SETTINGS: [AtomicU8; MODULE_SETTINGS_LEN] = [const { AtomicU8::new(0) }; MODULE_SETTINGS_LEN];\n"
harness += "\n".join(block(source, ("const fn " if name == "set_default_layer_color" else "fn ") + name + "(") for name in functions)
runtime = (ROOT / "keyboards/k04/src/module_settings.rs").read_text()
runtime_parts = [line for line in runtime.splitlines() if re.match(r"const \w+:[^\n]+;$", line)]
runtime_parts += ["#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n" + block(runtime, "pub enum ModuleSelection"), block(runtime, "impl ModuleSelection")]
runtime_parts += ['static SETTINGS: [AtomicU8; SETTINGS_LEN] = [const { AtomicU8::new(0) }; SETTINGS_LEN];']
for name in ["apply_settings_packet", "module_selection", "ensure_initialized", "module_selections", "set_layer_color_index", "layer_color_index", "byte", "unpack_color"]:
    marker = ("pub(crate) fn " if name == "apply_settings_packet" else "pub fn " if name == "module_selection" else "fn ") + name + "("
    runtime_parts.append(block(runtime, marker))
harness += "\nmod runtime { use super::*;\n" + "\n".join(runtime_parts) + "\nfn apply_module_selection_runtime() {}\npub fn base_color() -> u8 { layer_color_index(0) }\n}\n"
harness += "\nmod rmk { pub mod input_device { pub mod rotary_encoder { pub fn set_encoder_steps(_: u8, _: u8) {} } } }\n"
harness += r'''
fn editor_value(value: u8) -> u8 {
    if board::AUTO_MODULES { value.clamp(1, 3) } else { value.min(3) }
}

fn assert_runtime_selection(left: u8, right: u8) {
    for (side, editor) in [(0, left), (1, right)] {
        let expected = if board::AUTO_MODULES { 4 } else { editor };
        let selection = runtime::module_selection(side);
        assert_eq!(selection as u8, expected, "hardware selection is independent of Phenom's editor");
        assert_eq!(selection.allows(runtime::ModuleSelection::Trackball), expected == 2 || expected == 4);
        assert_eq!(selection.allows(runtime::ModuleSelection::Touchpad), expected == 3 || expected == 4);
    }
}

#[test]
fn defaults_editor_categories_legacy_storage_and_auto_round_trip() {
    reset_module_settings();
    let base_color = if cfg!(feature="auto_modules") { 1 } else { 0 };
    assert_eq!(module_layer_color_index(0), base_color, "factory base layer: Phenom white, K04 off");
    assert_eq!(runtime::base_color(), base_color, "local fallback agrees before settings arrive");
    for color in [0, 1, 19] {
        module_set_layer_color_index(0, color);
        let stored = serialize_module_settings();
        reset_module_settings();
        deserialize_module_settings(&stored);
        assert_eq!(module_layer_color_index(0), color, "saved explicit color survives loading");
        runtime::apply_settings_packet(&module_settings_sync_packet());
        assert_eq!(runtime::base_color(), color, "settings sync preserves saved color");
    }
    reset_module_settings();
    assert_eq!(board::AUTO_MODULES, cfg!(feature="auto_modules"));
    assert_eq!(board::K04_EXTENDED_SETTINGS, !board::AUTO_MODULES);
    assert_eq!((module_selection_value(0), module_selection_value(1)), (3, 2), "factory editor categories");
    runtime::apply_settings_packet(&module_settings_sync_packet());
    assert_runtime_selection(3, 2);
    module_set_bt_profile_color_index(4, 24); // Sets the color bit adjacent to legacy Auto flags.
    module_set_byte(IDX_AXIS_FLAGS, 0x0d);
    module_set_byte(IDX_AUTO_LAYER_TIMEOUT, AUTO_LAYER_TIMEOUT_MAX_INDEX);
    let original = serialize_module_settings();
    assert_ne!(original[28] & 1, 0);
    assert_eq!(original[28] & 2, if board::K04_EXTENDED_SETTINGS { 2 } else { 0 });
    for left in 0..=3 { for right in 0..=3 {
        module_set_module_selection(0, left);
        module_set_module_selection(1, right);
        let expected = (editor_value(left), editor_value(right));
        assert_eq!((module_selection_value(0), module_selection_value(1)), expected);
        runtime::apply_settings_packet(&module_settings_sync_packet());
        assert_runtime_selection(expected.0, expected.1);
        let saved = serialize_module_settings();
        reset_module_settings();
        deserialize_module_settings(&saved);
        assert_eq!((module_selection_value(0), module_selection_value(1)), expected);
        assert_eq!(serialize_module_settings(), saved, "current storage round trip");
        assert_eq!(saved[28] & 0x3f, original[28] & 0x3f);
        assert_eq!(saved[31] & 0xf0, original[31] & 0xf0, "adjacent axis flags");
        for (i, byte) in saved.iter().enumerate() {
            if i != 28 && i != 31 { assert_eq!(*byte, original[i], "unrelated storage byte {i}"); }
        }
        let packet = module_settings_sync_packet();
        assert_eq!(packet[25] & 15, saved[31] & 15);
        assert_eq!(packet[25] & 0xc0, if board::K04_EXTENDED_SETTINGS { 0xc0 } else { 0 });
        runtime::apply_settings_packet(&packet);
        assert_runtime_selection(expected.0, expected.1);
    }}
    // All byte inputs are bounded categories, never a Phenom hardware switch.
    for side in 0..=1 {
        for value in 0..=u8::MAX {
            deserialize_module_settings(&original);
            module_set_module_selection(side, value);
            let expected = if side == 0 { (editor_value(value), 2) } else { (3, editor_value(value)) };
            assert_eq!((module_selection_value(0), module_selection_value(1)), expected);
            runtime::apply_settings_packet(&module_settings_sync_packet());
            assert_runtime_selection(expected.0, expected.1);
            let saved = serialize_module_settings();
            deserialize_module_settings(&saved);
            assert_eq!((module_selection_value(0), module_selection_value(1)), expected);
            assert_eq!(serialize_module_settings(), saved);
        }
    }
    // Retain all pre-Auto module values, storage lengths and v9/v10 admission.
    for left in 0..4 { for right in 0..4 {
        let mut old = original;
        old[28] &= 0x3f;
        old[31] = (old[31] & 0xf0) | left | (right << 2);
        for version in [9, 10] {
            old[0] = version;
            for len in [32, 33, 34] {
                deserialize_module_settings(&old[..len]);
                let expected = (editor_value(left), editor_value(right));
                assert_eq!((module_selection_value(0), module_selection_value(1)), expected);
                assert_eq!(module_bt_profile_color_index(4), 24);
                runtime::apply_settings_packet(&module_settings_sync_packet());
                assert_runtime_selection(expected.0, expected.1);
                let normalized = serialize_module_settings();
                // Legacy Auto flags may be retained, but must not leak into colors,
                // the extended timeout bit, axis flags or any other saved byte.
                for flags in [0x40, 0x80, 0xc0] {
                    let mut flagged = old;
                    flagged[28] |= flags;
                    deserialize_module_settings(&flagged[..len]);
                    assert_eq!((module_selection_value(0), module_selection_value(1)), expected);
                    let saved = serialize_module_settings();
                    assert_eq!(saved[28] & 0xc0, if board::AUTO_MODULES { flags } else { 0 });
                    assert_eq!(saved[28] & 0x3f, normalized[28] & 0x3f);
                    for (i, byte) in saved.iter().enumerate() {
                        if i != 28 { assert_eq!(*byte, normalized[i], "legacy flags changed storage byte {i}"); }
                    }
                    let packet = module_settings_sync_packet();
                    assert_eq!(packet[25] & 0x30, if board::AUTO_MODULES { flags >> 2 } else { 0 });
                    runtime::apply_settings_packet(&packet);
                    assert_runtime_selection(expected.0, expected.1);
                    for side in 0..=1 {
                        let before_edit = serialize_module_settings();
                        module_set_module_selection(side, if side == 0 { left } else { right });
                        let cleared = serialize_module_settings();
                        assert_eq!(cleared[28] & 0xc0, before_edit[28] & 0xc0 & !(0x40 << side),
                                   "editing clears only that side's legacy Auto flag");
                        assert_eq!(cleared[28] & 0x3f, normalized[28] & 0x3f);
                        assert_eq!(cleared[31] & 0xf0, before_edit[31] & 0xf0);
                        for (i, byte) in cleared.iter().enumerate() {
                            if i != 28 && i != 31 { assert_eq!(*byte, before_edit[i], "editing changed storage byte {i}"); }
                        }
                        runtime::apply_settings_packet(&module_settings_sync_packet());
                        assert_runtime_selection(expected.0, expected.1);
                    }
                }
            }
        }
    }}
}
'''
with tempfile.TemporaryDirectory(prefix="phenom-auto-modules-") as directory:
    root = Path(directory)
    for enabled in [False, True]:
        board = "phenom" if enabled else "k04"
        print(f"{board}: production board policy, storage and runtime fixture", flush=True)
        constants = "\n".join(line for line in (ROOT / f"keyboards/{board}/src/board.rs").read_text().splitlines()
                              if line.startswith(("pub const DEFAULT_", "pub const AUTO_MODULES",
                                                  "pub const LED_TIMEOUT_MAX_SEC", "pub const K04_EXTENDED_SETTINGS")))
        (root / "test.rs").write_text(harness.replace("@BOARD@", constants))
        toolchain = os.environ.get("RMK_STABLE_TOOLCHAIN") or re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
        args = ["rustup", "run", toolchain, "rustc", "--edition=2021", "--cfg", 'feature="standalone"', "--test", str(root / "test.rs"), "-o", str(root / "test")]
        if enabled: args += ["--cfg", 'feature="auto_modules"']
        subprocess.run(args, check=True)
        subprocess.run([str(root / "test")], check=True)
