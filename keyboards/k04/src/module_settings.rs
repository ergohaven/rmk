use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embassy_sync::watch::Watch;
use rmk::event::{PeripheralSettingsEvent, SleepStateEvent};
use rmk::macros::processor;
use rmk::split::ble::central::{set_split_link_profile, SplitLinkProfile};

const VERSION: u8 = if cfg!(feature = "standalone") { 10 } else { 9 };
const SETTINGS_LEN: usize = 45;
const TOUCH_DPI_BASE: u16 = 400;

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
const IDX_MODULE_SELECT: usize = 39;
const IDX_AXIS_FLAGS: usize = 42;
const IDX_LEFT_ENCODER_STEPS: usize = 43;
const IDX_RIGHT_ENCODER_STEPS: usize = 44;
const AUTO_FLAG_TOUCH_GESTURES_LEFT: u8 = 1 << 4;
const AUTO_FLAG_TOUCH_GESTURES_RIGHT: u8 = 1 << 5;
const AUTO_FLAG_CHARGE_INDICATOR_DISABLED: u8 = 1 << 6;

const BALL_DPI_TABLE: [u16; 16] = [
    200, 400, 600, 800, 1000, 1200, 1400, 1600, 1800, 2000, 2200, 2400, 2600, 2800, 3000, 3200,
];

static SETTINGS: [AtomicU8; SETTINGS_LEN] = [const { AtomicU8::new(0) }; SETTINGS_LEN];
static MODULE_SELECTIONS: Watch<rmk::RawMutex, [ModuleSelection; 2], 4> = Watch::new_with([ModuleSelection::None; 2]);
static MODULE_SLEEPING: AtomicBool = AtomicBool::new(false);
static MODULE_SLEEP_STATE: Watch<rmk::RawMutex, bool, 4> = Watch::new_with(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleSelection {
    None,
    Encoder,
    Trackball,
    Touchpad,
}

impl ModuleSelection {
    fn from_raw(value: u8) -> Self {
        match value & 0x03 {
            1 => Self::Encoder,
            2 => Self::Trackball,
            3 => Self::Touchpad,
            _ => Self::None,
        }
    }

    fn is_pointing(self) -> bool {
        matches!(self, Self::Trackball | Self::Touchpad)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[processor(subscribe = [PeripheralSettingsEvent, SleepStateEvent])]
pub struct ModuleSettingsSync;

impl ModuleSettingsSync {
    pub fn new() -> Self {
        ensure_initialized();
        Self
    }

    async fn on_peripheral_settings_event(&mut self, event: PeripheralSettingsEvent) {
        apply_settings_packet(&event.0);
    }

    async fn on_sleep_state_event(&mut self, event: SleepStateEvent) {
        MODULE_SLEEPING.store(event.0, Ordering::Release);
        MODULE_SLEEP_STATE.sender().send_if_modified(|current| {
            if current.as_ref() == Some(&event.0) {
                false
            } else {
                *current = Some(event.0);
                true
            }
        });
    }
}

pub fn led_brightness() -> u8 {
    byte(IDX_LED_BRIGHTNESS)
}

pub fn charge_indicator_enabled() -> bool {
    byte(IDX_AUTO_FLAGS) & AUTO_FLAG_CHARGE_INDICATOR_DISABLED == 0
}

pub fn led_timeout_sec() -> u8 {
    byte(IDX_LED_TIMEOUT_SEC)
}

pub fn layer_color(layer: u8) -> Rgb {
    palette(layer_color_index(layer))
}

pub fn bt_profile_color(profile: u8) -> Rgb {
    let index = if profile < 5 {
        byte(IDX_BT_PROFILE_COLORS + usize::from(profile)).min(24)
    } else {
        1
    };
    palette(index)
}

pub fn ball_cpi(side: u8) -> u16 {
    let idx = byte(if side == 0 {
        IDX_LEFT_BALL_DPI
    } else {
        IDX_RIGHT_BALL_DPI
    }) as usize;
    BALL_DPI_TABLE[idx.min(BALL_DPI_TABLE.len() - 1)]
}

pub fn scale_touch_delta(value: i16, side: u8) -> i16 {
    let idx = byte(if side == 0 {
        IDX_LEFT_TOUCH_DPI
    } else {
        IDX_RIGHT_TOUCH_DPI
    });
    let touch_dpi = (u16::from(idx.min(9)) + 1) * 100;
    let scaled = i32::from(value) * i32::from(touch_dpi) / i32::from(TOUCH_DPI_BASE);
    scaled.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

pub fn touch_gestures_enabled(side: u8) -> bool {
    let mask = if side == 0 {
        AUTO_FLAG_TOUCH_GESTURES_LEFT
    } else {
        AUTO_FLAG_TOUCH_GESTURES_RIGHT
    };
    byte(IDX_AUTO_FLAGS) & mask != 0
}

pub fn module_selection(side: u8) -> ModuleSelection {
    ensure_initialized();
    module_selections()[usize::from(side.min(1))]
}

pub async fn wait_for_module_selection_change(side: u8, current: ModuleSelection) -> ModuleSelection {
    ensure_initialized();
    let side = usize::from(side.min(1));
    let mut receiver = MODULE_SELECTIONS
        .receiver()
        .expect("K:04 module selection receiver capacity exhausted");
    receiver.get_and(|selections| selections[side] != current).await[side]
}

pub fn module_sleeping() -> bool {
    MODULE_SLEEPING.load(Ordering::Acquire)
}

pub async fn wait_for_module_sleep_change(current: bool) -> bool {
    let mut receiver = MODULE_SLEEP_STATE
        .receiver()
        .expect("K:04 module sleep receiver capacity exhausted");
    receiver.get_and(|sleeping| *sleeping != current).await
}

pub(crate) fn apply_settings_packet(data: &[u8; 27]) {
    if data[0] == VERSION | 0x40 {
        ensure_initialized();
        let left_steps = data[1].min(7);
        let right_steps = data[2].min(7);
        SETTINGS[IDX_LEFT_ENCODER_STEPS].store(left_steps, Ordering::Relaxed);
        SETTINGS[IDX_RIGHT_ENCODER_STEPS].store(right_steps, Ordering::Relaxed);
        rmk::input_device::rotary_encoder::set_encoder_steps(0, left_steps + 1);
        rmk::input_device::rotary_encoder::set_encoder_steps(1, right_steps + 1);
        return;
    }
    if data[0] == VERSION | 0x80 {
        let mut profile = 0usize;
        while profile < 5 {
            SETTINGS[IDX_BT_PROFILE_COLORS + profile].store(data[1 + profile].min(24), Ordering::Relaxed);
            profile += 1;
        }
        return;
    }
    if data[0] != VERSION {
        return;
    }
    ensure_initialized();
    SETTINGS[0].store(VERSION, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_MODE].store(data[1] & 0x03, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_MODE].store((data[1] >> 2) & 0x03, Ordering::Relaxed);
    SETTINGS[IDX_AUTO_LAYER].store((data[1] >> 4) & 0x0f, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_BALL_AXIS].store(data[2] & 0x03, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_BALL_AXIS].store((data[2] >> 2) & 0x03, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_TOUCH_AXIS].store((data[2] >> 4) & 0x03, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_TOUCH_AXIS].store((data[2] >> 6) & 0x03, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_BALL_DPI].store(data[3] & 0x0f, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_BALL_DPI].store((data[3] >> 4) & 0x0f, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_TOUCH_DPI].store((data[4] & 0x0f).min(9), Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_TOUCH_DPI].store(((data[4] >> 4) & 0x0f).min(9), Ordering::Relaxed);
    SETTINGS[IDX_LEFT_SCROLL_SENS].store(data[5].max(1), Ordering::Relaxed);
    SETTINGS[IDX_LEFT_SNIPER_SENS].store(data[6].max(1), Ordering::Relaxed);
    SETTINGS[IDX_LEFT_TEXT_SENS].store(data[7].max(1), Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_SCROLL_SENS].store(data[8].max(1), Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_SNIPER_SENS].store(data[9].max(1), Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_TEXT_SENS].store(data[10].max(1), Ordering::Relaxed);
    SETTINGS[IDX_FLAGS].store(data[11], Ordering::Relaxed);
    SETTINGS[IDX_AUTO_FLAGS].store(data[12], Ordering::Relaxed);
    SETTINGS[IDX_LED_BRIGHTNESS].store(data[13], Ordering::Relaxed);
    SETTINGS[IDX_LED_TIMEOUT_SEC].store(data[14], Ordering::Relaxed);

    let mut layer = 0u8;
    while layer < 16 {
        set_layer_color_index(layer, unpack_color(data, 15, layer).min(24));
        layer += 1;
    }
    SETTINGS[IDX_MODULE_SELECT].store(data[25] & 0x0f, Ordering::Relaxed);
    SETTINGS[IDX_AXIS_FLAGS].store(data[26] & 0x0f, Ordering::Relaxed);
    apply_module_selection_runtime();
}

fn ensure_initialized() {
    if SETTINGS[0].load(Ordering::Relaxed) == VERSION {
        return;
    }
    SETTINGS[0].store(VERSION, Ordering::Relaxed);
    let default_ball_dpi = if cfg!(feature = "production_v22") { 2 } else { 4 };
    SETTINGS[IDX_LEFT_BALL_DPI].store(default_ball_dpi, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_BALL_DPI].store(default_ball_dpi, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_TOUCH_DPI].store(3, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_TOUCH_DPI].store(3, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_SCROLL_SENS].store(8, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_SNIPER_SENS].store(4, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_TEXT_SENS].store(16, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_SCROLL_SENS].store(8, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_SNIPER_SENS].store(4, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_TEXT_SENS].store(16, Ordering::Relaxed);
    SETTINGS[IDX_LED_BRIGHTNESS].store(8, Ordering::Relaxed);
    SETTINGS[IDX_LED_TIMEOUT_SEC].store(1, Ordering::Relaxed);
    SETTINGS[IDX_LEFT_ENCODER_STEPS].store(0, Ordering::Relaxed);
    SETTINGS[IDX_RIGHT_ENCODER_STEPS].store(0, Ordering::Relaxed);
    rmk::input_device::rotary_encoder::set_encoder_steps(0, 1);
    rmk::input_device::rotary_encoder::set_encoder_steps(1, 1);
    set_layer_color_index(0, 0);
    set_layer_color_index(1, 2);
    set_layer_color_index(2, 16);
    set_layer_color_index(3, 24);
    set_layer_color_index(4, 6);
    set_layer_color_index(5, 8);
    set_layer_color_index(6, 10);
    set_layer_color_index(7, 11);
    set_layer_color_index(8, 15);
    set_layer_color_index(9, 19);
    set_layer_color_index(10, 20);
    set_layer_color_index(11, 21);
    set_layer_color_index(12, 22);
    set_layer_color_index(13, 23);
    set_layer_color_index(14, 3);
    set_layer_color_index(15, 17);
    SETTINGS[IDX_BT_PROFILE_COLORS].store(2, Ordering::Relaxed);
    SETTINGS[IDX_BT_PROFILE_COLORS + 1].store(16, Ordering::Relaxed);
    SETTINGS[IDX_BT_PROFILE_COLORS + 2].store(6, Ordering::Relaxed);
    SETTINGS[IDX_BT_PROFILE_COLORS + 3].store(8, Ordering::Relaxed);
    SETTINGS[IDX_BT_PROFILE_COLORS + 4].store(19, Ordering::Relaxed);
    apply_module_selection_runtime();
}

fn module_selections() -> [ModuleSelection; 2] {
    let packed = SETTINGS[IDX_MODULE_SELECT].load(Ordering::Relaxed);
    [
        ModuleSelection::from_raw(packed),
        ModuleSelection::from_raw(packed >> 2),
    ]
}

fn apply_module_selection_runtime() {
    let selections = module_selections();
    MODULE_SELECTIONS.sender().send_if_modified(|current| {
        if current.as_ref() == Some(&selections) {
            false
        } else {
            *current = Some(selections);
            true
        }
    });

    rmk::input_device::rotary_encoder::set_encoder_enabled(0, selections[0] == ModuleSelection::Encoder);
    rmk::input_device::rotary_encoder::set_encoder_enabled(1, selections[1] == ModuleSelection::Encoder);

    #[cfg(feature = "qube")]
    {
        let _ = set_split_link_profile(0, link_profile(selections[0]));
        let _ = set_split_link_profile(1, link_profile(selections[1]));
    }
    #[cfg(not(feature = "qube"))]
    {
        // Standalone K:04 has one split peripheral: the right half.
        let _ = set_split_link_profile(0, link_profile(selections[1]));
    }
}

fn link_profile(selection: ModuleSelection) -> SplitLinkProfile {
    if selection.is_pointing() {
        SplitLinkProfile::Pointing
    } else {
        SplitLinkProfile::Keyboard
    }
}

fn byte(idx: usize) -> u8 {
    ensure_initialized();
    SETTINGS[idx].load(Ordering::Relaxed)
}

fn layer_color_index(layer: u8) -> u8 {
    if layer >= 16 {
        return 0;
    }
    let bit = usize::from(layer) * 5;
    let byte_idx = IDX_LAYER_COLORS_PACKED + bit / 8;
    let shift = bit % 8;
    let mut raw = u16::from(byte(byte_idx)) >> shift;
    if shift > 3 {
        raw |= u16::from(byte(byte_idx + 1)) << (8 - shift);
    }
    (raw as u8) & 0x1f
}

fn set_layer_color_index(layer: u8, value: u8) {
    if layer >= 16 {
        return;
    }
    let bit = usize::from(layer) * 5;
    let byte_idx = IDX_LAYER_COLORS_PACKED + bit / 8;
    let shift = bit % 8;
    let mask = 0x1fu16 << shift;
    let raw = (u16::from(value.min(24)) & 0x1f) << shift;

    let mut combined = u16::from(SETTINGS[byte_idx].load(Ordering::Relaxed));
    if shift > 3 {
        combined |= u16::from(SETTINGS[byte_idx + 1].load(Ordering::Relaxed)) << 8;
    }
    combined = (combined & !mask) | raw;
    SETTINGS[byte_idx].store(combined as u8, Ordering::Relaxed);
    if shift > 3 {
        SETTINGS[byte_idx + 1].store((combined >> 8) as u8, Ordering::Relaxed);
    }
}

fn unpack_color(data: &[u8], base: usize, index: u8) -> u8 {
    let bit = usize::from(index) * 5;
    let byte_idx = base + bit / 8;
    let shift = bit % 8;
    let mut raw = u16::from(data[byte_idx]) >> shift;
    if shift > 3 {
        raw |= u16::from(data[byte_idx + 1]) << (8 - shift);
    }
    (raw as u8) & 0x1f
}

fn palette(index: u8) -> Rgb {
    match index {
        1 => Rgb { r: 255, g: 255, b: 255 },
        2 => Rgb { r: 255, g: 0, b: 0 },
        3 => Rgb { r: 255, g: 64, b: 0 },
        4 => Rgb { r: 218, g: 165, b: 32 },
        5 => Rgb { r: 255, g: 215, b: 0 },
        6 => Rgb { r: 255, g: 255, b: 0 },
        7 => Rgb { r: 128, g: 255, b: 0 },
        8 => Rgb { r: 0, g: 255, b: 0 },
        9 => Rgb { r: 0, g: 128, b: 0 },
        10 => Rgb { r: 0, g: 255, b: 128 },
        11 => Rgb { r: 0, g: 224, b: 192 },
        12 => Rgb { r: 0, g: 128, b: 128 },
        13 => Rgb { r: 0, g: 255, b: 255 },
        14 => Rgb { r: 0, g: 128, b: 255 },
        15 => Rgb { r: 0, g: 191, b: 255 },
        16 => Rgb { r: 0, g: 0, b: 255 },
        17 => Rgb { r: 75, g: 0, b: 130 },
        18 => Rgb { r: 128, g: 0, b: 255 },
        19 => Rgb { r: 255, g: 0, b: 255 },
        20 => Rgb { r: 255, g: 64, b: 128 },
        21 => Rgb { r: 255, g: 96, b: 80 },
        22 => Rgb { r: 255, g: 128, b: 114 },
        23 => Rgb { r: 255, g: 180, b: 120 },
        24 => Rgb { r: 255, g: 128, b: 0 },
        _ => Rgb { r: 0, g: 0, b: 0 },
    }
}
