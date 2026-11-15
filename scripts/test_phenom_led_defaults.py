#!/usr/bin/env python3
"""Run the production LED settings codec/cache; hardware and event delivery are excluded."""
import os
import json
import re
import subprocess
import tempfile
from pathlib import Path
from test_k04_led_legacy import block

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'keyboards/k04/src/layer_names.rs').read_text()
constants = source[source.index('const MODULE_SETTINGS_VERSION:'):source.index('// `VialCommand::BehaviorSettingQuery`')]
functions = ['module_get_setting', 'module_qsid_width', 'module_set_setting', 'module_qsid_value',
             'serialize_module_settings', 'deserialize_module_settings', 'reset_module_settings',
             'ensure_module_settings_initialized', 'module_byte', 'module_set_byte', 'pack_color',
             'unpack_color', 'set_default_layer_color', 'module_layer_color_index', 'module_set_layer_color_index',
             'module_bt_profile_color_index', 'module_set_bt_profile_color_index', 'module_settings_sync_packet',
             'module_encoder_settings_sync_packet', 'module_selection_value', 'module_set_module_selection',
             'module_flag', 'module_set_flag', 'module_axis_flag', 'module_set_axis_flag', 'module_auto_flag',
             'module_set_auto_flag', 'module_touch_gestures', 'module_set_touch_gestures']
harness = '#![allow(dead_code)]\nuse std::sync::atomic::{AtomicU8, Ordering};\nmod board { @BOARD@ }\n' + constants
harness += '\nstatic MODULE_SETTINGS: [AtomicU8; MODULE_SETTINGS_LEN] = [const { AtomicU8::new(0) }; MODULE_SETTINGS_LEN];\nfn publish_module_settings() {}\nstatic DISPLAY_SETTINGS: [AtomicU8; 3] = [const { AtomicU8::new(0) }; 3];\nfn set_display_setting(_:u16,_:u8) { panic!("display is outside LED probe"); }\n'
for name in functions:
    harness += block(source, ('const fn ' if name in ['module_qsid_width', 'set_default_layer_color'] else 'fn ') + name + '(') + '\n'
runtime = (ROOT / 'keyboards/k04/src/module_settings.rs').read_text()
parts = [line for line in runtime.splitlines() if re.match(r'const \w+:[^\n]+;$', line)]
parts += ['#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n' + block(runtime, 'pub enum ModuleSelection'), block(runtime, 'impl ModuleSelection')]
parts += ['static SETTINGS: [AtomicU8; SETTINGS_LEN] = [const { AtomicU8::new(0) }; SETTINGS_LEN];']
for name in ['apply_settings_packet', 'ensure_initialized', 'module_selections', 'set_layer_color_index', 'unpack_color', 'led_brightness', 'led_timeout_sec', 'byte']:
    marker = ('pub(crate) fn ' if name == 'apply_settings_packet' else 'pub fn ' if name in ['led_brightness', 'led_timeout_sec'] else 'fn ') + name + '('
    parts.append(block(runtime, marker))
harness += '\nmod runtime { use super::*;\n' + '\n'.join(parts) + '\nfn apply_module_selection_runtime() {}\n}\n'
harness += '\nmod rmk { pub mod input_device { pub mod rotary_encoder { pub fn set_encoder_steps(_:u8,_:u8) {} } } }\n'
harness += r'''
#[test]
fn factory_defaults_legacy_values_full_range_and_split_packets() {
    let phenom = cfg!(feature="phenom");
    reset_module_settings();
    let mut out = [0; 2];
    assert_eq!(module_get_setting(316, &mut out), Some(2));
    assert_eq!(out[0], if phenom {128} else {8});
    assert_eq!(runtime::led_brightness(), if phenom {128} else {8});
    assert_eq!(u16::from(runtime::led_timeout_sec()), if phenom {600} else {1});
    let width = if phenom {2} else {1};
    assert_eq!(module_get_setting(317, &mut out), Some(width));
    assert_eq!(u16::from_le_bytes(out), if phenom {600} else {1});
    for old_timeout in [0,1,42,255] {
        for fixture in @LEGACY_BLOBS@ { for len in [32,33,34] {
            let mut old = fixture;
            old[14] = old_timeout;
            assert_eq!(old[28] & 0x3c, 0, "pinned serializer left timeout bits unused");
            deserialize_module_settings(&old[..len]);
            assert_eq!(module_get_setting(317, &mut out), Some(width));
            assert_eq!(u16::from_le_bytes(out), u16::from(old_timeout));
            assert_eq!(module_qsid_value(316), Some(37));
        }}
    }
    reset_module_settings();
    module_set_bt_profile_color_index(4, 24); // Adjacent saved color bit must survive.
    let before = serialize_module_settings();
    let values: &[u16] = if phenom { &[0,1,255,256,600,900,1800,3600] } else { &[0,1,42,255] };
    for &value in values {
        let bytes = value.to_le_bytes();
        let mut padded = [0u8; 28]; // Vial/Rynk board-hook contract.
        padded[..2].copy_from_slice(&bytes);
        assert!(module_set_setting(317, &padded));
        let saved = serialize_module_settings();
        assert_eq!(saved.len(), before.len());
        for i in 0..saved.len() {
            if i == 14 {continue;}
            if i == 28 {assert_eq!(saved[i] & !0x3c, before[i] & !0x3c);} else {assert_eq!(saved[i], before[i], "storage byte {i}");}
        }
        reset_module_settings();
        deserialize_module_settings(&saved);
        assert_eq!(module_get_setting(317, &mut out), Some(width));
        assert_eq!(u16::from_le_bytes(out), value);
        let main = module_settings_sync_packet();
        let encoder = module_encoder_settings_sync_packet();
        for packets in [[main,encoder], [encoder,main], [encoder,main]] {
            for packet in packets {runtime::apply_settings_packet(&packet);}
            assert_eq!(u16::from(runtime::led_timeout_sec()), value, "split packet order");
        }
        assert_eq!(module_bt_profile_color_index(4), 24);
    }
    if phenom {
        assert!(!module_set_setting(317, &[1]));
        assert!(module_get_setting(317, &mut [0]).is_none());
        let mut padded = [0u8; 28];
        padded[..2].copy_from_slice(&65535u16.to_le_bytes());
        assert!(module_set_setting(317, &padded));
        module_get_setting(317, &mut out);
        assert_eq!(u16::from_le_bytes(out), 3600);
        // Old centrals still use the original one-byte seconds packet.
        let mut legacy = module_settings_sync_packet();
        legacy[25] &= !0x80;
        legacy[14] = 42;
        runtime::apply_settings_packet(&legacy);
        assert_eq!(u16::from(runtime::led_timeout_sec()), 42);
    }
}
'''
fixtures = json.loads((ROOT/'scripts/fixtures/phenom-led-settings-513718e9.json').read_text())
assert fixtures['source_commit'] == '513718e941e3a6f2f0990f5e9db97b952588b54c'
harness = harness.replace('@LEGACY_BLOBS@', str([item['module_bytes'] for item in fixtures['fixtures']]))
with tempfile.TemporaryDirectory(prefix='phenom-led-defaults-') as directory:
    root = Path(directory)
    toolchain = os.environ.get('RMK_STABLE_TOOLCHAIN') or re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT/'rust-toolchain.toml').read_text()).group(1)
    for board in ['k04', 'phenom']:
        constants = '\n'.join(line for line in (ROOT/f'keyboards/{board}/src/board.rs').read_text().splitlines() if line.startswith(('pub const DEFAULT_', 'pub const LED_TIMEOUT_MAX_SEC', 'pub const AUTO_MODULES')))
        (root/'test.rs').write_text(harness.replace('@BOARD@', constants))
        args = ['rustup', 'run', toolchain, 'rustc', '--edition=2021', '--cfg', 'feature="standalone"', '--test', str(root/'test.rs'), '-o', str(root/'test')]
        if board == 'phenom': args += ['--cfg', 'feature="phenom"']
        subprocess.run(args, check=True)
        subprocess.run([str(root/'test')], check=True)
