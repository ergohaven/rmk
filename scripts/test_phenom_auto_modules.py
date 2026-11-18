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
for name in ["apply_settings_packet", "module_selection", "ensure_initialized", "module_selections", "set_layer_color_index", "unpack_color"]:
    marker = ("pub(crate) fn " if name == "apply_settings_packet" else "pub fn " if name == "module_selection" else "fn ") + name + "("
    runtime_parts.append(block(runtime, marker))
harness += "\nmod runtime { use super::*;\n" + "\n".join(runtime_parts) + "\nfn apply_module_selection_runtime() {}\n}\n"
harness += "\nmod rmk { pub mod input_device { pub mod rotary_encoder { pub fn set_encoder_steps(_: u8, _: u8) {} } } }\n"
harness += r'''
#[test]
fn defaults_manual_overrides_legacy_storage_and_auto_round_trip() {
    reset_module_settings();
    let default = if cfg!(feature="auto_modules") { 4 } else { 3 };
    assert_eq!(module_selection_value(0), default, "factory left module");
    assert_eq!(module_selection_value(1), if cfg!(feature="auto_modules") {4} else {2});
    module_set_bt_profile_color_index(4, 24); // Sets the color bit adjacent to Auto flags.
    let original = serialize_module_settings();
    assert_ne!(original[28] & 1, 0);
    let max = if cfg!(feature="auto_modules") {4} else {3};
    for left in 0..=max { for right in 0..=max {
        module_set_module_selection(0, left);
        module_set_module_selection(1, right);
        let saved = serialize_module_settings();
        reset_module_settings();
        deserialize_module_settings(&saved);
        assert_eq!((module_selection_value(0), module_selection_value(1)), (left,right));
        assert_eq!(saved[28] & 0x3f, original[28] & 0x3f);
        for (i, byte) in saved.iter().enumerate() {
            if i != 28 && i != 31 { assert_eq!(*byte, original[i], "unrelated storage byte {i}"); }
        }
        let packet = module_settings_sync_packet();
        assert_eq!(packet[25] & 15, saved[31] & 15);
        runtime::apply_settings_packet(&packet);
        assert_eq!(runtime::module_selection(0) as u8, left);
        assert_eq!(runtime::module_selection(1) as u8, right);
        for (side, selected) in [(0, left), (1, right)] {
            let selection = runtime::module_selection(side);
            assert_eq!(selection.allows(runtime::ModuleSelection::Trackball), selected == 2 || selected == 4);
            assert_eq!(selection.allows(runtime::ModuleSelection::Touchpad), selected == 3 || selected == 4);
        }
    }}
    // A pre-Auto v10 blob has only 0..3 module values and no reserved Auto bits.
    for left in 0..4 { for right in 0..4 {
        let mut old = original;
        old[28] &= 0x3f;
        old[31] = (old[31] & 0xf0) | left | (right << 2);
        for version in [9, 10] {
            old[0] = version;
            for len in [32, 33, 34] {
                deserialize_module_settings(&old[..len]);
                assert_eq!((module_selection_value(0), module_selection_value(1)), (left,right));
                assert_eq!(module_bt_profile_color_index(4), 24);
                runtime::apply_settings_packet(&module_settings_sync_packet());
                assert_eq!(runtime::module_selection(0) as u8, left);
                assert_eq!(runtime::module_selection(1) as u8, right);
            }
        }
    }}
}
'''
with tempfile.TemporaryDirectory(prefix="phenom-auto-modules-") as directory:
    root = Path(directory)
    for enabled in [False, True]:
        board = "phenom" if enabled else "k04"
        constants = "\n".join(line for line in (ROOT / f"keyboards/{board}/src/board.rs").read_text().splitlines()
                              if line.startswith(("pub const DEFAULT_", "pub const AUTO_MODULES", "pub const LED_TIMEOUT_MAX_SEC")))
        (root / "test.rs").write_text(harness.replace("@BOARD@", constants))
        toolchain = os.environ.get("RMK_STABLE_TOOLCHAIN") or re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
        args = ["rustup", "run", toolchain, "rustc", "--edition=2021", "--cfg", 'feature="standalone"', "--test", str(root / "test.rs"), "-o", str(root / "test")]
        if enabled: args += ["--cfg", 'feature="auto_modules"']
        subprocess.run(args, check=True)
        subprocess.run([str(root / "test")], check=True)
