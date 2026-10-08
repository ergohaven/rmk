use byteorder::{BigEndian, ByteOrder, LittleEndian};
use embassy_time::Instant;
use postcard::experimental::max_size::MaxSize;
use rmk_types::action::{Action, KeyAction};
use rmk_types::battery::BatteryStatus;
use rmk_types::combo::Combo as ComboConfig;
use rmk_types::constants::{COMBO_MAX_NUM, MORSE_MAX_NUM};
use rmk_types::morse::{DOUBLE_TAP, HOLD, HOLD_AFTER_TAP, MorsePattern, TAP};
use rmk_types::protocol::vial::{VIA_PROTOCOL_VERSION, ViaCommand, ViaKeyboardInfo};
use vial::process_vial;

use crate::channel::{HOST_REQUEST_CHANNEL, try_send_host_reply};
use crate::config::{RmkConfig, VialConfig};
use crate::core_traits::Runnable;
use crate::hid::ViaReport;
use crate::host::context::KeyboardContext;
use crate::host::via::keycode_convert::{from_via_keycode, to_via_keycode};
use crate::{MACRO_SPACE_SIZE, boot};

pub(crate) mod keycode_convert;
mod vial;
#[cfg(feature = "vial_lock")]
mod vial_lock;

const HOST_DATA_TIME: u8 = 0xAA;
const HOST_DATA_VOLUME: u8 = 0xAB;
const HOST_DATA_LAYOUT: u8 = 0xAC;
const HOST_DATA_MEDIA_ARTIST: u8 = 0xAD;
const HOST_DATA_MEDIA_TITLE: u8 = 0xAE;
const ERGOHAVEN_CUSTOM_NAMESPACE: u8 = 0xE8;
const ERGOHAVEN_CUSTOM_BATTERY_HALVES: u8 = 0x01;
const ERGOHAVEN_BATTERY_HALVES_VERSION: u8 = 0x01;
const ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS: u8 = 0x02;
const ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION: u8 = 0x03;
const ERGOHAVEN_CUSTOM_NEXT_NATIVE_KEY_ACTION: u8 = 0x04;
const ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION: u8 = 0x05;
const ERGOHAVEN_CUSTOM_NEXT_NATIVE_DYNAMIC_ACTION: u8 = 0x06;
const ERGOHAVEN_CUSTOM_COMBO_LAYER: u8 = 0x07;
const ERGOHAVEN_NATIVE_KEY_ACTION_VERSION: u8 = 0x01;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_GET_SET: u16 = 0x0001;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_UNIVERSAL_SYMBOLS: u16 = 0x0002;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_RUSSIAN_LETTERS: u16 = 0x0004;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_COMBO_OUTPUT: u16 = 0x0008;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_MORSE_ACTIONS: u16 = 0x0010;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_COMBO_LAYER: u16 = 0x0020;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_VIAL_MACRO_EXT: u16 = 0x0040;
const ERGOHAVEN_NATIVE_KEY_ACTION_CAP_REPEAT_KEY: u16 = 0x0080;
const NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT: u8 = 0x00;
const NATIVE_DYNAMIC_ACTION_KIND_MORSE: u8 = 0x01;
const NATIVE_KEY_ACTION_STATUS_OK: u8 = 0x00;
const NATIVE_KEY_ACTION_STATUS_END: u8 = 0x01;
const NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION: u8 = 0x02;
const NATIVE_KEY_ACTION_STATUS_INVALID_POSITION: u8 = 0x03;
const NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD: u8 = 0x04;
const NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET: usize = 6;
const NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET: usize = 8;
const NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET: usize = 8;
const NATIVE_KEY_ACTION_MAX_PAYLOAD: usize = 32 - NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET;
const VIAL_MACRO_CHUNK_SIZE: usize = 28;
const VIAL_MACRO_COUNT: usize = 32;

const _: () = core::assert!(KeyAction::POSTCARD_MAX_SIZE <= NATIVE_KEY_ACTION_MAX_PAYLOAD);

fn process_host_data_packet(data: &[u8; 32]) -> bool {
    match data[0] {
        HOST_DATA_TIME => {
            crate::host_data::update_time(data[1], data[2]);
            true
        }
        HOST_DATA_LAYOUT => {
            crate::host_data::update_layout(data[1]);
            true
        }
        HOST_DATA_MEDIA_ARTIST => {
            crate::host_data::update_media_artist(host_data_text(data));
            true
        }
        HOST_DATA_MEDIA_TITLE => {
            crate::host_data::update_media_title(host_data_text(data));
            true
        }
        HOST_DATA_VOLUME => true,
        _ => false,
    }
}

fn host_data_text(data: &[u8; 32]) -> &str {
    let len = (data[1] as usize).min(30);
    let bytes = &data[2..2 + len];
    match core::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => core::str::from_utf8(&bytes[..err.valid_up_to()]).unwrap_or(""),
    }
}

fn battery_level_byte(status: rmk_types::battery::BatteryStatus) -> Option<u8> {
    match status {
        rmk_types::battery::BatteryStatus::Available { level: Some(level), .. } if level <= 100 => Some(level),
        _ => None,
    }
}

fn init_native_key_action_response(report: &mut ViaReport, subcommand: u8) {
    let command = report.output_data[0];
    report.input_data.fill(0);
    report.input_data[0] = command;
    report.input_data[1] = ERGOHAVEN_CUSTOM_NAMESPACE;
    report.input_data[2] = subcommand;
    report.input_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
}

fn native_key_position_valid(ctx: &KeyboardContext<'_>, layer: u8, row: u8, col: u8) -> bool {
    let (rows, cols, layers) = ctx.keymap_dimensions();
    (layer as usize) < layers && (row as usize) < rows && (col as usize) < cols
}

const fn native_key_action_capabilities() -> u16 {
    let capabilities = ERGOHAVEN_NATIVE_KEY_ACTION_CAP_GET_SET
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_COMBO_OUTPUT
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_MORSE_ACTIONS
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_COMBO_LAYER
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_VIAL_MACRO_EXT
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_REPEAT_KEY;
    #[cfg(feature = "universal_symbols")]
    let capabilities = capabilities
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_UNIVERSAL_SYMBOLS
        | ERGOHAVEN_NATIVE_KEY_ACTION_CAP_RUSSIAN_LETTERS;
    capabilities
}

fn native_dynamic_morse_pattern(field: u8) -> Option<MorsePattern> {
    match field {
        0 => Some(TAP),
        1 => Some(HOLD),
        2 => Some(DOUBLE_TAP),
        3 => Some(HOLD_AFTER_TAP),
        _ => None,
    }
}

fn native_dynamic_action_get(ctx: &KeyboardContext<'_>, kind: u8, index: u8, field: u8) -> Option<KeyAction> {
    match kind {
        NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT if field == 0 => ctx.with_combos(|combos| {
            combos
                .get(index as usize)
                .map(|combo| combo.as_ref().map_or(KeyAction::No, |combo| combo.config.output))
        }),
        NATIVE_DYNAMIC_ACTION_KIND_MORSE => {
            let pattern = native_dynamic_morse_pattern(field)?;
            ctx.get_morse(index)
                .map(|morse| morse.get(pattern).map_or(KeyAction::No, KeyAction::Single))
        }
        _ => None,
    }
}

async fn native_dynamic_action_set(
    ctx: &KeyboardContext<'_>,
    kind: u8,
    index: u8,
    field: u8,
    action: KeyAction,
) -> Result<(), ()> {
    match kind {
        NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT if field == 0 => {
            let Some(mut config) = ctx.with_combos(|combos| {
                combos.get(index as usize).map(|combo| {
                    combo
                        .as_ref()
                        .map_or_else(ComboConfig::empty, |combo| combo.config.clone())
                })
            }) else {
                return Err(());
            };
            config.output = action;
            ctx.set_combo(index, config).await;
            Ok(())
        }
        NATIVE_DYNAMIC_ACTION_KIND_MORSE => {
            let Some(pattern) = native_dynamic_morse_pattern(field) else {
                return Err(());
            };
            let action = match action {
                KeyAction::No => Action::No,
                KeyAction::Single(action) => action,
                _ => return Err(()),
            };
            let Some(mut updated) = ctx.get_morse(index) else {
                return Err(());
            };
            updated.put(pattern, action).map_err(|_| ())?;
            ctx.update_morse(index, |morse| *morse = updated).await;
            Ok(())
        }
        _ => Err(()),
    }
}

fn combo_layer_get(ctx: &KeyboardContext<'_>, index: u8) -> Option<Option<u8>> {
    ctx.with_combos(|combos| {
        combos
            .get(index as usize)
            .map(|combo| combo.as_ref().and_then(|combo| combo.config.layer))
    })
}

async fn combo_layer_set(ctx: &KeyboardContext<'_>, index: u8, layer: Option<u8>) -> Result<(), ()> {
    let Some(mut config) = ctx.with_combos(|combos| {
        combos.get(index as usize).map(|combo| {
            combo
                .as_ref()
                .map_or_else(ComboConfig::empty, |combo| combo.config.clone())
        })
    }) else {
        return Err(());
    };
    config.layer = layer;
    ctx.set_combo(index, config).await;
    Ok(())
}

fn native_dynamic_action_at_flat_index(ctx: &KeyboardContext<'_>, flat_index: usize) -> Option<KeyAction> {
    let combo_count = COMBO_MAX_NUM.min(u8::MAX as usize);
    let morse_count = MORSE_MAX_NUM.min(u8::MAX as usize);
    if flat_index < combo_count {
        return native_dynamic_action_get(ctx, NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT, flat_index as u8, 0);
    }
    let morse_flat = flat_index - combo_count;
    let morse_index = morse_flat / 4;
    let field = morse_flat % 4;
    if morse_index >= morse_count {
        return None;
    }
    native_dynamic_action_get(ctx, NATIVE_DYNAMIC_ACTION_KIND_MORSE, morse_index as u8, field as u8)
}

fn encode_native_key_action(report: &mut ViaReport, payload_offset: usize, action: KeyAction) -> bool {
    let mut encoded = [0u8; NATIVE_KEY_ACTION_MAX_PAYLOAD];
    let Ok(bytes) = postcard::to_slice(&action, &mut encoded) else {
        return false;
    };
    if payload_offset + bytes.len() > report.input_data.len() {
        return false;
    }
    report.input_data[payload_offset - 1] = bytes.len() as u8;
    report.input_data[payload_offset..payload_offset + bytes.len()].copy_from_slice(bytes);
    true
}

fn process_native_key_action_get(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION);
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let (layer, row, col) = (report.output_data[4], report.output_data[5], report.output_data[6]);
    if !native_key_position_valid(ctx, layer, row, col) {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
        return;
    }
    let action = ctx.get_action(layer, row, col);
    if !encode_native_key_action(report, NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET, action) {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
    }
}

async fn process_native_key_action_set(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION);
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let (layer, row, col) = (report.output_data[4], report.output_data[5], report.output_data[6]);
    if !native_key_position_valid(ctx, layer, row, col) {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
        return;
    }
    let payload_len = report.output_data[7] as usize;
    if payload_len == 0 || payload_len > NATIVE_KEY_ACTION_MAX_PAYLOAD {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
        return;
    }
    let payload =
        &report.output_data[NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET + payload_len];
    let Ok(action) = postcard::from_bytes::<KeyAction>(payload) else {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
        return;
    };
    ctx.set_action(layer, row, col, action).await;
}

fn process_next_native_key_action_get(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NEXT_NATIVE_KEY_ACTION);
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let start = LittleEndian::read_u16(&report.output_data[4..6]) as usize;
    let (rows, cols, layers) = ctx.keymap_dimensions();
    let total = rows.saturating_mul(cols).saturating_mul(layers);
    for flat_index in start..total.min(u16::MAX as usize) {
        let action = ctx.get_action_flat(flat_index);
        if action != KeyAction::No && to_via_keycode(action) == 0 {
            LittleEndian::write_u16(&mut report.input_data[5..7], flat_index as u16);
            if !encode_native_key_action(report, NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET, action) {
                report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
            }
            return;
        }
    }
    report.input_data[4] = NATIVE_KEY_ACTION_STATUS_END;
    LittleEndian::write_u16(&mut report.input_data[5..7], u16::MAX);
}

fn process_native_dynamic_action_get(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION);
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let (kind, index, field) = (report.output_data[4], report.output_data[5], report.output_data[6]);
    let Some(action) = native_dynamic_action_get(ctx, kind, index, field) else {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
        return;
    };
    if !encode_native_key_action(report, NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET, action) {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
    }
}

async fn process_native_dynamic_action_set(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION);
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let payload_len = report.output_data[7] as usize;
    if payload_len == 0 || payload_len > NATIVE_KEY_ACTION_MAX_PAYLOAD {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
        return;
    }
    let payload =
        &report.output_data[NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET + payload_len];
    let Ok(action) = postcard::from_bytes::<KeyAction>(payload) else {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
        return;
    };
    let (kind, index, field) = (report.output_data[4], report.output_data[5], report.output_data[6]);
    if native_dynamic_action_set(ctx, kind, index, field, action)
        .await
        .is_err()
    {
        report.input_data[4] = if native_dynamic_action_get(ctx, kind, index, field).is_some() {
            NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD
        } else {
            NATIVE_KEY_ACTION_STATUS_INVALID_POSITION
        };
    }
}

fn process_next_native_dynamic_action_get(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NEXT_NATIVE_DYNAMIC_ACTION);
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let start = LittleEndian::read_u16(&report.output_data[4..6]) as usize;
    let combo_count = COMBO_MAX_NUM.min(u8::MAX as usize);
    let morse_count = MORSE_MAX_NUM.min(u8::MAX as usize);
    let total = combo_count.saturating_add(morse_count.saturating_mul(4));
    for flat_index in start..total.min(u16::MAX as usize) {
        let Some(action) = native_dynamic_action_at_flat_index(ctx, flat_index) else {
            continue;
        };
        if action != KeyAction::No && to_via_keycode(action) == 0 {
            LittleEndian::write_u16(&mut report.input_data[5..7], flat_index as u16);
            if !encode_native_key_action(report, NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET, action) {
                report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
            }
            return;
        }
    }
    report.input_data[4] = NATIVE_KEY_ACTION_STATUS_END;
    LittleEndian::write_u16(&mut report.input_data[5..7], u16::MAX);
}

fn process_combo_layer_get(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_COMBO_LAYER);
    report.input_data[7] = report.output_data[4];
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let index = report.output_data[4];
    let Some(layer) = combo_layer_get(ctx, index) else {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
        return;
    };
    report.input_data[5] = u8::from(layer.is_some());
    report.input_data[6] = layer.unwrap_or(0);
}

async fn process_combo_layer_set(report: &mut ViaReport, ctx: &KeyboardContext<'_>) {
    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_COMBO_LAYER);
    report.input_data[7] = report.output_data[4];
    if report.output_data[3] != ERGOHAVEN_NATIVE_KEY_ACTION_VERSION {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_UNSUPPORTED_VERSION;
        return;
    }
    let index = report.output_data[4];
    if combo_layer_get(ctx, index).is_none() {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
        return;
    }
    let layer = match report.output_data[5] {
        0 => None,
        1 => Some(report.output_data[6]),
        _ => {
            report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD;
            return;
        }
    };
    if layer.is_some_and(|layer| layer as usize >= ctx.keymap_dimensions().2) {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
        return;
    }
    if combo_layer_set(ctx, index, layer).await.is_err() {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
    }
}

fn battery_halves_for_split(
    central: BatteryStatus,
    peripheral_0: BatteryStatus,
    peripheral_1: BatteryStatus,
    peripheral_count: usize,
    central_is_left: bool,
) -> (BatteryStatus, BatteryStatus) {
    if peripheral_count == 1 {
        if central_is_left {
            (central, peripheral_0)
        } else {
            (peripheral_0, central)
        }
    } else {
        (peripheral_0, peripheral_1)
    }
}

pub struct VialService<'a> {
    ctx: &'a KeyboardContext<'a>,
    vial_config: VialConfig<'static>,
    #[cfg(feature = "vial_lock")]
    locker: vial_lock::VialLock<'a>,
}

impl<'a> VialService<'a> {
    pub fn new(ctx: &'a KeyboardContext<'a>, config: &RmkConfig<'static>) -> Self {
        Self {
            ctx,
            vial_config: config.vial_config,
            #[cfg(feature = "vial_lock")]
            locker: vial_lock::VialLock::new(
                config.vial_config.unlock_keys,
                ctx.keymap,
                config.vial_config.vial_insecure,
            ),
        }
    }

    async fn process_via_packet(&mut self, report: &mut ViaReport) {
        let command_id = report.output_data[0];

        // Caller pre-fills `input_data` from `output_data`, so individual arms
        // only need to overwrite the bytes they actually change.
        match command_id.into() {
            ViaCommand::GetProtocolVersion => {
                BigEndian::write_u16(&mut report.input_data[1..3], VIA_PROTOCOL_VERSION);
            }
            ViaCommand::GetKeyboardValue => {
                // Check the second u8
                match report.output_data[1].try_into() {
                    Ok(v) => match v {
                        ViaKeyboardInfo::Uptime => {
                            let value = Instant::now().as_millis() as u32;
                            BigEndian::write_u32(&mut report.input_data[2..6], value);
                        }
                        ViaKeyboardInfo::LayoutOptions => {
                            let layout_option = self.ctx.layout_options().await;
                            BigEndian::write_u32(&mut report.input_data[2..6], layout_option);
                        }
                        #[cfg(not(feature = "vial_lock"))]
                        ViaKeyboardInfo::SwitchMatrixState => {
                            error!("It is not secure to use matrix tester without vial lock");
                        }
                        #[cfg(feature = "vial_lock")]
                        ViaKeyboardInfo::SwitchMatrixState if self.locker.is_unlocked() => {
                            self.ctx.read_matrix_state(&mut report.input_data[2..]);
                        }
                        ViaKeyboardInfo::FirmwareVersion => {
                            BigEndian::write_u32(&mut report.input_data[2..6], self.vial_config.firmware_version);
                        }
                        _ => (),
                    },
                    Err(e) => error!("Invalid subcommand: {} of GetKeyboardValue", e),
                }
            }
            ViaCommand::SetKeyboardValue => {
                // Check the second u8
                match report.output_data[1].try_into() {
                    Ok(v) => match v {
                        ViaKeyboardInfo::LayoutOptions => {
                            let layout_option = BigEndian::read_u32(&report.output_data[2..6]);
                            self.ctx.set_layout_options(layout_option).await;
                        }
                        ViaKeyboardInfo::DeviceIndication => {
                            let _device_indication = report.output_data[2];
                            warn!("SetKeyboardValue - DeviceIndication")
                        }
                        _ => (),
                    },
                    Err(e) => error!("Invalid subcommand: {} of GetKeyboardValue", e),
                }
            }
            ViaCommand::DynamicKeymapGetKeyCode => {
                let layer = report.output_data[1];
                let row = report.output_data[2];
                let col = report.output_data[3];
                let action = self.ctx.get_action(layer, row, col);
                let keycode = to_via_keycode(action);
                info!("Getting keycode: {:02X} at ({},{}), layer {}", keycode, row, col, layer);
                BigEndian::write_u16(&mut report.input_data[4..6], keycode);
            }
            ViaCommand::DynamicKeymapSetKeyCode => {
                let layer = report.output_data[1];
                let row = report.output_data[2];
                let col = report.output_data[3];
                let keycode = BigEndian::read_u16(&report.output_data[4..6]);
                let action = from_via_keycode(keycode);
                info!(
                    "Setting keycode: 0x{:02X} at ({},{}), layer {} as {:?}",
                    keycode, row, col, layer, action
                );
                self.ctx.set_action(layer, row, col, action).await;
            }
            ViaCommand::DynamicKeymapReset => {
                warn!("Dynamic keymap reset -- not supported")
            }
            ViaCommand::CustomSetValue => {
                if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION
                {
                    process_native_key_action_set(report, self.ctx).await;
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION
                {
                    process_native_dynamic_action_set(report, self.ctx).await;
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_COMBO_LAYER
                {
                    process_combo_layer_set(report, self.ctx).await;
                } else {
                    // backlight/rgblight/rgb matrix/led matrix/audio settings here
                    warn!("Custom set value -- not supported")
                }
            }
            ViaCommand::CustomGetValue => {
                if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_BATTERY_HALVES
                {
                    #[cfg(all(feature = "split", feature = "_ble"))]
                    crate::event::publish_event(crate::event::PeripheralBatteryRefreshEvent);

                    report.input_data[3] = ERGOHAVEN_BATTERY_HALVES_VERSION;
                    report.input_data[4] = 0;
                    report.input_data[5] = 0xFF;
                    report.input_data[6] = 0xFF;

                    #[cfg(all(feature = "split", feature = "_ble"))]
                    {
                        let (left, right) = battery_halves_for_split(
                            self.ctx.battery_status(),
                            self.ctx.peripheral_battery_status(0),
                            self.ctx.peripheral_battery_status(1),
                            crate::SPLIT_PERIPHERALS_NUM,
                            crate::SPLIT_CENTRAL_IS_LEFT,
                        );
                        if let Some(level) = battery_level_byte(left) {
                            report.input_data[4] |= 0x01;
                            report.input_data[5] = level;
                        }
                        if let Some(level) = battery_level_byte(right) {
                            report.input_data[4] |= 0x02;
                            report.input_data[6] = level;
                        }
                    }
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS
                {
                    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS);
                    LittleEndian::write_u16(&mut report.input_data[4..6], native_key_action_capabilities());
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION
                {
                    process_native_key_action_get(report, self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NEXT_NATIVE_KEY_ACTION
                {
                    process_next_native_key_action_get(report, self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION
                {
                    process_native_dynamic_action_get(report, self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NEXT_NATIVE_DYNAMIC_ACTION
                {
                    process_next_native_dynamic_action_get(report, self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_COMBO_LAYER
                {
                    process_combo_layer_get(report, self.ctx);
                } else {
                    // backlight/rgblight/rgb matrix/led matrix/audio settings here
                    warn!("Custom get value -- not supported")
                }
            }
            ViaCommand::CustomSave => {
                // backlight/rgblight/rgb matrix/led matrix/audio settings here
                warn!("Custom get value -- not supported")
            }
            ViaCommand::EepromReset => {
                warn!("Resetting storage..");
                self.ctx.reset_storage().await;
                // TODO: Reboot after a eeprom reset?
            }
            // Match Vial QMK: a locked board must not reboot into a bootloader
            // that accepts any firmware from the host.
            #[cfg(feature = "vial_lock")]
            ViaCommand::BootloaderJump if !self.locker.is_unlocked() => {
                warn!("Bootloader jump ignored while Vial is locked");
            }
            ViaCommand::BootloaderJump => {
                warn!("Bootloader jumping");
                boot::jump_to_bootloader();
            }
            ViaCommand::DynamicKeymapMacroGetCount => {
                report.input_data[1] = VIAL_MACRO_COUNT as u8;
                warn!("Macro get count -- to be implemented")
            }
            ViaCommand::DynamicKeymapMacroGetBufferSize => {
                report.input_data[1] = (MACRO_SPACE_SIZE as u16 >> 8) as u8;
                report.input_data[2] = (MACRO_SPACE_SIZE & 0xFF) as u8;
            }
            ViaCommand::DynamicKeymapMacroGetBuffer => {
                let offset = BigEndian::read_u16(&report.output_data[1..3]) as usize;
                let size = report.output_data[3] as usize;
                if size <= VIAL_MACRO_CHUNK_SIZE {
                    self.ctx.read_macro_buffer(offset, &mut report.input_data[4..4 + size]);
                    debug!("Get macro buffer: offset: {}, data: {:?}", offset, report.input_data);
                } else {
                    report.input_data[0] = 0xFF;
                }
            }
            ViaCommand::DynamicKeymapMacroSetBuffer => {
                // Every write writes all buffer space of the macro(if it's not empty)
                let offset = BigEndian::read_u16(&report.output_data[1..3]);
                // Current sequence size, <= 28
                let size = report.output_data[3];
                // `output_data` is 32 bytes, so the payload slice output_data[4..4 + size]
                // is only valid for size <= 28. Reject oversized writes instead of
                // panicking, mirroring the DynamicKeymapMacroGetBuffer handler above.
                if size as usize <= VIAL_MACRO_CHUNK_SIZE {
                    // End of current sequence in the macro cache
                    // The first sequence, reset the macro cache
                    if offset == 0 {
                        self.ctx.reset_macro_buffer();
                    }

                    info!("Setting macro buffer, offset: {}, size: {}", offset, size);
                    let transfer_complete = self.ctx.write_macro_buffer(
                        offset as usize,
                        &report.output_data[4..4 + size as usize],
                        VIAL_MACRO_COUNT,
                    );
                    info!("Macro transfer complete: {}", transfer_complete);
                } else {
                    report.input_data[0] = 0xFF;
                }
            }
            ViaCommand::DynamicKeymapMacroReset => {
                warn!("Macro reset -- to be implemented")
            }
            ViaCommand::DynamicKeymapGetLayerCount => {
                report.input_data[1] = self.ctx.keymap_dimensions().2 as u8;
            }
            ViaCommand::DynamicKeymapGetBuffer => {
                let offset = BigEndian::read_u16(&report.output_data[1..3]);
                // size <= 28
                let size = report.output_data[3];
                debug!("Getting keymap buffer, offset: {}, size: {}", offset, size);
                let mut idx = 4;
                let start = (offset / 2) as usize;
                let count = (size / 2) as usize;
                for i in 0..count {
                    let a = self.ctx.get_action_flat(start + i);
                    let kc = to_via_keycode(a);
                    BigEndian::write_u16(&mut report.input_data[idx..idx + 2], kc);
                    idx += 2;
                }
            }
            ViaCommand::DynamicKeymapSetBuffer => {
                debug!("Dynamic keymap set buffer");
                let offset = BigEndian::read_u16(&report.output_data[1..3]);
                // size <= 28
                let size = report.output_data[3];
                let mut idx = 4;
                let (rows, cols, _) = self.ctx.keymap_dimensions();
                for i in 0..(size as usize) {
                    let via_keycode = LittleEndian::read_u16(&report.output_data[idx..idx + 2]);
                    let action = from_via_keycode(via_keycode);
                    let flat_index = offset as usize + i;
                    self.ctx.try_set_action_flat(flat_index, action, rows, cols);
                    idx += 2;
                }
            }
            ViaCommand::DynamicKeymapGetEncoder => {
                warn!("Keymap get encoder -- not supported");
            }
            ViaCommand::DynamicKeymapSetEncoder => {
                warn!("Keymap set encoder -- not supported");
            }
            ViaCommand::Vial => {
                process_vial(
                    report,
                    &self.vial_config,
                    #[cfg(feature = "vial_lock")]
                    &mut self.locker,
                    self.ctx,
                )
                .await
            }
            ViaCommand::Unhandled => {
                info!("Unknown cmd: {:?}", report.output_data);
                report.input_data[0] = ViaCommand::Unhandled as u8
            }
        }
    }
}

impl Runnable for VialService<'_> {
    async fn run(&mut self) -> ! {
        loop {
            let (transport, output_data) = HOST_REQUEST_CHANNEL.receive().await;
            if process_host_data_packet(&output_data) {
                continue;
            }
            let mut report = ViaReport {
                input_data: output_data,
                output_data,
            };
            self.process_via_packet(&mut report).await;
            try_send_host_reply(transport, report.input_data);
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "storage")]
    use std::sync::{Mutex, OnceLock};

    use embassy_futures::block_on;
    use rmk_types::action::{Action, KeyAction};
    use rmk_types::battery::ChargeState;
    use rmk_types::keycode::{HidKeyCode, KeyCode};
    use rmk_types::modifier::ModifierCombination;

    use super::*;
    use crate::config::{BehaviorConfig, PositionalConfig};
    use crate::keymap::{KeyMap, KeymapData};

    #[cfg(feature = "storage")]
    fn macro_signal_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Build a minimal 1x1x1 keymap + `VialService` and run `f` against it.
    fn with_service<R>(f: impl FnOnce(&mut VialService) -> R) -> R {
        let mut data = KeymapData::new([[[KeyAction::No]]]);
        let mut behavior = BehaviorConfig::default();
        let positional = PositionalConfig::<1, 1>::default();
        let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
        let ctx = KeyboardContext::new(&keymap);
        let config = RmkConfig::default();
        let mut service = VialService::new(&ctx, &config);
        f(&mut service)
    }

    /// A `DynamicKeymapMacroSetBuffer` (0x0F) report with `offset = 0` and the
    /// given payload `size` byte. The caller mirrors `Runnable::run` by seeding
    /// `input_data` with a copy of `output_data`.
    fn macro_set_buffer_report_at(offset: u16, size: u8) -> ViaReport {
        let mut output_data = [0u8; 32];
        output_data[0] = 0x0F; // DynamicKeymapMacroSetBuffer
        output_data[1..3].copy_from_slice(&offset.to_be_bytes());
        output_data[3] = size;
        ViaReport {
            input_data: output_data,
            output_data,
        }
    }

    fn macro_set_buffer_report(size: u8) -> ViaReport {
        macro_set_buffer_report_at(0, size)
    }

    fn custom_report(command: ViaCommand, subcommand: u8) -> ViaReport {
        let mut output_data = [0u8; 32];
        output_data[0] = command as u8;
        output_data[1] = ERGOHAVEN_CUSTOM_NAMESPACE;
        output_data[2] = subcommand;
        ViaReport {
            input_data: output_data,
            output_data,
        }
    }

    fn native_dynamic_action_report(command: ViaCommand, kind: u8, index: u8, field: u8) -> ViaReport {
        let mut report = custom_report(command, ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION);
        report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
        report.output_data[4] = kind;
        report.output_data[5] = index;
        report.output_data[6] = field;
        report
    }

    fn native_dynamic_action_set_report(kind: u8, index: u8, field: u8, action: KeyAction) -> ViaReport {
        let mut report = native_dynamic_action_report(ViaCommand::CustomSetValue, kind, index, field);
        let mut encoded = [0u8; NATIVE_KEY_ACTION_MAX_PAYLOAD];
        let payload = postcard::to_slice(&action, &mut encoded).unwrap();
        report.output_data[7] = payload.len() as u8;
        report.output_data[NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET + payload.len()]
            .copy_from_slice(payload);
        report
    }

    fn combo_layer_report(command: ViaCommand, index: u8) -> ViaReport {
        let mut report = custom_report(command, ERGOHAVEN_CUSTOM_COMBO_LAYER);
        report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
        report.output_data[4] = index;
        report
    }

    fn combo_layer_set_report(index: u8, layer: Option<u8>) -> ViaReport {
        let mut report = combo_layer_report(ViaCommand::CustomSetValue, index);
        report.output_data[5] = u8::from(layer.is_some());
        report.output_data[6] = layer.unwrap_or(0);
        report
    }

    fn decode_combo_layer_response(report: &ViaReport) -> Option<u8> {
        (report.input_data[5] != 0).then_some(report.input_data[6])
    }

    fn decode_native_action_response(report: &ViaReport, payload_offset: usize) -> KeyAction {
        let len = report.input_data[payload_offset - 1] as usize;
        postcard::from_bytes(&report.input_data[payload_offset..payload_offset + len]).unwrap()
    }

    fn rich_mod_tap() -> KeyAction {
        KeyAction::TapHold(
            Action::KeyWithModifier(HidKeyCode::Kc0, ModifierCombination::LSHIFT),
            Action::Modifier(ModifierCombination::LCTRL),
            Default::default(),
        )
    }

    #[test]
    fn k04_micro_factory_mod_actions_keep_their_lossless_transport_path() {
        let standard_left_shift = KeyAction::TapHold(
            Action::Key(KeyCode::Hid(HidKeyCode::Minus)),
            Action::Modifier(ModifierCombination::LSHIFT),
            Default::default(),
        );
        let rich_left_ctrl = rich_mod_tap();
        let shifted_equal = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Equal, ModifierCombination::LSHIFT));
        let shifted_five = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Kc5, ModifierCombination::LSHIFT));
        let rich_right_ctrl = KeyAction::TapHold(
            Action::KeyWithModifier(HidKeyCode::LeftBracket, ModifierCombination::LSHIFT),
            Action::Modifier(ModifierCombination::RCTRL),
            Default::default(),
        );
        let standard_right_shift = KeyAction::TapHold(
            Action::Key(KeyCode::Hid(HidKeyCode::Semicolon)),
            Action::Modifier(ModifierCombination::RSHIFT),
            Default::default(),
        );

        assert_eq!(to_via_keycode(standard_left_shift), 0x222D);
        assert_eq!(to_via_keycode(rich_left_ctrl), 0);
        assert_eq!(to_via_keycode(shifted_equal), 0x022E);
        assert_eq!(to_via_keycode(shifted_five), 0x0222);
        assert_eq!(to_via_keycode(rich_right_ctrl), 0);
        assert_eq!(to_via_keycode(standard_right_shift), 0x3233);
    }

    #[test]
    fn reports_the_configured_runtime_firmware_version() {
        let mut data = KeymapData::new([[[KeyAction::No]]]);
        let mut behavior = BehaviorConfig::default();
        let positional = PositionalConfig::<1, 1>::default();
        let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
        let ctx = KeyboardContext::new(&keymap);
        let config = RmkConfig {
            vial_config: VialConfig {
                firmware_version: 0x0000_0103,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut service = VialService::new(&ctx, &config);
        let mut output_data = [0u8; 32];
        output_data[0] = ViaCommand::GetKeyboardValue as u8;
        output_data[1] = ViaKeyboardInfo::FirmwareVersion as u8;
        let mut report = ViaReport {
            input_data: output_data,
            output_data,
        };

        block_on(service.process_via_packet(&mut report));

        assert_eq!(&report.input_data[2..6], &[0x00, 0x00, 0x01, 0x03]);
    }

    #[cfg(feature = "vial_lock")]
    #[test]
    fn bootloader_jump_requires_vial_unlock() {
        use core::sync::atomic::Ordering;

        use crate::boot::BOOTLOADER_JUMPS;

        let jump = |vial_insecure: bool| {
            let mut data = KeymapData::new([[[KeyAction::No]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
            let ctx = KeyboardContext::new(&keymap);
            let config = RmkConfig {
                vial_config: VialConfig {
                    vial_insecure,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut service = VialService::new(&ctx, &config);
            let mut output_data = [0u8; 32];
            output_data[0] = ViaCommand::BootloaderJump as u8;
            let mut report = ViaReport {
                input_data: output_data,
                output_data,
            };
            let before = BOOTLOADER_JUMPS.load(Ordering::Relaxed);
            block_on(service.process_via_packet(&mut report));
            BOOTLOADER_JUMPS.load(Ordering::Relaxed) - before
        };

        assert_eq!(jump(false), 0, "a locked board must not jump to the bootloader");
        assert_eq!(jump(true), 1, "an unlocked board still jumps to the bootloader");
    }

    // `output_data` is [u8; 32], so the handler slices `output_data[4..4 + size]`.
    // size == 28 is the largest payload that fits (writes output_data[4..32]).
    #[test]
    fn macro_set_buffer_max_size_ok() {
        #[cfg(feature = "storage")]
        let _guard = macro_signal_test_lock().lock().unwrap();
        #[cfg(feature = "storage")]
        crate::channel::MACRO_FLASH_SIGNAL.reset();

        with_service(|service| {
            let mut report = macro_set_buffer_report(28);
            block_on(service.process_via_packet(&mut report));
        });

        #[cfg(feature = "storage")]
        crate::channel::MACRO_FLASH_SIGNAL.reset();
    }

    #[cfg(feature = "storage")]
    #[test]
    fn macro_chunks_queue_only_the_completed_snapshot() {
        let _guard = macro_signal_test_lock().lock().unwrap();
        crate::channel::MACRO_FLASH_SIGNAL.reset();

        with_service(|service| {
            let mut first = macro_set_buffer_report(28);
            first.output_data[4..8].fill(0x11);
            first.output_data[8..32].fill(0);
            first.input_data = first.output_data;
            block_on(service.process_via_packet(&mut first));
            assert!(crate::channel::MACRO_FLASH_SIGNAL.try_take().is_none());

            let mut second = macro_set_buffer_report_at(28, 8);
            second.output_data[4..12].fill(0);
            second.input_data = second.output_data;
            block_on(service.process_via_packet(&mut second));

            let snapshot = crate::channel::MACRO_FLASH_SIGNAL
                .try_take()
                .expect("latest macro snapshot");
            assert_eq!(&snapshot[..4], &[0x11; 4]);
            assert!(snapshot[4..].iter().all(|byte| *byte == 0));
        });

        crate::channel::MACRO_FLASH_SIGNAL.reset();
    }

    #[cfg(feature = "storage")]
    #[test]
    fn incomplete_macro_chunk_does_not_copy_a_flash_snapshot() {
        let _guard = macro_signal_test_lock().lock().unwrap();
        crate::channel::MACRO_FLASH_SIGNAL.reset();

        with_service(|service| {
            let mut report = macro_set_buffer_report(28);
            report.output_data[4..32].fill(0x33);
            report.input_data = report.output_data;
            block_on(service.process_via_packet(&mut report));

            assert!(crate::channel::MACRO_FLASH_SIGNAL.try_take().is_none());
        });

        crate::channel::MACRO_FLASH_SIGNAL.reset();
    }

    #[cfg(feature = "storage")]
    #[test]
    fn macro_chunk_reaching_buffer_end_commits() {
        let _guard = macro_signal_test_lock().lock().unwrap();
        crate::channel::MACRO_FLASH_SIGNAL.reset();

        with_service(|service| {
            let offset = (MACRO_SPACE_SIZE - VIAL_MACRO_CHUNK_SIZE) as u16;
            let mut report = macro_set_buffer_report_at(offset, VIAL_MACRO_CHUNK_SIZE as u8);
            report.output_data[4..32].fill(0x44);
            report.input_data = report.output_data;
            block_on(service.process_via_packet(&mut report));

            let snapshot = crate::channel::MACRO_FLASH_SIGNAL
                .try_take()
                .expect("completed macro snapshot");
            assert_eq!(&snapshot[MACRO_SPACE_SIZE - VIAL_MACRO_CHUNK_SIZE..], &[0x44; 28]);
        });

        crate::channel::MACRO_FLASH_SIGNAL.reset();
    }

    // size == 29 slices output_data[4..33], which is out of bounds. The sibling
    // DynamicKeymapMacroGetBuffer handler already rejects size > 28 with 0xFF;
    // SetBuffer must do the same instead of panicking.
    #[test]
    fn macro_set_buffer_oversize_rejected() {
        with_service(|service| {
            let mut report = macro_set_buffer_report(29);
            block_on(service.process_via_packet(&mut report));
            assert_eq!(report.input_data[0], 0xFF);
        });
    }

    #[test]
    fn native_key_action_capability_is_versioned() {
        with_service(|service| {
            let mut report = custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS);
            block_on(service.process_via_packet(&mut report));
            assert_eq!(report.input_data[3], ERGOHAVEN_NATIVE_KEY_ACTION_VERSION);
            assert_eq!(
                LittleEndian::read_u16(&report.input_data[4..6]),
                native_key_action_capabilities()
            );
            assert_ne!(
                native_key_action_capabilities() & ERGOHAVEN_NATIVE_KEY_ACTION_CAP_VIAL_MACRO_EXT,
                0
            );
            assert_ne!(
                native_key_action_capabilities() & ERGOHAVEN_NATIVE_KEY_ACTION_CAP_REPEAT_KEY,
                0
            );
        });
    }

    #[test]
    fn native_key_action_set_and_get_round_trip() {
        with_service(|service| {
            let action = rich_mod_tap();
            let mut encoded = [0u8; NATIVE_KEY_ACTION_MAX_PAYLOAD];
            let payload = postcard::to_slice(&action, &mut encoded).unwrap();
            let mut set_report = custom_report(ViaCommand::CustomSetValue, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION);
            set_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            set_report.output_data[7] = payload.len() as u8;
            set_report.output_data
                [NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET + payload.len()]
                .copy_from_slice(payload);
            block_on(service.process_via_packet(&mut set_report));
            assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut get_report = custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION);
            get_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            block_on(service.process_via_packet(&mut get_report));
            assert_eq!(get_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            let len = get_report.input_data[5] as usize;
            let decoded: KeyAction = postcard::from_bytes(
                &get_report.input_data
                    [NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET + len],
            )
            .unwrap();
            assert_eq!(decoded, action);
        });
    }

    #[test]
    fn native_key_action_scan_returns_only_vial_lossy_actions() {
        with_service(|service| {
            let action = rich_mod_tap();
            let mut encoded = [0u8; NATIVE_KEY_ACTION_MAX_PAYLOAD];
            let payload = postcard::to_slice(&action, &mut encoded).unwrap();
            let mut set_report = custom_report(ViaCommand::CustomSetValue, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION);
            set_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            set_report.output_data[7] = payload.len() as u8;
            set_report.output_data
                [NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_SET_PAYLOAD_OFFSET + payload.len()]
                .copy_from_slice(payload);
            block_on(service.process_via_packet(&mut set_report));

            let mut scan_report = custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NEXT_NATIVE_KEY_ACTION);
            scan_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            block_on(service.process_via_packet(&mut scan_report));
            assert_eq!(scan_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(LittleEndian::read_u16(&scan_report.input_data[5..7]), 0);
            let len = scan_report.input_data[7] as usize;
            let decoded: KeyAction = postcard::from_bytes(
                &scan_report.input_data
                    [NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET..NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET + len],
            )
            .unwrap();
            assert_eq!(decoded, action);

            LittleEndian::write_u16(&mut scan_report.output_data[4..6], 1);
            block_on(service.process_via_packet(&mut scan_report));
            assert_eq!(scan_report.input_data[4], NATIVE_KEY_ACTION_STATUS_END);
        });
    }

    #[test]
    fn native_dynamic_combo_output_set_and_get_round_trip() {
        with_service(|service| {
            let action = KeyAction::Single(Action::User(0x80));
            let mut set_report =
                native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT, 0, 0, action);
            block_on(service.process_via_packet(&mut set_report));
            assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut get_report = native_dynamic_action_report(
                ViaCommand::CustomGetValue,
                NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT,
                0,
                0,
            );
            block_on(service.process_via_packet(&mut get_report));
            assert_eq!(get_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(
                decode_native_action_response(&get_report, NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET),
                action
            );
        });
    }

    #[test]
    fn combo_layer_set_and_get_round_trip() {
        with_service(|service| {
            let mut set_report = combo_layer_set_report(0, Some(0));
            block_on(service.process_via_packet(&mut set_report));
            assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut get_report = combo_layer_report(ViaCommand::CustomGetValue, 0);
            block_on(service.process_via_packet(&mut get_report));
            assert_eq!(get_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(decode_combo_layer_response(&get_report), Some(0));

            let mut clear_report = combo_layer_set_report(0, None);
            block_on(service.process_via_packet(&mut clear_report));
            assert_eq!(clear_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            block_on(service.process_via_packet(&mut get_report));
            assert_eq!(decode_combo_layer_response(&get_report), None);
        });
    }

    #[test]
    fn combo_layer_rejects_out_of_range_layer() {
        with_service(|service| {
            let mut report = combo_layer_set_report(0, Some(1));
            block_on(service.process_via_packet(&mut report));
            assert_eq!(report.input_data[4], NATIVE_KEY_ACTION_STATUS_INVALID_POSITION);
        });
    }

    #[test]
    fn standard_vial_combo_write_preserves_native_layer() {
        with_service(|service| {
            let mut set_layer = combo_layer_set_report(0, Some(0));
            block_on(service.process_via_packet(&mut set_layer));
            assert_eq!(set_layer.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut output_data = [0u8; 32];
            output_data[0] = ViaCommand::Vial as u8;
            output_data[1] = rmk_types::protocol::vial::VialCommand::DynamicEntryOp as u8;
            output_data[2] = rmk_types::protocol::vial::VialDynamic::DynamicVialComboSet as u8;
            LittleEndian::write_u16(&mut output_data[4..6], 0x0004);
            LittleEndian::write_u16(&mut output_data[6..8], 0x0005);
            LittleEndian::write_u16(&mut output_data[12..14], 0x0006);
            let mut vial_set = ViaReport {
                input_data: output_data,
                output_data,
            };
            block_on(service.process_via_packet(&mut vial_set));

            let mut get_layer = combo_layer_report(ViaCommand::CustomGetValue, 0);
            block_on(service.process_via_packet(&mut get_layer));
            assert_eq!(get_layer.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(decode_combo_layer_response(&get_layer), Some(0));
        });
    }

    #[test]
    fn native_dynamic_morse_actions_set_and_get_round_trip() {
        with_service(|service| {
            for field in 0..4 {
                let action = KeyAction::Single(Action::User(0x80 + field));
                let mut set_report =
                    native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_MORSE, 0, field, action);
                block_on(service.process_via_packet(&mut set_report));
                assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

                let mut get_report = native_dynamic_action_report(
                    ViaCommand::CustomGetValue,
                    NATIVE_DYNAMIC_ACTION_KIND_MORSE,
                    0,
                    field,
                );
                block_on(service.process_via_packet(&mut get_report));
                assert_eq!(get_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
                assert_eq!(
                    decode_native_action_response(&get_report, NATIVE_KEY_ACTION_GET_PAYLOAD_OFFSET),
                    action
                );
            }
        });
    }

    #[test]
    fn native_dynamic_action_scan_uses_combo_then_morse_cursor_space() {
        with_service(|service| {
            let combo_action = KeyAction::Single(Action::User(0x80));
            let mut set_combo =
                native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT, 0, 0, combo_action);
            block_on(service.process_via_packet(&mut set_combo));

            let morse_action = KeyAction::Single(Action::User(0x81));
            let mut set_morse = native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_MORSE, 0, 2, morse_action);
            block_on(service.process_via_packet(&mut set_morse));

            let mut scan_report =
                custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NEXT_NATIVE_DYNAMIC_ACTION);
            scan_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            block_on(service.process_via_packet(&mut scan_report));
            assert_eq!(scan_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(LittleEndian::read_u16(&scan_report.input_data[5..7]), 0);
            assert_eq!(
                decode_native_action_response(&scan_report, NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET),
                combo_action
            );

            let combo_count = COMBO_MAX_NUM.min(u8::MAX as usize);
            LittleEndian::write_u16(&mut scan_report.output_data[4..6], combo_count as u16);
            block_on(service.process_via_packet(&mut scan_report));
            assert_eq!(scan_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(
                LittleEndian::read_u16(&scan_report.input_data[5..7]),
                (combo_count + 2) as u16
            );
            assert_eq!(
                decode_native_action_response(&scan_report, NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET),
                morse_action
            );
        });
    }

    #[test]
    fn native_dynamic_morse_rejects_composite_key_actions() {
        with_service(|service| {
            let mut report = native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_MORSE, 0, 0, rich_mod_tap());
            block_on(service.process_via_packet(&mut report));
            assert_eq!(report.input_data[4], NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD);
        });
    }

    fn battery(level: u8) -> BatteryStatus {
        BatteryStatus::Available {
            charge_state: ChargeState::Unknown,
            level: Some(level),
        }
    }

    #[test]
    fn no_qube_split_uses_central_and_first_peripheral_batteries() {
        assert_eq!(
            battery_halves_for_split(battery(80), battery(55), BatteryStatus::Unavailable, 1, true,),
            (battery(80), battery(55))
        );
    }

    #[test]
    fn right_central_split_reports_physical_battery_order() {
        assert_eq!(
            battery_halves_for_split(battery(80), battery(55), BatteryStatus::Unavailable, 1, false,),
            (battery(55), battery(80))
        );
    }

    #[test]
    fn qube_split_uses_both_peripheral_batteries() {
        assert_eq!(
            battery_halves_for_split(battery(100), battery(80), battery(55), 2, true),
            (battery(80), battery(55))
        );
    }
}
