use core::str;
use core::sync::atomic::{AtomicU8, Ordering};

use rmk::config::{VialDeviceSettings, VialDeviceSettingsData};
use rmk::event::{publish_event, PeripheralSettingsEvent, PeripheralSettingsRefreshEvent};
use rmk::macros::processor;

pub const LAYER_NAME_COUNT: usize = 16;
pub const LAYER_NAME_MAX: usize = 12;
const LAYER_NAME_QSID_BASE: u16 = 200;
const STORAGE_MARKER: u8 = 0xE4;
const STORAGE_VERSION: u8 = 3;
const PREVIOUS_STORAGE_VERSION: u8 = 2;
const LEGACY_STORAGE_VERSION: u8 = 1;
const STORAGE_HEADER_LEN: usize = 2;
const MODULE_STORAGE_OFFSET: usize = STORAGE_HEADER_LEN;
const LAYER_NAMES_STORAGE_OFFSET: usize = MODULE_STORAGE_OFFSET + MODULE_SETTINGS_STORAGE_LEN;
const LEGACY_LAYER_NAMES_STORAGE_OFFSET: usize = MODULE_STORAGE_OFFSET + LEGACY_MODULE_SETTINGS_STORAGE_LEN;

pub type LayerNameString = heapless::String<LAYER_NAME_MAX>;

const SETTING_KEYS: [u16; 82] = [
    120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142,
    143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212,
    213, 214, 215, 300, 301, 302, 303, 304, 305, 306, 307, 308, 309, 310, 311, 312, 313, 314, 315, 316, 317, 318, 319,
    320, 321, 322, 324, 325, 326, 327, 328, 329, 330, 331, 332, 333,
];

const MODULE_SETTINGS_VERSION: u8 = 9;
const MODULE_SETTINGS_LEN: usize = 45;
const LEGACY_MODULE_SETTINGS_STORAGE_LEN: usize = 32;
const MODULE_SETTINGS_STORAGE_LEN: usize = 33;
const MODULE_SETTINGS_SYNC_LEN: usize = 27;
const IDX_VERSION: usize = 0;
const IDX_LEFT_MODE: usize = 1;
const IDX_RIGHT_MODE: usize = 2;
const IDX_LEFT_BALL_AXIS: usize = 3;
const IDX_RIGHT_BALL_AXIS: usize = 4;
const IDX_LEFT_TOUCH_AXIS: usize = 5;
const IDX_RIGHT_TOUCH_AXIS: usize = 6;
const IDX_LEFT_BALL_DPI: usize = 7;
const IDX_RIGHT_BALL_DPI: usize = 8;
const IDX_LEFT_TOUCH_DPI: usize = 9;
const IDX_RIGHT_TOUCH_DPI: usize = 10;
const IDX_LEFT_SCROLL_SENS: usize = 11;
const IDX_LEFT_SNIPER_SENS: usize = 12;
const IDX_LEFT_TEXT_SENS: usize = 13;
const IDX_RIGHT_SCROLL_SENS: usize = 14;
const IDX_RIGHT_SNIPER_SENS: usize = 15;
const IDX_RIGHT_TEXT_SENS: usize = 16;
const IDX_FLAGS: usize = 17;
const IDX_AUTO_LAYER: usize = 18;
const IDX_AUTO_FLAGS: usize = 19;
const IDX_LED_BRIGHTNESS: usize = 20;
const IDX_LED_TIMEOUT_SEC: usize = 21;
const IDX_LAYER_COLORS_PACKED: usize = 22;
const IDX_BT_PROFILE_COLORS: usize = 32;
// Preserve the former deep-sleep byte so the v9 storage layout does not shift.
// It is intentionally neither advertised nor consumed by firmware behavior.
const IDX_RESERVED_DEEP_SLEEP_TIMEOUT: usize = 37;
const IDX_AUTO_LAYER_TIMEOUT: usize = 38;
const IDX_MODULE_SELECT: usize = 39;
const IDX_LEFT_ENCODER_INTERVAL: usize = 40;
const IDX_RIGHT_ENCODER_INTERVAL: usize = 41;
const IDX_AXIS_FLAGS: usize = 42;
const IDX_LEFT_ENCODER_STEPS: usize = 43;
const IDX_RIGHT_ENCODER_STEPS: usize = 44;

const FLAG_LEFT_INVERT_SCROLL_Y: u8 = 1 << 0;
const FLAG_RIGHT_INVERT_SCROLL_Y: u8 = 1 << 1;
const FLAG_LEFT_INVERT_TEXT_Y: u8 = 1 << 2;
const FLAG_RIGHT_INVERT_TEXT_Y: u8 = 1 << 3;
const FLAG_LEFT_ACCELERATION: u8 = 1 << 4;
const FLAG_RIGHT_ACCELERATION: u8 = 1 << 5;
const FLAG_LEFT_STICKY: u8 = 1 << 6;
const FLAG_RIGHT_STICKY: u8 = 1 << 7;
const DEFAULT_ACCELERATION_FLAGS: u8 = FLAG_LEFT_ACCELERATION | FLAG_RIGHT_ACCELERATION;

const AXIS_FLAG_LEFT_INVERT_SCROLL_X: u8 = 1 << 0;
const AXIS_FLAG_RIGHT_INVERT_SCROLL_X: u8 = 1 << 1;
const AXIS_FLAG_LEFT_INVERT_TEXT_X: u8 = 1 << 2;
const AXIS_FLAG_RIGHT_INVERT_TEXT_X: u8 = 1 << 3;

const AUTO_FLAG_TOUCH_GESTURES_LEFT: u8 = 1 << 4;
const AUTO_FLAG_TOUCH_GESTURES_RIGHT: u8 = 1 << 5;
const AUTO_FLAG_CHARGE_INDICATOR_DISABLED_BIT: u8 = 6;
const DEFAULT_AUTO_FLAGS: u8 = 1 | AUTO_FLAG_TOUCH_GESTURES_LEFT | AUTO_FLAG_TOUCH_GESTURES_RIGHT;

const MODULE_SELECT_TOUCH: u8 = 3;

const MODULE_DEFAULTS: [u8; MODULE_SETTINGS_LEN] = {
    let mut data = [0u8; MODULE_SETTINGS_LEN];
    data[IDX_VERSION] = MODULE_SETTINGS_VERSION;
    data[IDX_LEFT_BALL_DPI] = 4;
    data[IDX_RIGHT_BALL_DPI] = 4;
    data[IDX_LEFT_TOUCH_DPI] = 3;
    data[IDX_RIGHT_TOUCH_DPI] = 3;
    data[IDX_FLAGS] = DEFAULT_ACCELERATION_FLAGS;
    data[IDX_MODULE_SELECT] = (3 << 0) | (2 << 2);
    data[IDX_LEFT_SCROLL_SENS] = 8;
    data[IDX_RIGHT_SCROLL_SENS] = 8;
    data[IDX_LEFT_SNIPER_SENS] = 4;
    data[IDX_RIGHT_SNIPER_SENS] = 4;
    data[IDX_LEFT_TEXT_SENS] = 16;
    data[IDX_RIGHT_TEXT_SENS] = 16;
    data[IDX_AUTO_LAYER] = 4;
    data[IDX_AUTO_FLAGS] = DEFAULT_AUTO_FLAGS;
    data[IDX_LED_BRIGHTNESS] = 8;
    data[IDX_LED_TIMEOUT_SEC] = 1;
    data[IDX_RESERVED_DEEP_SLEEP_TIMEOUT] = 0;
    data[IDX_AUTO_LAYER_TIMEOUT] = 1;
    data[IDX_LEFT_ENCODER_INTERVAL] = 4;
    data[IDX_RIGHT_ENCODER_INTERVAL] = 4;
    data[IDX_LEFT_ENCODER_STEPS] = 0;
    data[IDX_RIGHT_ENCODER_STEPS] = 0;
    data = set_default_layer_color(data, 0, 0);
    data = set_default_layer_color(data, 1, 2);
    data = set_default_layer_color(data, 2, 16);
    data = set_default_layer_color(data, 3, 24);
    data = set_default_layer_color(data, 4, 6);
    data = set_default_layer_color(data, 5, 8);
    data = set_default_layer_color(data, 6, 10);
    data = set_default_layer_color(data, 7, 11);
    data = set_default_layer_color(data, 8, 15);
    data = set_default_layer_color(data, 9, 19);
    data = set_default_layer_color(data, 10, 20);
    data = set_default_layer_color(data, 11, 21);
    data = set_default_layer_color(data, 12, 22);
    data = set_default_layer_color(data, 13, 23);
    data = set_default_layer_color(data, 14, 3);
    data = set_default_layer_color(data, 15, 17);
    data[IDX_BT_PROFILE_COLORS] = 2;
    data[IDX_BT_PROFILE_COLORS + 1] = 16;
    data[IDX_BT_PROFILE_COLORS + 2] = 6;
    data[IDX_BT_PROFILE_COLORS + 3] = 8;
    data[IDX_BT_PROFILE_COLORS + 4] = 19;
    data
};
const _: () = assert!(MODULE_DEFAULTS[IDX_FLAGS] == DEFAULT_ACCELERATION_FLAGS);
const _: () = assert!(MODULE_DEFAULTS[IDX_AUTO_FLAGS] == DEFAULT_AUTO_FLAGS);

static LAYER_NAME_LEN: [AtomicU8; LAYER_NAME_COUNT] = [const { AtomicU8::new(0) }; LAYER_NAME_COUNT];
static LAYER_NAME_BYTES: [AtomicU8; LAYER_NAME_COUNT * LAYER_NAME_MAX] =
    [const { AtomicU8::new(0) }; LAYER_NAME_COUNT * LAYER_NAME_MAX];
static LAYER_NAMES_VERSION: AtomicU8 = AtomicU8::new(0);
static MODULE_SETTINGS: [AtomicU8; MODULE_SETTINGS_LEN] = [const { AtomicU8::new(0) }; MODULE_SETTINGS_LEN];

pub const fn vial_device_settings() -> VialDeviceSettings<'static> {
    VialDeviceSettings {
        setting_keys: &SETTING_KEYS,
        get_setting,
        set_setting,
        serialize,
        deserialize,
    }
}

pub fn version() -> u8 {
    LAYER_NAMES_VERSION.load(Ordering::Relaxed)
}

pub fn copy_layer_name(layer: u8, out: &mut LayerNameString) -> bool {
    let index = layer as usize;
    if index >= LAYER_NAME_COUNT {
        return false;
    }

    let len = LAYER_NAME_LEN[index].load(Ordering::Acquire) as usize;
    if len == 0 || len > LAYER_NAME_MAX {
        return false;
    }

    let mut bytes = [0u8; LAYER_NAME_MAX];
    let base = index * LAYER_NAME_MAX;
    for (offset, byte) in bytes.iter_mut().take(len).enumerate() {
        *byte = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
    }

    let Ok(name) = str::from_utf8(&bytes[..len]) else {
        return false;
    };
    out.clear();
    out.push_str(name).is_ok()
}

fn get_setting(qsid: u16, out: &mut [u8]) -> Option<usize> {
    if let Some(index) = layer_index(qsid) {
        let len = LAYER_NAME_LEN[index].load(Ordering::Acquire) as usize;
        let copy_len = len.min(LAYER_NAME_MAX).min(out.len().saturating_sub(1));
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
    } else {
        module_get_setting(qsid, out)
    }
}

fn set_setting(qsid: u16, value: &[u8]) -> bool {
    if let Some(index) = layer_index(qsid) {
        let end = value
            .iter()
            .position(|&byte| byte == 0 || byte == 0xFF)
            .unwrap_or(value.len());
        let Ok(text) = str::from_utf8(&value[..end]) else {
            return false;
        };
        store_layer_name(index, text);
        true
    } else {
        module_set_setting(qsid, value)
    }
}

fn serialize() -> VialDeviceSettingsData {
    let mut data = VialDeviceSettingsData::empty();
    data.data[0] = STORAGE_MARKER;
    data.data[1] = STORAGE_VERSION;
    data.data[MODULE_STORAGE_OFFSET..LAYER_NAMES_STORAGE_OFFSET].copy_from_slice(&serialize_module_settings());

    let mut pos = LAYER_NAMES_STORAGE_OFFSET;
    for index in 0..LAYER_NAME_COUNT {
        if pos >= data.data.len() {
            break;
        }
        let len = LAYER_NAME_LEN[index].load(Ordering::Acquire).min(LAYER_NAME_MAX as u8) as usize;
        let copy_len = len.min(data.data.len().saturating_sub(pos + 1));
        data.data[pos] = copy_len as u8;
        pos += 1;
        let base = index * LAYER_NAME_MAX;
        for offset in 0..copy_len {
            data.data[pos + offset] = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
        }
        pos += copy_len;
    }
    data.len = pos as u8;
    data
}

fn deserialize(bytes: &[u8]) {
    if bytes.first() == Some(&STORAGE_MARKER) {
        let profile = match bytes.get(1).copied() {
            Some(STORAGE_VERSION) if bytes.len() >= LAYER_NAMES_STORAGE_OFFSET => {
                Some((LAYER_NAMES_STORAGE_OFFSET, false))
            }
            Some(PREVIOUS_STORAGE_VERSION) if bytes.len() >= LAYER_NAMES_STORAGE_OFFSET => {
                Some((LAYER_NAMES_STORAGE_OFFSET, true))
            }
            Some(LEGACY_STORAGE_VERSION) if bytes.len() >= LEGACY_LAYER_NAMES_STORAGE_OFFSET => {
                Some((LEGACY_LAYER_NAMES_STORAGE_OFFSET, true))
            }
            _ => None,
        };
        if let Some((layer_names_offset, migrate_placeholders)) = profile {
            deserialize_module_settings(&bytes[MODULE_STORAGE_OFFSET..layer_names_offset]);
            deserialize_compact_layer_names(&bytes[layer_names_offset..]);
            if migrate_placeholders {
                migrate_legacy_placeholders();
            }
            LAYER_NAMES_VERSION.fetch_add(1, Ordering::Relaxed);
            publish_module_settings();
            return;
        }
    }

    deserialize_module_settings(&[]);
    deserialize_fixed_layer_names(bytes);
    migrate_legacy_placeholders();
    LAYER_NAMES_VERSION.fetch_add(1, Ordering::Relaxed);
    publish_module_settings();
}

fn deserialize_fixed_layer_names(bytes: &[u8]) {
    clear_layer_names();
    let mut pos = 0usize;
    for index in 0..LAYER_NAME_COUNT {
        if pos + LAYER_NAME_MAX + 1 > bytes.len() {
            break;
        }
        let len = bytes[pos].min(LAYER_NAME_MAX as u8) as usize;
        store_raw_layer_name(index, &bytes[pos + 1..pos + 1 + len]);
        pos += LAYER_NAME_MAX + 1;
    }
}

fn deserialize_compact_layer_names(bytes: &[u8]) {
    clear_layer_names();
    let mut pos = 0usize;
    for index in 0..LAYER_NAME_COUNT {
        if pos >= bytes.len() {
            break;
        }
        let len = bytes[pos].min(LAYER_NAME_MAX as u8) as usize;
        pos += 1;
        let available = len.min(bytes.len().saturating_sub(pos));
        store_raw_layer_name(index, &bytes[pos..pos + available]);
        pos += available;
    }
}

fn store_layer_name(index: usize, text: &str) {
    let mut sanitized = LayerNameString::new();
    let mut chars = text.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' && chars.peek() == Some(&'%') {
            let _ = chars.next();
        }
        if sanitized.push(ch).is_err() {
            break;
        }
    }
    store_raw_layer_name(index, sanitized.as_bytes());
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
        let byte = bytes.get(offset).copied().unwrap_or(0);
        LAYER_NAME_BYTES[base + offset].store(byte, Ordering::Relaxed);
    }
    LAYER_NAME_LEN[index].store(len as u8, Ordering::Release);
}

fn clear_layer_names() {
    for index in 0..LAYER_NAME_COUNT {
        store_raw_layer_name(index, &[]);
    }
}

fn migrate_legacy_placeholders() {
    let mut bytes = [0u8; LAYER_NAME_MAX];
    for (index, default_name) in crate::DEFAULT_LAYER_NAMES.iter().enumerate() {
        let len = LAYER_NAME_LEN[index].load(Ordering::Acquire).min(LAYER_NAME_MAX as u8) as usize;
        let base = index * LAYER_NAME_MAX;
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = LAYER_NAME_BYTES[base + offset].load(Ordering::Relaxed);
        }
        if crate::default_layer_names::is_legacy_placeholder(index, &bytes[..len]) {
            store_raw_layer_name(index, default_name.as_bytes());
        }
    }
}

fn layer_index(qsid: u16) -> Option<usize> {
    let offset = qsid.checked_sub(LAYER_NAME_QSID_BASE)?;
    (offset < LAYER_NAME_COUNT as u16).then_some(offset as usize)
}

fn module_get_setting(qsid: u16, out: &mut [u8]) -> Option<usize> {
    let value = module_qsid_value(qsid)?;
    let width = module_qsid_width(qsid)?;
    if out.len() < width {
        return None;
    }
    out[..width].fill(0);
    out[0] = value;
    Some(width)
}

fn module_qsid_width(qsid: u16) -> Option<usize> {
    match qsid {
        120..=152 | 300..=315 | 317..=333 => Some(1),
        316 => Some(2),
        _ => None,
    }
}

fn module_set_setting(qsid: u16, data: &[u8]) -> bool {
    let value = match data.first() {
        Some(value) => *value,
        None => return false,
    };
    match qsid {
        120 => module_set_byte(IDX_LEFT_BALL_DPI, value.min(15)),
        121 => module_set_byte(IDX_RIGHT_BALL_DPI, value.min(15)),
        122 => module_set_byte(IDX_LEFT_TOUCH_DPI, value.min(9)),
        123 => module_set_byte(IDX_RIGHT_TOUCH_DPI, value.min(9)),
        124 => module_set_byte(IDX_LEFT_SNIPER_SENS, value.max(1)),
        125 => module_set_byte(IDX_LEFT_SCROLL_SENS, value.max(1)),
        126 => module_set_byte(IDX_LEFT_TEXT_SENS, value.max(1)),
        127 => module_set_byte(IDX_RIGHT_SNIPER_SENS, value.max(1)),
        128 => module_set_byte(IDX_RIGHT_SCROLL_SENS, value.max(1)),
        129 => module_set_byte(IDX_RIGHT_TEXT_SENS, value.max(1)),
        130 => module_set_byte(IDX_LEFT_BALL_AXIS, value.min(3)),
        131 => module_set_byte(IDX_RIGHT_BALL_AXIS, value.min(3)),
        132 => module_set_byte(IDX_LEFT_TOUCH_AXIS, value.min(3)),
        133 => module_set_byte(IDX_RIGHT_TOUCH_AXIS, value.min(3)),
        134 => module_set_byte(IDX_LEFT_MODE, value.min(3)),
        135 => module_set_byte(IDX_RIGHT_MODE, value.min(3)),
        136 => module_set_flag(FLAG_LEFT_INVERT_SCROLL_Y, value != 0),
        137 => module_set_flag(FLAG_LEFT_ACCELERATION, value != 0),
        138 => module_set_flag(FLAG_RIGHT_INVERT_SCROLL_Y, value != 0),
        139 => module_set_flag(FLAG_RIGHT_ACCELERATION, value != 0),
        140 => module_set_flag(FLAG_LEFT_STICKY, value != 0),
        141 => module_set_flag(FLAG_RIGHT_STICKY, value != 0),
        142 => module_set_auto_flag(0, value != 0),
        143 => module_set_byte(IDX_AUTO_LAYER, value.min(15)),
        144 => module_set_auto_flag(1, value != 0),
        145 => module_set_auto_flag(2, value != 0),
        146 => module_set_auto_flag(3, value != 0),
        147 => module_set_flag(FLAG_LEFT_INVERT_TEXT_Y, value != 0),
        148 => module_set_flag(FLAG_RIGHT_INVERT_TEXT_Y, value != 0),
        149 => module_set_module_selection(0, value),
        150 => module_set_module_selection(1, value),
        151 => module_set_touch_gestures(0, value != 0),
        152 => module_set_touch_gestures(1, value != 0),
        300..=315 => module_set_layer_color_index((qsid - 300) as u8, value.min(24)),
        316 => module_set_byte(IDX_LED_BRIGHTNESS, value),
        317 => module_set_byte(IDX_LED_TIMEOUT_SEC, value),
        318..=322 => module_set_bt_profile_color_index((qsid - 318) as u8, value.min(24)),
        324 => module_set_byte(IDX_AUTO_LAYER_TIMEOUT, value.min(5)),
        325 => module_set_byte(IDX_LEFT_ENCODER_INTERVAL, value.min(9)),
        326 => module_set_byte(IDX_RIGHT_ENCODER_INTERVAL, value.min(9)),
        327 => module_set_axis_flag(AXIS_FLAG_LEFT_INVERT_SCROLL_X, value != 0),
        328 => module_set_axis_flag(AXIS_FLAG_RIGHT_INVERT_SCROLL_X, value != 0),
        329 => module_set_axis_flag(AXIS_FLAG_LEFT_INVERT_TEXT_X, value != 0),
        330 => module_set_axis_flag(AXIS_FLAG_RIGHT_INVERT_TEXT_X, value != 0),
        331 => module_set_auto_flag(AUTO_FLAG_CHARGE_INDICATOR_DISABLED_BIT, value == 0),
        332 => module_set_byte(IDX_LEFT_ENCODER_STEPS, value.min(7)),
        333 => module_set_byte(IDX_RIGHT_ENCODER_STEPS, value.min(7)),
        _ => return false,
    }
    publish_module_settings();
    true
}

pub fn publish_module_settings() {
    let settings = module_settings_sync_packet();
    // Storage is deserialized before registered event processors subscribe.
    // Apply the snapshot directly as well as publishing it so the selected
    // module and its split-link profile are correct on the very first boot
    // pass instead of waiting for a later refresh from Vial or the split link.
    crate::module_settings::apply_settings_packet(&settings);
    publish_event(PeripheralSettingsEvent(settings));
    #[cfg(not(feature = "qube"))]
    {
        let profile = module_profile_settings_sync_packet();
        crate::module_settings::apply_settings_packet(&profile);
        publish_event(PeripheralSettingsEvent(profile));
    }
    let encoder = module_encoder_settings_sync_packet();
    crate::module_settings::apply_settings_packet(&encoder);
    publish_event(PeripheralSettingsEvent(encoder));
}

/// Re-sends the settings snapshot whenever a half's link comes up.
///
/// The halves keep their settings in RAM only, so one that reboots on its own
/// runs on hardcoded defaults — touch gestures off, encoder steps at 1 — until
/// something republishes them. Without this the next Vial edit was the only
/// thing that could heal it.
#[processor(subscribe = [PeripheralSettingsRefreshEvent])]
pub struct ModuleSettingsBroadcast;

impl ModuleSettingsBroadcast {
    pub fn new() -> Self {
        Self
    }

    async fn on_peripheral_settings_refresh_event(&mut self, _event: PeripheralSettingsRefreshEvent) {
        publish_module_settings();
    }
}

#[cfg(not(feature = "qube"))]
fn module_profile_settings_sync_packet() -> [u8; MODULE_SETTINGS_SYNC_LEN] {
    let mut data = [0u8; MODULE_SETTINGS_SYNC_LEN];
    data[0] = MODULE_SETTINGS_VERSION | 0x80;
    let mut profile = 0u8;
    while profile < 5 {
        data[1 + usize::from(profile)] = module_bt_profile_color_index(profile);
        profile += 1;
    }
    data
}

fn module_encoder_settings_sync_packet() -> [u8; MODULE_SETTINGS_SYNC_LEN] {
    let mut data = [0u8; MODULE_SETTINGS_SYNC_LEN];
    data[0] = MODULE_SETTINGS_VERSION | 0x40;
    data[1] = module_byte(IDX_LEFT_ENCODER_STEPS).min(7);
    data[2] = module_byte(IDX_RIGHT_ENCODER_STEPS).min(7);
    data
}

fn module_settings_sync_packet() -> [u8; MODULE_SETTINGS_SYNC_LEN] {
    ensure_module_settings_initialized();
    let mut data = [0u8; MODULE_SETTINGS_SYNC_LEN];
    data[0] = MODULE_SETTINGS_VERSION;
    data[1] = (module_byte(IDX_LEFT_MODE).min(3) & 0x03)
        | ((module_byte(IDX_RIGHT_MODE).min(3) & 0x03) << 2)
        | ((module_byte(IDX_AUTO_LAYER).min(15) & 0x0f) << 4);
    data[2] = (module_byte(IDX_LEFT_BALL_AXIS).min(3) & 0x03)
        | ((module_byte(IDX_RIGHT_BALL_AXIS).min(3) & 0x03) << 2)
        | ((module_byte(IDX_LEFT_TOUCH_AXIS).min(3) & 0x03) << 4)
        | ((module_byte(IDX_RIGHT_TOUCH_AXIS).min(3) & 0x03) << 6);
    data[3] = (module_byte(IDX_LEFT_BALL_DPI).min(15) & 0x0f) | ((module_byte(IDX_RIGHT_BALL_DPI).min(15) & 0x0f) << 4);
    data[4] = (module_byte(IDX_LEFT_TOUCH_DPI).min(9) & 0x0f) | ((module_byte(IDX_RIGHT_TOUCH_DPI).min(9) & 0x0f) << 4);
    data[5] = module_byte(IDX_LEFT_SCROLL_SENS);
    data[6] = module_byte(IDX_LEFT_SNIPER_SENS);
    data[7] = module_byte(IDX_LEFT_TEXT_SENS);
    data[8] = module_byte(IDX_RIGHT_SCROLL_SENS);
    data[9] = module_byte(IDX_RIGHT_SNIPER_SENS);
    data[10] = module_byte(IDX_RIGHT_TEXT_SENS);
    data[11] = module_byte(IDX_FLAGS);
    data[12] = module_byte(IDX_AUTO_FLAGS);
    data[13] = module_byte(IDX_LED_BRIGHTNESS);
    data[14] = module_byte(IDX_LED_TIMEOUT_SEC);

    let mut layer = 0u8;
    while layer < 16 {
        pack_color(&mut data, 15, layer, module_layer_color_index(layer));
        layer += 1;
    }
    data[25] = module_byte(IDX_MODULE_SELECT) & 0x0f;
    data[26] = (module_byte(IDX_AXIS_FLAGS) & 0x0f) | ((module_byte(IDX_AUTO_LAYER_TIMEOUT).min(5) & 0x0f) << 4);
    data
}

fn module_qsid_value(qsid: u16) -> Option<u8> {
    Some(match qsid {
        120 => module_byte(IDX_LEFT_BALL_DPI),
        121 => module_byte(IDX_RIGHT_BALL_DPI),
        122 => module_byte(IDX_LEFT_TOUCH_DPI),
        123 => module_byte(IDX_RIGHT_TOUCH_DPI),
        124 => module_byte(IDX_LEFT_SNIPER_SENS),
        125 => module_byte(IDX_LEFT_SCROLL_SENS),
        126 => module_byte(IDX_LEFT_TEXT_SENS),
        127 => module_byte(IDX_RIGHT_SNIPER_SENS),
        128 => module_byte(IDX_RIGHT_SCROLL_SENS),
        129 => module_byte(IDX_RIGHT_TEXT_SENS),
        130 => module_byte(IDX_LEFT_BALL_AXIS),
        131 => module_byte(IDX_RIGHT_BALL_AXIS),
        132 => module_byte(IDX_LEFT_TOUCH_AXIS),
        133 => module_byte(IDX_RIGHT_TOUCH_AXIS),
        134 => module_byte(IDX_LEFT_MODE),
        135 => module_byte(IDX_RIGHT_MODE),
        136 => module_flag(FLAG_LEFT_INVERT_SCROLL_Y) as u8,
        137 => module_flag(FLAG_LEFT_ACCELERATION) as u8,
        138 => module_flag(FLAG_RIGHT_INVERT_SCROLL_Y) as u8,
        139 => module_flag(FLAG_RIGHT_ACCELERATION) as u8,
        140 => module_flag(FLAG_LEFT_STICKY) as u8,
        141 => module_flag(FLAG_RIGHT_STICKY) as u8,
        142 => module_auto_flag(0) as u8,
        143 => module_byte(IDX_AUTO_LAYER),
        144 => module_auto_flag(1) as u8,
        145 => module_auto_flag(2) as u8,
        146 => module_auto_flag(3) as u8,
        147 => module_flag(FLAG_LEFT_INVERT_TEXT_Y) as u8,
        148 => module_flag(FLAG_RIGHT_INVERT_TEXT_Y) as u8,
        149 => module_selection_value(0),
        150 => module_selection_value(1),
        151 => module_touch_gestures(0) as u8,
        152 => module_touch_gestures(1) as u8,
        300..=315 => module_layer_color_index((qsid - 300) as u8),
        316 => module_byte(IDX_LED_BRIGHTNESS),
        317 => module_byte(IDX_LED_TIMEOUT_SEC),
        318..=322 => module_bt_profile_color_index((qsid - 318) as u8),
        324 => module_byte(IDX_AUTO_LAYER_TIMEOUT).min(5),
        325 => module_byte(IDX_LEFT_ENCODER_INTERVAL).min(9),
        326 => module_byte(IDX_RIGHT_ENCODER_INTERVAL).min(9),
        327 => module_axis_flag(AXIS_FLAG_LEFT_INVERT_SCROLL_X) as u8,
        328 => module_axis_flag(AXIS_FLAG_RIGHT_INVERT_SCROLL_X) as u8,
        329 => module_axis_flag(AXIS_FLAG_LEFT_INVERT_TEXT_X) as u8,
        330 => module_axis_flag(AXIS_FLAG_RIGHT_INVERT_TEXT_X) as u8,
        331 => (!module_auto_flag(AUTO_FLAG_CHARGE_INDICATOR_DISABLED_BIT)) as u8,
        332 => module_byte(IDX_LEFT_ENCODER_STEPS).min(7),
        333 => module_byte(IDX_RIGHT_ENCODER_STEPS).min(7),
        _ => return None,
    })
}

fn serialize_module_settings() -> [u8; MODULE_SETTINGS_STORAGE_LEN] {
    ensure_module_settings_initialized();
    let mut data = [0u8; MODULE_SETTINGS_STORAGE_LEN];
    data[0] = MODULE_SETTINGS_VERSION;
    data[1] = (module_byte(IDX_LEFT_MODE).min(3) & 0x03)
        | ((module_byte(IDX_RIGHT_MODE).min(3) & 0x03) << 2)
        | ((module_byte(IDX_AUTO_LAYER).min(15) & 0x0f) << 4);
    data[2] = (module_byte(IDX_LEFT_BALL_AXIS).min(3) & 0x03)
        | ((module_byte(IDX_RIGHT_BALL_AXIS).min(3) & 0x03) << 2)
        | ((module_byte(IDX_LEFT_TOUCH_AXIS).min(3) & 0x03) << 4)
        | ((module_byte(IDX_RIGHT_TOUCH_AXIS).min(3) & 0x03) << 6);
    data[3] = (module_byte(IDX_LEFT_BALL_DPI).min(15) & 0x0f) | ((module_byte(IDX_RIGHT_BALL_DPI).min(15) & 0x0f) << 4);
    data[4] = (module_byte(IDX_LEFT_TOUCH_DPI).min(9) & 0x0f) | ((module_byte(IDX_RIGHT_TOUCH_DPI).min(9) & 0x0f) << 4);
    data[5] = module_byte(IDX_LEFT_SCROLL_SENS);
    data[6] = module_byte(IDX_LEFT_SNIPER_SENS);
    data[7] = module_byte(IDX_LEFT_TEXT_SENS);
    data[8] = module_byte(IDX_RIGHT_SCROLL_SENS);
    data[9] = module_byte(IDX_RIGHT_SNIPER_SENS);
    data[10] = module_byte(IDX_RIGHT_TEXT_SENS);
    data[11] = module_byte(IDX_FLAGS);
    data[12] = module_byte(IDX_AUTO_FLAGS);
    data[13] = module_byte(IDX_LED_BRIGHTNESS);
    data[14] = module_byte(IDX_LED_TIMEOUT_SEC);

    let mut i = 0u8;
    while i < 16 {
        pack_color(&mut data, 15, i, module_layer_color_index(i));
        i += 1;
    }
    while i < 21 {
        pack_color(&mut data, 15, i, module_bt_profile_color_index(i - 16));
        i += 1;
    }
    data[29] = (module_byte(IDX_RESERVED_DEEP_SLEEP_TIMEOUT) & 0x0f)
        | ((module_byte(IDX_LEFT_ENCODER_INTERVAL).min(9) & 0x0f) << 4);
    data[30] =
        module_byte(IDX_AUTO_LAYER_TIMEOUT).min(5) | ((module_byte(IDX_RIGHT_ENCODER_INTERVAL).min(9) & 0x0f) << 4);
    data[31] = (module_byte(IDX_MODULE_SELECT) & 0x0f) | ((module_byte(IDX_AXIS_FLAGS) & 0x0f) << 4);
    data[32] = (module_byte(IDX_LEFT_ENCODER_STEPS).min(7) & 0x0f)
        | ((module_byte(IDX_RIGHT_ENCODER_STEPS).min(7) & 0x0f) << 4);
    data
}

fn deserialize_module_settings(data: &[u8]) {
    if !matches!(
        data.len(),
        LEGACY_MODULE_SETTINGS_STORAGE_LEN | MODULE_SETTINGS_STORAGE_LEN
    ) || data[0] != MODULE_SETTINGS_VERSION
    {
        reset_module_settings();
        return;
    }

    reset_module_settings();
    MODULE_SETTINGS[IDX_LEFT_MODE].store(data[1] & 0x03, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_MODE].store((data[1] >> 2) & 0x03, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_AUTO_LAYER].store((data[1] >> 4) & 0x0f, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_BALL_AXIS].store(data[2] & 0x03, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_BALL_AXIS].store((data[2] >> 2) & 0x03, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_TOUCH_AXIS].store((data[2] >> 4) & 0x03, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_TOUCH_AXIS].store((data[2] >> 6) & 0x03, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_BALL_DPI].store(data[3] & 0x0f, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_BALL_DPI].store((data[3] >> 4) & 0x0f, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_TOUCH_DPI].store((data[4] & 0x0f).min(9), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_TOUCH_DPI].store(((data[4] >> 4) & 0x0f).min(9), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_SCROLL_SENS].store(data[5].max(1), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_SNIPER_SENS].store(data[6].max(1), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_TEXT_SENS].store(data[7].max(1), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_SCROLL_SENS].store(data[8].max(1), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_SNIPER_SENS].store(data[9].max(1), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_TEXT_SENS].store(data[10].max(1), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_FLAGS].store(data[11], Ordering::Relaxed);
    MODULE_SETTINGS[IDX_AUTO_FLAGS].store(data[12], Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LED_BRIGHTNESS].store(data[13], Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LED_TIMEOUT_SEC].store(data[14], Ordering::Relaxed);

    let mut i = 0u8;
    while i < 16 {
        module_set_layer_color_index(i, unpack_color(data, 15, i).min(24));
        i += 1;
    }
    while i < 21 {
        module_set_bt_profile_color_index(i - 16, unpack_color(data, 15, i).min(24));
        i += 1;
    }
    MODULE_SETTINGS[IDX_RESERVED_DEEP_SLEEP_TIMEOUT].store(data[29] & 0x0f, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_LEFT_ENCODER_INTERVAL].store((data[29] >> 4).min(9), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_AUTO_LAYER_TIMEOUT].store((data[30] & 0x0f).min(5), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_RIGHT_ENCODER_INTERVAL].store((data[30] >> 4).min(9), Ordering::Relaxed);
    MODULE_SETTINGS[IDX_MODULE_SELECT].store(data[31] & 0x0f, Ordering::Relaxed);
    MODULE_SETTINGS[IDX_AXIS_FLAGS].store((data[31] >> 4) & 0x0f, Ordering::Relaxed);
    if let Some(encoder_steps) = data.get(32).copied() {
        MODULE_SETTINGS[IDX_LEFT_ENCODER_STEPS].store((encoder_steps & 0x0f).min(7), Ordering::Relaxed);
        MODULE_SETTINGS[IDX_RIGHT_ENCODER_STEPS].store(((encoder_steps >> 4) & 0x0f).min(7), Ordering::Relaxed);
    }
}

fn ensure_module_settings_initialized() {
    if MODULE_SETTINGS[IDX_VERSION].load(Ordering::Relaxed) == MODULE_SETTINGS_VERSION {
        return;
    }
    reset_module_settings();
}

fn reset_module_settings() {
    for (idx, value) in MODULE_DEFAULTS.iter().enumerate() {
        MODULE_SETTINGS[idx].store(*value, Ordering::Relaxed);
    }
}

fn module_byte(idx: usize) -> u8 {
    ensure_module_settings_initialized();
    MODULE_SETTINGS[idx].load(Ordering::Relaxed)
}

fn module_set_byte(idx: usize, value: u8) {
    ensure_module_settings_initialized();
    MODULE_SETTINGS[idx].store(value, Ordering::Relaxed);
}

fn module_flag(mask: u8) -> bool {
    (module_byte(IDX_FLAGS) & mask) != 0
}

fn module_set_flag(mask: u8, enabled: bool) {
    let mut flags = module_byte(IDX_FLAGS);
    if enabled {
        flags |= mask;
    } else {
        flags &= !mask;
    }
    module_set_byte(IDX_FLAGS, flags);
}

fn module_axis_flag(mask: u8) -> bool {
    (module_byte(IDX_AXIS_FLAGS) & mask) != 0
}

fn module_set_axis_flag(mask: u8, enabled: bool) {
    let mut flags = module_byte(IDX_AXIS_FLAGS);
    if enabled {
        flags |= mask;
    } else {
        flags &= !mask;
    }
    module_set_byte(IDX_AXIS_FLAGS, flags & 0x0f);
}

fn module_auto_flag(bit: u8) -> bool {
    (module_byte(IDX_AUTO_FLAGS) & (1 << bit)) != 0
}

fn module_set_auto_flag(bit: u8, enabled: bool) {
    let mut flags = module_byte(IDX_AUTO_FLAGS);
    if enabled {
        flags |= 1 << bit;
    } else {
        flags &= !(1 << bit);
    }
    module_set_byte(IDX_AUTO_FLAGS, flags);
}

fn module_touch_gestures(side: u8) -> bool {
    let mask = match side {
        0 => AUTO_FLAG_TOUCH_GESTURES_LEFT,
        _ => AUTO_FLAG_TOUCH_GESTURES_RIGHT,
    };
    (module_byte(IDX_AUTO_FLAGS) & mask) != 0
}

fn module_set_touch_gestures(side: u8, enabled: bool) {
    let mask = match side {
        0 => AUTO_FLAG_TOUCH_GESTURES_LEFT,
        _ => AUTO_FLAG_TOUCH_GESTURES_RIGHT,
    };
    let mut flags = module_byte(IDX_AUTO_FLAGS);
    if enabled {
        flags |= mask;
    } else {
        flags &= !mask;
    }
    module_set_byte(IDX_AUTO_FLAGS, flags);
}

fn module_selection_value(side: u8) -> u8 {
    let shift = if side == 0 { 0 } else { 2 };
    (module_byte(IDX_MODULE_SELECT) >> shift) & 0x03
}

fn module_set_module_selection(side: u8, value: u8) {
    let shift = if side == 0 { 0 } else { 2 };
    let mut select = module_byte(IDX_MODULE_SELECT) & !(0x03 << shift);
    select |= (value.min(MODULE_SELECT_TOUCH) & 0x03) << shift;
    module_set_byte(IDX_MODULE_SELECT, select & 0x0f);
}

fn module_layer_color_index(layer: u8) -> u8 {
    if layer >= 16 {
        return 0;
    }
    let bit = usize::from(layer) * 5;
    let byte_idx = IDX_LAYER_COLORS_PACKED + bit / 8;
    let shift = bit % 8;
    let mut raw = u16::from(module_byte(byte_idx)) >> shift;
    if shift > 3 {
        raw |= u16::from(module_byte(byte_idx + 1)) << (8 - shift);
    }
    (raw as u8) & 0x1f
}

fn module_set_layer_color_index(layer: u8, value: u8) {
    if layer >= 16 {
        return;
    }
    let bit = usize::from(layer) * 5;
    let byte_idx = IDX_LAYER_COLORS_PACKED + bit / 8;
    let shift = bit % 8;
    let mask = 0x1fu16 << shift;
    let raw = (u16::from(value) & 0x1f) << shift;

    let mut combined = u16::from(module_byte(byte_idx));
    if shift > 3 {
        combined |= u16::from(module_byte(byte_idx + 1)) << 8;
    }
    combined = (combined & !mask) | raw;
    module_set_byte(byte_idx, combined as u8);
    if shift > 3 {
        module_set_byte(byte_idx + 1, (combined >> 8) as u8);
    }
}

fn module_bt_profile_color_index(profile: u8) -> u8 {
    if profile >= 5 {
        return 0;
    }
    module_byte(IDX_BT_PROFILE_COLORS + usize::from(profile)).min(24)
}

fn module_set_bt_profile_color_index(profile: u8, value: u8) {
    if profile >= 5 {
        return;
    }
    module_set_byte(IDX_BT_PROFILE_COLORS + usize::from(profile), value.min(24));
}

fn pack_color(out: &mut [u8], offset: usize, index: u8, value: u8) {
    let bit = usize::from(index) * 5;
    let byte_idx = offset + bit / 8;
    let shift = bit % 8;
    let raw = (u16::from(value) & 0x1f) << shift;
    out[byte_idx] |= raw as u8;
    if shift > 3 {
        out[byte_idx + 1] |= (raw >> 8) as u8;
    }
}

fn unpack_color(data: &[u8], offset: usize, index: u8) -> u8 {
    let bit = usize::from(index) * 5;
    let byte_idx = offset + bit / 8;
    let shift = bit % 8;
    let mut raw = u16::from(data[byte_idx]) >> shift;
    if shift > 3 {
        raw |= u16::from(data[byte_idx + 1]) << (8 - shift);
    }
    (raw as u8) & 0x1f
}

const fn set_default_layer_color(
    mut data: [u8; MODULE_SETTINGS_LEN],
    layer: usize,
    value: u8,
) -> [u8; MODULE_SETTINGS_LEN] {
    let bit = layer * 5;
    let byte_idx = IDX_LAYER_COLORS_PACKED + bit / 8;
    let shift = bit % 8;
    let raw = (value as u16 & 0x1f) << shift;
    data[byte_idx] |= raw as u8;
    if shift > 3 {
        data[byte_idx + 1] |= (raw >> 8) as u8;
    }
    data
}
