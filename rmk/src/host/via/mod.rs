use byteorder::{BigEndian, ByteOrder, LittleEndian};
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use postcard::experimental::max_size::MaxSize;
use rmk_types::action::{Action, KeyAction};
use rmk_types::combo::Combo as ComboConfig;
use rmk_types::constants::{COMBO_MAX_NUM, MORSE_MAX_NUM};
use rmk_types::morse::{DOUBLE_TAP, HOLD, HOLD_AFTER_TAP, MorsePattern, TAP};
use rmk_types::protocol::vial::{VIA_PROTOCOL_VERSION, ViaCommand, ViaKeyboardInfo};
use vial::process_vial;

use crate::config::{RmkConfig, VialConfig};
use crate::hid::ViaReport;
use crate::host::context::KeyboardContext;
use crate::host::via::keycode_convert::{from_via_keycode, to_via_keycode};
use crate::keymap::KeyMap;
use crate::{MACRO_MAX_NUM, MACRO_SPACE_SIZE, boot};

pub(crate) mod keycode_convert;
mod vial;

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
// v2: postcard layout of `KeyAction` follows upstream rmk-types
// (`KeyboardAction` variants dropped, `TapHold` carries a profile index).
const ERGOHAVEN_NATIVE_KEY_ACTION_VERSION: u8 = 0x02;
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
    crate::host_data::valid_text(&data[2..2 + len])
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
            match ctx.set_combo(index, config).await {
                Ok(true) => Ok(()),
                _ => Err(()),
            }
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
            ctx.update_morse(index, |morse| *morse = updated).await
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
    match ctx.set_combo(index, config).await {
        Ok(true) => Ok(()),
        _ => Err(()),
    }
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
    if ctx.stage_action(layer, row, col, action).await.is_err() {
        report.input_data[4] = NATIVE_KEY_ACTION_STATUS_INVALID_POSITION;
    }
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

pub struct VialService<'a> {
    ctx: KeyboardContext<'a>,
    vial_config: VialConfig<'static>,
    #[cfg(feature = "host_lock")]
    locker: crate::host::lock::HostLock<'a>,
}

impl<'a> VialService<'a> {
    pub fn new(keymap: &'a KeyMap<'a>, config: &RmkConfig<'static>) -> Self {
        Self {
            ctx: KeyboardContext::new(keymap),
            vial_config: config.vial_config,
            // Vial's poll cadence is ~100 ms (`VialCommand::UnlockPoll`).
            #[cfg(feature = "host_lock")]
            locker: crate::host::lock::HostLock::new(
                config.vial_config.unlock_keys,
                keymap,
                config.vial_config.insecure,
                embassy_time::Duration::from_millis(100),
            ),
        }
    }

    async fn process_via_packet(&self, report: &mut ViaReport) {
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
                            let layout_option = self.ctx.layout_options();
                            BigEndian::write_u32(&mut report.input_data[2..6], layout_option);
                        }
                        #[cfg(not(feature = "host_lock"))]
                        ViaKeyboardInfo::SwitchMatrixState => {
                            error!("It is not secure to use matrix tester without vial lock");
                        }
                        #[cfg(feature = "host_lock")]
                        ViaKeyboardInfo::SwitchMatrixState if self.locker.is_unlocked() => {
                            let bitmap = &mut report.input_data[2..];
                            self.ctx.read_matrix_state(bitmap);
                            // Vial wants each row's bytes big-endian (QMK matrix_row_t order).
                            let (rows, cols, _) = self.ctx.keymap_dimensions();
                            let row_len = cols.div_ceil(8);
                            if row_len > 1 {
                                let len = (rows * row_len).min(bitmap.len());
                                for row in bitmap[..len].chunks_mut(row_len) {
                                    row.reverse();
                                }
                            }
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
                            let _ = self.ctx.set_layout_options(layout_option).await;
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
                let _ = self.ctx.stage_action(layer, row, col, action).await;
            }
            ViaCommand::DynamicKeymapReset => {
                warn!("Dynamic keymap reset -- not supported")
            }
            ViaCommand::CustomSetValue => {
                if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION
                {
                    process_native_key_action_set(report, &self.ctx).await;
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION
                {
                    process_native_dynamic_action_set(report, &self.ctx).await;
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_COMBO_LAYER
                {
                    process_combo_layer_set(report, &self.ctx).await;
                } else {
                    // backlight/rgblight/rgb matrix/led matrix/audio settings here
                    warn!("Custom set value -- not supported")
                }
            }
            ViaCommand::CustomGetValue => {
                if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_BATTERY_HALVES
                {
                    let (mask, left, right) = self.ctx.battery_halves();
                    report.input_data[3] = ERGOHAVEN_BATTERY_HALVES_VERSION;
                    report.input_data[4] = mask;
                    report.input_data[5] = left;
                    report.input_data[6] = right;
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS
                {
                    init_native_key_action_response(report, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS);
                    LittleEndian::write_u16(&mut report.input_data[4..6], native_key_action_capabilities());
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION
                {
                    process_native_key_action_get(report, &self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NEXT_NATIVE_KEY_ACTION
                {
                    process_next_native_key_action_get(report, &self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NATIVE_DYNAMIC_ACTION
                {
                    process_native_dynamic_action_get(report, &self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_NEXT_NATIVE_DYNAMIC_ACTION
                {
                    process_next_native_dynamic_action_get(report, &self.ctx);
                } else if report.output_data[1] == ERGOHAVEN_CUSTOM_NAMESPACE
                    && report.output_data[2] == ERGOHAVEN_CUSTOM_COMBO_LAYER
                {
                    process_combo_layer_get(report, &self.ctx);
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
            #[cfg(feature = "host_lock")]
            ViaCommand::BootloaderJump if !self.locker.is_unlocked() => {
                warn!("Bootloader jump ignored while Vial is locked");
            }
            ViaCommand::BootloaderJump => {
                warn!("Bootloader jumping");
                #[cfg(feature = "storage")]
                crate::storage::flush_staged_writes().await;
                boot::jump_to_bootloader();
            }
            ViaCommand::DynamicKeymapMacroGetCount => {
                report.input_data[1] = MACRO_MAX_NUM as u8;
            }
            ViaCommand::DynamicKeymapMacroGetBufferSize => {
                BigEndian::write_u16(&mut report.input_data[1..3], MACRO_SPACE_SIZE as u16);
            }
            ViaCommand::DynamicKeymapMacroGetBuffer => {
                let offset = BigEndian::read_u16(&report.output_data[1..3]) as usize;
                let size = report.output_data[3] as usize;
                // The payload is `[4..4 + size]` of a 32-byte report.
                if size <= 28 {
                    let out = &mut report.input_data[4..4 + size];
                    self.ctx.keymap.macros(|m| {
                        let bytes = m.bytes().get(offset..).unwrap_or(&[]);
                        let n = bytes.len().min(size);
                        out[..n].copy_from_slice(&bytes[..n]);
                        out[n..].fill(0);
                    });
                } else {
                    report.input_data[0] = 0xFF;
                }
            }
            ViaCommand::DynamicKeymapMacroSetBuffer => {
                let offset = BigEndian::read_u16(&report.output_data[1..3]) as usize;
                let size = report.output_data[3] as usize;
                if size <= 28 {
                    // A save is one upload of many packets: persist it once, when the last arrives.
                    let upload = self
                        .ctx
                        .keymap
                        .macros(|m| m.write_upload(offset, &report.output_data[4..4 + size]));
                    #[cfg(feature = "storage")]
                    if let Some(changed) = upload
                        && crate::keyboard::macros::persist(self.ctx.keymap, changed)
                            .await
                            .is_err()
                    {
                        error!("Failed to save the macros");
                    }
                    #[cfg(not(feature = "storage"))]
                    if upload.is_some_and(|changed| !changed.is_empty()) {
                        warn!("Macros changed but not saved: there is no storage");
                    }
                } else {
                    report.input_data[0] = 0xFF;
                }
            }
            ViaCommand::DynamicKeymapMacroReset => {
                let changed = self.ctx.keymap.macros(|m| m.clear());
                #[cfg(feature = "storage")]
                if crate::keyboard::macros::persist(self.ctx.keymap, changed)
                    .await
                    .is_err()
                {
                    error!("Failed to save the macros");
                }
                #[cfg(not(feature = "storage"))]
                if !changed.is_empty() {
                    warn!("Macros changed but not saved: there is no storage");
                }
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
                    let (layer, in_layer) = (flat_index / (rows * cols), flat_index % (rows * cols));
                    let _ = self
                        .ctx
                        .stage_action(layer as u8, (in_layer / cols) as u8, (in_layer % cols) as u8, action)
                        .await;
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
                    #[cfg(feature = "host_lock")]
                    &self.locker,
                    &self.ctx,
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

impl VialService<'_> {
    /// Drive one Vial session against `rx`/`tx` (32-byte request → 32-byte
    /// response, processed in place). Returns on any read/write error;
    /// transport-specific reconnect lives in the caller.
    pub async fn run_session<R: Read, T: Write>(&self, rx: &mut R, tx: &mut T) {
        let mut buf = [0u8; 32];
        loop {
            if rx.read_exact(&mut buf).await.is_err() {
                return;
            }
            // Host data packets (time, layout, media) are one-way: no reply.
            if process_host_data_packet(&buf) {
                continue;
            }
            let mut report = ViaReport {
                input_data: buf,
                output_data: buf,
            };
            self.process_via_packet(&mut report).await;
            if tx.write_all(&report.input_data).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use rmk_types::action::{Action, KeyAction};
    use rmk_types::keycode::{HidKeyCode, KeyCode};
    use rmk_types::modifier::ModifierCombination;

    use super::*;
    use crate::config::{BehaviorConfig, PositionalConfig};
    use crate::keymap::{KeyMap, KeymapData};

    /// Build a minimal 1x1x1 keymap + `VialService` and run `f` against it.
    fn with_service<R>(f: impl FnOnce(&mut VialService) -> R) -> R {
        let mut data = KeymapData::new([[[KeyAction::No]]]);
        let mut behavior = BehaviorConfig::default();
        let positional = PositionalConfig::<1, 1>::default();
        let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
        let config = RmkConfig::default();
        let mut service = VialService::new(&keymap, &config);
        f(&mut service)
    }

    /// These tests run no storage task, so writes that wait for one would never be
    /// answered; the stand-in answers them.
    fn process(service: &mut VialService, report: &mut ViaReport) {
        block_on(embassy_futures::select::select(
            service.process_via_packet(report),
            crate::test_support::drain_flash_channel(),
        ));
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
        let config = RmkConfig {
            vial_config: VialConfig {
                firmware_version: 0x0000_0103,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut service = VialService::new(&keymap, &config);
        let mut output_data = [0u8; 32];
        output_data[0] = ViaCommand::GetKeyboardValue as u8;
        output_data[1] = ViaKeyboardInfo::FirmwareVersion as u8;
        let mut report = ViaReport {
            input_data: output_data,
            output_data,
        };

        process(&mut service, &mut report);

        assert_eq!(&report.input_data[2..6], &[0x00, 0x00, 0x01, 0x03]);
    }

    #[cfg(feature = "host_lock")]
    #[test]
    fn bootloader_jump_requires_vial_unlock() {
        use core::sync::atomic::Ordering;

        use crate::boot::BOOTLOADER_JUMPS;

        let jump = |insecure: bool| {
            let mut data = KeymapData::new([[[KeyAction::No]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
            let config = RmkConfig {
                vial_config: VialConfig {
                    insecure,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut service = VialService::new(&keymap, &config);
            let mut output_data = [0u8; 32];
            output_data[0] = ViaCommand::BootloaderJump as u8;
            let mut report = ViaReport {
                input_data: output_data,
                output_data,
            };
            let before = BOOTLOADER_JUMPS.load(Ordering::Relaxed);
            process(&mut service, &mut report);
            BOOTLOADER_JUMPS.load(Ordering::Relaxed) - before
        };

        assert_eq!(jump(false), 0, "a locked board must not jump to the bootloader");
        assert_eq!(jump(true), 1, "an unlocked board still jumps to the bootloader");
    }

    // `output_data` is [u8; 32], so the handler slices `output_data[4..4 + size]`.
    // size == 28 is the largest payload that fits (writes output_data[4..32]).
    #[test]
    fn macro_set_buffer_max_size_ok() {
        with_service(|service| {
            let mut report = macro_set_buffer_report(28);
            process(service, &mut report);
        });
    }

    // size == 29 slices output_data[4..33], which is out of bounds. The sibling
    // DynamicKeymapMacroGetBuffer handler already rejects size > 28 with 0xFF;
    // SetBuffer must do the same instead of panicking.
    #[test]
    fn macro_set_buffer_oversize_rejected() {
        with_service(|service| {
            let mut report = macro_set_buffer_report(29);
            process(service, &mut report);
            assert_eq!(report.input_data[0], 0xFF);
        });
    }

    #[cfg(feature = "storage")]
    type Part = crate::test_support::InMemoryFlash<16_384, 4_096, 1>;

    /// The packets of a Vial macro save: `slot0`, then every other slot empty, each
    /// slot ending in `0x00`, 28 bytes per packet.
    #[cfg(feature = "storage")]
    fn macro_upload(slot0: &[u8]) -> Vec<ViaReport> {
        let mut bytes = slot0.to_vec();
        bytes.resize(slot0.len() + MACRO_MAX_NUM, 0);
        bytes
            .chunks(28)
            .enumerate()
            .map(|(i, chunk)| {
                let mut report = macro_set_buffer_report_at((i * 28) as u16, chunk.len() as u8);
                report.output_data[4..4 + chunk.len()].copy_from_slice(chunk);
                report.input_data = report.output_data;
                report
            })
            .collect()
    }

    /// Run `body` against a Vial service over `part` with its storage task live, then
    /// boot `part` again and return the macro buffer it restores.
    #[cfg(feature = "storage")]
    fn with_storage(part: &Part, body: impl AsyncFnOnce(&VialService)) -> Vec<u8> {
        use embassy_futures::select::{Either, select};

        use crate::config::StorageConfig;
        use crate::core_traits::Runnable;
        use crate::storage::{Storage, async_flash_wrapper};

        crate::test_support::clear_flash_channel();
        crate::test_support::test_block_on(async {
            let config = StorageConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let mut storage = Box::pin(Storage::new(async_flash_wrapper(part.clone()), &config)).await;
            let mut data = KeymapData::new([[[KeyAction::No]]]);
            let mut behavior = BehaviorConfig::default();
            let keymap = KeyMap::new_from_storage(&mut data, Some(&mut storage), &mut behavior, &positional).await;
            let service = VialService::new(&keymap, &RmkConfig::default());
            if let Either::First(never) = select(storage.run(), body(&service)).await {
                never
            }

            let (flash, _) = storage.flash.destroy();
            let mut storage = Box::pin(Storage::new(flash.0, &config)).await;
            let mut data = KeymapData::new([[[KeyAction::No]]]);
            let mut behavior = BehaviorConfig::default();
            let keymap = KeyMap::new_from_storage(&mut data, Some(&mut storage), &mut behavior, &positional).await;
            keymap.macros(|m| m.bytes().to_vec())
        })
    }

    /// Save each of `slots` as macro 0, 2 s apart, and return the flash writes it took
    /// and the macro buffer the next boot restores.
    #[cfg(feature = "storage")]
    fn save_macros(slots: &[&[u8]]) -> (usize, Vec<u8>) {
        use embassy_time::{Duration, Timer};

        let part = Part::new();
        let mut writes = 0;
        let restored = with_storage(&part, async |service| {
            let before = part.writes();
            for (i, slot) in slots.iter().enumerate() {
                if i > 0 {
                    Timer::after(Duration::from_secs(2)).await;
                }
                for mut packet in macro_upload(slot) {
                    service.process_via_packet(&mut packet).await;
                    assert_eq!(part.writes(), before, "a macro packet reached flash");
                }
            }
            Timer::after(Duration::from_millis(2_900)).await;
            assert_eq!(part.writes(), before, "saved before the host went quiet for 3 s");
            Timer::after(Duration::from_millis(200)).await;
            writes = part.writes() - before;
        });
        (writes, restored)
    }

    #[cfg(feature = "storage")]
    #[test]
    fn macro_save_reaches_flash_once_after_the_host_goes_quiet() {
        let (writes, restored) = save_macros(&[b"first", b"second"]);
        assert!(restored.starts_with(b"second\0"));
        // The first save, replaced within the quiet period, costs no flash write.
        assert_eq!(writes, save_macros(&[b"second"]).0);
        assert!(writes > 0);
    }

    #[cfg(feature = "storage")]
    #[test]
    fn unfinished_macro_upload_is_not_saved() {
        use embassy_time::{Duration, Timer};

        let part = Part::new();
        let mut packets = macro_upload(b"partial");
        packets.pop();
        let restored = with_storage(&part, async |service| {
            let before = part.writes();
            for mut packet in packets {
                service.process_via_packet(&mut packet).await;
            }
            Timer::after(Duration::from_secs(10)).await;
            assert_eq!(part.writes(), before);
        });
        assert!(restored.iter().all(|&b| b == 0));
    }

    #[cfg(feature = "storage")]
    #[test]
    fn macro_reset_after_a_pending_save_wins() {
        use embassy_time::{Duration, Timer};

        let part = Part::new();
        let restored = with_storage(&part, async |service| {
            for mut packet in macro_upload(b"first") {
                service.process_via_packet(&mut packet).await;
            }
            let mut output_data = [0u8; 32];
            output_data[0] = ViaCommand::DynamicKeymapMacroReset as u8;
            let mut reset = ViaReport {
                input_data: output_data,
                output_data,
            };
            service.process_via_packet(&mut reset).await;
            Timer::after(Duration::from_secs(4)).await;
        });
        assert!(restored.iter().all(|&b| b == 0));
    }

    #[test]
    fn native_key_action_capability_is_versioned() {
        with_service(|service| {
            let mut report = custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION_CAPS);
            process(service, &mut report);
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
            process(service, &mut set_report);
            assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut get_report = custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NATIVE_KEY_ACTION);
            get_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            process(service, &mut get_report);
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
            process(service, &mut set_report);

            let mut scan_report = custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NEXT_NATIVE_KEY_ACTION);
            scan_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            process(service, &mut scan_report);
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
            process(service, &mut scan_report);
            assert_eq!(scan_report.input_data[4], NATIVE_KEY_ACTION_STATUS_END);
        });
    }

    #[test]
    fn native_dynamic_combo_output_set_and_get_round_trip() {
        with_service(|service| {
            let action = KeyAction::Single(Action::User(0x80));
            let mut set_report =
                native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT, 0, 0, action);
            process(service, &mut set_report);
            assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut get_report = native_dynamic_action_report(
                ViaCommand::CustomGetValue,
                NATIVE_DYNAMIC_ACTION_KIND_COMBO_OUTPUT,
                0,
                0,
            );
            process(service, &mut get_report);
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
            process(service, &mut set_report);
            assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            let mut get_report = combo_layer_report(ViaCommand::CustomGetValue, 0);
            process(service, &mut get_report);
            assert_eq!(get_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(decode_combo_layer_response(&get_report), Some(0));

            let mut clear_report = combo_layer_set_report(0, None);
            process(service, &mut clear_report);
            assert_eq!(clear_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

            process(service, &mut get_report);
            assert_eq!(decode_combo_layer_response(&get_report), None);
        });
    }

    #[test]
    fn combo_layer_rejects_out_of_range_layer() {
        with_service(|service| {
            let mut report = combo_layer_set_report(0, Some(1));
            process(service, &mut report);
            assert_eq!(report.input_data[4], NATIVE_KEY_ACTION_STATUS_INVALID_POSITION);
        });
    }

    #[test]
    fn standard_vial_combo_write_preserves_native_layer() {
        with_service(|service| {
            let mut set_layer = combo_layer_set_report(0, Some(0));
            process(service, &mut set_layer);
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
            process(service, &mut vial_set);

            let mut get_layer = combo_layer_report(ViaCommand::CustomGetValue, 0);
            process(service, &mut get_layer);
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
                process(service, &mut set_report);
                assert_eq!(set_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);

                let mut get_report = native_dynamic_action_report(
                    ViaCommand::CustomGetValue,
                    NATIVE_DYNAMIC_ACTION_KIND_MORSE,
                    0,
                    field,
                );
                process(service, &mut get_report);
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
            process(service, &mut set_combo);

            let morse_action = KeyAction::Single(Action::User(0x81));
            let mut set_morse = native_dynamic_action_set_report(NATIVE_DYNAMIC_ACTION_KIND_MORSE, 0, 2, morse_action);
            process(service, &mut set_morse);

            let mut scan_report =
                custom_report(ViaCommand::CustomGetValue, ERGOHAVEN_CUSTOM_NEXT_NATIVE_DYNAMIC_ACTION);
            scan_report.output_data[3] = ERGOHAVEN_NATIVE_KEY_ACTION_VERSION;
            process(service, &mut scan_report);
            assert_eq!(scan_report.input_data[4], NATIVE_KEY_ACTION_STATUS_OK);
            assert_eq!(LittleEndian::read_u16(&scan_report.input_data[5..7]), 0);
            assert_eq!(
                decode_native_action_response(&scan_report, NATIVE_KEY_ACTION_NEXT_PAYLOAD_OFFSET),
                combo_action
            );

            let combo_count = COMBO_MAX_NUM.min(u8::MAX as usize);
            LittleEndian::write_u16(&mut scan_report.output_data[4..6], combo_count as u16);
            process(service, &mut scan_report);
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
            process(service, &mut report);
            assert_eq!(report.input_data[4], NATIVE_KEY_ACTION_STATUS_INVALID_PAYLOAD);
        });
    }

    /// A `GetKeyboardValue(LayoutOptions)` (0x02 0x02) or
    /// `SetKeyboardValue(LayoutOptions)` (0x03 0x02) report carrying `value`
    /// as the big-endian payload.
    fn layout_options_report(command_id: u8, value: u32) -> ViaReport {
        let mut output_data = [0u8; 32];
        output_data[0] = command_id;
        output_data[1] = 0x02; // ViaKeyboardInfo::LayoutOptions
        BigEndian::write_u32(&mut output_data[2..6], value);
        ViaReport {
            input_data: output_data,
            output_data,
        }
    }

    // GetKeyboardValue(LayoutOptions) must return the value written by
    // SetKeyboardValue(LayoutOptions) instead of a hardcoded 0, so the Vial
    // GUI shows the user's selection again after a reconnect.
    #[test]
    fn layout_options_set_then_get_roundtrip() {
        with_service(|service| {
            let mut set = layout_options_report(0x03, 0x00C0_FFEE);
            process(service, &mut set);

            let mut get = layout_options_report(0x02, 0);
            process(service, &mut get);
            assert_eq!(BigEndian::read_u32(&get.input_data[2..6]), 0x00C0_FFEE);
        });
    }
}
