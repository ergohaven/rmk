//! Persistent per-physical-slot encoder settings for split Ergohaven boards.
//!
//! The including crate provides `ENCODER_COUNT` (2 or 6) and `layer_names`.

use core::sync::atomic::{AtomicU8, Ordering};

use rmk::config::{VialDeviceSettings, VialDeviceSettingsData};
use rmk::event::{publish_event, PeripheralSettingsEvent, PeripheralSettingsRefreshEvent};
use rmk::input_device::encoder_settings::{
    apply_settings_packet, decode_storage_settings, encode_settings_packet, EncoderSetting,
};
use rmk::macros::processor;

const STORAGE_MARKER: u8 = 0xE6;
const STORAGE_VERSION: u8 = 1;
const STORAGE_HEADER_LEN: usize = 2;
const PACKET_MARKER: u8 = 0xEC;
const MAX_ENCODERS: usize = 6;
const SETTINGS_STORAGE_OFFSET: usize = crate::layer_names::SERIALIZED_LEN;
const SERIALIZED_LEN: usize = SETTINGS_STORAGE_OFFSET + STORAGE_HEADER_LEN + crate::ENCODER_COUNT;

const _: () = assert!(crate::ENCODER_COUNT <= MAX_ENCODERS);
const _: () = assert!(SERIALIZED_LEN <= 224);
const _: () = assert!(SERIALIZED_LEN <= u8::MAX as usize);

static SETTINGS: [AtomicU8; MAX_ENCODERS] = [const { AtomicU8::new(0) }; MAX_ENCODERS];

pub const fn vial_device_settings() -> VialDeviceSettings<'static> {
    VialDeviceSettings {
        setting_keys: crate::ENCODER_SETTING_KEYS,
        get_setting,
        set_setting,
        serialize,
        deserialize,
    }
}

fn qsid_slot(qsid: u16) -> Option<(usize, bool)> {
    let offset = qsid.checked_sub(340)?;
    let id = usize::from(offset / 2);
    (id < crate::ENCODER_COUNT).then_some((id, offset & 1 != 0))
}

fn get_setting(qsid: u16, out: &mut [u8]) -> Option<usize> {
    if qsid < 340 {
        return crate::layer_names::get_setting(qsid, out);
    }
    let (id, is_steps) = qsid_slot(qsid)?;
    let setting = EncoderSetting::decode(SETTINGS[id].load(Ordering::Relaxed));
    *out.first_mut()? = if is_steps {
        setting.steps - 1
    } else {
        setting.interval_index
    };
    Some(1)
}

fn set_setting(qsid: u16, value: &[u8]) -> bool {
    if qsid < 340 {
        return crate::layer_names::set_setting(qsid, value);
    }
    let Some((id, is_steps)) = qsid_slot(qsid) else {
        return false;
    };
    let Some(value) = value.first().copied() else {
        return false;
    };
    let old = EncoderSetting::decode(SETTINGS[id].load(Ordering::Relaxed));
    let setting = if is_steps {
        if value > 7 {
            return false;
        }
        EncoderSetting::new(old.interval_index, value + 1)
    } else {
        if value > 9 {
            return false;
        }
        EncoderSetting::new(value, old.steps)
    };
    SETTINGS[id].store(setting.encode(), Ordering::Relaxed);
    publish_settings();
    true
}

fn snapshot() -> [EncoderSetting; crate::ENCODER_COUNT] {
    core::array::from_fn(|id| EncoderSetting::decode(SETTINGS[id].load(Ordering::Relaxed)))
}

fn publish_settings() {
    publish_event(PeripheralSettingsEvent(encode_settings_packet(
        PACKET_MARKER,
        &snapshot(),
    )));
}

fn serialize() -> VialDeviceSettingsData {
    // Keep the legacy layer-name payload at offset zero. Older firmware can
    // therefore still restore names after a rollback and simply ignores this
    // appended encoder-settings extension.
    let mut out = crate::layer_names::serialize();
    out.data[SETTINGS_STORAGE_OFFSET] = STORAGE_MARKER;
    out.data[SETTINGS_STORAGE_OFFSET + 1] = STORAGE_VERSION;
    let mut id = 0;
    while id < crate::ENCODER_COUNT {
        out.data[SETTINGS_STORAGE_OFFSET + STORAGE_HEADER_LEN + id] = SETTINGS[id].load(Ordering::Relaxed);
        id += 1;
    }
    out.len = SERIALIZED_LEN as u8;
    out
}

fn deserialize(bytes: &[u8]) {
    // The layer-name decoder accepts a longer payload, so both legacy records
    // and the append-only extension preserve all existing names.
    crate::layer_names::deserialize(bytes);
    let settings_end = SETTINGS_STORAGE_OFFSET + STORAGE_HEADER_LEN + crate::ENCODER_COUNT;
    if bytes.len() >= settings_end
        && bytes[SETTINGS_STORAGE_OFFSET] == STORAGE_MARKER
        && bytes[SETTINGS_STORAGE_OFFSET + 1] == STORAGE_VERSION
    {
        let settings_start = SETTINGS_STORAGE_OFFSET + STORAGE_HEADER_LEN;
        let decoded = decode_storage_settings::<{ crate::ENCODER_COUNT }>(Some(&bytes[settings_start..settings_end]));
        let mut id = 0;
        while id < crate::ENCODER_COUNT {
            SETTINGS[id].store(decoded[id].encode(), Ordering::Relaxed);
            id += 1;
        }
    } else {
        // Migration from the previous layer-name-only DeviceSettings payload.
        let defaults = decode_storage_settings::<{ crate::ENCODER_COUNT }>(None);
        let mut id = 0;
        while id < crate::ENCODER_COUNT {
            SETTINGS[id].store(defaults[id].encode(), Ordering::Relaxed);
            id += 1;
        }
    }
    publish_settings();
}

#[processor(subscribe = [PeripheralSettingsEvent, PeripheralSettingsRefreshEvent])]
pub struct EncoderSettingsSync;

impl EncoderSettingsSync {
    pub fn new() -> Self {
        // Apply persisted central settings immediately when processors are
        // constructed. On peripherals this installs the historical defaults
        // until the central snapshot arrives over the split link.
        let _ = apply_settings_packet(PACKET_MARKER, &encode_settings_packet(PACKET_MARKER, &snapshot()));
        Self
    }

    async fn on_peripheral_settings_event(&mut self, event: PeripheralSettingsEvent) {
        let _ = apply_settings_packet(PACKET_MARKER, &event.0);
    }

    async fn on_peripheral_settings_refresh_event(&mut self, _event: PeripheralSettingsRefreshEvent) {
        publish_settings();
    }
}
