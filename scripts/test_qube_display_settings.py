#!/usr/bin/env python3
"""Exercise Qube display settings through the firmware's actual Vial callbacks."""
from pathlib import Path
import json
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
expected = [
    {"type": "integer", "title": "Display brightness", "qsid": 336, "min": 0, "max": 100, "width": 1},
    {"type": "select", "title": "Display background", "qsid": 337, "variants": ["Black", "Blue"]},
    {"type": "select", "title": "Display text size", "qsid": 338, "variants": ["Compact", "Large"]},
]
for suffix in ["", "_mini", "_micro"]:
    qube = json.loads((ROOT / f"keyboards/k04/vial_qube{suffix}.json").read_text())
    display = [group for group in qube["settings"] if group["name"] == "Qube display"]
    assert len(display) == 1 and display[0]["fields"] == expected
    standalone = json.loads((ROOT / f"keyboards/k04/vial{suffix}.json").read_text())
    assert all(field["qsid"] not in range(336, 339)
               for group in standalone["settings"] for field in group["fields"])

board_constants = "\n".join(
    line for line in (ROOT / "keyboards/k04/src/board.rs").read_text().splitlines()
    if line.startswith("pub const DEFAULT_")
)
assert board_constants, "Board factory defaults missing"

config_source = (ROOT / 'rmk/src/config/vial.rs').read_text()
max_len = re.search(r'pub const VIAL_DEVICE_SETTINGS_MAX_LEN: usize = (\d+);', config_source)
assert max_len, "Vial storage capacity declaration changed"
source = (ROOT / 'keyboards/k04/src/layer_names.rs').read_text()
source = source.replace('use rmk::macros::processor;\n', '')
source = source.replace('use portable_atomic::{AtomicU8, Ordering};', 'use std::sync::atomic::{AtomicU8, Ordering};')
source = re.sub(r'#\[processor\(subscribe = \[PeripheralSettingsRefreshEvent\]\)\]', '', source)
# Host shims only replace the event transport and fixed-capacity string dependency.
prefix = r'''
#![allow(dead_code)]
extern crate self as rmk;
mod board {
@BOARD_CONSTANTS@
}
mod config {
    pub struct VialDeviceSettings<'a> {
        pub setting_keys: &'a [u16],
        pub get_setting: fn(u16, &mut [u8]) -> Option<usize>,
        pub set_setting: fn(u16, &[u8]) -> bool,
        pub serialize: fn() -> VialDeviceSettingsData,
        pub deserialize: fn(&[u8]),
    }
    pub struct VialDeviceSettingsData { pub data: [u8; @MAX_LEN@], pub len: u8 }
    impl VialDeviceSettingsData { pub fn empty() -> Self { Self { data: [0;@MAX_LEN@], len: 0 } } }
}
mod event {
    pub struct PeripheralSettingsEvent(pub [u8;27]);
    pub struct PeripheralSettingsRefreshEvent;
    pub fn publish_event<T>(_: T) {}
}
mod heapless { pub type String<const N: usize> = super::FixedString; }
#[derive(Default)]
pub struct FixedString(std::string::String);
impl FixedString {
    fn new() -> Self { Self::default() }
    fn push(&mut self, c: char) -> Result<(), ()> { self.0.push(c); Ok(()) }
    fn clear(&mut self) { self.0.clear(); }
    fn push_str(&mut self, s: &str) -> Result<(), ()> { self.0.push_str(s); Ok(()) }
    fn as_str(&self) -> &str { &self.0 }
    fn as_bytes(&self) -> &[u8] { self.0.as_bytes() }
}
mod default_layer_names {
    pub const STANDARD_WITH_MOUSE: [&str;16] = ["Base";16];
    pub fn is_legacy_placeholder(_: usize, _: &[u8]) -> bool { false }
}
const DEFAULT_LAYER_NAMES: [&str;16] = default_layer_names::STANDARD_WITH_MOUSE;
'''
prefix = prefix.replace('@MAX_LEN@', max_len.group(1)).replace('@BOARD_CONSTANTS@', board_constants)
tests = r'''
#[test]
fn display_settings_vial_and_storage_round_trip() {
    let api = layer_names::vial_device_settings();
    (api.deserialize)(&[]);
    let get = |id| { let mut out=[0;16]; assert_eq!((api.get_setting)(id,&mut out),Some(1)); out[0] };
    if cfg!(feature="qube") {
        for id in 336..=338 { assert!(api.setting_keys.contains(&id)); }
        assert_eq!([get(336),get(337),get(338)], [100,0,1]);
        assert!((api.set_setting)(336,&[37]));
        assert!((api.set_setting)(337,&[1]));
        assert!((api.set_setting)(338,&[0]));
        assert!((api.set_setting)(316,&[17,0]));
        // All sixteen names fit without truncation at ten bytes each.
        for id in 200..216 { assert!((api.set_setting)(id,b"abcdefghij")); }
        for id in (300..316).chain(318..323) { assert!((api.set_setting)(id,&[24])); }
        assert!((api.set_setting)(332,&[5]));
        assert!((api.set_setting)(333,&[6]));
        let saved=(api.serialize)();
        assert!(saved.len as usize <= saved.data.len());
        (api.deserialize)(&[]);
        (api.deserialize)(&saved.data[..saved.len as usize]);
        assert_eq!([get(336),get(337),get(338)], [37,1,0]);
        let mut out=[0;16];
        assert_eq!((api.get_setting)(316,&mut out),Some(2)); assert_eq!(out[0],17);
        for id in 200..216 {
            assert_eq!((api.get_setting)(id,&mut out),Some(11)); assert_eq!(&out[..10],b"abcdefghij");
        }
        for id in (300..316).chain(318..323) { assert_eq!(get(id),24); }
        assert_eq!([get(332),get(333)], [5,6]);
        // An existing v5 blob keeps all names/modules and gains readable display defaults.
        let mut legacy=saved.data;
        legacy[1]=5;
        legacy[2+28] &= 0x01;
        legacy[2+32] &= 0x77;
        (api.deserialize)(&legacy[..saved.len as usize]);
        assert_eq!([get(336),get(337),get(338)], [100,0,1]);
        assert_eq!((api.get_setting)(215,&mut out),Some(11)); assert_eq!(&out[..10],b"abcdefghij");
        assert_eq!([get(332),get(333)], [5,6]);
        // Full legacy capacity stays available: no new trailer and no overflow.
        for id in 200..216 { assert!((api.set_setting)(id,b"abcdefghijkl")); }
        let full=(api.serialize)();
        assert_eq!(full.len as usize, full.data.len());
        (api.deserialize)(&full.data[..full.len as usize]);
        assert_eq!([get(336),get(337),get(338)], [100,0,1]);
        assert_eq!([get(332),get(333)], [5,6]);
        for id in 200..214 {
            assert_eq!((api.get_setting)(id,&mut out),Some(13)); assert_eq!(&out[..12],b"abcdefghijkl");
        }
        assert!((api.set_setting)(336,&[0]));
        assert!((api.set_setting)(337,&[1]));
        assert!((api.set_setting)(338,&[0]));
        let updated=(api.serialize)();
        assert_eq!(&full.data[36..], &updated.data[36..]);
        (api.deserialize)(&updated.data[..updated.len as usize]);
        assert_eq!([get(336),get(337),get(338)], [0,1,0]);
        for (id,max) in [(336,100),(337,1),(338,1)] {
            assert!((api.set_setting)(id,&[255])); assert_eq!(get(id),max);
            assert!(!(api.set_setting)(id,&[]));
        }
        // The largest encoder steps must remain independent of both display flags.
        for id in [332,333] { assert!((api.set_setting)(id,&[7])); }
        let maximum=(api.serialize)();
        (api.deserialize)(&maximum.data[..maximum.len as usize]);
        assert_eq!([get(332),get(333)], [7,7]);
        assert_eq!([get(336),get(337),get(338)], [100,1,1]);
    } else {
        for id in 336..=338 {
            assert!(!api.setting_keys.contains(&id));
            assert_eq!((api.get_setting)(id,&mut [0;16]),None);
            assert!(!(api.set_setting)(id,&[1]));
        }
        assert_eq!((api.serialize)().data[1],5);
    }
}
'''
with tempfile.TemporaryDirectory(prefix='qube-settings-test-') as d:
    path=Path(d)/'test.rs'
    path.write_text(prefix+'\nmod layer_names {\nuse crate::heapless;\n'+source+'\n}\n'+tests)
    for feature in ['qube','standalone']:
        binary=Path(d)/feature
        subprocess.run([shutil.which('rustc') or str(Path.home()/'.cargo/bin/rustc'),'--edition=2024','--test','--cfg',f'feature="{feature}"',str(path),'-o',str(binary)],check=True)
        subprocess.run([str(binary),'--test-threads=1'],check=True)
