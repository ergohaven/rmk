//! Persistent Vial settings for the optional Velvet right trackball.
//!
//! Layer names continue to use the shared `layer_names` storage prefix. The
//! compact Velvet pointing packet is appended to that record so there remains
//! one `DeviceSettings` owner and one flash write path.

use core::sync::atomic::{AtomicU8, Ordering};

use rmk::config::{VialDeviceSettings, VialDeviceSettingsData};
use rmk::event::{ActionEvent, PeripheralSettingsEvent, PeripheralSettingsRefreshEvent, publish_event};
use rmk::macros::processor;

use crate::velvet_pointing::{
    FLAG_ACCELERATION, FLAG_INVERT_SCROLL_X, FLAG_INVERT_SCROLL_Y, FLAG_INVERT_TEXT_X, FLAG_INVERT_TEXT_Y, FLAG_STICKY,
    FLAG_TRACKBALL_DISABLED, IDX_AUTO_FLAGS, IDX_AUTO_LAYER, IDX_AUTO_LAYER_TIMEOUT, IDX_AXIS, IDX_DPI, IDX_FLAGS,
    IDX_MODE, IDX_SCROLL_SENS, IDX_SNIPER_SENS, IDX_TEXT_SENS, IDX_VERSION, LEGACY_SETTINGS_STORAGE_LEN,
    SETTINGS_STORAGE_LEN, SETTINGS_VERSION, default_settings_packet, sanitize_settings_packet,
};

const SETTING_KEYS: [u16; 35] = [
    121, 127, 128, 129, 131, 135, 138, 139, 141, 142, 143, 144, 145, 146, 148, 200, 201, 202, 203, 204, 205, 206, 207,
    208, 209, 210, 211, 212, 213, 214, 215, 324, 328, 330, 334,
];
const STORAGE_OFFSET: usize = crate::layer_names::SERIALIZED_LEN;
const SERIALIZED_LEN: usize = STORAGE_OFFSET + SETTINGS_STORAGE_LEN;
const _: () = assert!(SERIALIZED_LEN <= 224);
const _: () = assert!(SERIALIZED_LEN <= u8::MAX as usize);

static SETTINGS: [AtomicU8; SETTINGS_STORAGE_LEN] = [const { AtomicU8::new(0) }; SETTINGS_STORAGE_LEN];

pub const fn vial_device_settings() -> VialDeviceSettings<'static> {
    VialDeviceSettings {
        setting_keys: &SETTING_KEYS,
        get_setting,
        set_setting,
        serialize,
        deserialize,
    }
}

fn get_setting(qsid: u16, out: &mut [u8]) -> Option<usize> {
    if (200..=215).contains(&qsid) {
        return crate::layer_names::get_setting(qsid, out);
    }
    let byte = out.first_mut()?;
    *byte = match qsid {
        121 => setting(IDX_DPI),
        127 => setting(IDX_SNIPER_SENS),
        128 => setting(IDX_SCROLL_SENS),
        129 => setting(IDX_TEXT_SENS),
        131 => setting(IDX_AXIS),
        135 => setting(IDX_MODE),
        138 => flag(FLAG_INVERT_SCROLL_Y) as u8,
        139 => flag(FLAG_ACCELERATION) as u8,
        141 => flag(FLAG_STICKY) as u8,
        142 => auto_flag(0) as u8,
        143 => setting(IDX_AUTO_LAYER),
        144 => auto_flag(1) as u8,
        145 => auto_flag(2) as u8,
        146 => auto_flag(3) as u8,
        148 => flag(FLAG_INVERT_TEXT_Y) as u8,
        324 => setting(IDX_AUTO_LAYER_TIMEOUT).min(5),
        328 => flag(FLAG_INVERT_SCROLL_X) as u8,
        330 => flag(FLAG_INVERT_TEXT_X) as u8,
        334 => (!flag(FLAG_TRACKBALL_DISABLED)) as u8,
        _ => return None,
    };
    Some(1)
}

fn set_setting(qsid: u16, value: &[u8]) -> bool {
    if (200..=215).contains(&qsid) {
        return crate::layer_names::set_setting(qsid, value);
    }
    let Some(value) = value.first().copied() else {
        return false;
    };
    match qsid {
        121 => set_byte(IDX_DPI, value.min(15)),
        127 => set_byte(IDX_SNIPER_SENS, value.max(1)),
        128 => set_byte(IDX_SCROLL_SENS, value.max(1)),
        129 => set_byte(IDX_TEXT_SENS, value.max(1)),
        131 => set_byte(IDX_AXIS, value.min(3)),
        135 => set_byte(IDX_MODE, value.min(3)),
        138 => set_flag(FLAG_INVERT_SCROLL_Y, value != 0),
        139 => set_flag(FLAG_ACCELERATION, value != 0),
        141 => set_flag(FLAG_STICKY, value != 0),
        142 => set_auto_flag(0, value != 0),
        143 => set_byte(IDX_AUTO_LAYER, value.min(15)),
        144 => set_auto_flag(1, value != 0),
        145 => set_auto_flag(2, value != 0),
        146 => set_auto_flag(3, value != 0),
        148 => set_flag(FLAG_INVERT_TEXT_Y, value != 0),
        324 => set_byte(IDX_AUTO_LAYER_TIMEOUT, value.min(5)),
        328 => set_flag(FLAG_INVERT_SCROLL_X, value != 0),
        330 => set_flag(FLAG_INVERT_TEXT_X, value != 0),
        334 => set_flag(FLAG_TRACKBALL_DISABLED, value == 0),
        _ => return false,
    }
    publish_settings();
    true
}

fn serialize() -> VialDeviceSettingsData {
    let mut data = crate::layer_names::serialize();
    let packet = settings_packet();
    data.data[STORAGE_OFFSET..SERIALIZED_LEN].copy_from_slice(&packet[..SETTINGS_STORAGE_LEN]);
    data.len = SERIALIZED_LEN as u8;
    data
}

fn deserialize(bytes: &[u8]) {
    crate::layer_names::deserialize(bytes);
    let available = bytes.len().saturating_sub(STORAGE_OFFSET).min(SETTINGS_STORAGE_LEN);
    let packet = if available >= LEGACY_SETTINGS_STORAGE_LEN {
        sanitize_settings_packet(&bytes[STORAGE_OFFSET..STORAGE_OFFSET + available])
    } else {
        default_settings_packet()
    };
    store_packet(&packet);
    publish_settings();
}

pub(crate) fn publish_settings() {
    publish_event(PeripheralSettingsEvent(settings_packet()));
}

#[processor(subscribe = [PeripheralSettingsRefreshEvent, ActionEvent])]
pub struct VelvetSettingsBroadcast;

impl VelvetSettingsBroadcast {
    pub const fn new() -> Self {
        Self
    }

    async fn on_action_event(&mut self, event: ActionEvent) {
        rmk::input_device::pointing_settings::handle_sticky_mode_key(event, vial_device_settings(), 13);
    }

    async fn on_peripheral_settings_refresh_event(&mut self, _event: PeripheralSettingsRefreshEvent) {
        publish_settings();
    }
}

fn settings_packet() -> [u8; 27] {
    ensure_initialized();
    let mut packet = [0u8; 27];
    for (index, byte) in packet.iter_mut().take(SETTINGS_STORAGE_LEN).enumerate() {
        *byte = SETTINGS[index].load(Ordering::Relaxed);
    }
    packet
}

fn store_packet(packet: &[u8; 27]) {
    for (index, value) in packet.iter().copied().take(SETTINGS_STORAGE_LEN).enumerate() {
        SETTINGS[index].store(value, Ordering::Relaxed);
    }
}

fn ensure_initialized() {
    if SETTINGS[IDX_VERSION].load(Ordering::Relaxed) == SETTINGS_VERSION {
        return;
    }
    store_packet(&default_settings_packet());
}

fn setting(index: usize) -> u8 {
    ensure_initialized();
    SETTINGS[index].load(Ordering::Relaxed)
}

fn set_byte(index: usize, value: u8) {
    ensure_initialized();
    SETTINGS[index].store(value, Ordering::Relaxed);
}

fn flag(mask: u8) -> bool {
    setting(IDX_FLAGS) & mask != 0
}

fn set_flag(mask: u8, enabled: bool) {
    ensure_initialized();
    if enabled {
        SETTINGS[IDX_FLAGS].fetch_or(mask, Ordering::Relaxed);
    } else {
        SETTINGS[IDX_FLAGS].fetch_and(!mask, Ordering::Relaxed);
    }
}

fn auto_flag(bit: u8) -> bool {
    setting(IDX_AUTO_FLAGS) & (1 << bit) != 0
}

fn set_auto_flag(bit: u8, enabled: bool) {
    ensure_initialized();
    let mask = 1 << bit;
    if enabled {
        SETTINGS[IDX_AUTO_FLAGS].fetch_or(mask, Ordering::Relaxed);
    } else {
        SETTINGS[IDX_AUTO_FLAGS].fetch_and(!mask, Ordering::Relaxed);
    }
}
