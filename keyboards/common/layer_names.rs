//! Shared persistent layer-name settings for Ergohaven firmware.
//!
//! Entropy discovers QSID 200..=215 through Vial's supported-settings query,
//! then reads and writes one UTF-8 name per layer. The data is stored in RMK's
//! existing `DeviceSettings` record, so it survives power cycles without a
//! second storage path.

use core::str;
use core::sync::atomic::{AtomicU8, Ordering};

use rmk::config::{VialDeviceSettings, VialDeviceSettingsData};

pub const LAYER_NAME_COUNT: usize = 16;
pub const LAYER_NAME_MAX: usize = 12;

const LAYER_NAME_QSID_BASE: u16 = 200;
const STORAGE_MARKER: u8 = 0xE5;
const STORAGE_VERSION: u8 = 2;
const LEGACY_STORAGE_VERSION: u8 = 1;
const STORAGE_HEADER_LEN: usize = 2;
const STORAGE_ENTRY_LEN: usize = 1 + LAYER_NAME_MAX;
pub(crate) const SERIALIZED_LEN: usize = STORAGE_HEADER_LEN + LAYER_NAME_COUNT * STORAGE_ENTRY_LEN;
const SETTING_KEYS: [u16; LAYER_NAME_COUNT] = [
    200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215,
];

const _: () = assert!(SERIALIZED_LEN <= 224);
const _: () = assert!(SERIALIZED_LEN <= u8::MAX as usize);

// A fresh store skips deserialize; publish the same defaults on the first GET.
static LAYER_NAME_LEN: [AtomicU8; LAYER_NAME_COUNT] = {
    let mut lengths = [const { AtomicU8::new(0) }; LAYER_NAME_COUNT];
    let mut index = 0;
    while index < LAYER_NAME_COUNT {
        let len = crate::DEFAULT_LAYER_NAMES[index].len();
        assert!(len <= LAYER_NAME_MAX);
        lengths[index] = AtomicU8::new(len as u8);
        index += 1;
    }
    lengths
};
static LAYER_NAME_BYTES: [AtomicU8; LAYER_NAME_COUNT * LAYER_NAME_MAX] = {
    let mut bytes = [const { AtomicU8::new(0) }; LAYER_NAME_COUNT * LAYER_NAME_MAX];
    let mut index = 0;
    while index < LAYER_NAME_COUNT {
        let name = crate::DEFAULT_LAYER_NAMES[index].as_bytes();
        let mut offset = 0;
        while offset < name.len() {
            bytes[index * LAYER_NAME_MAX + offset] = AtomicU8::new(name[offset]);
            offset += 1;
        }
        index += 1;
    }
    bytes
};
static LAYER_NAMES_VERSION: AtomicU8 = AtomicU8::new(0);

pub const fn vial_device_settings() -> VialDeviceSettings<'static> {
    VialDeviceSettings {
        setting_keys: &SETTING_KEYS,
        get_setting,
        set_setting,
        serialize,
        deserialize,
        on_keymap_loaded: Some(on_keymap_loaded),
    }
}

#[allow(dead_code)]
pub fn version() -> u8 {
    LAYER_NAMES_VERSION.load(Ordering::Relaxed)
}

#[allow(dead_code)]
pub fn copy_layer_name(layer: u8, out: &mut [u8; LAYER_NAME_MAX]) -> Option<usize> {
    let index = usize::from(layer);
    if index >= LAYER_NAME_COUNT {
        return None;
    }

    let len = usize::from(LAYER_NAME_LEN[index].load(Ordering::Acquire));
    if len > LAYER_NAME_MAX {
        return None;
    }

    out.fill(0);
    let base = index * LAYER_NAME_MAX;
    for (offset, byte) in out.iter_mut().take(len).enumerate() {
        *byte = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
    }
    str::from_utf8(&out[..len]).ok().map(|_| len)
}

pub(crate) fn get_setting(qsid: u16, out: &mut [u8]) -> Option<usize> {
    let index = layer_index(qsid)?;
    let len = usize::from(LAYER_NAME_LEN[index].load(Ordering::Acquire)).min(LAYER_NAME_MAX);
    let copy_len = len.min(out.len().saturating_sub(1));
    let base = index * LAYER_NAME_MAX;
    for (offset, byte) in out.iter_mut().take(copy_len).enumerate() {
        *byte = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
    }
    if out.len() > copy_len {
        out[copy_len] = 0;
        Some(copy_len + 1)
    } else {
        Some(copy_len)
    }
}

pub(crate) fn set_setting(qsid: u16, value: &[u8]) -> bool {
    let Some(index) = layer_index(qsid) else {
        return false;
    };
    let end = value
        .iter()
        .position(|&byte| byte == 0 || byte == 0xFF)
        .unwrap_or(value.len());
    let Ok(text) = str::from_utf8(&value[..end]) else {
        return false;
    };
    store_layer_name(index, text);
    true
}

pub(crate) fn serialize() -> VialDeviceSettingsData {
    let mut data = VialDeviceSettingsData::empty();
    data.data[0] = STORAGE_MARKER;
    data.data[1] = STORAGE_VERSION;

    let mut pos = STORAGE_HEADER_LEN;
    for index in 0..LAYER_NAME_COUNT {
        let len = usize::from(LAYER_NAME_LEN[index].load(Ordering::Acquire)).min(LAYER_NAME_MAX);
        data.data[pos] = len as u8;
        pos += 1;
        let base = index * LAYER_NAME_MAX;
        for offset in 0..LAYER_NAME_MAX {
            data.data[pos + offset] = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
        }
        pos += LAYER_NAME_MAX;
    }
    data.len = SERIALIZED_LEN as u8;
    data
}

pub(crate) fn deserialize(bytes: &[u8]) {
    clear_layer_names();
    if bytes.len() < SERIALIZED_LEN
        || bytes[0] != STORAGE_MARKER
        || !matches!(bytes[1], STORAGE_VERSION | LEGACY_STORAGE_VERSION)
    {
        load_default_layer_names();
        LAYER_NAMES_VERSION.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let mut pos = STORAGE_HEADER_LEN;
    for index in 0..LAYER_NAME_COUNT {
        let len = usize::from(bytes[pos]).min(LAYER_NAME_MAX);
        pos += 1;
        store_raw_layer_name(index, &bytes[pos..pos + len]);
        pos += LAYER_NAME_MAX;
    }
    migrate_factory_layer_names(None);
    LAYER_NAMES_VERSION.fetch_add(1, Ordering::Relaxed);
}

fn store_layer_name(index: usize, text: &str) {
    let mut bytes = [0u8; LAYER_NAME_MAX];
    let mut len = 0usize;
    let mut chars = text.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' && chars.peek() == Some(&'%') {
            let _ = chars.next();
        }
        let mut encoded = [0u8; 4];
        let encoded = ch.encode_utf8(&mut encoded).as_bytes();
        if len + encoded.len() > LAYER_NAME_MAX {
            break;
        }
        bytes[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    store_raw_layer_name(index, &bytes[..len]);
    LAYER_NAMES_VERSION.fetch_add(1, Ordering::Relaxed);
}

fn store_raw_layer_name(index: usize, bytes: &[u8]) {
    if index >= LAYER_NAME_COUNT {
        return;
    }
    let len = bytes.len().min(LAYER_NAME_MAX);
    LAYER_NAME_LEN[index].store(0, Ordering::Release);
    let base = index * LAYER_NAME_MAX;
    for offset in 0..LAYER_NAME_MAX {
        LAYER_NAME_BYTES[base + offset].store(bytes.get(offset).copied().unwrap_or(0), Ordering::Relaxed);
    }
    LAYER_NAME_LEN[index].store(len as u8, Ordering::Release);
}

fn clear_layer_names() {
    for index in 0..LAYER_NAME_COUNT {
        store_raw_layer_name(index, &[]);
    }
}

fn load_default_layer_names() {
    for (index, name) in crate::DEFAULT_LAYER_NAMES.iter().enumerate() {
        store_raw_layer_name(index, name.as_bytes());
    }
}

pub(crate) fn on_keymap_loaded(keymap: &rmk::keymap::KeyMap<'_>) {
    migrate_factory_layer_names(Some(keymap));
    LAYER_NAMES_VERSION.fetch_add(1, Ordering::Relaxed);
}

fn migrate_factory_layer_names(keymap: Option<&rmk::keymap::KeyMap<'_>>) {
    let mut bytes = [0u8; LAYER_NAME_MAX];
    for (index, default_name) in crate::DEFAULT_LAYER_NAMES.iter().enumerate() {
        let len = usize::from(LAYER_NAME_LEN[index].load(Ordering::Acquire)).min(LAYER_NAME_MAX);
        let base = index * LAYER_NAME_MAX;
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
        }
        if !crate::default_layer_names::is_factory_name(index, &bytes[..len], default_name) {
            return;
        }
    }
    for (index, default_name) in crate::DEFAULT_LAYER_NAMES.iter().enumerate() {
        // Empty defaults are finalized only after restoring the user's actions.
        if default_name.is_empty() && keymap.is_none_or(|map| map.layer_has_assignments(index)) {
            continue;
        }
        store_raw_layer_name(index, default_name.as_bytes());
    }
}

fn layer_index(qsid: u16) -> Option<usize> {
    let offset = qsid.checked_sub(LAYER_NAME_QSID_BASE)?;
    (offset < LAYER_NAME_COUNT as u16).then_some(usize::from(offset))
}
