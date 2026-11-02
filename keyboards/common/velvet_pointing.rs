//! Runtime pointing settings shared by Velvet standalone and Velvet + Qube.

use rmk::event::{
    ActionEvent, AutoMouseLayerConfigEvent, LayerChangeEvent, PeripheralSettingsEvent, PointingProcessorEvent,
    PointingSetCpiEvent, PointingTransformEvent, publish_event_async,
};
use rmk::input_device::pointing::{
    CaretConfig, CursorConfig, PointingMode, PointingModeKeyState, ScrollAxis, ScrollConfig, SniperConfig,
};
use rmk::macros::processor;
use rmk::types::action::Action;
use rmk::types::keycode::HidKeyCode;

pub(crate) const SETTINGS_VERSION: u8 = 0x56;
pub(crate) const LEGACY_SETTINGS_STORAGE_LEN: usize = 10;
pub(crate) const SETTINGS_STORAGE_LEN: usize = 11;

pub(crate) const IDX_VERSION: usize = 0;
pub(crate) const IDX_MODE: usize = 1;
pub(crate) const IDX_AXIS: usize = 2;
pub(crate) const IDX_DPI: usize = 3;
pub(crate) const IDX_SNIPER_SENS: usize = 4;
pub(crate) const IDX_SCROLL_SENS: usize = 5;
pub(crate) const IDX_TEXT_SENS: usize = 6;
pub(crate) const IDX_FLAGS: usize = 7;
pub(crate) const IDX_AUTO_LAYER: usize = 8;
pub(crate) const IDX_AUTO_FLAGS: usize = 9;
pub(crate) const IDX_AUTO_LAYER_TIMEOUT: usize = 10;

pub(crate) const FLAG_INVERT_SCROLL_Y: u8 = 1 << 0;
pub(crate) const FLAG_INVERT_TEXT_Y: u8 = 1 << 1;
pub(crate) const FLAG_ACCELERATION: u8 = 1 << 2;
pub(crate) const FLAG_STICKY: u8 = 1 << 3;
pub(crate) const FLAG_INVERT_SCROLL_X: u8 = 1 << 4;
pub(crate) const FLAG_INVERT_TEXT_X: u8 = 1 << 5;
pub(crate) const FLAG_TRACKBALL_DISABLED: u8 = 1 << 6;
pub(crate) const FLAGS_MASK: u8 = FLAG_INVERT_SCROLL_Y
    | FLAG_INVERT_TEXT_Y
    | FLAG_ACCELERATION
    | FLAG_STICKY
    | FLAG_INVERT_SCROLL_X
    | FLAG_INVERT_TEXT_X
    | FLAG_TRACKBALL_DISABLED;

const TRACKBALL_DEVICE_ID: u8 = 0;
const LAYER_SCROLL: u8 = 5;
const LAYER_SNIPER: u8 = 6;
const USER_SNIPER: u8 = 10;
const USER_SCROLL: u8 = 11;
const USER_TEXT: u8 = 12;
const AUTO_LAYER_TIMEOUT_MS_TABLE: [u32; 6] = [250, 500, 750, 1000, 1250, 1500];
// Ignore isolated one-count PMW3610 idle jitter. Unlike the old coherent
// accumulator, this threshold never turns repeated +/-1 noise into activity.
const AUTO_LAYER_MOTION_THRESHOLD: u16 = 2;

const DPI_TABLE: [u16; 16] = [
    200, 400, 600, 800, 1000, 1200, 1400, 1600, 1800, 2000, 2200, 2400, 2600, 2800, 3000, 3200,
];

pub(crate) const fn default_settings_packet() -> [u8; 27] {
    let mut data = [0u8; 27];
    data[IDX_VERSION] = SETTINGS_VERSION;
    data[IDX_MODE] = 0;
    data[IDX_AXIS] = 0;
    data[IDX_DPI] = 4;
    data[IDX_SNIPER_SENS] = 4;
    data[IDX_SCROLL_SENS] = 8;
    data[IDX_TEXT_SENS] = 16;
    data[IDX_FLAGS] = 0;
    data[IDX_AUTO_LAYER] = 4;
    data[IDX_AUTO_FLAGS] = 1;
    data[IDX_AUTO_LAYER_TIMEOUT] = 1;
    data
}

pub(crate) fn sanitize_settings_packet(data: &[u8]) -> [u8; 27] {
    if data.len() < LEGACY_SETTINGS_STORAGE_LEN || data[IDX_VERSION] != SETTINGS_VERSION {
        return default_settings_packet();
    }

    let mut sanitized = default_settings_packet();
    let copy_len = data.len().min(SETTINGS_STORAGE_LEN);
    sanitized[..copy_len].copy_from_slice(&data[..copy_len]);
    sanitized[IDX_MODE] = sanitized[IDX_MODE].min(3);
    sanitized[IDX_AXIS] = sanitized[IDX_AXIS].min(3);
    sanitized[IDX_DPI] = sanitized[IDX_DPI].min(15);
    sanitized[IDX_SNIPER_SENS] = sanitized[IDX_SNIPER_SENS].max(1);
    sanitized[IDX_SCROLL_SENS] = sanitized[IDX_SCROLL_SENS].max(1);
    sanitized[IDX_TEXT_SENS] = sanitized[IDX_TEXT_SENS].max(1);
    sanitized[IDX_FLAGS] &= FLAGS_MASK;
    sanitized[IDX_AUTO_LAYER] = sanitized[IDX_AUTO_LAYER].min(15);
    sanitized[IDX_AUTO_FLAGS] &= 0x0f;
    sanitized[IDX_AUTO_LAYER_TIMEOUT] = sanitized[IDX_AUTO_LAYER_TIMEOUT].min(5);
    sanitized
}

pub(crate) fn cpi_from_packet(data: &[u8]) -> Option<u16> {
    let settings = Settings::from_packet(data)?;
    Some(DPI_TABLE[usize::from(settings.dpi_index)])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Cursor,
    Sniper,
    Scroll,
    Text,
}

impl Mode {
    fn from_value(value: u8) -> Self {
        match value.min(3) {
            1 => Self::Sniper,
            2 => Self::Scroll,
            3 => Self::Text,
            _ => Self::Cursor,
        }
    }
}

#[derive(Clone, Copy)]
struct Settings {
    mode: Mode,
    axis: u8,
    dpi_index: u8,
    sniper_sens: u8,
    scroll_sens: u8,
    text_sens: u8,
    flags: u8,
    auto_layer: u8,
    auto_flags: u8,
    auto_layer_timeout_index: u8,
}

impl Settings {
    fn defaults() -> Self {
        Self::from_packet(&default_settings_packet()).expect("default Velvet settings are valid")
    }

    fn from_packet(data: &[u8]) -> Option<Self> {
        if data.len() < LEGACY_SETTINGS_STORAGE_LEN || data[IDX_VERSION] != SETTINGS_VERSION {
            return None;
        }
        Some(Self {
            mode: Mode::from_value(data[IDX_MODE]),
            axis: data[IDX_AXIS].min(3),
            dpi_index: data[IDX_DPI].min(15),
            sniper_sens: data[IDX_SNIPER_SENS].max(1),
            scroll_sens: data[IDX_SCROLL_SENS].max(1),
            text_sens: data[IDX_TEXT_SENS].max(1),
            flags: data[IDX_FLAGS] & FLAGS_MASK,
            auto_layer: data[IDX_AUTO_LAYER].min(15),
            auto_flags: data[IDX_AUTO_FLAGS] & 0x0f,
            auto_layer_timeout_index: data.get(IDX_AUTO_LAYER_TIMEOUT).copied().unwrap_or(1).min(5),
        })
    }

    fn flag(self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    fn auto_layer_enabled(self, mode: Mode) -> bool {
        let bit = match mode {
            Mode::Cursor => 0,
            Mode::Sniper => 1,
            Mode::Scroll => 2,
            Mode::Text => 3,
        };
        self.auto_flags & (1 << bit) != 0
    }

    fn trackball_enabled(self) -> bool {
        !self.flag(FLAG_TRACKBALL_DISABLED)
    }

    fn auto_layer_timeout_ms(self) -> u32 {
        AUTO_LAYER_TIMEOUT_MS_TABLE[usize::from(self.auto_layer_timeout_index)]
    }

    fn pointing_mode(self, mode: Mode) -> PointingMode {
        match mode {
            Mode::Cursor => PointingMode::Cursor(CursorConfig::default()),
            Mode::Sniper => PointingMode::Sniper(SniperConfig {
                divisor: self.sniper_sens,
                ..Default::default()
            }),
            Mode::Scroll => PointingMode::Scroll(ScrollConfig {
                wheel: ScrollAxis {
                    divisor: self.scroll_sens,
                    invert: self.flag(FLAG_INVERT_SCROLL_Y),
                    ..Default::default()
                },
                pan: ScrollAxis {
                    divisor: self.scroll_sens,
                    invert: self.flag(FLAG_INVERT_SCROLL_X),
                    ..Default::default()
                },
            }),
            Mode::Text => PointingMode::Caret(CaretConfig {
                invert_x: self.flag(FLAG_INVERT_TEXT_X),
                invert_y: self.flag(FLAG_INVERT_TEXT_Y),
                threshold: i16::from(self.text_sens),
                keycode_up: HidKeyCode::Up,
                keycode_down: HidKeyCode::Down,
                keycode_left: HidKeyCode::Left,
                keycode_right: HidKeyCode::Right,
                ..Default::default()
            }),
        }
    }
}

#[processor(
    subscribe = [ActionEvent, LayerChangeEvent, PeripheralSettingsEvent]
)]
pub struct VelvetPointingMode {
    settings: Settings,
    layer_mode: Option<Mode>,
    mode_key: PointingModeKeyState<Mode>,
    published_mode: PointingMode,
    published_axis: u8,
    published_acceleration: bool,
}

impl VelvetPointingMode {
    pub fn new() -> Self {
        Self {
            settings: Settings::defaults(),
            layer_mode: None,
            mode_key: PointingModeKeyState::new(),
            published_mode: PointingMode::default(),
            published_axis: 0,
            published_acceleration: false,
        }
    }

    async fn on_peripheral_settings_event(&mut self, event: PeripheralSettingsEvent) {
        let Some(settings) = Settings::from_packet(&event.0) else {
            return;
        };
        self.settings = settings;
        self.mode_key.set_sticky_enabled(self.settings.flag(FLAG_STICKY));
        self.publish_current(true).await;
    }

    async fn on_layer_change_event(&mut self, LayerChangeEvent(layer): LayerChangeEvent) {
        self.layer_mode = match layer {
            LAYER_SCROLL => Some(Mode::Scroll),
            LAYER_SNIPER => Some(Mode::Sniper),
            _ => None,
        };
        self.publish_current(false).await;
    }

    async fn on_action_event(&mut self, event: ActionEvent) {
        let Action::User(id) = event.action else {
            return;
        };
        let mode = match id {
            USER_SNIPER => Some(Mode::Sniper),
            USER_SCROLL => Some(Mode::Scroll),
            USER_TEXT => Some(Mode::Text),
            _ => None,
        };
        let Some(mode) = mode else {
            return;
        };

        self.mode_key
            .handle(mode, event.keyboard_event.pressed, self.settings.flag(FLAG_STICKY));
        self.publish_current(false).await;
    }

    fn current_mode(&self) -> Mode {
        self.mode_key
            .mode_override()
            .or(self.layer_mode)
            .unwrap_or(self.settings.mode)
    }

    async fn publish_current(&mut self, force: bool) {
        let mode = if self.settings.trackball_enabled() {
            self.settings.pointing_mode(self.current_mode())
        } else {
            PointingMode::Disabled
        };
        if force || self.published_mode != mode {
            publish_event_async(PointingProcessorEvent {
                device_id: TRACKBALL_DEVICE_ID,
                mode,
            })
            .await;
            self.published_mode = mode;
        }

        let acceleration = self.settings.flag(FLAG_ACCELERATION);
        if force || self.published_axis != self.settings.axis || self.published_acceleration != acceleration {
            publish_event_async(PointingTransformEvent {
                device_id: TRACKBALL_DEVICE_ID,
                rotation: self.settings.axis,
                acceleration,
            })
            .await;
            self.published_axis = self.settings.axis;
            self.published_acceleration = acceleration;
        }

        let mode = self.current_mode();
        publish_event_async(AutoMouseLayerConfigEvent {
            device_id: TRACKBALL_DEVICE_ID,
            enabled: self.settings.trackball_enabled()
                && self.settings.auto_layer_enabled(mode)
                && self.settings.auto_layer != 0,
            target_layer: self.settings.auto_layer,
            timeout_ms: self.settings.auto_layer_timeout_ms(),
            threshold: AUTO_LAYER_MOTION_THRESHOLD,
        })
        .await;
    }
}

#[processor(subscribe = [PeripheralSettingsEvent])]
pub struct VelvetPointingSettingsSync;

impl VelvetPointingSettingsSync {
    pub const fn new() -> Self {
        Self
    }

    async fn on_peripheral_settings_event(&mut self, event: PeripheralSettingsEvent) {
        let Some(cpi) = cpi_from_packet(&event.0) else {
            return;
        };
        publish_event_async(PointingSetCpiEvent {
            device_id: TRACKBALL_DEVICE_ID,
            cpi,
        })
        .await;
    }
}
