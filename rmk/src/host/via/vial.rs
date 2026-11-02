use byteorder::{BigEndian, ByteOrder, LittleEndian};
use rmk_types::action::KeyAction;
use rmk_types::constants::{COMBO_MAX_LENGTH, COMBO_MAX_NUM, FORK_MAX_NUM, MORSE_MAX_NUM};
use rmk_types::fork::{Fork, ForkOptions, StateBits};
use rmk_types::modifier::ModifierCombination;
use rmk_types::morse::{DOUBLE_TAP, HOLD, HOLD_AFTER_TAP, Morse, TAP};
use rmk_types::protocol::vial::{
    SettingKey, VIAL_EP_SIZE, VIAL_KEY_OVERRIDE_OPTION_BITS, VIAL_PROTOCOL_VERSION, VialCommand, VialDynamic,
};

use crate::config::VialConfig;
use crate::hid::ViaReport;
use crate::host::context::KeyboardContext;
use crate::host::settings;
use crate::host::via::keycode_convert::{from_via_keycode, to_via_keycode};

/// Note: vial uses little endian, while via uses big endian
pub(crate) async fn process_vial<'a>(
    report: &mut ViaReport,
    vial_config: &VialConfig<'a>,
    #[cfg(feature = "host_lock")] locker: &crate::host::lock::HostLock<'_>,
    ctx: &KeyboardContext<'_>,
) {
    // report.output_data[0] == 0xFE -> vial commands
    let vial_command = report.output_data[1].into();
    debug!("Received vial command: {:?}", vial_command);
    match vial_command {
        VialCommand::GetKeyboardId => {
            // Returns vial protocol version + vial keyboard id
            LittleEndian::write_u32(&mut report.input_data[0..4], VIAL_PROTOCOL_VERSION);
            report.input_data[4..12].clone_from_slice(vial_config.vial_keyboard_id);
            debug!("Vial return: {:?}", report.input_data);
        }
        VialCommand::GetSize => {
            LittleEndian::write_u32(&mut report.input_data[0..4], vial_config.vial_keyboard_def.len() as u32);
        }
        VialCommand::GetKeyboardDef => {
            let page = LittleEndian::read_u16(&report.output_data[2..4]) as usize;
            let start = page * VIAL_EP_SIZE;
            let mut end = start + VIAL_EP_SIZE;
            let vial_keyboard_def = &vial_config.vial_keyboard_def;
            if end < start || start >= vial_keyboard_def.len() {
                return;
            }
            if end > vial_keyboard_def.len() {
                end = vial_keyboard_def.len();
            }
            vial_keyboard_def[start..end].iter().enumerate().for_each(|(i, v)| {
                report.input_data[i] = *v;
            });
            debug!(
                "Vial return: page:{} start:{} end: {}, data: {:?}",
                page, start, end, report.input_data
            );
        }
        VialCommand::GetUnlockStatus => {
            // Reset all data to 0xFF(it's required!)
            report.input_data.fill(0xFF);
            #[cfg(feature = "host_lock")]
            {
                // Unlocked
                report.input_data[0] = locker.is_unlocked() as u8;
                // Unlock in progress
                report.input_data[1] = locker.is_unlocking() as u8;
                // Unlock keys
                for (idx, (row, col)) in vial_config.unlock_keys.iter().enumerate() {
                    report.input_data[2 + idx * 2] = *row;
                    report.input_data[3 + idx * 2] = *col;
                }
            }
            #[cfg(not(feature = "host_lock"))]
            {
                // Unlocked
                report.input_data[0] = 1;
                // Unlock in progress
                report.input_data[1] = 0;
                warn!("Vial lock feature is not enabled");
            }
        }
        VialCommand::UnlockStart => {
            #[cfg(feature = "host_lock")]
            locker.unlocking();
            #[cfg(not(feature = "host_lock"))]
            error!("Vial lock feature is not enabled");
        }
        VialCommand::UnlockPoll => {
            #[cfg(feature = "host_lock")]
            {
                // Match Vial QMK: once unlocked, a poll must report
                // `unlocked=1, in_progress=0`. Re-arming unconditionally would
                // keep `in_progress` set forever, and hosts that wait for it to
                // clear (Entropy ≥ 0.4.0) never finish the unlock.
                if !locker.is_unlocked() {
                    locker.unlocking();
                }
                let counter = locker.check_unlock();
                report.input_data[0] = locker.is_unlocked() as u8;
                report.input_data[1] = locker.is_unlocking() as u8;
                report.input_data[2] = counter;
            }
            #[cfg(not(feature = "host_lock"))]
            error!("Vial lock feature is not enabled");
        }
        VialCommand::Lock => {
            #[cfg(feature = "host_lock")]
            locker.lock();
            #[cfg(not(feature = "host_lock"))]
            error!("Vial lock feature is not enabled");
        }
        VialCommand::BehaviorSettingQuery => {
            report.input_data.fill(0xFF);
            let after = u16::from_le_bytes([report.output_data[2], report.output_data[3]]);
            for (slot, key) in report
                .input_data
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(settings::keys_after(after, vial_config.device_settings))
            {
                slot.copy_from_slice(&key.to_le_bytes());
            }
        }
        VialCommand::GetBehaviorSetting => {
            report.input_data.fill(0xFF);
            let id = u16::from_le_bytes([report.output_data[2], report.output_data[3]]);
            let value = &mut report.input_data[1..];
            report.input_data[0] = match settings::get(ctx, vial_config.device_settings, id, value) {
                Some(_) => 0,
                None => 0xFF,
            };
        }
        VialCommand::SetBehaviorSetting => {
            let id = u16::from_le_bytes([report.output_data[2], report.output_data[3]]);
            let value = report.output_data[4..]
                .try_into()
                .expect("a Vial report holds a whole value");
            let served = settings::set(ctx, vial_config.device_settings, id, value).await;
            // Only a board's own setting reports its status.
            if SettingKey::from(id) == SettingKey::None {
                report.input_data[0] = if served { 0 } else { 0xFF };
            }
        }
        VialCommand::DynamicEntryOp => {
            let vial_dynamic = report.output_data[2].into();
            match vial_dynamic {
                VialDynamic::DynamicVialGetNumberOfEntries => {
                    debug!("DynamicEntryOp - DynamicVialGetNumberOfEntries");
                    report.input_data[0] = core::cmp::min(MORSE_MAX_NUM, 255) as u8; // Tap dance entries
                    report.input_data[1] = core::cmp::min(COMBO_MAX_NUM, 255) as u8; // Combo entries
                    report.input_data[2] = core::cmp::min(FORK_MAX_NUM, 255) as u8; // Key override entries
                    report.input_data[31] = 1 // Enable caps word
                }
                VialDynamic::DynamicVialMorseGet => {
                    debug!("DynamicEntryOp - DynamicVialMorseGet");
                    report.input_data[0] = 0; // Index 0 is the return code, 0 means success

                    let morse_idx = report.output_data[3];
                    if let Some(morse) = ctx.get_morse(morse_idx) {
                        // Pack morse data into report
                        LittleEndian::write_u16(
                            &mut report.input_data[1..3],
                            to_via_keycode(morse.get(TAP).map_or(KeyAction::No, KeyAction::Single)),
                        );
                        LittleEndian::write_u16(
                            &mut report.input_data[3..5],
                            to_via_keycode(morse.get(HOLD).map_or(KeyAction::No, KeyAction::Single)),
                        );
                        LittleEndian::write_u16(
                            &mut report.input_data[5..7],
                            to_via_keycode(morse.get(DOUBLE_TAP).map_or(KeyAction::No, KeyAction::Single)),
                        );
                        LittleEndian::write_u16(
                            &mut report.input_data[7..9],
                            to_via_keycode(morse.get(HOLD_AFTER_TAP).map_or(KeyAction::No, KeyAction::Single)),
                        );
                        let timeout_ms = morse.profile.hold_timeout_ms().unwrap_or(250);
                        LittleEndian::write_u16(&mut report.input_data[9..11], timeout_ms);
                    } else {
                        report.input_data[1..11].fill(0);
                    }
                }
                VialDynamic::DynamicVialMorseSet => {
                    debug!("DynamicEntryOp - DynamicVialMorseSet");
                    report.input_data[0] = 0; // Index 0 is the return code, 0 means success

                    let morse_idx = report.output_data[3];

                    if (morse_idx as usize) < ctx.morses_len() {
                        // Extract morse (also known as "tap dance" in vial)
                        let tap = from_via_keycode(LittleEndian::read_u16(&report.output_data[4..6]));
                        let hold = from_via_keycode(LittleEndian::read_u16(&report.output_data[6..8]));
                        let double_tap = from_via_keycode(LittleEndian::read_u16(&report.output_data[8..10]));
                        let hold_after_tap = from_via_keycode(LittleEndian::read_u16(&report.output_data[10..12]));
                        let timeout_ms = LittleEndian::read_u16(&report.output_data[12..14]);

                        let _ = ctx
                            .update_morse(morse_idx, |morse: &mut Morse| {
                                let _ = morse.put(TAP, tap.to_action());
                                let _ = morse.put(DOUBLE_TAP, double_tap.to_action());
                                let _ = morse.put(HOLD, hold.to_action());
                                let _ = morse.put(HOLD_AFTER_TAP, hold_after_tap.to_action());
                                morse.profile.set_hold_timeout_ms(timeout_ms);
                                morse.profile.set_gap_timeout_ms(timeout_ms);
                            })
                            .await;
                    }
                }
                VialDynamic::DynamicVialComboGet => {
                    debug!("DynamicEntryOp - DynamicVialComboGet");
                    report.input_data[0] = 0; // Index 0 is the return code, 0 means success

                    let combo_idx = report.output_data[3] as usize;
                    ctx.with_combos(|combos| {
                        if let Some(Some(combo)) = combos.get(combo_idx) {
                            // Combo components
                            for i in 0..COMBO_MAX_LENGTH {
                                let kc = combo.config.actions.get(i).copied().unwrap_or(KeyAction::No);
                                LittleEndian::write_u16(
                                    &mut report.input_data[1 + i * 2..3 + i * 2],
                                    to_via_keycode(kc),
                                );
                            }
                            // Combo output
                            LittleEndian::write_u16(
                                &mut report.input_data[1 + COMBO_MAX_LENGTH * 2..3 + COMBO_MAX_LENGTH * 2],
                                to_via_keycode(combo.config.output),
                            );
                        } else {
                            report.input_data[1..3 + COMBO_MAX_LENGTH * 2].fill(0);
                        }
                    });
                }
                VialDynamic::DynamicVialComboSet => {
                    debug!("DynamicEntryOp - DynamicVialComboSet");
                    report.input_data[0] = 0; // Index 0 is the return code, 0 means success

                    use rmk_types::combo::Combo as ComboConfig;

                    let combo_idx = report.output_data[3];

                    let mut actions = heapless::Vec::<KeyAction, COMBO_MAX_LENGTH>::new();
                    let mut overflow = false;
                    for i in 0..COMBO_MAX_LENGTH {
                        let action =
                            from_via_keycode(LittleEndian::read_u16(&report.output_data[4 + i * 2..6 + i * 2]));
                        if !action.is_empty() && actions.push(action).is_err() {
                            overflow = true;
                            break;
                        }
                    }
                    if overflow {
                        return;
                    }
                    let output = from_via_keycode(LittleEndian::read_u16(
                        &report.output_data[4 + COMBO_MAX_LENGTH * 2..6 + COMBO_MAX_LENGTH * 2],
                    ));
                    let layer = ctx.with_combos(|combos| {
                        combos
                            .get(combo_idx as usize)
                            .and_then(Option::as_ref)
                            .and_then(|combo| combo.config.layer)
                    });
                    let config = ComboConfig { actions, output, layer };
                    let _ = ctx.set_combo(combo_idx, config).await;
                }
                VialDynamic::DynamicVialKeyOverrideGet => {
                    debug!("DynamicEntryOp - DynamicVialKeyOverrideGet");
                    report.input_data[0] = 0; // Index 0 is the return code, 0 means success

                    let fork_idx = report.output_data[3];
                    let entry = ctx
                        .get_fork(fork_idx)
                        .as_ref()
                        .map(fork_to_vial_key_override)
                        .unwrap_or_default();
                    report.input_data[1..11].copy_from_slice(&entry);
                }
                VialDynamic::DynamicVialKeyOverrideSet => {
                    debug!("DynamicEntryOp - DynamicVialKeyOverrideSet");
                    report.input_data[0] = 0; // Index 0 is the return code, 0 means success

                    let fork_idx = report.output_data[3];
                    let fork = vial_key_override_to_fork(&report.output_data[4..14]);
                    let _ = ctx.set_fork(fork_idx, fork).await;
                }
                VialDynamic::Unhandled => {
                    warn!("DynamicEntryOp - Unhandled -- subcommand not recognized");
                    report.input_data.fill(0x00);
                }
            }
        }
        VialCommand::GetEncoder => {
            let layer = report.output_data[2];
            let index = report.output_data[3];
            debug!("Received Vial - GetEncoder, encoder idx: {} at layer: {}", index, layer);

            // Get encoder value
            if let Some(encoder_action) = ctx.get_encoder(layer, index) {
                let clockwise = to_via_keycode(encoder_action.clockwise);
                let counter_clockwise = to_via_keycode(encoder_action.counter_clockwise);
                BigEndian::write_u16(&mut report.input_data[0..2], counter_clockwise);
                BigEndian::write_u16(&mut report.input_data[2..4], clockwise);
                return;
            }

            // Clear returned value, aka `KeyAction::No`
            report.input_data.fill(0x0);
        }
        VialCommand::SetEncoder => {
            let layer = report.output_data[2];
            let index = report.output_data[3];
            let clockwise = report.output_data[4];
            debug!(
                "Received Vial - SetEncoder, encoder idx: {} clockwise: {} at layer: {}",
                index, clockwise, layer
            );
            let keycode = BigEndian::read_u16(&report.output_data[5..7]);
            let action = from_via_keycode(keycode);
            info!("Setting encoder action (clockwise: {}): {:?}", clockwise, action);
            let _ = ctx.set_encoder_direction(layer, index, clockwise == 1, action).await;
        }
        _ => (),
    }
}

/// Serialize a fork into Vial's 10-byte `vial_key_override_entry_t` (little endian).
///
/// Best-effort where forks are richer than key overrides: LED/mouse match
/// conditions and a custom `negative_output` are not representable.
fn fork_to_vial_key_override(fork: &Fork) -> [u8; 10] {
    let mut entry = [0u8; 10];
    if fork.trigger == KeyAction::No {
        return entry;
    }
    LittleEndian::write_u16(&mut entry[0..2], to_via_keycode(fork.trigger));
    LittleEndian::write_u16(&mut entry[2..4], to_via_keycode(fork.positive_output));
    let layers = if fork.layers == u32::MAX {
        0xFFFF
    } else {
        fork.layers as u16
    };
    LittleEndian::write_u16(&mut entry[4..6], layers);
    entry[6] = fork.match_any.modifiers.into_bits();
    entry[7] = fork.match_none.modifiers.into_bits();
    entry[8] = fork.suppressed_modifiers.into_bits();
    entry[9] = fork.options.into_bits() & VIAL_KEY_OVERRIDE_OPTION_BITS;
    entry
}

/// Deserialize Vial's 10-byte `vial_key_override_entry_t` into a fork.
fn vial_key_override_to_fork(entry: &[u8]) -> Fork {
    let trigger = from_via_keycode(LittleEndian::read_u16(&entry[0..2]));
    if trigger == KeyAction::No {
        return Fork::empty();
    }
    let layers = LittleEndian::read_u16(&entry[4..6]);
    Fork {
        trigger,
        // Pass the trigger through when the condition is not met
        negative_output: trigger,
        positive_output: from_via_keycode(LittleEndian::read_u16(&entry[2..4])),
        match_any: StateBits {
            modifiers: ModifierCombination::from_bits(entry[6]),
            ..Default::default()
        },
        match_none: StateBits {
            modifiers: ModifierCombination::from_bits(entry[7]),
            ..Default::default()
        },
        suppressed_modifiers: ModifierCombination::from_bits(entry[8]),
        layers: if layers == 0xFFFF { u32::MAX } else { layers as u32 },
        options: ForkOptions::from_bits(entry[9] & VIAL_KEY_OVERRIDE_OPTION_BITS),
        // QMK key overrides don't cascade
        bindable: false,
    }
}

#[cfg(test)]
#[cfg(feature = "storage")]
mod tests {
    use rmk_types::action::Action;
    use rmk_types::combo::Combo as ComboConfig;
    use rmk_types::keycode::{HidKeyCode, KeyCode};
    use rmk_types::protocol::vial::{
        VIAL_KEY_OVERRIDE_ALL_ACTIVATIONS, VIAL_KEY_OVERRIDE_ENABLED, VIAL_KEY_OVERRIDE_ONE_MOD,
    };
    use sequential_storage::map::Value;

    use super::*;
    use crate::COMBO_MAX_LENGTH;
    use crate::storage::StorageValue;
    #[test]
    fn test_combo_serialization_deserialization() {
        let mut actions = heapless::Vec::<KeyAction, COMBO_MAX_LENGTH>::new();
        actions
            .push(KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::Kc1))))
            .unwrap();
        let combo_config = ComboConfig {
            actions,
            output: KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::Space))),
            layer: None,
        };
        let mut buffer = [0u8; 64];
        let storage_data = StorageValue::Combo(combo_config.clone());
        let serialized_size = Value::serialize_into(&storage_data, &mut buffer).unwrap();
        // Deserialization
        let deserialized_data = StorageValue::deserialize_from(&buffer[..serialized_size]).unwrap();
        // Validation
        match deserialized_data {
            (StorageValue::Combo(deserialized_config), _) => {
                assert_eq!(deserialized_config, combo_config);
            }
            _ => panic!("Expected Combo"),
        }
    }

    // Shift+Backspace -> Delete, the canonical key override
    fn shift_backspace_wire_entry() -> [u8; 10] {
        let mut wire = [0u8; 10];
        LittleEndian::write_u16(&mut wire[0..2], HidKeyCode::Backspace as u16);
        LittleEndian::write_u16(&mut wire[2..4], HidKeyCode::Delete as u16);
        LittleEndian::write_u16(&mut wire[4..6], 0xFFFF); // all layers
        wire[6] = 0x22; // trigger_mods: LShift | RShift
        wire[7] = 0x44; // negative_mod_mask: LAlt | RAlt
        wire[8] = 0x02; // suppressed_mods: LShift
        wire[9] = VIAL_KEY_OVERRIDE_ENABLED | VIAL_KEY_OVERRIDE_ALL_ACTIVATIONS | VIAL_KEY_OVERRIDE_ONE_MOD;
        wire
    }

    #[test]
    fn test_key_override_wire_roundtrip() {
        let wire = shift_backspace_wire_entry();
        let fork = vial_key_override_to_fork(&wire);

        assert!(fork.options.enabled());
        assert!(!fork.bindable);
        assert_eq!(
            fork.trigger,
            KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::Backspace)))
        );
        assert_eq!(fork.negative_output, fork.trigger);
        assert_eq!(
            fork.positive_output,
            KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::Delete)))
        );
        assert_eq!(fork.match_any.modifiers.into_bits(), 0x22);
        assert_eq!(fork.match_none.modifiers.into_bits(), 0x44);
        assert_eq!(fork.suppressed_modifiers.into_bits(), 0x02);
        assert!(fork.options.one_mod());
        assert_eq!(fork.layers, u32::MAX); // 0xFFFF = all layers

        assert_eq!(fork_to_vial_key_override(&fork), wire);
    }

    #[test]
    fn test_key_override_all_mode_layers_and_free_suppress() {
        let mut wire = shift_backspace_wire_entry();
        wire[9] &= !VIAL_KEY_OVERRIDE_ONE_MOD; // AND semantics (QMK default)
        LittleEndian::write_u16(&mut wire[4..6], 0x0005); // layers 0 and 2 only
        wire[8] = 0x44; // suppress LAlt|RAlt, none of which are trigger mods

        let fork = vial_key_override_to_fork(&wire);
        assert!(!fork.options.one_mod()); // QMK AND semantics
        assert_eq!(fork.layers, 0x0005);
        assert_eq!(fork.suppressed_modifiers.into_bits(), 0x44);

        assert_eq!(fork_to_vial_key_override(&fork), wire);
    }

    #[test]
    fn test_key_override_option_bits_roundtrip() {
        let mut wire = shift_backspace_wire_entry();
        // Only mod-down activation + both "no" bits
        wire[9] = VIAL_KEY_OVERRIDE_ENABLED | 0b0011_0010;

        let fork = vial_key_override_to_fork(&wire);
        assert!(!fork.options.activate_on_trigger_down());
        assert!(fork.options.activate_on_required_mod_down());
        assert!(!fork.options.activate_on_negative_mod_up());
        assert!(fork.options.no_reregister_trigger());
        assert!(fork.options.no_unregister_on_other_key_down());
        assert!(!fork.options.one_mod());
        assert!(fork.options.enabled());

        assert_eq!(fork_to_vial_key_override(&fork), wire);
    }

    #[test]
    fn test_key_override_empty_and_disabled() {
        // An all-zero wire entry maps to the empty fork and back
        let fork = vial_key_override_to_fork(&[0u8; 10]);
        assert_eq!(fork, Fork::empty());
        assert_eq!(fork_to_vial_key_override(&fork), [0u8; 10]);

        // A disabled entry keeps its configuration
        let mut wire = shift_backspace_wire_entry();
        wire[9] &= !VIAL_KEY_OVERRIDE_ENABLED;
        let fork = vial_key_override_to_fork(&wire);
        assert!(!fork.options.enabled());
        assert_eq!(fork.match_any.modifiers.into_bits(), 0x22);
        assert_eq!(fork_to_vial_key_override(&fork), wire);
    }

    #[test]
    fn test_fork_serialization_deserialization() {
        let wire = shift_backspace_wire_entry();
        let fork = vial_key_override_to_fork(&wire);
        let mut buffer = [0u8; 64];
        let storage_data = StorageValue::Fork(fork);
        let serialized_size = Value::serialize_into(&storage_data, &mut buffer).unwrap();
        let deserialized_data = StorageValue::deserialize_from(&buffer[..serialized_size]).unwrap();
        match deserialized_data {
            (StorageValue::Fork(deserialized_fork), _) => {
                assert_eq!(deserialized_fork, fork);
            }
            _ => panic!("Expected Fork"),
        }
    }
}

#[cfg(test)]
#[cfg(feature = "host_lock")]
mod lock_tests {
    use embassy_futures::block_on;

    use super::*;
    use crate::config::{BehaviorConfig, PositionalConfig};
    use crate::event::KeyboardEvent;
    use crate::host::lock::HostLock;
    use crate::keymap::{KeyMap, KeymapData};

    fn vial_report(command: VialCommand) -> ViaReport {
        let mut output_data = [0u8; 32];
        output_data[0] = 0xFE;
        output_data[1] = command as u8;
        ViaReport {
            input_data: output_data,
            output_data,
        }
    }

    #[test]
    fn factory_df_unlock_is_physical_and_advertised_for_all_profiles() {
        use rmk_types::action::Action;
        use rmk_types::keycode::{HidKeyCode, KeyCode};

        use crate::event::KeyboardEventSource;

        let profiles = [
            ("k03", [(2, 2), (2, 1)], [(0, 5), (0, 4)]),
            ("k04", [(2, 2), (2, 1)], [(1, 4), (1, 3)]),
            ("k04_mini", [(1, 2), (1, 1)], [(0, 5), (0, 4)]),
            ("k04_micro", [(1, 2), (1, 1)], [(0, 4), (0, 3)]),
            ("imperial44", [(1, 2), (1, 1)], [(0, 5), (0, 4)]),
            ("op36", [(1, 2), (1, 1)], [(0, 4), (0, 3)]),
            ("velvet", [(1, 3), (1, 4)], [(0, 0), (0, 1)]),
            ("op36_qube", [(1, 2), (1, 1)], [(0, 4), (0, 3)]),
            ("k03_qube", [(2, 2), (2, 1)], [(0, 5), (0, 4)]),
            ("velvet_qube", [(1, 3), (1, 4)], [(0, 0), (0, 1)]),
            ("imperial44_qube", [(1, 2), (1, 1)], [(0, 5), (0, 4)]),
            ("k04_qube", [(2, 2), (2, 1)], [(0, 5), (0, 4)]),
            ("k04_qube_mini", [(1, 2), (1, 1)], [(0, 5), (0, 4)]),
            ("k04_qube_micro", [(1, 2), (1, 1)], [(0, 4), (0, 3)]),
        ];
        for (name, unlock_keys, old_keys) in profiles {
            let mut data = KeymapData::new([[[KeyAction::No; 6]; 3]; 2]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<3, 6>::default();
            let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
            let ctx = KeyboardContext::new(&keymap);
            let config = VialConfig {
                unlock_keys: &unlock_keys,
                ..Default::default()
            };
            let locker = HostLock::new(&unlock_keys, &keymap, false, embassy_time::Duration::from_millis(100));
            let mut command = |command| {
                let mut report = vial_report(command);
                block_on(process_vial(&mut report, &config, &locker, &ctx));
                report.input_data
            };
            let status = command(VialCommand::GetUnlockStatus);
            assert_eq!(
                &status[..6],
                &[
                    0,
                    0,
                    unlock_keys[0].0,
                    unlock_keys[0].1,
                    unlock_keys[1].0,
                    unlock_keys[1].1
                ],
                "{name}"
            );
            assert!(status[6..].iter().all(|byte| *byte == 0xFF), "{name}");
            command(VialCommand::UnlockStart);
            assert_eq!(&command(VialCommand::UnlockPoll)[..3], &[0, 1, 2], "{name}: no keys");
            for (row, col) in old_keys {
                keymap.update_matrix_state(&KeyboardEvent::key(row, col, true));
            }
            assert_eq!(&command(VialCommand::UnlockPoll)[..3], &[0, 1, 2], "{name}: old chord");
            for (row, col) in old_keys {
                keymap.update_matrix_state(&KeyboardEvent::key(row, col, false));
            }
            let [(d_row, d_col), (f_row, f_col)] = unlock_keys;
            keymap.update_matrix_state(&KeyboardEvent::key(d_row, d_col, true));
            assert_eq!(&command(VialCommand::UnlockPoll)[..3], &[0, 1, 1], "{name}: D only");
            keymap.update_matrix_state(&KeyboardEvent::key(d_row, d_col, false));
            keymap.update_matrix_state(&KeyboardEvent::key(f_row, f_col, true));
            assert_eq!(&command(VialCommand::UnlockPoll)[..3], &[0, 1, 1], "{name}: F only");
            keymap.update_matrix_state(&KeyboardEvent::key(f_row, f_col, false));

            // Remap the physical unlock positions to No; move D/F elsewhere,
            // on both layers. Unlock must not follow those current keycodes.
            for layer in 0..2 {
                for (row, col) in unlock_keys {
                    keymap.set_action_at(KeyboardEventSource::key_pos(col, row), layer, KeyAction::No);
                }
                for ((row, col), code) in old_keys.into_iter().zip([HidKeyCode::D, HidKeyCode::F]) {
                    keymap.set_action_at(
                        KeyboardEventSource::key_pos(col, row),
                        layer,
                        KeyAction::Single(Action::Key(KeyCode::Hid(code))),
                    );
                }
            }
            keymap.activate_layer(1);
            for (row, col) in old_keys {
                keymap.update_matrix_state(&KeyboardEvent::key(row, col, true));
            }
            assert_eq!(
                &command(VialCommand::UnlockPoll)[..3],
                &[0, 1, 2],
                "{name}: remapped D/F elsewhere"
            );
            for (row, col) in old_keys {
                keymap.update_matrix_state(&KeyboardEvent::key(row, col, false));
            }
            for (row, col) in unlock_keys {
                keymap.update_matrix_state(&KeyboardEvent::key(row, col, true));
            }
            assert_eq!(
                &command(VialCommand::UnlockPoll)[..3],
                &[1, 0, 0],
                "{name}: physical D/F"
            );
            assert_eq!(
                &command(VialCommand::UnlockPoll)[..3],
                &[1, 0, 0],
                "{name}: finished remains finished"
            );
            for (row, col) in unlock_keys {
                keymap.update_matrix_state(&KeyboardEvent::key(row, col, false));
            }
            command(VialCommand::Lock);
            assert_eq!(command(VialCommand::GetUnlockStatus)[0], 0, "{name}: relock");
            assert_eq!(
                &command(VialCommand::UnlockPoll)[..3],
                &[0, 1, 2],
                "{name}: relocked, released"
            );
        }
    }

    #[test]
    fn unlock_poll_reports_finished_unlock_like_vial_qmk() {
        let mut data = KeymapData::new([[[KeyAction::No; 2]]]);
        let mut behavior = BehaviorConfig::default();
        let positional = PositionalConfig::<1, 2>::default();
        let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
        let ctx = KeyboardContext::new(&keymap);
        let unlock_keys = [(0, 0), (0, 1)];
        let vial_config = VialConfig {
            unlock_keys: &unlock_keys,
            ..Default::default()
        };
        let locker = HostLock::new(&unlock_keys, &keymap, false, embassy_time::Duration::from_millis(100));
        let mut start = vial_report(VialCommand::UnlockStart);
        block_on(process_vial(&mut start, &vial_config, &locker, &ctx));
        let mut poll = || {
            let mut report = vial_report(VialCommand::UnlockPoll);
            block_on(process_vial(&mut report, &vial_config, &locker, &ctx));
            (report.input_data[0], report.input_data[1], report.input_data[2])
        };

        assert_eq!(poll(), (0, 1, 2));

        keymap.update_matrix_state(&KeyboardEvent::key(0, 0, true));
        keymap.update_matrix_state(&KeyboardEvent::key(0, 1, true));
        // The poll that sees every key held already reports the unlock
        assert_eq!(poll(), (1, 0, 0));
        // and later polls must not re-enter the in-progress state
        assert_eq!(poll(), (1, 0, 0));
    }
}
