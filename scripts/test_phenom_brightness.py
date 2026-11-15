#!/usr/bin/env python3
"""Check factory bindings and execute the production brightness processor."""
import os
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
for suffix in ["", "_mini", "_micro"]:
    config = (ROOT / f"keyboards/phenom/keyboard{suffix}.toml").read_text()
    lower = config.split('name = "Lower"', 1)[1].split('[[keymap.layer]]', 1)[0]
    assert 'encoders = [["RgbVai", "RgbVad"], ["RgbVai", "RgbVad"]]' in lower
    assert "action.subs = 3" in config
central = (ROOT / "keyboards/phenom/src/central.rs").read_text()
assert "#[register_processor]\n    fn brightness()" in central
source = (ROOT / "keyboards/phenom/src/brightness.rs").read_text()
source = source.replace('use rmk::macros::processor;\n', '').replace('#[processor(subscribe = [ActionEvent])]\n', '')
harness = r'''#![allow(dead_code)]
extern crate self as rmk;
mod types { pub mod action {
    #[derive(Clone,Copy)] pub enum LightAction { RgbVai, RgbVad, RgbTog }
    #[derive(Clone,Copy)] pub enum Action { Light(LightAction), User(u8) }
}}
mod event { pub struct KeyboardEvent { pub pressed: bool } pub struct ActionEvent { pub action: crate::types::action::Action, pub keyboard_event: KeyboardEvent } }
mod config {
    pub type VialDeviceSettingsData = u8;
    @SETTINGS_STRUCT@
}
static VALUE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static FLASH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
mod storage { pub(crate) fn queue_device_settings_flash_write(value:u8) { assert_eq!(value,crate::VALUE.load(std::sync::atomic::Ordering::Relaxed)); crate::FLASH.fetch_add(1,std::sync::atomic::Ordering::Relaxed); } }
mod layer_names {
    pub fn vial_device_settings() -> crate::config::VialDeviceSettings<'static> { crate::config::VialDeviceSettings {
        setting_keys: &[316], deserialize: |_| {},
        get_setting: |id,out| { assert_eq!(id,316); out[0]=crate::VALUE.load(std::sync::atomic::Ordering::Relaxed); out[1]=0; Some(2) },
        set_setting: |id,data| { assert_eq!(id,316); assert_eq!(data.len(),2); assert_eq!(data[1],0); crate::VALUE.store(data[0],std::sync::atomic::Ordering::Relaxed); true },
        serialize: || crate::VALUE.load(std::sync::atomic::Ordering::Relaxed),
    }}
}
'''
config = (ROOT/"rmk/src/config/vial.rs").read_text()
from test_k04_led_legacy import block
implementation = block(config, "impl VialDeviceSettings<'_>")
harness = harness.replace("@SETTINGS_STRUCT@", block(config, "pub struct VialDeviceSettings<"))
harness += "\nuse config::VialDeviceSettings;\n" + implementation + "\n" + source
harness += r'''
#[test]
fn brightness_events_persist_press_only_changes_and_saturate() {
    use types::action::{Action,LightAction::*};
    use std::sync::atomic::Ordering;
    struct Noop; impl std::task::Wake for Noop { fn wake(self:std::sync::Arc<Self>) {} }
    let waker=std::task::Waker::from(std::sync::Arc::new(Noop));
    let mut context=std::task::Context::from_waker(&waker);
    let mut processor=Brightness::new();
    let persist=usize::from(cfg!(feature="storage") && (cfg!(feature="ergohaven_rynk") || cfg!(feature="vial")));
    for (action,pressed,expected,writes) in [
        (Action::Light(RgbVai),true,17,1),(Action::Light(RgbVai),true,34,2),
        (Action::Light(RgbVad),false,34,2),(Action::User(28),true,34,2),
        (Action::Light(RgbTog),true,34,2),(Action::Light(RgbVad),true,17,3),
        (Action::Light(RgbVad),true,0,4),(Action::Light(RgbVad),true,0,4),
    ] {
        let mut future=std::pin::pin!(processor.on_action_event(event::ActionEvent {action,keyboard_event:event::KeyboardEvent {pressed}}));
        assert!(std::future::Future::poll(future.as_mut(),&mut context).is_ready());
        assert_eq!(VALUE.load(Ordering::Relaxed),expected);
        assert_eq!(FLASH.load(Ordering::Relaxed),writes*persist);
    }
    VALUE.store(250,Ordering::Relaxed);
    for writes in [5,5] {
        let mut future=std::pin::pin!(processor.on_action_event(event::ActionEvent {action:Action::Light(RgbVai),keyboard_event:event::KeyboardEvent {pressed:true}}));
        assert!(std::future::Future::poll(future.as_mut(),&mut context).is_ready());
        assert_eq!(VALUE.load(Ordering::Relaxed),255);
        assert_eq!(FLASH.load(Ordering::Relaxed),writes*persist);
    }
}
'''
with tempfile.TemporaryDirectory(prefix="phenom-brightness-") as directory:
    root=Path(directory)
    (root/"test.rs").write_text(harness)
    channel=os.environ.get("RMK_STABLE_TOOLCHAIN") or re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT/"rust-toolchain.toml").read_text()).group(1)
    for features in [[], ["storage"], ["storage", "vial"], ["storage", "ergohaven_rynk"]]:
        args=["rustup","run",channel,"rustc","--edition=2021","--test",str(root/"test.rs"),"-o",str(root/"test")]
        for feature in features: args += ["--cfg", f'feature="{feature}"']
        subprocess.run(args,check=True)
        subprocess.run([str(root/"test")],check=True)
